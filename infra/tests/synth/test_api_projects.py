"""How the api stack keeps projects apart: each project's context, coordinator and grants reach
only its own project."""

import dataclasses
import json
from collections.abc import Iterable
from pathlib import Path

import aws_cdk as cdk
import pytest
from aws_cdk import aws_lambda as lambda_
from aws_cdk.assertions import Template

from biffix_infra.app_graph import Stacks
from biffix_infra.config import (
    DataSource,
    Project,
)
from biffix_infra.constructs import batch_access
from biffix_infra.stacks.project_stack import ProjectStack
from tests.helpers import REPO_ROOT
from tests.synth.synth_helpers import (
    _BATCH,
    BATCH_JOBS,
    CONFIG,
    LEGACY,
    Json,
    _actions,
    _build,
    _context_parameter,
    _grants,
    _lambda_env,
    _logical_id,
    _project_context,
    _resources,
    _statements_for,
)


def test_each_project_s_coordinator_reaches_only_its_own_project(
    two_project_stacks: Stacks,
) -> None:
    # A run's blast radius is one project. The coordinator runs the workflow's code,
    # so each project's coordinator role names only its own bucket, key, data sources and run role.
    stacks = two_project_stacks
    api = stacks["api"]
    t = Template.from_stack(api)
    projects = {p.project_id: p for p in stacks["projects"]}
    assert set(api.coordinator_roles) == set(projects) == {CONFIG.projects[0].id, "other", "legacy"}
    assert set(api.coordinator_job_definitions) == set(projects)
    for pid, role in api.coordinator_roles.items():
        statements = _statements_for(t, api, role)
        named = " ".join(r for st in statements for r in _resources(st))
        for other_id in projects:
            # Cross-stack references carry the exporting stack's name (`project-<id>`).
            mentioned = f"Import:project-{other_id}:" in named
            assert mentioned == (other_id == pid), (pid, other_id)
        # Its only PassRole is its own project's run role.
        passes = [st for st in statements if _grants(st, "iam:PassRole")]
        (only,) = [r for st in passes for r in _resources(st)]
        assert f"project-{pid}:" in only and "RunBaseRole" in only, only
        # An imported bucket or key is a literal ARN: only legacy's coordinator names them.
        assert ("legacy-bucket" in named) == (pid == "legacy"), (pid, named)
        assert ("key/legacy-1" in named) == (pid == "legacy"), (pid, named)
        # The data sources it reads are its own project's (`other` has one, the others none).
        reads_core = any("seq-core" in r for st in statements for r in _resources(st))
        assert reads_core == (pid == "other"), pid
    # Each project's template runs as its own role; the backend launches the default project's.
    for pid, job_definition in api.coordinator_job_definitions.items():
        props = t.find_resources("AWS::Batch::JobDefinition")[_logical_id(api, job_definition)][
            "Properties"
        ]
        assert props["JobDefinitionName"] == f"biffix-acme-coordinator-{pid}"
        role_id = _logical_id(api, api.coordinator_roles[pid])
        assert props["ContainerProperties"]["JobRoleArn"] == {"Fn::GetAtt": [role_id, "Arn"]}
        # Its Nextflow tasks log to its own project's group (the entrypoint's
        # aws.batch.logsGroup), which that project's stack creates with the standard retention.
        env = {e["Name"]: e["Value"] for e in props["ContainerProperties"]["Environment"]}
        assert env["BIFFIX_TASK_LOG_GROUP"] == f"/biffix/project-{pid}/batch", env
        (project,) = [p for p in stacks["projects"] if p.project_id == pid]
        (group,) = Template.from_stack(project).find_resources("AWS::Logs::LogGroup").values()
        assert group["Properties"]["LogGroupName"] == f"/biffix/project-{pid}/batch"
        assert group["Properties"]["RetentionInDays"] == 365
    # The backend launches a run from its project's template, named in that project's context
    # (an intra-stack reference to the revisioned ARN), never from its environment.
    for fn in (api.backend_lambda, api.sweep_lambda):
        assert "BIFFIX_COORDINATOR_JOB_DEF" not in _lambda_env(api, fn)
    for pid, job_definition in api.coordinator_job_definitions.items():
        context = _project_context(api, pid)
        lid = _logical_id(api, job_definition)
        assert context["coordinator_job_definition"] == f"{{Ref:{lid}}}", (pid, context)
    # Each project's run base role (its stack's) reaches only its own bucket, key and data
    # sources: another project's would render as an import of that project's stack, or (an
    # imported bucket or key) as its literal ARN.
    for project in stacks["projects"]:
        pt = Template.from_stack(project)
        named = " ".join(
            r for st in _statements_for(pt, project, project.run_base_role) for r in _resources(st)
        )
        assert "Import:project-" not in named, (project.project_id, named)
        assert ("legacy-bucket" in named) == (project.project_id == "legacy"), named
        assert ("key/legacy-1" in named) == (project.project_id == "legacy"), named
        assert ("seq-core" in named) == (project.project_id == "other"), named


