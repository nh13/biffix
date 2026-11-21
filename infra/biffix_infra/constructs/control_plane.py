"""What the control plane's Lambdas (the backend API and the sweep) are granted to serve projects,
build coordinator variants, and launch runs (registering the task job definitions their child jobs
run on). Both Lambdas need exactly the same environment and scope for each, so both are wired by
these one set of methods instead of each hand-copying it.

Creates no resources: each method adds environment variables and policy statements to a Lambda.
"""

from __future__ import annotations

from typing import TYPE_CHECKING

from aws_cdk import Stack
from aws_cdk import aws_iam as iam
from aws_cdk import aws_lambda as lambda_
from aws_cdk import aws_secretsmanager as secretsmanager

from biffix_infra.constructs.batch_access import (
    EXECUTION_TAG,
    PROJECT_TAG,
    RUN_TAG,
    SINGLE_JOB_EXECUTION,
    batch_arn,
)
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
        task_prefix: str,
        default_project_id: str,
        compute: ComputeStack,
        build: BuildStack,
        build_project_names: dict[str, str],
        coordinator_image_uri: str,
        worker_image_repo_uri: str,
        capability_secret: secretsmanager.ISecret,
        single_job_instance_types: dict[str, list[str]] | None = None,
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
        # name → exact instance types of each single-job queue-class, in config order (its queue
        # is `compute.single_job_queues[name]`).
        self._single_job_instance_types = single_job_instance_types or {}
        # Every coordinator variant's job definition is named `<prefix><runtime key12>-<base12>-
        # <template8>`: the template hash makes a variant per (runtime, base, project template).
        # A tasks-mode variant (a clone of the coordinator template) has the `rt-` prefix and a
        # single-job one (a clone of the single-job template) the `sj-` prefix, so IAM tells them
        # apart: each is submitted only to its own queues (`launch`).
        self._variant_prefix = f"biffix-{tenant}-coordinator-rt-"
        self._variant_job_definitions = batch_arn(
            stack, "job-definition", f"{self._variant_prefix}*"
        )
        self._single_job_variant_prefix = f"biffix-{tenant}-coordinator-sj-"
        self._single_job_variant_job_definitions = batch_arn(
            stack, "job-definition", f"{self._single_job_variant_prefix}*"
        )
        # Every task job definition (`<prefix><sm|nf>-<32 hex>`, one per project, tenant image and
        # engine; `DeployNames.task_job_definition_prefix`) is registered by a launch; coordinators
        # may submit only these.
        self._task_prefix = task_prefix
        self._task_job_definitions = batch_arn(stack, "job-definition", f"{self._task_prefix}*")

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
        variants = [self._variant_job_definitions]
        if self._single_job_instance_types:
            fn.add_environment(
                "BIFFIX_SINGLE_JOB_VARIANT_JOB_DEFINITION_PREFIX", self._single_job_variant_prefix
            )
            variants.append(self._single_job_variant_job_definitions)
        # Where the image builds push: a built image is trusted only from there.
        fn.add_environment("BIFFIX_BUILDS_IMAGE_REPO", self._build.builds_repo.repository_uri)
        self._build.grant_start_builds(fn)
        # Describe (to find a variant registered before) has no resource-level permissions;
        # registering (and tagging) is limited to the variant names, and to what
        # `_deny_unsafe_registrations` lets through.
        fn.add_to_role_policy(
            iam.PolicyStatement(actions=["batch:DescribeJobDefinitions"], resources=["*"])
        )
        fn.add_to_role_policy(
            iam.PolicyStatement(
                actions=["batch:RegisterJobDefinition", "batch:TagResource"],
                resources=variants,
            )
        )
        self._deny_unsafe_registrations(fn)
        # A clone passes the template's roles, and only the templates' own: the coordinator
        # template's job and execution roles, and the single-job template's (its job role is the
        # project's run base role). So a variant runs as exactly the project whose template it was
        # cloned from.
        for coordinator in self._coordinators.values():
            coordinator.grant_pass_roles(fn)

    def launch(self, fn: lambda_.Function, ingest_url: str) -> None:
        """Let `fn` launch a run's coordinator in any project — the API Lambda from a live submit,
        the sweep Lambda for a run parked in `preparing_images` once its images are ready — and
        register the task job definitions the run's child jobs run on (`_register_tasks`). The
        coordinator reports to `ingest_url`."""
        compute = self._compute
        # The run's project context (its role, which Batch delivers to the run's child jobs; where
        # the run's files go; the template its variant is cloned from) is the project's SSM
        # parameter (`read_projects`). Here: the tenant ECR repo holding workflow worker images (a
        # child job's image is `{repo}@{version.image_digest}`), the queue the coordinator runs on
        # and the URL it reports to.
        fn.add_environment("BIFFIX_WORKER_IMAGE_REPO", self._worker_image_repo_uri)
        fn.add_environment("BIFFIX_TASK_JOB_DEFINITION_PREFIX", self._task_prefix)
        self._register_tasks(fn)
        fn.add_environment("BIFFIX_COORDINATOR_QUEUE", compute.coordinator_queue.job_queue_arn)
        # The queues a whole run may launch on instead (single-job mode), each with the instance
        # types the backend sizes its whole-host request from: a JSON array in deploy-config
        # order, the first the default. An array, not an object keyed by name: CDK renders an
        # object through JavaScript, which puts integer-like keys first and so could change which
        # queue-class is first.
        single_job_queues = self._single_job_queue_arns()
        if single_job_queues:
            fn.add_environment(
                "BIFFIX_SINGLE_JOB_QUEUES",
                self._stack.to_json_string(
                    [
                        {
                            "name": name,
                            "queue": arn,
                            "instance_types": self._single_job_instance_types[name],
                        }
                        for name, arn in single_job_queues.items()
                    ]
                ),
            )
            # The vCPUs and memory of those instance types (the whole-host request). Describe has
            # no resource-level permissions.
            fn.add_to_role_policy(
                iam.PolicyStatement(actions=["ec2:DescribeInstanceTypes"], resources=["*"])
            )
        coordinator_queue = compute.coordinator_queue.job_queue_arn
        launch_queues = [coordinator_queue, *single_job_queues.values()]
        fn.add_environment("BIFFIX_INGEST_URL", ingest_url)
        # The samplesheets/`images.json` a launch writes under the run's own `inputs/` folder, in
        # whichever project the run is in.
        for project in self._projects:
            project.grant_backend_run_inputs(fn)
        job_arn = batch_arn(self._stack, "job", "*")
        # Runs launch on coordinator variants, whose job definitions the backend registers
        # itself: tasks-mode (`rt-`) variants on the coordinator queue, single-job (`sj-`)
        # variants on the single-job queues. IAM authorizes a SubmitJob's queue and its job
        # definition as separate resource checks, each against every statement, so the pairing
        # rests on the request's tags, which every check sees alike: the backend tags a single-job
        # coordinator job `EXECUTION_TAG=SINGLE_JOB_EXECUTION` and a tasks-mode one not at all. A
        # crossed pair then fails one of its checks (a single-job variant on the coordinator
        # queue: the queue needs the tag absent, the definition needs it present). The job ARN is
        # in both, under the same condition, should Batch authorize the new job too.
        fn.add_to_role_policy(
            iam.PolicyStatement(
                actions=["batch:SubmitJob"],
                resources=[coordinator_queue, self._variant_job_definitions, job_arn],
                conditions={"Null": {f"aws:RequestTag/{EXECUTION_TAG}": "true"}},
            )
        )
        if single_job_queues:
            fn.add_to_role_policy(
                iam.PolicyStatement(
                    actions=["batch:SubmitJob"],
                    resources=[
                        *single_job_queues.values(),
                        self._single_job_variant_job_definitions,
                        job_arn,
                    ],
                    conditions={
                        "StringEquals": {f"aws:RequestTag/{EXECUTION_TAG}": SINGLE_JOB_EXECUTION}
                    },
                )
            )
        # SubmitJob takes no idempotency token: when its answer is lost, the launcher lists the
        # run's coordinator by its job name to find a job it may have created (binding it, or
        # stopping it before launching again). ListJobs has no resource-level permissions.
        fn.add_to_role_policy(iam.PolicyStatement(actions=["batch:ListJobs"], resources=["*"]))
        # The coordinator job is created with its run and project tags (a tag-on-create, which
        # Batch authorizes as TagResource on the new job and on the queue the submit names —
        # verified live: without the queue every tagged submit is denied), so it is protected like
        # every child job:
        # coordinators may not re-tag a job that carries a run tag, nor tag or terminate another
        # project's (ComputeStack.grant_coordinator). Only those tags, and only a project of this
        # deploy (the backend tags a run with its version's project; each coordinator role's own
        # tag conditions still keep a run inside its project), plus a single-job run's execution
        # tag, whose only value is `SINGLE_JOB_EXECUTION`.
        fn.add_to_role_policy(
            iam.PolicyStatement(
                actions=["batch:TagResource"],
                resources=[job_arn, *launch_queues],
                conditions={
                    "ForAllValues:StringEquals": {
                        "aws:TagKeys": [RUN_TAG, PROJECT_TAG, EXECUTION_TAG]
                    },
                    "StringEquals": {
                        f"aws:RequestTag/{PROJECT_TAG}": [p.project_id for p in self._projects]
                    },
                    "StringEqualsIfExists": {
                        f"aws:RequestTag/{EXECUTION_TAG}": SINGLE_JOB_EXECUTION
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

    def sweep_launches(self, fn: lambda_.Function) -> None:
        """Let `fn`, the sweep Lambda, which also has `launch`, release the parked launch of a
        run that will never launch: before dropping it, the sweep stops any coordinator an earlier
        launch may have left running, and passes over an earlier launch's queue it cannot list
        only once DescribeJobQueues says Batch deleted it. DescribeJobQueues has no resource-level
        permissions. The API Lambda never asks, so it is not granted it."""
        fn.add_to_role_policy(
            iam.PolicyStatement(actions=["batch:DescribeJobQueues"], resources=["*"])
        )

    def _single_job_queue_arns(self) -> dict[str, str]:
        """name → job queue ARN of each single-job queue-class, in config order."""
        queues = self._compute.single_job_queues
        if set(queues) != set(self._single_job_instance_types):
            raise ValueError(
                f"single-job queue-classes {sorted(queues)} and their instance types "
                f"{sorted(self._single_job_instance_types)} disagree"
            )
        return {name: queues[name].job_queue_arn for name in self._single_job_instance_types}

    def _register_tasks(self, fn: lambda_.Function) -> None:
        """Let `fn` register the task job definitions a launch needs: one per (project, tenant
        image, engine), reused across runs. A coordinator runs the workflow's own code, so it may
        not register any job definition (batch_access); the backend registers them within what
        IAM enforces here: named with the task prefix, tagged with one of this deploy's projects
        and nothing else, logging to a project's task log group, never privileged, pulling only
        from the tenant's registry, and running as a project's run role."""
        # Describe (to find a definition a launch registered before) has no resource-level
        # permissions.
        fn.add_to_role_policy(
            iam.PolicyStatement(actions=["batch:DescribeJobDefinitions"], resources=["*"])
        )
        # Registering creates the definition with its project tag (a tag-on-create, which Batch
        # authorizes as TagResource on the new definition too), so both are allowed only for a
        # project of this deploy, with no tag but the project: a coordinator submits a task
        # definition only when it carries its own project's tag (batch_access). Its tasks log to
        # a project's own group, or, naming none, Batch's default; never elsewhere (the key is
        # absent from a TagResource, hence IfExists).
        fn.add_to_role_policy(
            iam.PolicyStatement(
                actions=["batch:RegisterJobDefinition", "batch:TagResource"],
                resources=[self._task_job_definitions],
                conditions={
                    "StringEquals": {
                        f"aws:RequestTag/{PROJECT_TAG}": [p.project_id for p in self._projects]
                    },
                    "ForAllValues:StringEquals": {"aws:TagKeys": [PROJECT_TAG]},
                    "StringEqualsIfExists": {
                        "batch:AWSLogsGroup": [p.task_log_group_name for p in self._projects]
                    },
                },
            )
        )
        self._deny_unsafe_registrations(fn)
        # A task definition runs as its project's run base role, which registering it passes: so
        # this passes each project's run base role (`build_variants` passes the coordinator
        # template roles a variant clones).
        for project in self._projects:
            project.run_base_role.grant_pass_role(fn.grant_principal)

    def _deny_unsafe_registrations(self, fn: lambda_.Function) -> None:
        """Deny `fn` registering any job definition that is privileged or names an image outside
        the tenant's registry, whichever allow (`build_variants`, `launch`) lets it register.
        Called by both, so each is complete on its own (IAM merges the repeated denies)."""
        # The compute environments are shared by every project (and their hosts refuse privileged
        # containers anyway, host_boot.host_user_data): no definition `fn` registers, task or
        # coordinator variant, is privileged.
        job_definitions = batch_arn(self._stack, "job-definition", "*")
        fn.add_to_role_policy(
            iam.PolicyStatement(
                effect=iam.Effect.DENY,
                actions=["batch:RegisterJobDefinition"],
                resources=[job_definitions],
                conditions={"Bool": {"batch:Privileged": "true"}},
            )
        )
        # Every image a definition it registers names is in this account's registry, in the
        # deploy's region: tasks run workflow images imported or built into the workflow and
        # builds repositories (a run's image map is strict, so no task pulls from elsewhere), and
        # the generic worker and the coordinator variants are the coordinator repository's. A
        # definition naming no image (the key absent) matches the negated condition, so it is
        # refused too.
        stack = self._stack
        registry = f"{stack.account}.dkr.ecr.{stack.region}.{stack.url_suffix}/*"
        fn.add_to_role_policy(
            iam.PolicyStatement(
                effect=iam.Effect.DENY,
                actions=["batch:RegisterJobDefinition"],
                resources=[job_definitions],
                conditions={"StringNotLike": {"batch:Image": registry}},
            )
        )
