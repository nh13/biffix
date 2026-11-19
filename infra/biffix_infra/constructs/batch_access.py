"""The Batch permissions a project's coordinator role holds (`grant_coordinator_batch`), in named
groups, and the Batch names and tags they are written against.

A coordinator never creates a job definition. The backend registers, at launch, one task job
definition per (project, tenant image, engine), named `biffix-<tenant>_task-<sm|nf>-<hash>`
(`DeployNames.task_job_definition_prefix`): tagged with its project only, never privileged, its
image in the tenant's ECR, its job role the project's run role and its logs in the project's task
log group (`ControlPlaneGrants.launch`). The engines only submit those, by `name:revision`. The
`job-definition/*` and `job/*` wildcards are constructed ARNs on purpose: those resources are
created at runtime, so they have no CDK token to reference.
"""

from __future__ import annotations

from aws_cdk import ArnFormat, Stack
from aws_cdk import aws_iam as iam

# The Batch tag naming the run a job belongs to (the backend's `aws::RUN_TAG`): the engine ingest
# route accepts a coordinator's claim on a job only when this tag names the claiming run.
RUN_TAG = "biffix:run"
# The Batch tag naming the project a job or a task job definition belongs to (the backend's
# `aws::PROJECT_TAG`): a project's coordinator role may submit, tag and terminate only jobs tagged
# with its own project, and submit only the task job definitions tagged with it.
PROJECT_TAG = "biffix:project"
# The log group every Batch job writes to unless its job definition names another: the awslogs
# driver's default. Coordinators log here (and so did the Snakemake task jobs of runtimes whose
# executor predated the log-group setting, which no longer launch); task jobs log to their
# project's own group (ProjectStack.task_log_group), which every task job definition names.
BATCH_LOG_GROUP = "/aws/batch/job"


def grant_coordinator_batch(
    role: iam.IRole,
    *,
    stack: Stack,
    queue_arns: list[str],
    task_prefix: str,
    project_id: str,
    task_log_group: str,
) -> None:
    """Grant a project's coordinator role every Batch permission the engine head job needs, on
    `stack`'s worker queues (`queue_arns`) and the backend's task job definitions (named with
    `task_prefix`, `DeployNames.task_job_definition_prefix`).

    `project_id` is the coordinator's project: the role may submit only jobs tagged with it, from
    task job definitions tagged with it, and terminate or tag only its jobs. `task_log_group` is
    the name of the project's own Batch task log group: the only project log group it may read.

    The coordinator runs the workflow's own code, so denies bound what the allows cannot, whatever
    allow is added later: it may never register, deregister or untag a job definition, nor tag one
    that is not already its own project's (so it can never make one that is privileged, mounts the
    host, pulls an image from outside the tenant or runs as another role, nor make one its
    project's; it tags its own task definitions only as a tagged submit requires), never submit a
    job definition other than a task definition of its own project, never re-tag or untag a job's
    run tag (a job's run is fixed when the job is created, which is what the engine ingest route's
    claim check trusts), and never tag another project's job. It passes no role: SubmitJob passes
    none, and a task definition already names its project's run role."""
    arns = _Arns(stack, task_prefix)
    _grant_submit(role, arns, queue_arns, project_id)
    _deny_foreign_submit(role, arns, project_id)
    _deny_job_definition_changes(role, arns, project_id)
    _grant_describe(role)
    _grant_tagging(role, arns, queue_arns, project_id)
    _grant_task_logs(role, arns, task_log_group)


def batch_arn(stack: Stack, resource: str, name: str) -> str:
    """A Batch ARN (`…:<resource>/<name>`) in `stack`'s partition, region and account."""
    return stack.format_arn(service="batch", resource=resource, resource_name=name)


def log_group_arn(stack: Stack, name: str) -> str:
    """A CloudWatch Logs log-group ARN (`…:log-group:<name>`) in `stack`'s partition, region and
    account."""
    return stack.format_arn(
        service="logs",
        resource="log-group",
        resource_name=name,
        arn_format=ArnFormat.COLON_RESOURCE_NAME,
    )


class _Arns:
    """The Batch and Logs ARNs the grants name, in `stack`'s partition, region and account."""

    def __init__(self, stack: Stack, task_prefix: str) -> None:
        self._stack = stack
        self.job = self.batch("job", "*")
        self.job_queue = self.batch("job-queue", "*")
        self.job_definition = self.batch("job-definition", "*")
        # Every task job definition, and every revision of one (`<name>:<n>`).
        self.task_job_definitions = self.batch("job-definition", f"{task_prefix}*")
        self.task_revisions = self.batch("job-definition", f"{task_prefix}*:*")

    def batch(self, resource: str, name: str) -> str:
        return batch_arn(self._stack, resource, name)

    def log_group(self, name: str) -> str:
        return log_group_arn(self._stack, name)


def _own_project_request(project_id: str) -> dict[str, str]:
    return {f"aws:RequestTag/{PROJECT_TAG}": project_id}


def _own_project_resource(project_id: str) -> dict[str, str]:
    return {f"aws:ResourceTag/{PROJECT_TAG}": project_id}