# The backend's golden example of a project context (its `ProjectContext` parses it).
PROJECT_CONTEXT_FIXTURE = REPO_ROOT / "crates/biffix-backend/tests/fixtures/project_context.json"


def test_each_project_s_context_is_an_ssm_parameter_both_lambdas_read(
    two_project_stacks: Stacks,
) -> None:
    # The backend routes each request to a project and reads the projects from SSM (one Standard
    # String parameter each, under the tenant's path), so its environment stays the same size
    # however many projects a deploy has.
    stacks = two_project_stacks
    api = stacks["api"]
    t = Template.from_stack(api)
    default = CONFIG.projects[0].id
    assert set(api.project_contexts) == {default, "other", "legacy"}
    for project in stacks["projects"]:
        pid = project.project_id
        props = _context_parameter(api, pid)
        assert props["Name"] == f"/biffix/acme/projects/{pid}"
        assert (props["Type"], props["Tier"]) == ("String", "Standard")
        context = _project_context(api, pid)
        assert context["id"] == pid
        assert context["task_log_group"] == f"/biffix/project-{pid}/batch"
        assert context["data_sources"] == [s.to_backend() for s in project.data_sources]
        own = f"{{Import:project-{pid}:"
        assert context["run_role_arn"].startswith(own) and "RunBaseRole" in context["run_role_arn"]
        if pid == "legacy":
            # An imported bucket and key are literal; it has no CORS rules applied.
            assert context["bucket"] == "legacy-bucket"
            assert (context["uploads"], context["viewer_cors"]) == (False, False)
        else:
            assert context["bucket"].startswith(own) and "ProjectBucket" in context["bucket"]
            assert (context["uploads"], context["viewer_cors"]) == (True, True)
    # Both Lambdas find the parameters by path, may read only that path, and deploy after them.
    path = "arn:aws:ssm:{Ref:AWS::Region}:{Ref:AWS::AccountId}:parameter/biffix/acme/projects"
    functions = t.find_resources("AWS::Lambda::Function")
    for fn in (api.backend_lambda, api.sweep_lambda):
        env = _lambda_env(api, fn)
        assert env["BIFFIX_PROJECTS_PATH"] == "/biffix/acme/projects/"
        assert env["BIFFIX_DEFAULT_PROJECT"] == default
        ssm = {
            frozenset(_actions(st)): _resources(st)
            for st in _statements_for(t, api, fn.role)
            if any(a.startswith("ssm:") for a in _actions(st))
        }
        assert ssm == {frozenset({"ssm:GetParametersByPath"}): [path, f"{path}/*"]}, ssm
        depends = functions[_logical_id(api, fn)]["DependsOn"]
        for parameter in api.project_contexts.values():
            assert _logical_id(api, parameter) in depends


