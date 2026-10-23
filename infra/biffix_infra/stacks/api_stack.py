"""ApiStack — the submit pipeline.

The thing that submits (`BackendApi`: backend Lambda + HTTP API + Cognito authorizer), the
identities it submits as and the job definitions it submits (a `Coordinator` per project: its role
and job definition template) all live here, together, on purpose:

- Every runtime role sits at this DAG sink, so `resource.grant_*(role)` from compute/project/
  persistence attaches to the role's own policy with no cross-stack cycle, so the stacks need no
  hand-built dependency graph.
- The coordinator job definition's ARN carries its revision and churns on every image bump. Keeping
  it INTRA-STACK (next to the backend that grants SubmitJob on it) means the churning ARN is never a
  CloudFormation export, so it can be replaced in place (an export in use cannot be updated).

A run's blast radius is one project: its coordinator runs the workflow's own code, so
each project has its own coordinator role, granted only that project's bucket, key, data sources and
run role. Each project's template runs as its role, and a variant (cloned from a template by the
backend) inherits it.

The backend routes every request to a project (a version's, or a run's) and launches the run from
that project's template. It learns the projects from one SSM parameter per project,
`/biffix/<tenant>/projects/<id>` (`Coordinator`), not from its environment: Lambda caps a
function's environment at 4 KB, and the parameters keep it the same size however many projects a
deploy has. Only the control plane (the backend and sweep Lambdas, and the image builds) is granted
every project (`ControlPlaneGrants`); each coordinator and run role still reaches its own project
only.
"""

from __future__ import annotations

import json
from typing import Any

from aws_cdk import CfnOutput, Duration, Stack
from aws_cdk import aws_cognito as cognito
from aws_cdk import aws_ec2 as ec2
from aws_cdk import aws_ecs as ecs
from aws_cdk import aws_iam as iam
from aws_cdk import aws_rds as rds
from aws_cdk import aws_secretsmanager as secretsmanager
from aws_cdk import aws_sns as sns
from constructs import Construct

from biffix_infra.config import DeployConfig
from biffix_infra.constructs.alarms import Alarms
from biffix_infra.constructs.backend_api import BackendApi
from biffix_infra.constructs.backend_sweep import BackendSweep
from biffix_infra.constructs.batch_access import BATCH_LOG_GROUP, log_group_arn
from biffix_infra.constructs.control_plane import ControlPlaneGrants
from biffix_infra.constructs.coordinator import Coordinator
from biffix_infra.lambda_artifacts import rust_lambda_code
from biffix_infra.rbac import RBAC_GROUP_ROLES
from biffix_infra.stacks.build_stack import BuildStack
from biffix_infra.stacks.compute_stack import ComputeStack
from biffix_infra.stacks.project_stack import ProjectStack
from biffix_infra.wiring import wire_db