def _own_project_request_and_resource(project_id: str) -> dict[str, str]:
    """A request tagging, of a resource tagged, with `project_id` (a `StringEquals` condition)."""
    return {**_own_project_request(project_id), **_own_project_resource(project_id)}


def _another_projects(project_id: str) -> dict[str, dict[str, str]]:
    """The condition matching a resource tagged with a project other than `project_id`."""
    return {
        "Null": {f"aws:ResourceTag/{PROJECT_TAG}": "false"},
        "StringNotEquals": {f"aws:ResourceTag/{PROJECT_TAG}": project_id},
    }


_TAGS_THE_PROJECT = {"ForAnyValue:StringEquals": {"aws:TagKeys": [PROJECT_TAG]}}


def _grant_submit(role: iam.IRole, arns: _Arns, queue_arns: list[str], project_id: str) -> None:
    """Submit and terminate this project's jobs."""
    # Every child job is born tagged with this project (the engines tag it from the
    # coordinator's BIFFIX_PROJECT), and a submit without that tag, or naming another
    # project, is refused. SubmitJob authorizes the queue, the job and the job definition
    # separately: queues and jobs here ...
    role.add_to_principal_policy(
        iam.PolicyStatement(
            actions=["batch:SubmitJob"],
            resources=[*queue_arns, arns.job],
            conditions={"StringEquals": _own_project_request(project_id), **_TAGS_THE_PROJECT},
        )
    )
    # ... and a task job definition only as a revision tagged with this project: a task
    # definition runs as its project's run role. Batch evaluates a job definition's tags on
    # SubmitJob only for a `name:revision` ARN (a revisionless submit evaluates none; verified
    # live), and the engines submit the backend's definitions by `name:revision`, so the
    # tag-conditioned allow names only revisions.
    role.add_to_principal_policy(
        iam.PolicyStatement(
            actions=["batch:SubmitJob"],
            resources=[arns.task_revisions],
            conditions={
                "StringEquals": _own_project_request_and_resource(project_id),
                **_TAGS_THE_PROJECT,
            },
        )
    )
    # TerminateJob acts on the JOB resource. Without it, the executor's on-failure cleanup (it
    # terminates the surviving sibling jobs when one child fails) is denied, so those jobs run
    # to completion and burn compute after the run has already failed. Only this project's
    # jobs: a run cannot stop another project's.
    role.add_to_principal_policy(
        iam.PolicyStatement(
            actions=["batch:TerminateJob"],
            resources=[arns.job],
            conditions={"StringEquals": _own_project_resource(project_id)},
        )
    )


def _deny_foreign_submit(role: iam.IRole, arns: _Arns, project_id: str) -> None:
    """Never submit a job definition other than this project's task definitions."""
    # Every job definition but the task definitions is unusable, whatever allow is added later:
    # the coordinator definitions (each runs as a project's COORDINATOR role), the definitions
    # the engines registered before the backend took that over (`nf-*`, `snakejob-def-*`) and
    # anything else in the account. A deny on everything EXCEPT the task definitions
    # (NotResource) rather than a deny per known prefix, so a name nobody thought of is covered
    # too. SubmitJob also authorizes the job and its queue, which the NotResource leaves out:
    # the allows above decide those.
    role.add_to_principal_policy(
        iam.PolicyStatement(
            effect=iam.Effect.DENY,
            actions=["batch:SubmitJob"],
            not_resources=[arns.task_job_definitions, arns.job, arns.job_queue],
        )
    )
    # Nor a task definition not tagged with this project (another project's runs as that
    # project's run role). An absent tag (a revisionless submit, whose tags Batch does not
    # evaluate) makes the negated condition true, so that is denied too.
    role.add_to_principal_policy(
        iam.PolicyStatement(
            effect=iam.Effect.DENY,
            actions=["batch:SubmitJob"],
            resources=[arns.task_job_definitions],
            conditions={"StringNotEquals": _own_project_resource(project_id)},
        )
    )


def _deny_job_definition_changes(role: iam.IRole, arns: _Arns, project_id: str) -> None:
    """Never create, remove or untag a job definition, nor tag one into this project."""
    # A job definition picks the container's privileges, host mounts, image and role, and the
    # compute environments are shared by every project, so only the backend registers them.
    role.add_to_principal_policy(
        iam.PolicyStatement(
            effect=iam.Effect.DENY,
            actions=["batch:RegisterJobDefinition", "batch:DeregisterJobDefinition"],
            resources=["*"],
        )
    )
    # Re-tagging a definition as this project's would make it submittable here. A tagged submit
    # needs TagResource on its own task definition (see _grant_tagging), so tagging is not denied
    # outright: a definition that does not already carry this project's tag (another project's,
    # or one with none: a negated condition is true for an absent key), and a request that does
    # not tag this project, are denied, so no definition is ever made this project's.
    for condition in (_own_project_resource(project_id), _own_project_request(project_id)):
        role.add_to_principal_policy(
            iam.PolicyStatement(
                effect=iam.Effect.DENY,
                actions=["batch:TagResource"],
                resources=[arns.job_definition],
                conditions={"StringNotEquals": condition},
            )
        )
    role.add_to_principal_policy(
        iam.PolicyStatement(
            effect=iam.Effect.DENY,
            actions=["batch:UntagResource"],
            resources=[arns.job_definition],
        )
    )