def test_the_project_context_has_exactly_the_backend_s_keys(default_stacks: Stacks) -> None:
    # The contract with the backend (crates/biffix-backend/src/config/projects.rs): the context CDK
    # writes has exactly the top-level keys of the backend's golden example, and a data source with
    # every optional key set has exactly the keys of the golden one. The fixture is part of the
    # contract: its absence is a failure, never a skip.
    assert PROJECT_CONTEXT_FIXTURE.is_file(), f"missing {PROJECT_CONTEXT_FIXTURE}"
    golden = json.loads(PROJECT_CONTEXT_FIXTURE.read_text())
    api = default_stacks["api"]
    context = _project_context(api, api.default_project_id)
    assert sorted(context) == sorted(golden)
    (golden_source,) = golden["data_sources"]
    full = DataSource(
        id="refs",
        name="Refs",
        bucket="refs",
        description="Reference genomes.",
        kind="references",
        viewer_cors_applied=True,
    )
    assert sorted(full.to_backend()) == sorted(golden_source)


def test_a_project_context_that_could_outgrow_its_parameter_is_refused(tmp_path: Path) -> None:
    # A Standard parameter holds 4 KB: synth refuses a context that could exceed it once its
    # tokens resolve, rather than CloudFormation failing mid-deploy.
    sources = [
        DataSource(id=f"s{i}", name=f"Source {i}", bucket=f"b{i}", description="d" * 200)
        for i in range(20)
    ]
    big = dataclasses.replace(CONFIG, projects=[Project(id="demo", data_sources=tuple(sources))])
    with pytest.raises(ValueError, match="'demo': its context could be"):
        _build(tmp_path, config=big)


def _objects_by_project(
    resources: Iterable[str], projects: list[ProjectStack]
) -> dict[str, set[str]]:
    """The key patterns `resources` (rendered) grant, by the project whose bucket they are in;
    fails if any resource is not an object pattern of a project bucket."""
    patterns: dict[str, set[str]] = {}
    for r in resources:
        for project in projects:
            pid = project.project_id
            imported = f"Import:project-{pid}:" in r and "ProjectBucket" in r and "}/" in r
            name = project.bucket.bucket_name
            literal = None if cdk.Token.is_unresolved(name) else name
            if imported:
                patterns.setdefault(pid, set()).add(r.partition("}/")[2])
                break
            if literal and r.startswith(f"arn:aws:s3:::{literal}/"):
                patterns.setdefault(pid, set()).add(r.removeprefix(f"arn:aws:s3:::{literal}/"))
                break
        else:
            raise AssertionError(f"not an object pattern of a project bucket: {r}")
    return patterns


def _is_object_pattern(resource: str) -> bool:
    """Whether a rendered S3 resource names objects (`<bucket arn>/<pattern>`), not a bucket."""
    if "}/" in resource:
        return True
    bucket_and_key = resource.removeprefix("arn:aws:s3:::")
    return bucket_and_key != resource and "/" in bucket_and_key


def test_the_backend_s_s3_grants_cover_every_project(two_project_stacks: Stacks) -> None:
    # The backend serves every project, so it holds each project's control-plane grants: read
    # its bucket, write runs' inputs and staged sources, and run uploads where the bucket takes
    # them; the sweep writes each project's runs' inputs and provenance.
    stacks = two_project_stacks
    api = stacks["api"]
    t = Template.from_stack(api)
    projects = stacks["projects"]

    def granted(fn: lambda_.IFunction, action: str) -> dict[str, set[str]]:
        # Object patterns only (a bucket ARN is ListBucket's), of project buckets only (`other`'s
        # data source is read too, by the data-sources test's measure).
        return _objects_by_project(
            {
                r
                for st in _statements_for(t, api, fn.role)
                if _grants(st, action)
                for r in _resources(st)
                if _is_object_pattern(r) and "seq-core" not in r
            },
            projects,
        )

    common = {"biffix/runs/*/inputs/*", "workflow-sources/*"}
    assert granted(api.backend_lambda, "s3:PutObject") == {
        CONFIG.projects[0].id: common | {"uploads/*"},
        "other": common | {"uploads/*"},
        # An imported bucket without the CORS rule takes no uploads.
        "legacy": common,
    }
    assert granted(api.backend_lambda, "s3:GetObject") == {p.project_id: {"*"} for p in projects}
    assert granted(api.sweep_lambda, "s3:PutObject") == {
        p.project_id: {"biffix/runs/*/inputs/*", "biffix/runs/*/provenance/*"} for p in projects
    }


