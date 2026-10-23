"""The Batch permissions a project's coordinator role holds (`grant_coordinator_batch`), in named
groups, and the Batch names and tags they are written against.

The `job-definition/*` and `job/*` wildcards are constructed ARNs on purpose: the engines REGISTER
child job definitions at runtime, so those resources have no CDK token to reference.
"""

from __future__ import annotations

from aws_cdk import ArnFormat, Stack
from aws_cdk import aws_iam as iam

# The Batch tag naming the run a job belongs to (the backend's `aws::RUN_TAG`): the engine ingest
# route accepts a coordinator's claim on a job only when this tag names the claiming run.
RUN_TAG = "biffix:run"
# The Batch tag naming the project a job belongs to (the backend's `aws::PROJECT_TAG`): a project's
# coordinator role may submit, tag and terminate only jobs tagged with its own project.
PROJECT_TAG = "biffix:project"
# Every coordinator job definition (the per-project templates and the variants the backend clones
# from them) is named `biffix-<tenant>-coordinator…`; the engines name their child job definitions
# otherwise.
COORDINATOR_JOB_DEFINITIONS = "biffix-*"
# The Snakemake AWS Batch executor (the pinned fork) names each child job definition
# `snakejob-def-<rule>-<uuid>`, registers it with its job's own tags (SNAKEMAKE_AWS_BATCH_JOB_TAGS,
# so `biffix:run` and `biffix:project`) and deregisters it on cleanup.
SNAKEMAKE_JOB_DEFINITIONS = "snakejob-def-*"
# A revision of one (`snakejob-def-…:<n>`): the executor submits `name:revision`, so, as for
# Nextflow below, the tag-conditioned allow names only revisions.
SNAKEMAKE_JOB_DEFINITION_REVISIONS = "snakejob-def-*:*"
# nf-amazon names each child job definition `nf-<image, sanitized>`, reuses a definition whose
# `nf-token` parameter (a hash covering the job role and log group) matches, and never deregisters
# one. nf-biffix's `awsbatch` executor (BiffixAwsBatchTaskHandler) registers each one tagged
# `biffix:project` (next to nf-amazon's `nextflow.io/*` tags), hashes the project into the token,
# and reuses only a definition tagged with its project, so IAM can require the tag.
NEXTFLOW_JOB_DEFINITIONS = "nf-*"
# A revision of one (`nf-…:<n>`). Batch evaluates a job definition's tags on SubmitJob only for a
# `name:revision` ARN (a revisionless submit evaluates none; verified live), and nf-amazon always
# submits `name:revision`, so the tag-conditioned allow names only revisions.
NEXTFLOW_JOB_DEFINITION_REVISIONS = "nf-*:*"
# The log group every Batch job writes to unless its job definition names another: the awslogs
# driver's default. Coordinators log here, and the Snakemake task jobs of a runtime whose executor
# predates the log-group setting; other task jobs log to their project's own group
# (ProjectStack.task_log_group).
BATCH_LOG_GROUP = "/aws/batch/job"


