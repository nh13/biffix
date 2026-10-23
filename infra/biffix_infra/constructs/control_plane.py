"""What the control plane's Lambdas (the backend API and the sweep) are granted to serve projects,
build coordinator variants and launch runs. Both Lambdas need exactly the same environment and
scope for each, so both are wired by these one set of methods instead of each hand-copying it.

Creates no resources: each method adds environment variables and policy statements to a Lambda.
"""

from __future__ import annotations

from typing import TYPE_CHECKING

from aws_cdk import Stack
from aws_cdk import aws_iam as iam
from aws_cdk import aws_lambda as lambda_
from aws_cdk import aws_secretsmanager as secretsmanager

from biffix_infra.constructs.batch_access import PROJECT_TAG, RUN_TAG, batch_arn
from biffix_infra.constructs.coordinator import Coordinator
from biffix_infra.wiring import any_ecr_repository_arn, grant_secret_read

if TYPE_CHECKING:
    from biffix_infra.stacks.build_stack import BuildStack
    from biffix_infra.stacks.compute_stack import ComputeStack
    from biffix_infra.stacks.project_stack import ProjectStack


class ControlPlaneGrants:
    """The control plane's grants in `stack`, over every project of the deploy (`projects`, each
    with its `coordinators[project_id]`). The backend serves every project (each request is
    routed to one), so it holds each project's grants; a run's own roles (coordinator, run base
    role) still reach only their project, so the isolation boundary does not widen."""

    def __init__(
        self,
        stack: Stack,
        *,
        tenant: str,
        projects: list[ProjectStack],
        coordinators: dict[str, Coordinator],
        projects_path: str,
        default_project_id: str,
        compute: ComputeStack,
        build: BuildStack,
        build_project_names: dict[str, str],
        coordinator_image_uri: str,
        worker_image_repo_uri: str,
        capability_secret: secretsmanager.ISecret,
    ) -> None:
        self._stack = stack
        self._projects = projects
        self._coordinators = coordinators
        self._projects_path = projects_path
        self._default_project_id = default_project_id
        self._compute = compute
        self._build = build
        self._build_project_names = build_project_names
        self._coordinator_image_uri = coordinator_image_uri
        self._worker_image_repo_uri = worker_image_repo_uri
        self._capability_secret = capability_secret
        # Every coordinator variant's job definition is named `<prefix><runtime key12>-<base12>-
        # <template8>`: the template hash makes a variant per (runtime, base, project template).
        self._variant_prefix = f"biffix-{tenant}-coordinator-rt-"
        self._variant_job_definitions = batch_arn(
            stack, "job-definition", f"{self._variant_prefix}*"
        )

    def read_projects(self, fn: lambda_.Function) -> None:
        """Let `fn` read every project's context: where the parameters are, which project is the
        default, and read on that path only. `fn` depends on each parameter, so a deploy updates
        the contexts before the code that reads them."""
        fn.add_environment("BIFFIX_PROJECTS_PATH", self._projects_path)
        fn.add_environment("BIFFIX_DEFAULT_PROJECT", self._default_project_id)
        # The backend's only SSM call is GetParametersByPath, sent the path with no trailing
        # slash (`/biffix/<tenant>/projects`): IAM authorizes it on that path's parameter ARN.
        # `<path>/*` is granted beside it so the grant holds however IAM forms the resource of a
        # path request (it also matches a slash-terminated path), and matches nothing outside the
        # path (unlike `projects*`). No GetParameter(s): nothing reads a parameter by name.
        path = self._projects_path.strip("/")
        fn.add_to_role_policy(
            iam.PolicyStatement(
                actions=["ssm:GetParametersByPath"],
                resources=[
                    self._stack.format_arn(service="ssm", resource="parameter", resource_name=name)
                    for name in (path, f"{path}/*")
                ],
            )
        )
        for coordinator in self._coordinators.values():
            fn.node.add_dependency(coordinator.context)

    def build_variants(self, fn: lambda_.Function) -> None:
        """Let `fn` build coordinator variants (base image + engine runtime, in CodeBuild) and
        register each variant's job definition as a clone of a project's template."""
        fn.add_environment("BIFFIX_COORDINATOR_IMAGE", self._coordinator_image_uri)
        for env, name in self._build_project_names.items():
            fn.add_environment(env, name)
        fn.add_environment("BIFFIX_VARIANT_JOB_DEFINITION_PREFIX", self._variant_prefix)
        # Where the image builds push: a built image is trusted only from there.
        fn.add_environment("BIFFIX_BUILDS_IMAGE_REPO", self._build.builds_repo.repository_uri)
        self._build.grant_start_builds(fn)
        # Describe has no resource-level permissions; registering (and tagging) is limited to the
        # variant names.
        fn.add_to_role_policy(
            iam.PolicyStatement(actions=["batch:DescribeJobDefinitions"], resources=["*"])
        )
        fn.add_to_role_policy(
            iam.PolicyStatement(
                actions=["batch:RegisterJobDefinition", "batch:TagResource"],
                resources=[self._variant_job_definitions],
            )
        )
        # A clone passes the template's roles. Only the templates' own (each project's coordinator
        # role, never a run role), so a variant runs as exactly the project whose template it was
        # cloned from.
        for coordinator in self._coordinators.values():
            coordinator.grant_pass_roles(fn)

    def launch(self, fn: lambda_.Function, ingest_url: str) -> None:
        """Let `fn` launch a run's coordinator in any project — the API Lambda from a live submit,
        the sweep Lambda for a run parked in `preparing_images` once its images are ready. The
        coordinator reports to `ingest_url`."""
        compute = self._compute
        # The run's project context (its role, which Batch delivers to the run's child jobs; where
        # the run's files go; the template its variant is cloned from) is the project's SSM
        # parameter (`read_projects`). Here: the tenant ECR repo holding workflow worker images (a
        # child job's image is `{repo}@{version.image_digest}`), the queue the coordinator runs on
        # and the URL it reports to.
        fn.add_environment("BIFFIX_WORKER_IMAGE_REPO", self._worker_image_repo_uri)
        fn.add_environment("BIFFIX_COORDINATOR_QUEUE", compute.coordinator_queue.job_queue_arn)
        fn.add_environment("BIFFIX_INGEST_URL", ingest_url)
        # The samplesheets/`images.json` a launch writes under the run's own `inputs/` folder, in
        # whichever project the run is in.
        for project in self._projects:
            project.grant_backend_run_inputs(fn)
        # Runs launch on coordinator variants, whose job definitions the backend registers
        # itself: SubmitJob on the coordinator queue with a job definition of the variant name
        # only.
        fn.add_to_role_policy(
            iam.PolicyStatement(
                actions=["batch:SubmitJob"],
                resources=[compute.coordinator_queue.job_queue_arn, self._variant_job_definitions],
            )
        )
        # SubmitJob takes no idempotency token: when its answer is lost, the launcher lists the
        # run's coordinator by its job name to find a job it may have created (binding it, or
        # stopping it before launching again). ListJobs has no resource-level permissions.
        fn.add_to_role_policy(iam.PolicyStatement(actions=["batch:ListJobs"], resources=["*"]))
        job_arn = batch_arn(self._stack, "job", "*")
        # The coordinator job is created with its run and project tags (a tag-on-create, which
        # Batch authorizes as TagResource on the new job and on the queue the submit names —
        # verified live: without the queue every tagged submit is denied), so it is protected like
        # every child job:
        # coordinators may not re-tag a job that carries a run tag, nor tag or terminate another
        # project's (ComputeStack.grant_coordinator). Only those tags, and only a project of this
        # deploy (the backend tags a run with its version's project; each coordinator role's own
        # tag conditions still keep a run inside its project).
        fn.add_to_role_policy(
            iam.PolicyStatement(
                actions=["batch:TagResource"],
                resources=[job_arn, compute.coordinator_queue.job_queue_arn],
                conditions={
                    "ForAllValues:StringEquals": {"aws:TagKeys": [RUN_TAG, PROJECT_TAG]},
                    "StringEquals": {
                        f"aws:RequestTag/{PROJECT_TAG}": [p.project_id for p in self._projects]
                    },
                },
            )
        )
        # Cancel (including one racing a launch) stops spend by terminating the coordinator job.
        # TerminateJob authorizes on the JOB resource only, and a job id is not known until
        # submit, so this is the account's Batch job ARN space; callers only ever pass the
        # coordinator job id recorded for a run they may launch.
        fn.add_to_role_policy(
            iam.PolicyStatement(actions=["batch:TerminateJob"], resources=[job_arn])
        )
        # Registration, submit and a parked run's launch pin images to immutable digests: a private
        # ECR tag is resolved with BatchGetImage (a pull permission, so another account's
        # repository whose policy grants this account pull resolves too). Read-only; any
        # repository, because the reference comes from the registered or submitted image, and the
        # repository policy still decides. BatchGetImage counts as a pull (it updates the image's
        # last-pulled time, which pull-based lifecycle rules read). A pull-through-cache repository
        # would also need ecr:BatchImportUpstreamImage to resolve a tag it has not cached yet;
        # that is not granted (the fleet reads no pull-through caches). Imports copy and tag
        # images, not the backend.
        fn.add_to_role_policy(
            iam.PolicyStatement(
                actions=["ecr:BatchGetImage"],
                resources=[any_ecr_repository_arn(self._stack)],
            )
        )
        # The capability-token signing key lives in persistence (run-invalidating state); a
        # launch mints the run's ingest token with it. Without it the minter emits dead
        # `ingest-disabled` tokens, so a launcher refuses to start a run that cannot report.
        fn.add_environment("BIFFIX_CAPABILITY_SECRET_ARN", self._capability_secret.secret_arn)
        grant_secret_read(fn, self._capability_secret)