def test_image_builds_read_workflow_sources_in_every_project_bucket(
    two_project_stacks: Stacks,
) -> None:
    # A version's build context is staged in its project's bucket, so the image builds read
    # `workflow-sources/` in every project's (and decrypt with an imported key).
    stacks = two_project_stacks
    build = stacks["build"]
    t = Template.from_stack(build)
    for arch, project in build.build_projects.items():
        statements = _statements_for(t, build, project.role)
        reads = {
            r
            for st in statements
            if _grants(st, "s3:GetObject")
            for r in _resources(st)
            if r.endswith("/workflow-sources/*")
        }
        assert _objects_by_project(reads, stacks["projects"]) == {
            p.project_id: {"workflow-sources/*"} for p in stacks["projects"]
        }, (arch, reads)
        decrypts = {r for st in statements if _grants(st, "kms:Decrypt") for r in _resources(st)}
        assert LEGACY.key_arn in decrypts, (arch, decrypts)


# A role's statements on Batch, by (effect, action): their (resources, condition) pairs.
BatchPolicy = dict[tuple[str, str], list[tuple[list[str], Json | None]]]


def _coordinator_batch_policy(statements: list[Json]) -> BatchPolicy:
    """A coordinator role's statements on Batch jobs, by (effect, action): their (resources,
    condition) pairs."""
    policy: BatchPolicy = {}
    job_actions = ("batch:SubmitJob", "batch:TerminateJob", "batch:TagResource")
    definition_actions = ("batch:RegisterJobDefinition", "batch:DeregisterJobDefinition")
    for st in statements:
        for action in _actions(st):
            if action in (*job_actions, *definition_actions, "batch:UntagResource"):
                policy.setdefault((st["Effect"], action), []).append(
                    (sorted(_resources(st)), st.get("Condition"))
                )
    return {k: sorted(v, key=json.dumps) for k, v in policy.items()}