def grant_coordinator_batch(
    role: iam.IRole,
    *,
    stack: Stack,
    queue_arns: list[str],
    run_role: iam.IRole,
    project_id: str,
    task_log_group: str,
) -> None:
    """Grant a project's coordinator role every Batch permission the engine head job needs, on
    `stack`'s queues (`queue_arns`) and job definitions.

    `run_role` is the coordinator's own project's run role: the only role it may pass to the
    child job definitions it registers, so a run's children stay in its project too.
    `project_id` is that project: the role may submit only jobs tagged with it, and terminate
    or tag only its jobs. `task_log_group` is the name of the project's own Batch task log
    group (the Nextflow task jobs' `aws.batch.logsGroup`): the only project log group it may
    read, or point a job definition at.

    The coordinator runs the workflow's own code, so denies bound what the allows cannot: it
    may never re-tag or untag a job's run tag (a job's run is fixed when the job is created,
    which is what the engine ingest route's claim check trusts), never tag another project's
    job, never launch a coordinator job definition (which runs as a project's coordinator
    role), and never launch a child job definition not tagged with its own project (which may
    carry another project's run role). Every child job definition is born tagged with its
    project (Snakemake's from its job tags, Nextflow's by nf-biffix's executor), and IAM
    requires that tag when a Nextflow one is registered, so none is created untagged."""
    arns = _Arns(stack)
    _grant_submit(role, arns, queue_arns, project_id)
    _deny_foreign_submit(role, arns, project_id)
    _grant_describe(role)
    _grant_job_definitions(role, arns, project_id, task_log_group)
    _grant_tagging(role, arns, queue_arns, project_id)
    # Registering a job definition with a jobRoleArn requires passing that role: this
    # project's run role only, never another project's (nor any other biffix role).
    run_role.grant_pass_role(role)
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

    def __init__(self, stack: Stack) -> None:
        self._stack = stack
        self.job = self.batch("job", "*")
        self.job_definition = self.batch("job-definition", "*")
        self.snakemake_job_definitions = self.batch("job-definition", SNAKEMAKE_JOB_DEFINITIONS)
        self.snakemake_revisions = self.batch("job-definition", SNAKEMAKE_JOB_DEFINITION_REVISIONS)
        self.nextflow_job_definitions = self.batch("job-definition", NEXTFLOW_JOB_DEFINITIONS)
        self.nextflow_revisions = self.batch("job-definition", NEXTFLOW_JOB_DEFINITION_REVISIONS)

    def batch(self, resource: str, name: str) -> str:
        return batch_arn(self._stack, resource, name)

    def log_group(self, name: str) -> str:
        return log_group_arn(self._stack, name)


def _own_project_request(project_id: str) -> dict[str, str]:
    return {f"aws:RequestTag/{PROJECT_TAG}": project_id}


