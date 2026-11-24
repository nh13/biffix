"""AWS Batch EC2 queue classes.

The compute stack owns the Batch fleet: one `BatchQueueClass` (compute environment + job queue) per
queue-class. Worker jobs need no job definition of this stack's: the backend registers a task job
definition per (project, tenant image, engine) at launch (named with
`DeployNames.task_job_definition_prefix`), and the engines' AWS Batch executors submit those.
It does NOT own the coordinator role or the coordinator job definition — those live in the `api`
stack next to the backend that submits them, so the coordinator's churning revisioned job-def ARN
never crosses a stack boundary. The Batch IAM knowledge the coordinator role needs is exposed here
as `grant_coordinator(role)` (`constructs.batch_access`), called by the api stack; because the
role lives downstream of this stack, the grant references queue TOKENS directly and CDK infers the
edge — no string-constructed ARNs.

Every compute environment boots its hosts from a launch template whose user data
(`biffix_infra.host_boot`) turns off privileged containers and installs the pinned host AWS CLI; a
single-job queue-class's hosts also set up the relaxed seccomp profile and the instance-store
`/scratch` before Docker starts. The template also encrypts every host's root volume and requires
IMDSv2.
"""

from __future__ import annotations

from typing import Any

from aws_cdk import Stack
from aws_cdk import aws_batch as batch
from aws_cdk import aws_ec2 as ec2
from aws_cdk import aws_iam as iam
from aws_cdk import aws_logs as logs
from aws_cdk import aws_s3 as s3
from constructs import Construct

from biffix_infra.config import DeployConfig
from biffix_infra.constructs.batch_access import BATCH_LOG_GROUP, grant_coordinator_batch
from biffix_infra.constructs.batch_queue import BatchQueueClass
from biffix_infra.host_boot import host_user_data
from biffix_infra.wiring import LOG_RETENTION


class ComputeStack(Stack):
    def __init__(
        self,
        scope: Construct,
        cid: str,
        *,
        vpc: ec2.IVpc,
        config: DeployConfig,
        host_bucket: s3.IBucket,
        worker_subnets: ec2.SubnetSelection | None = None,
        **kwargs: Any,
    ) -> None:
        super().__init__(scope, cid, **kwargs)
        self._task_prefix = config.task_job_definition_prefix

        def boot(*, single_job: bool) -> ec2.MultipartUserData:
            return host_user_data(
                config.host_aws_cli,
                bucket_name=host_bucket.bucket_name,
                region=config.region,
                single_job=single_job,
            )

        user_data = boot(single_job=False)
        single_job_user_data = (
            boot(single_job=True) if config.single_job_queue_classes else user_data
        )
        self.queue_classes: dict[str, BatchQueueClass] = {
            qc.name: BatchQueueClass(
                self,
                f"Queue-{qc.name}",
                qc=qc,
                queue_name=config.queue_name(qc),
                vpc=vpc,
                # Workers go where `worker_egress` puts them. The coordinator keeps the NAT (it
                # talks to the API, Batch and the engines' package hosts), and so does a single-job
                # host: its job is a run's coordinator too (it reports to the API, which isolated
                # worker subnets cannot reach) as well as all of its tasks.
                subnets=None if qc.coordinator or qc.single_job else worker_subnets,
                user_data=single_job_user_data if qc.single_job else user_data,
                host_bucket=host_bucket,
                instance_families=config.instance_families,
            )
            for qc in config.queue_classes
        }
        self.queues: dict[str, batch.JobQueue] = {
            name: qc.queue for name, qc in self.queue_classes.items()
        }
        self.coordinator_queue = self.queues[config.coordinator_queue_class.name]
        # name → queue for each single-job queue-class, in config order: the backend submits a
        # single-job run to one (ControlPlaneGrants.launch); no coordinator may.
        self.single_job_queues: dict[str, batch.JobQueue] = {
            qc.name: self.queues[qc.name] for qc in config.single_job_queue_classes
        }
        # name → queue for each queue-class coordinators submit tasks to, in config order: every
        # one but the coordinator's and the single-job ones.
        self.worker_queues: dict[str, batch.JobQueue] = {
            qc.name: self.queues[qc.name] for qc in config.worker_queue_classes
        }

        # Coordinators log to Batch's default group, which Batch creates on first use with no
        # retention; the backend reads run logs from it (`aws::BATCH_LOG_GROUP`). Set its retention
        # in place (it may already exist, so it is not a LogGroup resource this stack could own).
        logs.LogRetention(
            self,
            "BatchJobLogRetention",
            log_group_name=BATCH_LOG_GROUP,
            retention=LOG_RETENTION,
        )

    def grant_coordinator(self, role: iam.IRole, *, project_id: str, task_log_group: str) -> None:
        """Grant a project's coordinator role every Batch permission the engine head job needs on
        this fleet's worker queues (`grant_coordinator_batch`): never the coordinator queue, where
        child jobs never go, so a workflow cannot spend its on-demand capacity or fill it and hold
        up every project's runs, nor a single-job queue, whose whole hosts are for the whole runs
        the backend submits. Called by the api stack (which owns the per-project roles), so this
        references queue TOKENS and CDK infers the edge."""
        grant_coordinator_batch(
            role,
            stack=self,
            queue_arns=[queue.job_queue_arn for queue in self.worker_queues.values()],
            task_prefix=self._task_prefix,
            project_id=project_id,
            task_log_group=task_log_group,
        )