def _expected_coordinator_batch_policy(project: str, queues: list[str]) -> BatchPolicy:
    job_definitions = f"{_BATCH}:job-definition/*"
    snakemake = f"{_BATCH}:job-definition/snakejob-def-*"
    nextflow = f"{_BATCH}:job-definition/nf-*"
    snakemake_revisions = f"{_BATCH}:job-definition/snakejob-def-*:*"
    nextflow_revisions = f"{_BATCH}:job-definition/nf-*:*"
    own = {"aws:RequestTag/biffix:project": project}
    own_resource = {"aws:ResourceTag/biffix:project": project}
    tags_the_project = {"ForAnyValue:StringEquals": {"aws:TagKeys": ["biffix:project"]}}
    # A resource tagged with another project.
    another = {
        "Null": {"aws:ResourceTag/biffix:project": "false"},
        "StringNotEquals": own_resource,
    }
    return {
        # A child job is submitted only tagged with this project, from a Snakemake or Nextflow
        # definition revision tagged with this project; no other definition at all.
        ("Allow", "batch:SubmitJob"): sorted(
            [
                (
                    sorted([*queues, BATCH_JOBS]),
                    {"StringEquals": own, **tags_the_project},
                ),
                (
                    sorted([snakemake_revisions, nextflow_revisions]),
                    {"StringEquals": {**own, **own_resource}, **tags_the_project},
                ),
            ],
            key=json.dumps,
        ),
        # Never a coordinator job definition (another project's coordinator role), nor a child
        # job definition tagged with another project (its run role), nor an engine's definition
        # not tagged with this project (untagged, or submitted by its bare name).
        ("Deny", "batch:SubmitJob"): sorted(
            [
                ([f"{_BATCH}:job-definition/biffix-*"], None),
                ([job_definitions], another),
                (sorted([snakemake, nextflow]), {"StringNotEquals": own_resource}),
            ],
            key=json.dumps,
        ),
        # Only this project's jobs are terminated.
        ("Allow", "batch:TerminateJob"): [([BATCH_JOBS], {"StringEquals": own_resource})],
        # Both engines' definitions are born tagged with this project; Snakemake's (only) are
        # deregistered; both log to this project's own group (or Batch's default).
        ("Allow", "batch:RegisterJobDefinition"): sorted(
            [
                (
                    [engine],
                    {
                        "StringEquals": own,
                        "StringEqualsIfExists": {
                            "batch:AWSLogsGroup": f"/biffix/project-{project}/batch"
                        },
                    },
                )
                for engine in (snakemake, nextflow)
            ],
            key=json.dumps,
        ),
        ("Allow", "batch:DeregisterJobDefinition"): [([snakemake], {"StringEquals": own_resource})],
        # Snakemake's job definitions (it tags its own); Nextflow's, jobs, and the queues a tagged
        # submit names (Batch authorizes that tag-on-create on the queue too), only with this
        # project's tag.
        ("Allow", "batch:TagResource"): sorted(
            [
                ([snakemake], None),
                (sorted([BATCH_JOBS, *queues, nextflow]), {"StringEquals": own}),
            ],
            key=json.dumps,
        ),
        ("Deny", "batch:TagResource"): sorted(
            [
                # The run tag is write-once: no job that carries one is re-tagged.
                ([BATCH_JOBS], {"Null": {"aws:ResourceTag/biffix:run": "false"}}),
                # No other project's job or job definition is tagged at all.
                (sorted([BATCH_JOBS, job_definitions]), another),
            ],
            key=json.dumps,
        ),
        ("Deny", "batch:UntagResource"): [
            (
                sorted([BATCH_JOBS, job_definitions]),
                {"ForAnyValue:StringEquals": {"aws:TagKeys": ["biffix:run", "biffix:project"]}},
            )
        ],
    }