def _own_project_resource(project_id: str) -> dict[str, str]:
    return {f"aws:ResourceTag/{PROJECT_TAG}": project_id}


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
    # ... and an engine's child job definition only as a revision tagged with this project: a
    # child definition runs as the run role of the project that registered it. No other child
    # job definition is submittable at all.
    role.add_to_principal_policy(
        iam.PolicyStatement(
            actions=["batch:SubmitJob"],
            resources=[arns.snakemake_revisions, arns.nextflow_revisions],
            conditions={
                "StringEquals": {
                    **_own_project_request(project_id),
                    **_own_project_resource(project_id),
                },
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
    """Never submit a job definition that runs as another role than this project's run role."""
    # A coordinator job definition runs as a project's COORDINATOR role, so submitting another
    # project's would escape this one. No coordinator ever launches one (the backend does).
    role.add_to_principal_policy(
        iam.PolicyStatement(
            effect=iam.Effect.DENY,
            actions=["batch:SubmitJob"],
            resources=[arns.batch("job-definition", COORDINATOR_JOB_DEFINITIONS)],
        )
    )
    # Nor another project's child job definition, which runs as that project's run role.
    role.add_to_principal_policy(
        iam.PolicyStatement(
            effect=iam.Effect.DENY,
            actions=["batch:SubmitJob"],
            resources=[arns.job_definition],
            conditions=_another_projects(project_id),
        )
    )
    # Nor any engine's child definition not tagged with this project, so no future allow can
    # reopen one. An absent tag (an untagged definition, or a revisionless submit, whose tags
    # Batch does not evaluate) makes the negated condition true, so that is denied too; the
    # deny above, which needs the tag, cannot stop a submit of another project's definition by
    # its bare name.
    role.add_to_principal_policy(
        iam.PolicyStatement(
            effect=iam.Effect.DENY,
            actions=["batch:SubmitJob"],
            resources=[arns.snakemake_job_definitions, arns.nextflow_job_definitions],
            conditions={"StringNotEquals": _own_project_resource(project_id)},
        )
    )


def _grant_describe(role: iam.IRole) -> None:
    """The Describe/List calls the engines make; none supports resource-level permissions."""
    role.add_to_principal_policy(
        iam.PolicyStatement(
            # The aws-batch executor fork detects whether a queue is EC2 or Fargate before
            # building each child job definition, walking queue → compute environment:
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
    # The engines register their own child job definitions, each engine under its own name
    # prefix.
    role.add_to_principal_policy(
        iam.PolicyStatement(actions=["batch:DescribeJobDefinitions"], resources=["*"])
    )


def _grant_job_definitions(
    role: iam.IRole, arns: _Arns, project_id: str, task_log_group: str
) -> None:
    """Register (and, for Snakemake, deregister) this project's child job definitions."""
    # Snakemake registers one per child job, tagged with this project (as its job is), and
    # deregisters it on cleanup: only this project's. It logs to this project's group
    # (--aws-batch-log-group) or, with no log group of its own, Batch's default: never into
    # another project's.
    role.add_to_principal_policy(
        iam.PolicyStatement(
            actions=["batch:RegisterJobDefinition"],
            resources=[arns.snakemake_job_definitions],
            conditions={
                "StringEquals": _own_project_request(project_id),
                "StringEqualsIfExists": {"batch:AWSLogsGroup": task_log_group},
            },
        )
    )
    role.add_to_principal_policy(
        iam.PolicyStatement(
            actions=["batch:DeregisterJobDefinition"],
            resources=[arns.snakemake_job_definitions],
            conditions={"StringEquals": _own_project_resource(project_id)},
        )
    )
    # nf-biffix's executor registers each Nextflow definition tagged with this project, and a
    # registration without that tag is refused, so no untagged nf-* definition is ever created
    # (Batch lets a coordinator tag an UNTAGGED definition as its own, verified live, so one
    # would be anyone's). It logs to this project's group or, with no log group of its own,
    # Batch's default: never into another project's. nf-amazon never deregisters one, so
    # nothing may.
    role.add_to_principal_policy(
        iam.PolicyStatement(
            actions=["batch:RegisterJobDefinition"],
            resources=[arns.nextflow_job_definitions],
            conditions={
                "StringEquals": _own_project_request(project_id),
                "StringEqualsIfExists": {"batch:AWSLogsGroup": task_log_group},
            },
        )
    )


def _grant_tagging(role: iam.IRole, arns: _Arns, queue_arns: list[str], project_id: str) -> None:
    """Tag what the engines create, with this project only; never re-tag a job's run."""
    # The engines tag what they create: their job definitions (Snakemake with the run and
    # project, nf-biffix with the project next to nf-amazon's `nextflow.io/*` tags) and every
    # child job at submit (its run and project). Batch authorizes a tag-on-create as
    # TagResource on the new resource, and a tagged SubmitJob also as TagResource on the job
    # queue it names (verified live: without the queue, every tagged submit is denied). So
    # TagResource is allowed on Snakemake's job definitions, and on Nextflow's, jobs and this
    # role's queues when the tags name this project. Nothing untags.
    role.add_to_principal_policy(
        iam.PolicyStatement(
            actions=["batch:TagResource"],
            resources=[arns.snakemake_job_definitions],
        )
    )
    role.add_to_principal_policy(
        iam.PolicyStatement(
            actions=["batch:TagResource"],
            resources=[arns.job, *queue_arns, arns.nextflow_job_definitions],
            conditions={"StringEquals": _own_project_request(project_id)},
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
    # Nor may it tag another project's job or job definition at all (whatever tags it has):
    # re-tagging another project's definition as its own would let it submit that definition.
    # Only a resource that HAS another project's tag: Batch evaluates a definition's
    # tag-on-create as if the new revision already carried the request's tags, so denying a
    # tag on any already-tagged definition would refuse every registration (verified live).
    role.add_to_principal_policy(
        iam.PolicyStatement(
            effect=iam.Effect.DENY,
            actions=["batch:TagResource"],
            resources=[arns.job, arns.job_definition],
            conditions=_another_projects(project_id),
        )
    )
    role.add_to_principal_policy(
        iam.PolicyStatement(
            effect=iam.Effect.DENY,
            actions=["batch:UntagResource"],
            resources=[arns.job, arns.job_definition],
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
