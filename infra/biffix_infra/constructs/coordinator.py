"""One project's coordinator: the role its runs' coordinators run as, the job definition template
they are cloned from, and the project's context the backend reads (ApiStack's module docstring says
why all three live in the api stack)."""

from __future__ import annotations

import json
from typing import TYPE_CHECKING

from aws_cdk import Duration, Size, Tags, Token
from aws_cdk import aws_batch as batch
from aws_cdk import aws_ecs as ecs
from aws_cdk import aws_iam as iam
from aws_cdk import aws_ssm as ssm
from constructs import Construct

from biffix_infra.constructs.batch_access import PROJECT_TAG

if TYPE_CHECKING:
    from biffix_infra.stacks.compute_stack import ComputeStack
    from biffix_infra.stacks.project_stack import ProjectStack

# Attempts of a coordinator job: the first plus one Batch retry after an infrastructure failure.
# The run's capability token must outlive every attempt: change the backend's copies of these two
# with them (`crates/biffix-backend/src/auth/capability.rs`).
COORDINATOR_ATTEMPTS = 2
# Each attempt's timeout.
COORDINATOR_TIMEOUT_HOURS = 24
# A Standard SSM String parameter's largest value.
_SSM_STANDARD_MAX_BYTES = 4096
# The longest a token in a project context (a bucket name, a role or job-definition ARN) resolves
# to: well over a CDK-named role's or job definition's ARN (~100-150 characters).
_TOKEN_MAX_CHARS = 256


class Coordinator(Construct):
    """`project`'s coordinator role (`role`), the job definition template its runs' coordinators
    are cloned from (`job_definition`, `biffix-<tenant>-coordinator-<project>`), and its context
    parameter (`context`, `<projects_path><project>`)."""

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
        # and name this project's resources only: its bucket, key and data sources, its run role.
        compute.grant_coordinator(
            self.role,
            run_role=project.run_base_role,
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
                # child job definitions registered by the engine run their own command, so the
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
            # The infrastructure retry layer: an attempt whose host went away (or whose image
            # could not be pulled, so nothing ran) is retried by Batch under the same job id; any
            # other failure is the run's and ends it (the engine retries its own tasks).
            retry_attempts=COORDINATOR_ATTEMPTS,
            retry_strategies=[
                batch.RetryStrategy.of(
                    batch.Action.RETRY, batch.Reason.custom(on_status_reason="Host EC2*")
                ),
                batch.RetryStrategy.of(batch.Action.RETRY, batch.Reason.CANNOT_PULL_CONTAINER),
                batch.RetryStrategy.of(batch.Action.EXIT, batch.Reason.custom(on_reason="*")),
            ],
            timeout=Duration.hours(COORDINATOR_TIMEOUT_HOURS),
        )
        self.context = self._context(project, projects_path)

    def grant_pass_roles(self, grantee: iam.IGrantable) -> None:
        """Let `grantee` register a clone of the template: a clone passes both of its roles, the
        job role and the execution role."""
        self.role.grant_pass_role(grantee.grant_principal)
        self.job_definition.container.execution_role.grant_pass_role(grantee.grant_principal)

    def _context(self, project: ProjectStack, projects_path: str) -> ssm.StringParameter:
        """`project`'s context, as the backend reads it: an SSM String parameter
        `/biffix/<tenant>/projects/<id>` holding one JSON object (the backend's `ProjectContext`;
        `crates/biffix-backend/tests/fixtures/project_context.json` is its golden example):

        - `id`, `bucket`, `run_role_arn` (the run base role its child jobs run as),
        - `coordinator_job_definition` (its template's revisioned ARN, which a launch clones),
        - `task_log_group`,
        - `uploads`, `viewer_cors` (whether its bucket has the browser-upload and the alignment
          viewer's CORS rules), and `data_sources` (the file picker's, as `DataSource.to_backend`).

        Created here, not in the project's stack, so the template ARN, which churns on every image
        bump, stays an intra-stack token rather than a CloudFormation export. A Standard parameter
        holds 4 KB; a project with a couple of data sources uses ~800 bytes, and synth refuses a
        context that could outgrow it (`_context_size_bound`) rather than fail mid-deploy."""
        context = {
            "id": project.project_id,
            "bucket": project.bucket.bucket_name,
            "run_role_arn": project.run_base_role.role_arn,
            "coordinator_job_definition": self.job_definition.job_definition_arn,
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