def _grant_describe(role: iam.IRole) -> None:
    """The Describe/List calls the engines make; none supports resource-level permissions."""
    role.add_to_principal_policy(
        iam.PolicyStatement(
            # The aws-batch executor fork detects whether a queue is EC2 or Fargate before
            # submitting each child job, walking queue → compute environment:
            # DescribeJobQueues then DescribeComputeEnvironments. Missing either raises
            # WorkflowError at first submit.
            actions=[
                "batch:DescribeJobs",
                "batch:ListJobs",
                "batch:DescribeJobQueues",
                "batch:DescribeComputeEnvironments",
            ],
            resources=["*"],
        )
    )
    # The Snakemake executor describes the task definition a rule maps to, to check the rule's
    # shared_memory_size_mb against it.
    role.add_to_principal_policy(
        iam.PolicyStatement(actions=["batch:DescribeJobDefinitions"], resources=["*"])
    )


def _grant_tagging(role: iam.IRole, arns: _Arns, queue_arns: list[str], project_id: str) -> None:
    """Tag each child job, and the task definition it names, with this project only; never re-tag
    a job's run."""
    # The engines tag every child job at submit (its run and project). Batch authorizes a
    # tag-on-create as TagResource on the new job, and a tagged SubmitJob also as TagResource on
    # the job queue it names (verified live: without the queue, every tagged submit is denied).
    # So TagResource is allowed on jobs and this role's queues when the tags name this project.
    role.add_to_principal_policy(
        iam.PolicyStatement(
            actions=["batch:TagResource"],
            resources=[arns.job, *queue_arns],
            conditions={"StringEquals": _own_project_request(project_id)},
        )
    )
    # A tagged SubmitJob is authorized as TagResource on the job definition it names too
    # (verified live: without it, every tagged submit is denied). So this project's own task
    # definitions may be tagged, by a request that tags this project: the project tag cannot change
    # (_deny_job_definition_changes denies every other job-definition tag). Other keys may be set
    # that way and never removed (UntagResource is denied), which grants nothing: the backend reads
    # only the project tag of a definition, and its definitions do not propagate tags to tasks. The
    # keys cannot be bounded: a submit carries every tag of the job, and a workflow's own
    # (SNAKEMAKE_AWS_BATCH_JOB_TAGS, Nextflow resourceLabels) are kept.
    role.add_to_principal_policy(
        iam.PolicyStatement(
            actions=["batch:TagResource"],
            resources=[arns.task_revisions],
            conditions={"StringEquals": _own_project_request_and_resource(project_id)},
        )
    )
    # A job's run tag is write-once. A job gets it when it is created (a tag-on-create, while
    # the job has no tags), so a TagResource on a job that already carries it is a re-tag, and
    # no coordinator may drop it. Otherwise a run could relabel another run's job as its own
    # and claim that job's AWS facts, or strand another run's jobs untracked. The backend tags
    # each coordinator job with its run and project at launch, so every biffix job is born
    # tagged.
    role.add_to_principal_policy(
        iam.PolicyStatement(
            effect=iam.Effect.DENY,
            actions=["batch:TagResource"],
            resources=[arns.job],
            conditions={"Null": {f"aws:ResourceTag/{RUN_TAG}": "false"}},
        )
    )
    # Nor may it tag another project's job at all (whatever tags it has).
    role.add_to_principal_policy(
        iam.PolicyStatement(
            effect=iam.Effect.DENY,
            actions=["batch:TagResource"],
            resources=[arns.job],
            conditions=_another_projects(project_id),
        )
    )
    role.add_to_principal_policy(
        iam.PolicyStatement(
            effect=iam.Effect.DENY,
            actions=["batch:UntagResource"],
            resources=[arns.job],
            conditions={"ForAnyValue:StringEquals": {"aws:TagKeys": [RUN_TAG, PROJECT_TAG]}},
        )
    )


def _grant_task_logs(role: iam.IRole, arns: _Arns, task_log_group: str) -> None:
    """Read this project's task logs (and list log groups)."""
    # nf-amazon surfaces a task's failure by reading its CloudWatch log stream, in this
    # project's task log group. Snakemake's executor reads no task logs, and Snakemake tasks
    # log there too. No coordinator may read Batch's default /aws/batch/job group: it holds
    # every project's coordinator logs (entrypoint.sh, "Project isolation"). DescribeLogGroups
    # (names and metadata, no content; the executor's log-group preflight) has no
    # resource-level permissions.
    role.add_to_principal_policy(
        iam.PolicyStatement(
            actions=["logs:GetLogEvents", "logs:DescribeLogStreams"],
            resources=[arns.log_group(task_log_group), arns.log_group(f"{task_log_group}:*")],
        )
    )
    role.add_to_principal_policy(
        iam.PolicyStatement(actions=["logs:DescribeLogGroups"], resources=["*"])
    )
