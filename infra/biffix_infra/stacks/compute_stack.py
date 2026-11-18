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

Every compute environment boots its hosts from a launch template whose user data turns off
privileged containers in the ECS agent and installs the pinned host AWS CLI
(`biffix_infra/host/install_aws_cli.sh`) from the reference bucket's `host/` prefix; Nextflow
bind-mounts the CLI into each task (`aws.batch.cliPath`), so task images need none. The template
also encrypts every host's root volume and requires IMDSv2.
"""

from __future__ import annotations

import posixpath
from typing import Any

from aws_cdk import Stack
from aws_cdk import aws_batch as batch
from aws_cdk import aws_ec2 as ec2
from aws_cdk import aws_iam as iam
from aws_cdk import aws_logs as logs
from aws_cdk import aws_s3 as s3
from constructs import Construct

from biffix_infra.config import HOST_AWS_CLI_PATH, DeployConfig, HostAwsCli
from biffix_infra.constructs.batch_access import BATCH_LOG_GROUP, grant_coordinator_batch
from biffix_infra.constructs.batch_queue import BatchQueueClass
from biffix_infra.paths import HOST_DIR
from biffix_infra.wiring import LOG_RETENTION

_INSTALL_AWS_CLI = HOST_DIR / "install_aws_cli.sh"
# The ECS agent setting that makes it refuse to start a privileged container, appended to its config
# before the agent starts (on the ECS-optimized AMI the agent starts after cloud-init has run the
# user data).
DISABLE_PRIVILEGED = "ECS_DISABLE_PRIVILEGED=true"


def host_install_header(
    cli: HostAwsCli,
    *,
    bucket_name: str,
    region: str,
    cli_dir: str = posixpath.dirname(posixpath.dirname(HOST_AWS_CLI_PATH)),
) -> list[str]:
    """The variable assignments the host boot script (`install_aws_cli.sh`) reads, as the user data
    sets them. One function, so the boot-script test runs the script against exactly the names and
    per-arch values the stack emits."""
    return [
        f"BIFFIX_HOST_BUCKET={bucket_name}",
        f"BIFFIX_REGION={region}",
        f"BIFFIX_AWS_CLI_DIR={cli_dir}",
        f"BIFFIX_AWS_CLI_KEY_PREFIX={posixpath.dirname(cli.key('x86_64'))}/",
        f"BIFFIX_AWS_CLI_SHA256_X86_64={cli.sha256_x86_64}",
        f"BIFFIX_AWS_CLI_SHA256_AARCH64={cli.sha256_aarch64}",
    ]


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

        host_user_data = self._host_user_data(config, host_bucket)
        self.queue_classes: dict[str, BatchQueueClass] = {
            qc.name: BatchQueueClass(
                self,
                f"Queue-{qc.name}",
                qc=qc,
                queue_name=config.queue_name(qc),
                vpc=vpc,
                # Workers go where `worker_egress` puts them; the coordinator keeps the NAT (it
                # talks to the API, Batch and the engines' package hosts).
                subnets=None if qc.coordinator else worker_subnets,
                user_data=host_user_data,
                host_bucket=host_bucket,
                instance_families=config.instance_families,
            )
            for qc in config.queue_classes
        }
        self.queues: dict[str, batch.JobQueue] = {
            name: qc.queue for name, qc in self.queue_classes.items()
        }
        self.coordinator_queue = self.queues[config.coordinator_queue_class.name]

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
        up every project's runs. Called by the api stack (which owns the per-project roles), so
        this references queue TOKENS and CDK infers the edge."""
        grant_coordinator_batch(
            role,
            stack=self,
            queue_arns=[
                queue.job_queue_arn
                for queue in self.queues.values()
                if queue is not self.coordinator_queue
            ],
            task_prefix=self._task_prefix,
            project_id=project_id,
            task_log_group=task_log_group,
        )

    @staticmethod
    def _host_user_data(config: DeployConfig, host_bucket: s3.IBucket) -> ec2.MultipartUserData:
        """The boot script every host runs, as the MIME multipart user data Batch requires (Batch
        merges its own ECS agent part in). Two parts today: turning off privileged containers, then
        the host AWS CLI install; the read-only reference mount (Mountpoint for S3) is added as a
        further part."""
        # Nothing biffix runs on Batch is privileged: the coordinator job definition is not
        # (constructs/coordinator.py), the backend never registers a privileged task definition
        # (and IAM refuses one, ControlPlaneGrants.launch), and the image builds that need a
        # privileged Docker daemon run in CodeBuild, not on Batch. So every host, coordinator and
        # worker alike, refuses one: a job definition registered outside biffix's control still
        # cannot get a privileged container on hosts every project shares.
        harden = ec2.UserData.for_linux()
        harden.add_commands(f"echo {DISABLE_PRIVILEGED} >> /etc/ecs/ecs.config")
        install = ec2.UserData.for_linux()
        install.add_commands(
            *host_install_header(
                config.host_aws_cli, bucket_name=host_bucket.bucket_name, region=config.region
            ),
            _INSTALL_AWS_CLI.read_text(),
        )
        user_data = ec2.MultipartUserData()
        user_data.add_part(ec2.MultipartBody.from_user_data(harden))
        user_data.add_part(ec2.MultipartBody.from_user_data(install))
        return user_data