def test_a_coordinator_cannot_move_a_job_s_tags_or_reach_another_project_s_jobs(
    two_project_stacks: Stacks,
) -> None:
    # The engine ingest route trusts a job's `biffix:run` tag, and each coordinator role belongs
    # to one project. IAM makes the run tag write-once (a coordinator tags a job only as it creates
    # it, never re-tags or untags), confines submit, terminate and tag to jobs of its own project,
    # and never lets it submit a coordinator job definition (another project's role).
    stacks = two_project_stacks
    api = stacks["api"]
    t = Template.from_stack(api)
    assert len(api.coordinator_roles) == 3
    for pid, role in api.coordinator_roles.items():
        statements = _statements_for(t, api, role)
        # Every worker queue (imported from the compute stack), each once; never the coordinator
        # queue, where child jobs never go.
        queues = sorted(
            {
                r
                for st in statements
                for r in _resources(st)
                if r.startswith("{Import:compute:") and "JobQueueArn" in r
            }
        )
        assert len(queues) == len(stacks["compute"].queues) - 1, queues
        assert not any("Queuecoordinator" in q for q in queues), queues
        policy = _coordinator_batch_policy(statements)
        assert policy == _expected_coordinator_batch_policy(pid, queues), (pid, policy)
        allows = [st for st in statements if st["Effect"] == "Allow"]
        # Logs: its own project's task log group only, never the shared Batch job log group
        # (every project's coordinator logs); DescribeLogGroups has no resource-level
        # permissions.
        logs = {r for st in allows if "logs:GetLogEvents" in _actions(st) for r in _resources(st)}
        arn = "arn:aws:logs:{Ref:AWS::Region}:{Ref:AWS::AccountId}:log-group:"
        group = f"{arn}/biffix/project-{pid}/batch"
        assert logs == {group, f"{group}:*"}, logs
        assert [_resources(st) for st in allows if "logs:DescribeLogGroups" in _actions(st)] == [
            ["*"]
        ]
    # The backend creates each coordinator job with its run and project tags (the run's
    # project, any of this deploy's), may tag nothing else, and tells the coordinator its project.
    default = CONFIG.projects[0].id
    for fn in (api.backend_lambda, api.sweep_lambda):
        (job_tag,) = [
            st
            for st in _statements_for(t, api, fn.role)
            if "batch:TagResource" in _actions(st) and BATCH_JOBS in _resources(st)
        ]
        # The new job and the coordinator queue the submit names (Batch authorizes a tagged
        # submit's tag-on-create on both; without the queue every launch is denied).
        (queue,) = [r for r in _resources(job_tag) if "JobQueueArn" in r]
        assert "Queuecoordinator" in queue, queue
        assert set(_resources(job_tag)) == {BATCH_JOBS, queue}
        assert job_tag["Condition"] == {
            "ForAllValues:StringEquals": {"aws:TagKeys": ["biffix:run", "biffix:project"]},
            "StringEquals": {
                "aws:RequestTag/biffix:project": [default, "other", "legacy"],
            },
        }
        env = _lambda_env(api, fn)
        assert "BIFFIX_PROJECT" not in env
        assert env["BIFFIX_DEFAULT_PROJECT"] == default
    aws = REPO_ROOT / "crates/biffix-backend/src/aws"

    def compact(path: str) -> str:
        # Layout-insensitive: rustfmt may wrap a call (adding a trailing comma).
        return "".join((aws / path).read_text().split()).replace(",)", ")")

    # The tag and the coordinator's BIFFIX_PROJECT are the launch's project (the run's version's).
    assert "coordinator_job_tags(&launch.run_id,&launch.project.id)" in compact("provider/batch.rs")
    env_rs = compact("coordinator_env.rs")
    assert 'constPROJECT:&str="BIFFIX_PROJECT";' in env_rs
    assert "(PROJECT,launch.project.id.clone())" in env_rs
    batch = (aws / "batch.rs").read_text()
    assert f'pub(crate) const RUN_TAG: &str = "{batch_access.RUN_TAG}";' in batch
    assert f'pub(crate) const PROJECT_TAG: &str = "{batch_access.PROJECT_TAG}";' in batch


def test_the_backend_reads_task_logs_from_the_shared_and_every_project_s_group(
    two_project_stacks: Stacks,
) -> None:
    # Coordinators and Snakemake tasks log to /aws/batch/job, Nextflow tasks to their project's
    # group; the backend reads streams in exactly the shared group and every project's, and learns
    # each project's group from its context.
    stacks = two_project_stacks
    api = stacks["api"]
    t = Template.from_stack(api)
    groups = [f"/biffix/project-{p.project_id}/batch" for p in stacks["projects"]]
    arn = "arn:aws:logs:{Ref:AWS::Region}:{Ref:AWS::AccountId}:log-group:"
    (reads,) = [
        st
        for st in _statements_for(t, api, api.backend_lambda.role)
        if "logs:GetLogEvents" in _actions(st)
    ]
    assert _actions(reads) == {"logs:GetLogEvents"}
    assert sorted(_resources(reads)) == sorted(
        f"{arn}{g}:log-stream:*" for g in ("/aws/batch/job", *groups)
    )
    assert "BIFFIX_TASK_LOG_GROUP" not in _lambda_env(api, api.backend_lambda)
    for project in stacks["projects"]:
        context = _project_context(api, project.project_id)
        assert context["task_log_group"] == f"/biffix/project-{project.project_id}/batch"
