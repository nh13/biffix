"""One project's coordinator: the role its runs' coordinators run as, the job definition templates
they are cloned from (tasks mode's, and single-job mode's when the deploy has a single-job
queue-class), and the project's context the backend reads (ApiStack's module docstring says why
they live in the api stack)."""

from __future__ import annotations

import json
from typing import TYPE_CHECKING, cast

from aws_cdk import Duration, Size, Tags, Token
from aws_cdk import aws_batch as batch
from aws_cdk import aws_ecs as ecs
from aws_cdk import aws_iam as iam
from aws_cdk import aws_ssm as ssm
from constructs import Construct

from biffix_infra.constructs.batch_access import PROJECT_TAG
from biffix_infra.host_boot import HOST_STATE_DIR, INSTANCE_STORE_MARKER, SCRATCH_DIR

if TYPE_CHECKING:
    from biffix_infra.stacks.compute_stack import ComputeStack
    from biffix_infra.stacks.project_stack import ProjectStack

# Attempts of a coordinator job: the first plus one Batch retry after an infrastructure failure.
# The run's capability token must outlive every attempt: change the backend's copies of these two
# with them (`crates/biffix-backend/src/auth/capability.rs`).
COORDINATOR_ATTEMPTS = 2
# Each attempt's timeout.
COORDINATOR_TIMEOUT_HOURS = 24
# A single-job job definition's placeholder size: the backend overrides it at submit with the whole
# host's vCPUs and memory (containerOverrides.resourceRequirements).
SINGLE_JOB_PLACEHOLDER_CPUS = 1
SINGLE_JOB_PLACEHOLDER_MEMORY_GIB = 2
# The device apptainer mounts each task's SIF image through (squashfuse), passed into the job.
FUSE_DEVICE = "/dev/fuse"
# A Standard SSM String parameter's largest value.
_SSM_STANDARD_MAX_BYTES = 4096
# The longest a token in a project context (a bucket name, a role or job-definition ARN) resolves
# to: well over a CDK-named role's or job definition's ARN (~100-150 characters).
_TOKEN_MAX_CHARS = 256