class ApiStack(Stack):
    def __init__(
        self,
        scope: Construct,
        cid: str,
        *,
        config: DeployConfig,
        vpc: ec2.IVpc,
        db: rds.DatabaseCluster,
        db_secret: secretsmanager.ISecret,
        capability_secret: secretsmanager.ISecret,
        user_pool: cognito.IUserPool,
        user_pool_client: cognito.IUserPoolClient,
        cli_user_pool_client: cognito.IUserPoolClient | None,
        cli_redirect_uris: list[str] | None,
        permissions_boundary: iam.IManagedPolicy,
        compute: ComputeStack,
        projects: list[ProjectStack],
        default_project: ProjectStack,
        coordinator_image: ecs.ContainerImage,
        coordinator_image_uri: str,
        worker_image_repo_uri: str,
        build: BuildStack,
        alerts_topic: sns.ITopic,
        **kwargs: Any,
    ) -> None:
        super().__init__(scope, cid, **kwargs)

        # ── One coordinator (role + job definition template + context) per project ──
        # name → queue ARN for every WORKER queue-class, in config order; the entrypoint resolves
        # the queue the engine submits child jobs to (--aws-batch-job-queue takes an ARN), by
        # default the first. The coordinator's queue-class is left out: child jobs never go there.
        queue_map = self.to_json_string(
            {
                name: queue.job_queue_arn
                for name, queue in compute.queues.items()
                if name != config.coordinator_queue_class.name
            }
        )
        self.projects_path = f"/biffix/{config.tenant}/projects/"
        coordinators = {
            project.project_id: Coordinator(
                self,
                f"Coordinator-{project.project_id}",
                tenant=config.tenant,
                region=config.region,
                project=project,
                compute=compute,
                permissions_boundary=permissions_boundary,
                image=coordinator_image,
                queue_map=queue_map,
                projects_path=self.projects_path,
            )
            for project in projects
        }
        self.coordinator_roles = {pid: c.role for pid, c in coordinators.items()}
        self.coordinator_job_definitions = {
            pid: c.job_definition for pid, c in coordinators.items()
        }
        # Each project's context, as the backend reads it (see the module docstring).
        self.project_contexts = {pid: c.context for pid, c in coordinators.items()}
        # The project a request that names none is routed to.
        self.default_project_id = default_project.project_id
        grants = ControlPlaneGrants(
            self,
            tenant=config.tenant,
            projects=projects,
            coordinators=coordinators,
            projects_path=self.projects_path,
            default_project_id=self.default_project_id,
            compute=compute,
            build=build,
            # One image build per architecture BuildStack builds for; config names them (literal
            # names, not the projects' cross-stack tokens).
            build_project_names={
                "BIFFIX_RUNTIME_BUILD_PROJECT": config.runtime_build_project_name,
                "BIFFIX_IMAGE_IMPORT_PROJECT": config.image_import_project_name,
                **{
                    f"BIFFIX_IMAGE_BUILD_PROJECT_{arch.upper()}": config.image_build_project_name(
                        arch
                    )
                    for arch in build.build_projects
                },
            },
            coordinator_image_uri=coordinator_image_uri,
            worker_image_repo_uri=worker_image_repo_uri,
            capability_secret=capability_secret,
        )

        # ── Backend API — Rust Lambda behind an HTTP API Gateway ──
        backend = BackendApi(
            self,
            "BackendApi",
            code=rust_lambda_code("biffix-backend-lambda"),
            vpc=vpc,
            region=config.region,
            user_pool=user_pool,
            user_pool_client=user_pool_client,
            cli_user_pool_client=cli_user_pool_client,
            cli_redirect_uris=cli_redirect_uris,
            public_base_url=config.public_base_url,
            cognito_domain_prefix=config.cognito_domain_prefix,
        )
        self.backend_lambda = backend.function
        self.http_api = backend.http_api
        # The backend reads the projection (sqlx over the DB port) and reads the DB secret.
        wire_db(self.backend_lambda, db, db_secret)
        # Backend runtime wiring: the submit pipeline resolves the
        # version's project (its bucket, run role, template, flags and data sources, from the
        # project's SSM parameter), reads that bucket, and launches the coordinator job, which
        # reports to this API (it posts {url}/events, which the HTTP API serves at the root — no
        # dependency on the edge/CloudFront domain).
        grants.read_projects(self.backend_lambda)
        # Registration starts the variant build a new runtime needs; submit launches on the variant.
        grants.build_variants(self.backend_lambda)
        grants.launch(self.backend_lambda, self.http_api.api_endpoint)
        self._configure_backend(config)
        self._grant_backend(projects, user_pool)

        # ── Backend sweep — scheduled work (provenance crates, builds, parked launches) ──
        sweep = BackendSweep(
            self,
            "BackendSweep",
            code=rust_lambda_code("biffix-backend-sweep"),
            vpc=vpc,
            build_project_names=config.build_project_names,
        )
        self.sweep_lambda = sweep.function
        grants.build_variants(self.sweep_lambda)
        wire_db(self.sweep_lambda, db, db_secret)
        grants.read_projects(self.sweep_lambda)
        # Its S3 writes are each run's `provenance/` folder (the RO-Crate) and, from a launch, the
        # `inputs/` folder (`images.json`), in whichever project the run is in. It reads nothing
        # else a launch needs from S3 (no grant_backend_read).
        for project in projects:
            project.grant_backend_run_provenance(self.sweep_lambda)
        grants.launch(self.sweep_lambda, self.http_api.api_endpoint)

        self._wire_alerts(alerts_topic, config.alarm_prefix)

        CfnOutput(self, "ApiEndpoint", value=self.http_api.api_endpoint)

    def _configure_backend(self, config: DeployConfig) -> None:
        """The deploy's settings the backend serves: retention, queue-classes, roles."""
        fn = self.backend_lambda
        # The launch form shows the deploy's retention period for intermediate files.
        if config.intermediates_retention_days is not None:
            fn.add_environment("BIFFIX_RETENTION_DAYS", str(config.intermediates_retention_days))
        queue_class_names = [qc.name for qc in config.queue_classes if not qc.coordinator]
        fn.add_environment("BIFFIX_QUEUE_CLASSES", ",".join(queue_class_names))
        # Each physical queue's capacity: the fallback spot/on-demand label for a host whose own
        # EC2 lifecycle is unknown. Compact (the queue-name prefix once, then each capacity's
        # queue-classes): Lambda caps a function's whole environment at 4 KB, and a map keyed by
        # full queue names grows by ~45 bytes per queue-class.
        by_capacity: dict[str, list[str]] = {}
        for qc in config.queue_classes:
            by_capacity.setdefault(qc.capacity, []).append(qc.name)
        fn.add_environment(
            "BIFFIX_QUEUE_CAPACITIES",
            json.dumps(
                {"prefix": config.queue_name_prefix}
                | {cap: sorted(names) for cap, names in by_capacity.items()},
                sort_keys=True,
                separators=(",", ":"),
            ),
        )
        fn.add_environment("BIFFIX_AUTHENTICATED_DEFAULT_ROLE", config.authenticated_default_role)
        # The Cognito groups IdentityStack creates, from the same one mapping (rbac.py): a group
        # can never exist without the backend knowing its role.
        fn.add_environment("BIFFIX_GROUP_ROLE_MAP", json.dumps(RBAC_GROUP_ROLES, sort_keys=True))

    def _grant_backend(self, projects: list[ProjectStack], user_pool: cognito.IUserPool) -> None:
        """What the backend reads and writes beyond a launch. Identity-based, on its own role: this
        stack is downstream of every resource, so the constructs' own grant_* methods work with no
        cycle and keep the action set and resource ARNs in lockstep with the resource."""
        fn = self.backend_lambda
        for project in projects:
            # Read the project bucket to HEAD-check submit input URIs (grant_read adds kms:Decrypt
            # for a CMK bucket; grant_backend_read also covers the imported-bucket BYOK case).
            project.grant_backend_read(fn)
            project.grant_data_sources_read(fn)
            # Sign presigned PUTs for S3-staged workflow source (private-repo support): the
            # backend role must itself hold s3:PutObject on the staging prefix. Scoped to
            # `workflow-sources/*`.
            project.grant_backend_source_staging(fn)
            # Run browser uploads under `uploads/` (presigned multipart parts), only into a bucket
            # that takes them (it has the CORS rule).
            if project.uploads_enabled:
                project.grant_backend_uploads(fn)
        # Resolve a run submitter's email/name at submit (AdminGetUser); scoped to this user pool.
        user_pool.grant(fn, "cognito-idp:AdminGetUser")
        # The backend serves task/coordinator logs through its own auth (so a viewer needs a biffix
        # login, not an AWS console account): it reads the Batch job log streams via GetLogEvents.
        # Scoped to the fleet-wide `/aws/batch/job` group (coordinators) and every project's task
        # log group (Snakemake and Nextflow tasks), which it reads from the project's SSM parameter.
        # The stream name is derived server-side from the run/task the caller is authorized to
        # see, never client-supplied. No DescribeLogStreams — the backend never enumerates, it
        # reads the stream from the projection.
        fn.add_to_role_policy(
            iam.PolicyStatement(
                actions=["logs:GetLogEvents"],
                resources=[
                    log_group_arn(self, f"{group}:log-stream:*")
                    for group in (BATCH_LOG_GROUP, *(p.task_log_group_name for p in projects))
                ],
            )
        )
        # Engine ingest (POST /events) accepts a task_submitted claim on a Batch job only when the
        # job's `biffix:run` tag names the claiming run, which it reads with DescribeJobs. Describe
        # has no resource-level permissions.
        fn.add_to_role_policy(iam.PolicyStatement(actions=["batch:DescribeJobs"], resources=["*"]))

    def _wire_alerts(self, topic: sns.ITopic, prefix: str) -> None:
        """Alarm on the serving path and the sweep (each alarm named under the deploy's alarm
        `prefix`), and let the backend manage the topic's email subscribers (the Settings page
        lists, adds and removes them; the SNS subscriptions are the only record)."""
        self.backend_lambda.add_environment("BIFFIX_ALERT_TOPIC_ARN", topic.topic_arn)
        self.backend_lambda.add_to_role_policy(
            iam.PolicyStatement(
                actions=["sns:ListSubscriptionsByTopic", "sns:Subscribe"],
                resources=[topic.topic_arn],
            )
        )
        # Unsubscribe authorizes on the SUBSCRIPTION ARN, which is `<topic arn>:<id>`.
        self.backend_lambda.add_to_role_policy(
            iam.PolicyStatement(actions=["sns:Unsubscribe"], resources=[f"{topic.topic_arn}:*"])
        )
        alarms = Alarms(self, topic=topic, prefix=prefix)
        alarms.add(
            "BackendErrorsAlarm",
            description="The backend API Lambda is failing repeatedly.",
            metric=self.backend_lambda.metric_errors(period=Duration.minutes(15), statistic="Sum"),
            threshold=5,
        )
        alarms.add(
            "SweepErrorsAlarm",
            description="The backend sweep Lambda has failed on three consecutive ticks: parked "
            "runs are not launched, builds not followed, or provenance not written.",
            metric=self.sweep_lambda.metric_errors(period=Duration.minutes(5), statistic="Sum"),
            threshold=1,
            evaluation_periods=3,
        )
        alarms.add(
            "Api5xxAlarm",
            description="The API is returning server errors (5xx) to callers.",
            metric=self.http_api.metric_server_error(period=Duration.minutes(5), statistic="Sum"),
            threshold=10,
        )