class Coordinator(Construct):
    """`project`'s coordinator role (`role`), the job definition template its runs' coordinators
    are cloned from (`job_definition`, `biffix-<tenant>-coordinator-<project>`), with `single_job`
    the template its single-job runs are cloned from (`single_job_job_definition`,
    `biffix-<tenant>-coordinator-single-<project>`; else None), and its context parameter
    (`context`, `<projects_path><project>`)."""

    def __init__(
        self,
        scope: Construct,
        cid: str,
        *,
        tenant: str,
        region: str,
        project: ProjectStack,
        compute: ComputeStack,
        permissions_boundary: iam.IManagedPolicy,
        image: ecs.ContainerImage,
        queue_map: str,
        projects_path: str,
        single_job: bool = False,
        scratch_dir: str = SCRATCH_DIR,
    ) -> None:
        super().__init__(scope, cid)
        pid = project.project_id
        self.role = iam.Role(
            self,
            "Role",
            assumed_by=iam.ServicePrincipal("ecs-tasks.amazonaws.com"),
            permissions_boundary=permissions_boundary,
        )
        Tags.of(self.role).add(PROJECT_TAG, pid)
        # Grants attach to the role directly — no string ARNs, no per-project inline policy —
        # and name this project's resources only: its bucket, key, data sources and task log
        # group. It passes no role: its child jobs run on the backend's task job definitions,
        # which already name the project's run role.
        compute.grant_coordinator(
            self.role,
            project_id=pid,
            task_log_group=project.task_log_group_name,
        )
        project.grant_coordinator_access(self.role)

        # The template: runs never use it directly. A run executes on a coordinator variant (the
        # base image + the version's engine runtime layer), and Batch cannot override a job's
        # image, so the backend registers one job definition per variant by cloning this one with
        # the image swapped. Everything else (role, command, resources, retries) is the template's.
        self.job_definition = batch.EcsJobDefinition(
            self,
            "JobDefinition",
            # A stable name for humans/console; the backend describes it by its revisioned ARN.
            job_definition_name=f"biffix-{tenant}-coordinator-{pid}",
            container=batch.EcsEc2ContainerDefinition(
                self,
                "Container",
                image=image,
                # The image carries no ENTRYPOINT on purpose (see docker/coordinator/Dockerfile):
                # the task job definitions the backend registers run their own command, so the
                # coordinator command is expressed here.
                command=["biffix-coordinator"],
                cpu=2,
                memory=Size.gibibytes(4),
                job_role=self.role,
                # The worker image is per version (its default image, or the variant itself), so
                # the backend passes BIFFIX_WORKER_IMAGE at submit rather than baking one in here.
                environment={
                    "BIFFIX_QUEUE_CLASS_MAP": queue_map,
                    # --aws-batch-region is required and has no env-var support in the plugin.
                    "BIFFIX_REGION": region,
                    # The project's own task log group: Nextflow task jobs log there
                    # (`aws.batch.logsGroup`), out of other projects' coordinators' reach.
                    "BIFFIX_TASK_LOG_GROUP": project.task_log_group_name,
                },
            ),
            retry_attempts=COORDINATOR_ATTEMPTS,
            retry_strategies=_coordinator_retry_strategies(),
            timeout=Duration.hours(COORDINATOR_TIMEOUT_HOURS),
        )
        self.single_job_job_definition = (
            self._single_job_template(
                tenant=tenant,
                region=region,
                project=project,
                image=image,
                scratch_dir=scratch_dir,
            )
            if single_job
            else None
        )
        self.context = self._context(project, projects_path)

    def _single_job_template(
        self,
        *,
        tenant: str,
        region: str,
        project: ProjectStack,
        image: ecs.ContainerImage,
        scratch_dir: str,
    ) -> batch.EcsJobDefinition:
        """The template a single-job run's coordinator variant is cloned from: the whole run (the
        engine and every task it runs, each in apptainer nested in the job's container) in
        one job on a single-job queue-class's whole host. The tasks-mode template's image, command,
        retries and timeout, but:

        - it runs as the project's RUN BASE ROLE: it submits no Batch job, so it needs none of the
          coordinator role's Batch grants, only the project's data and the image pulls the run
          base role gets in such a deploy (ApiStack);
        - `/dev/fuse` is passed in (apptainer mounts each SIF through squashfuse) and an init
          process reaps the tasks' processes; never privileged (the hosts refuse it anyway);
        - the host's instance-store `/scratch` is its work root, read-write (BIFFIX_SCRATCH_ROOT);
        - the host's own state directory (`HOST_STATE_DIR`) is mounted READ-ONLY, so the job can
          see the marker the host's boothook writes once the instance store is mounted
          (BIFFIX_INSTANCE_STORE_MARKER) but no run can remove or fake it;
        - its size is a placeholder: the backend asks for the whole host at submit."""
        container = batch.EcsEc2ContainerDefinition(
            self,
            "SingleJobContainer",
            image=image,
            command=["biffix-coordinator"],
            cpu=SINGLE_JOB_PLACEHOLDER_CPUS,
            memory=Size.gibibytes(SINGLE_JOB_PLACEHOLDER_MEMORY_GIB),
            job_role=project.run_base_role,
            privileged=False,
            linux_parameters=batch.LinuxParameters(
                self, "SingleJobLinuxParameters", init_process_enabled=True
            ),
            volumes=[
                batch.EcsVolume.host(
                    name="scratch",
                    host_path=scratch_dir,
                    container_path=scratch_dir,
                    readonly=False,
                ),
                batch.EcsVolume.host(
                    name="biffix-host-state",
                    host_path=HOST_STATE_DIR,
                    container_path=HOST_STATE_DIR,
                    readonly=True,
                ),
            ],
            # No BIFFIX_QUEUE_CLASS_MAP: a single-job run submits no task to a queue.
            environment={
                "BIFFIX_REGION": region,
                "BIFFIX_TASK_LOG_GROUP": project.task_log_group_name,
                # The scratch root and the instance-store marker, named here once (host_boot) so
                # the coordinator need not hard-code what the host's boothook sets up.
                "BIFFIX_SCRATCH_ROOT": scratch_dir,
                "BIFFIX_INSTANCE_STORE_MARKER": INSTANCE_STORE_MARKER,
            },
        )
        job_definition = batch.EcsJobDefinition(
            self,
            "SingleJobDefinition",
            job_definition_name=f"biffix-{tenant}-coordinator-single-{project.project_id}",
            container=container,
            retry_attempts=COORDINATOR_ATTEMPTS,
            retry_strategies=_coordinator_retry_strategies(),
            timeout=Duration.hours(COORDINATOR_TIMEOUT_HOURS),
        )
        # `/dev/fuse`, set on the resource: aws-cdk-lib's `batch.DevicePermission` values are
        # lowercase ("read", "write"), and Batch takes only READ, WRITE and MKNOD.
        cfn = cast(batch.CfnJobDefinition, job_definition.node.default_child)
        cfn.add_property_override(
            "ContainerProperties.LinuxParameters.Devices",
            [
                {
                    "HostPath": FUSE_DEVICE,
                    "ContainerPath": FUSE_DEVICE,
                    "Permissions": ["READ", "WRITE"],
                }
            ],
        )
        return job_definition

    def grant_pass_roles(self, grantee: iam.IGrantable) -> None:
        """Let `grantee` register a clone of a template: a clone passes both of its roles, the
        job role and the execution role (the single-job template's job role is the project's run
        base role, which a launch passes too, `ControlPlaneGrants._register_tasks`: granted here
        as well, so this grant is complete on its own)."""
        self.role.grant_pass_role(grantee.grant_principal)
        self.job_definition.container.execution_role.grant_pass_role(grantee.grant_principal)
        if self.single_job_job_definition is not None:
            container = self.single_job_job_definition.container
            container.execution_role.grant_pass_role(grantee.grant_principal)
            if container.job_role is not None:
                container.job_role.grant_pass_role(grantee.grant_principal)

    def _context(self, project: ProjectStack, projects_path: str) -> ssm.StringParameter:
        """`project`'s context, as the backend reads it: an SSM String parameter
        `/biffix/<tenant>/projects/<id>` holding one JSON object (the backend's `ProjectContext`;
        `crates/biffix-backend/tests/fixtures/project_context.json` is its golden example):

        - `id`, `bucket`, `run_role_arn` (the run base role its child jobs run as),
        - `coordinator_job_definition` (its template's revisioned ARN, which a launch clones),
        - `single_job_job_definition` (the single-job template's revisioned ARN, which a
          single-job launch clones; null in a deploy with no single-job queue-class),
        - `task_log_group`,
        - `uploads`, `viewer_cors` (whether its bucket has the browser-upload and the alignment
          viewer's CORS rules), and `data_sources` (the file picker's, as `DataSource.to_backend`).

        Created here, not in the project's stack, so the template ARN, which churns on every image
        bump, stays an intra-stack token rather than a CloudFormation export. A Standard parameter
        holds 4 KB; a project with a couple of data sources uses ~800 bytes, and synth refuses a
        context that could outgrow it (`_context_size_bound`) rather than fail mid-deploy."""
        context: dict[str, object] = {
            "id": project.project_id,
            "bucket": project.bucket.bucket_name,
            "run_role_arn": project.run_base_role.role_arn,
            "coordinator_job_definition": self.job_definition.job_definition_arn,
            "single_job_job_definition": (
                self.single_job_job_definition.job_definition_arn
                if self.single_job_job_definition is not None
                else None
            ),
            "task_log_group": project.task_log_group_name,
            "uploads": project.uploads_enabled,
            "viewer_cors": project.viewer_cors,
            "data_sources": [s.to_backend() for s in project.data_sources],
        }
        bound = _context_size_bound(context)
        if bound > _SSM_STANDARD_MAX_BYTES:
            raise ValueError(
                f"project {project.project_id!r}: its context could be {bound} bytes, over the "
                f"{_SSM_STANDARD_MAX_BYTES}-byte SSM parameter it is written to; shorten its data "
                "sources' descriptions or register fewer data sources"
            )
        return ssm.StringParameter(
            self,
            "Context",
            parameter_name=f"{projects_path}{project.project_id}",
            description=f"biffix project `{project.project_id}`: its context for the backend",
            string_value=_json_with_tokens(context),
            tier=ssm.ParameterTier.STANDARD,
        )


def _coordinator_retry_strategies() -> list[batch.RetryStrategy]:
    """Both templates' infrastructure retry layer: an attempt whose host went away (or whose image
    could not be pulled, so nothing ran) is retried by Batch under the same job id; any other
    failure is the run's and ends it (the engine retries its own tasks)."""
    return [
        batch.RetryStrategy.of(
            batch.Action.RETRY, batch.Reason.custom(on_status_reason="Host EC2*")
        ),
        batch.RetryStrategy.of(batch.Action.RETRY, batch.Reason.CANNOT_PULL_CONTAINER),
        batch.RetryStrategy.of(batch.Action.EXIT, batch.Reason.custom(on_reason="*")),
    ]


def _context_size_bound(context: dict[str, object]) -> int:
    """An upper bound on `context`'s size as written (`_json_with_tokens`) once its tokens resolve
    at deploy: each unresolved string counted at `_TOKEN_MAX_CHARS`."""

    def bound(value: object) -> object:
        if isinstance(value, str) and Token.is_unresolved(value):
            return "x" * _TOKEN_MAX_CHARS
        if isinstance(value, dict):
            return {k: bound(v) for k, v in value.items()}
        if isinstance(value, list):
            return [bound(v) for v in value]
        return value

    return len(_json_with_tokens(bound(context)).encode())


def _json_with_tokens(value: object) -> str:
    """`value` as compact JSON, its CDK tokens (bucket names, ARNs) resolved at deploy.
    `Stack.to_json_string` would drop the keys whose value is None, but the backend's project
    context spells an absent value as `null`. The tokens' values (bucket names, ARNs) need no JSON
    escaping, so embedding them in the JSON text is safe."""
    return json.dumps(value, separators=(",", ":"))
