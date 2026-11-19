"""The api stack, the hub: the coordinator role and job definition, the backend Lambda and
the HTTP API."""

import base64
import dataclasses
import fnmatch
import gzip
import json
import posixpath
import re
from pathlib import Path
from typing import Any

import pytest
from aws_cdk.assertions import Match, Template
from constructs import IConstruct

from biffix_infra.app_graph import Stacks
from biffix_infra.config import DeployConfig
from biffix_infra.constructs.script_project import COMMON_SCRIPT, bundled_script
from biffix_infra.host_boot import (
    HOST_STATE_DIR,
    INSTANCE_STORE_MARKER,
    SCRATCH_DIR,
    single_job_boot_header,
)
from biffix_infra.rbac import RBAC_GROUP_ROLES
from biffix_infra.stacks.api_stack import ApiStack
from biffix_infra.stacks.build_stack import RUNTIME_BUILD_SCRIPT
from biffix_infra.stacks.project_stack import ProjectStack
from tests.deploy_config_file import write_config
from tests.helpers import REPO_ROOT
from tests.synth.synth_helpers import (
    _BATCH,
    BATCH_JOBS,
    CONFIG,
    SINGLE_JOB_CLASSES,
    SINGLE_JOB_CONFIG,
    SINGLE_JOB_VARIANT_JOB_DEFINITIONS,
    VARIANT_JOB_DEFINITIONS,
    Json,
    _actions,
    _actions_for_role,
    _build,
    _context_parts,
    _grants,
    _lambda_env,
    _logical_id,
    _policies,
    _project_context,
    _render,
    _resources,
    _single_stacks,
    _statements,
    _statements_for,
)


def _context_size(api: ApiStack, pid: str, resolved: int = 145) -> int:
    """An upper estimate of project `pid`'s context value once deployed: its literal text as it
    is, each deploy-time value (an ARN, a bucket name) at `resolved` bytes (the longest a real
    multi-project deploy resolved was 145)."""
    return sum(len(p) if isinstance(p, str) else resolved for p in _context_parts(api, pid))


def test_backend_gets_each_queues_capacity(default_stacks: Stacks) -> None:
    t = Template.from_stack(default_stacks["api"])
    (backend,) = [
        f
        for f in t.find_resources("AWS::Lambda::Function").values()
        if "BIFFIX_QUEUE_CLASSES" in f["Properties"].get("Environment", {}).get("Variables", {})
    ]
    raw = json.loads(backend["Properties"]["Environment"]["Variables"]["BIFFIX_QUEUE_CAPACITIES"])
    # The queue-name prefix once, then each capacity's queue-classes; decoded as the backend does
    # (queue_capacity.rs), each queue's own capacity by its physical name (swapped values would
    # break the backend's spot-interruption reasoning).
    prefix = raw.pop("prefix")
    capacities = {f"{prefix}{cls}": cap for cap, classes in raw.items() for cls in classes}
    assert capacities == {CONFIG.queue_name(qc): qc.capacity for qc in CONFIG.queue_classes}
    # The coordinator queue is always on-demand (a reclaim would kill the run); the default config's
    # cpu queue is spot.
    assert capacities[CONFIG.queue_name(CONFIG.coordinator_queue_class)] == "ON_DEMAND"
    assert capacities["biffix-acme-cpu-spot"] == "SPOT"


def _env_value_size(name: str, value: object) -> int:
    """A Lambda environment value's size once deployed: a literal as it is, a deploy-time value (an
    ARN, an image URI) 110 bytes. BIFFIX_SINGLE_JOB_QUEUES is JSON holding one queue ARN per
    single-job queue-class: its literal parts as they are, each ARN 110 bytes."""
    if isinstance(value, str):
        return len(value)
    if name == "BIFFIX_SINGLE_JOB_QUEUES":
        assert isinstance(value, dict), value
        sep, parts = value["Fn::Join"]
        assert sep == "", value
        return sum(len(p) if isinstance(p, str) else 110 for p in parts)
    return 110


def test_the_backend_environment_stays_well_inside_lambda_s_4_kb(tmp_path: Path) -> None:
    # Lambda refuses a function whose environment (names + values) exceeds 4 KB, and only at
    # deploy: a real deploy failed at 4108 bytes. Synth a deploy that size (13
    # worker queue-classes, a custom domain, Google federation, two data sources), plus two
    # single-job queue-classes, and bound the
    # backend's and the sweep's environments: literal values as they are, each
    # CloudFormation-resolved one (an ARN, an image URI) at 110 bytes (that deploy's
    # resolved values average 81, the longest 145), under a budget ~200 bytes short of the limit.
    # The projects' contexts live in SSM, not the environment, so adding a third and a fourth
    # project (each with a data source) does not grow it by a byte; each context fits a Standard
    # parameter (4 KB).
    arches = [f"arch-{a}" for a in ("c6a", "c7a", "c7i", "c7g", "c8g", "m7i", "m8a", "m8g")]
    arenas = ["arch-m8a-arena", "arch-m8g-arena", "arch-c8g64"]
    classes = (
        [{"name": "coordinator", "capacity": "ON_DEMAND", "coordinator": True}]
        + [
            {"name": n, "capacity": "SPOT" if n != "cpu-ondemand" else "ON_DEMAND"}
            for n in ["cpu-spot", "cpu-ondemand", *arches, *arenas]
        ]
        + [
            # Two single-job queue-classes: BIFFIX_SINGLE_JOB_QUEUES names each queue's ARN.
            {
                "name": f"single-{size}",
                "capacity": "ON_DEMAND",
                "single_job": True,
                "instances": [f"c6id.{size}"],
            }
            for size in ("8xlarge", "16xlarge")
        ]
    )
    source = {
        "bucket": "acme-genomics-data",
        "description": "FASTQs for the benchmark samples.",
        "viewer_cors_applied": True,
    }
    # Names at least as long as the real deploy's, so the budget below still bounds it.
    two_projects: list[dict[str, Any]] = [
        {
            "id": "genomics-reads",
            "bucket_name": "acme-genomics-data",
            "uploads_cors_applied": True,
            "viewer_cors_applied": False,
            "data_sources": [
                {"id": "bench-data", "name": "Benchmark samples", "prefix": "data/"} | source,
                {"id": "bench-refs", "name": "References", "prefix": "references/"} | source,
            ],
        },
        {"id": "imported-archive", "bucket_name": "acme-imported-archive"},
    ]
    more = [
        {
            "id": f"extra-{i}",
            "bucket_name": f"acme-extra-{i}",
            "key_arn": f"arn:aws:kms:us-east-1:111122223333:key/extra-{i}",
            "data_sources": [
                {
                    "id": "core",
                    "name": "Sequencing core deliveries",
                    "bucket": f"seq-core-{i}",
                    "prefix": "deliveries/lab-x/",
                    "description": "Run folders the sequencing core delivers here.",
                    "key_arn": f"arn:aws:kms:us-east-1:111122223333:key/core-{i}",
                    "viewer_cors_applied": True,
                }
            ],
        }
        for i in (3, 4)
    ]

    def sizes(name: str, projects: list[dict[str, Any]]) -> tuple[dict[str, int], ApiStack]:
        (tmp_path / name).mkdir()
        config = DeployConfig.load(
            write_config(
                tmp_path / name,
                tenant="acme-large",
                queue_classes=classes,
                cognito_domain_prefix="biffix-acme-genomics",
                app_domain="biffix.research.acme-genomics.example.org",
                google_oidc={"client_id": "7" * 40 + ".apps.googleusercontent.com"},
                projects=projects,
            )
        )
        api = _build(tmp_path / name, config)["api"]
        measured = {}
        for label, fn in (("backend", api.backend_lambda), ("sweep", api.sweep_lambda)):
            env = _lambda_env(api, fn)
            measured[label] = sum(len(k) + _env_value_size(k, v) for k, v in env.items())
        return measured, api

    two, _ = sizes("two", two_projects)
    four, api = sizes("four", two_projects + more)
    assert four == two, (two, four)
    assert four["backend"] <= 3900, four
    assert four["sweep"] <= 3900, four
    for pid in api.project_contexts:
        assert _context_size(api, pid) < 4096, (pid, _context_size(api, pid))
    # The live flags ride in the context: each source's `viewer_cors`, and the imported bucket
    # with only the upload rule applied.
    reads = _project_context(api, "genomics-reads")
    assert (reads["uploads"], reads["viewer_cors"]) == (True, False)
    assert all(s["viewer_cors"] for s in reads["data_sources"])


def test_api_uses_http_api_no_load_balancer(default_stacks: Stacks) -> None:
    stacks = default_stacks
    t = Template.from_stack(stacks["api"])
    t.resource_count_is("AWS::ApiGatewayV2::Api", 1)
    t.resource_count_is("AWS::ElasticLoadBalancingV2::LoadBalancer", 0)
    t.resource_count_is("AWS::ECS::Service", 0)
    t.resource_count_is("AWS::ApiGatewayV2::Authorizer", 1)
    t.has_resource_properties(
        "AWS::ApiGatewayV2::Authorizer", Match.object_like({"AuthorizerType": "JWT"})
    )
    # The coordinator job definitions live here (next to the backend that submits them): one
    # template per project.
    names = [
        r["Properties"]["JobDefinitionName"]
        for r in t.find_resources("AWS::Batch::JobDefinition").values()
    ]
    assert names == [f"biffix-acme-coordinator-{p.id}" for p in CONFIG.projects], names


def test_backend_submits_only_variant_job_definitions_on_the_coordinator_queue(
    default_stacks: Stacks,
) -> None:
    # Runs launch on coordinator variants (base image + engine runtime), whose job definitions the
    # backend registers: SubmitJob is granted on the coordinator queue and the variant NAME prefix
    # only, never on job definitions in general (nor on the template itself). The API Lambda (a
    # live submit) and the sweep (a parked run's launch) both get this from the one
    # `ControlPlaneGrants.launch` helper, so their statements must be identical, not merely two
    # independent matches of the same pattern.
    api = default_stacks["api"]
    t = Template.from_stack(api)
    (backend_submit,) = [
        st
        for st in _statements_for(t, api, api.backend_lambda.role)
        if _actions(st) == {"batch:SubmitJob"}
    ]
    (sweep_submit,) = [
        st
        for st in _statements_for(t, api, api.sweep_lambda.role)
        if _actions(st) == {"batch:SubmitJob"}
    ]
    assert backend_submit == sweep_submit, (backend_submit, sweep_submit)
    (queue,) = [r for r in _resources(backend_submit) if "JobQueueArn" in r]
    assert "Queuecoordinator" in queue, queue
    assert set(_resources(backend_submit)) == {queue, VARIANT_JOB_DEFINITIONS, BATCH_JOBS}
    # Only a job without the single-job execution tag (a tasks-mode coordinator).
    assert backend_submit["Condition"] == {"Null": {"aws:RequestTag/biffix:execution": "true"}}


def test_launchers_can_list_a_run_s_coordinators(default_stacks: Stacks) -> None:
    # A launch whose SubmitJob answer was lost looks the run's coordinator up by its job name
    # (ListJobs, which takes no resource-level permissions) before binding or relaunching it.
    api = default_stacks["api"]
    t = Template.from_stack(api)
    for role in (api.backend_lambda.role, api.sweep_lambda.role):
        (listing,) = [
            st for st in _statements_for(t, api, role) if "batch:ListJobs" in _actions(st)
        ]
        assert _actions(listing) == {"batch:ListJobs"}
        assert _resources(listing) == ["*"]


def test_the_api_lambda_times_out_when_the_backend_budgets_for(default_stacks: Stacks) -> None:
    # Handlers that wait or read against the clock (a cancel waiting out a launch, the samples
    # download) budget inside the backend's API_LAMBDA_TIMEOUT: it must be the deployed one.
    api = default_stacks["api"]
    t = Template.from_stack(api)
    logical_id = _logical_id(api, api.backend_lambda)
    timeout = t.to_json()["Resources"][logical_id]["Properties"]["Timeout"]
    src = (REPO_ROOT / "crates/biffix-backend/src/config/mod.rs").read_text()
    pinned = re.search(
        r"API_LAMBDA_TIMEOUT: std::time::Duration = std::time::Duration::from_secs\((\d+)\)", src
    )
    assert pinned, "API_LAMBDA_TIMEOUT not found"
    assert timeout == int(pinned.group(1))


def test_a_run_s_capability_token_outlives_every_coordinator_attempt(
    default_stacks: Stacks,
) -> None:
    # Batch retries a coordinator under the same job id and environment, so the token minted at
    # launch must last for every attempt's timeout: the backend sizes it from the job definition's
    # values, which must be the deployed ones.
    t = Template.from_stack(default_stacks["api"])
    jobdefs = t.find_resources("AWS::Batch::JobDefinition")
    src = (REPO_ROOT / "crates/biffix-backend/src/auth/capability.rs").read_text()
    pinned = {
        name: int(m.group(1))
        for name in ("COORDINATOR_ATTEMPTS", "COORDINATOR_TIMEOUT_HOURS")
        if (m := re.search(rf"const {name}: u64 = (\d+);", src))
    }
    assert set(pinned) == {"COORDINATOR_ATTEMPTS", "COORDINATOR_TIMEOUT_HOURS"}, pinned
    assert jobdefs
    for jobdef in jobdefs.values():
        props = jobdef["Properties"]
        assert props["RetryStrategy"]["Attempts"] == pinned["COORDINATOR_ATTEMPTS"]
        timeout_secs = pinned["COORDINATOR_TIMEOUT_HOURS"] * 3600
        assert props["Timeout"]["AttemptDurationSeconds"] == timeout_secs


def test_the_coordinator_template_bakes_no_worker_image(default_stacks: Stacks) -> None:
    # The worker image is per version (passed at submit); the template must not supply a default.
    t = Template.from_stack(default_stacks["api"])
    jobdefs = t.find_resources("AWS::Batch::JobDefinition")
    assert len(jobdefs) == len(CONFIG.projects), jobdefs.keys()
    for jobdef in jobdefs.values():
        env = jobdef["Properties"]["ContainerProperties"]["Environment"]
        names = {e["Name"] for e in env}
        assert "BIFFIX_WORKER_IMAGE" not in names
        assert {"BIFFIX_QUEUE_CLASS_MAP", "BIFFIX_REGION"} <= names
        # The queue map names the worker queue-classes only, in config order (the entrypoint's
        # default is the first): never the coordinator's, whatever it is called.
        (queue_map,) = [e["Value"] for e in env if e["Name"] == "BIFFIX_QUEUE_CLASS_MAP"]
        parsed = json.loads(_render(queue_map))
        workers = [qc.name for qc in CONFIG.queue_classes if not qc.coordinator]
        assert workers, "the default config has no worker queue-class"
        assert list(parsed) == workers, parsed
        # Each class names its own queue (its ARN, imported from the compute stack).
        compute = default_stacks["compute"]
        for name in workers:
            queue = _logical_id(compute, compute.queues[name])
            assert parsed[name].startswith("{Import:compute:"), parsed
            assert queue in parsed[name], (name, parsed)
            others = [q for q in compute.queues if q != name]
            assert not any(
                _logical_id(compute, compute.queues[o]) in parsed[name] for o in others
            ), (name, parsed)


def test_batch_retries_a_coordinator_only_after_an_infrastructure_failure(
    default_stacks: Stacks,
) -> None:
    # A lost host (or an image that never started) is retried by Batch under the same job id;
    # anything else ends the run, whose engine retries its own tasks. Variants copy this.
    t = Template.from_stack(default_stacks["api"])
    jobdefs = t.find_resources("AWS::Batch::JobDefinition")
    assert len(jobdefs) == len(CONFIG.projects), jobdefs.keys()
    for jobdef in jobdefs.values():
        strategy = jobdef["Properties"]["RetryStrategy"]
        assert strategy["Attempts"] == 2
        assert strategy["EvaluateOnExit"] == [
            {"Action": "RETRY", "OnStatusReason": "Host EC2*"},
            {"Action": "RETRY", "OnReason": "CannotPullContainerError:*"},
            {"Action": "EXIT", "OnReason": "*"},
        ], strategy


def test_the_sweep_function_is_an_output_for_deploy_sh(default_stacks: Stacks) -> None:
    """deploy.sh finds the sweep it invokes to build a new base image's variants by this output."""
    api = default_stacks["api"]
    t = Template.from_stack(api)
    t.has_output(
        "SweepFunctionName",
        {"Value": {"Ref": _logical_id(api, api.sweep_lambda)}},
    )


def test_backend_and_sweep_can_build_and_register_coordinator_variants(
    default_stacks: Stacks,
) -> None:
    api = default_stacks["api"]
    t = Template.from_stack(api)
    for fn in (api.backend_lambda, api.sweep_lambda):
        granted = _actions_for_role(t, api, fn.role)
        assert {
            "codebuild:StartBuild",
            "codebuild:BatchGetBuilds",
            "batch:DescribeJobDefinitions",
            "batch:RegisterJobDefinition",
            "batch:TagResource",
            "iam:PassRole",
        } <= granted, fn
    statements = [
        statement
        for statement in _statements(t)
        # The task definitions' grant (`test_backend_and_sweep_register_only_safe_task_
        # definitions`) is conditioned; the variants' is not.
        if _actions(statement) == {"batch:RegisterJobDefinition", "batch:TagResource"}
        and "Condition" not in statement
    ]
    assert len(statements) == 2  # backend + sweep
    for statement in statements:
        assert _resources(statement) == [VARIANT_JOB_DEFINITIONS], statement
    # A clone passes only a project template's roles (its job and execution roles); a task
    # definition, the project's run role.
    expected = _template_roles(api) | _run_roles(api, default_stacks["projects"])
    assert len(expected) == 3  # the one project's
    for fn in (api.backend_lambda, api.sweep_lambda):
        passes = [st for st in _statements_for(t, api, fn.role) if _grants(st, "iam:PassRole")]
        assert {r for st in passes for r in _resources(st)} == expected, passes
    funcs = t.find_resources("AWS::Lambda::Function").values()
    wired = [
        f["Properties"]["Environment"]["Variables"]
        for f in funcs
        if "BIFFIX_RUNTIME_BUILD_PROJECT" in f["Properties"]["Environment"]["Variables"]
    ]
    assert len(wired) == 2
    for env in wired:
        assert env["BIFFIX_RUNTIME_BUILD_PROJECT"] == "biffix-acme-runtime-build"
        assert env["BIFFIX_VARIANT_JOB_DEFINITION_PREFIX"] == "biffix-acme-coordinator-rt-"
        # No single-job queue-class, so no single-job variants.
        assert "BIFFIX_SINGLE_JOB_VARIANT_JOB_DEFINITION_PREFIX" not in env
        assert "BIFFIX_COORDINATOR_IMAGE" in env


def _template_roles(api: ApiStack) -> set[str]:
    """Every project template's job (coordinator) role and execution role, rendered."""
    return {
        _render(api.resolve(arn))
        for pid, role in api.coordinator_roles.items()
        for arn in (
            role.role_arn,
            api.coordinator_job_definitions[pid].container.execution_role.role_arn,
        )
    }


def _run_roles(api: ApiStack, projects: list[ProjectStack]) -> set[str]:
    """Every project's run base role, rendered as the api stack imports it."""
    return {_render(api.resolve(p.run_base_role.role_arn)) for p in projects}


def test_backend_and_sweep_pass_every_project_s_template_and_run_roles_only(
    two_project_stacks: Stacks,
) -> None:
    # A launch clones the run's project's template, so both Lambdas pass every project's
    # coordinator and execution roles, and registers the run's task job definitions, which run
    # as the project's run role: each project's run role too, and no other role.
    stacks = two_project_stacks
    api = stacks["api"]
    t = Template.from_stack(api)
    runs = _run_roles(api, stacks["projects"])
    assert all("RunBaseRole" in r for r in runs), runs
    expected = _template_roles(api) | runs
    assert len(expected) == 3 * len(stacks["projects"]) == 9
    for fn in (api.backend_lambda, api.sweep_lambda):
        passes = [st for st in _statements_for(t, api, fn.role) if _grants(st, "iam:PassRole")]
        passed = {r for st in passes for r in _resources(st)}
        assert passed == expected, passed


def test_backend_and_sweep_register_only_safe_task_definitions(
    two_project_stacks: Stacks,
) -> None:
    # A launch registers the task job definitions its child jobs run on (coordinators register
    # none). IAM bounds what the backend may register: the task prefix, tagged with one of this
    # deploy's projects and no other tag, logging to a project's task log group, never
    # privileged, and only images from the tenant's own registry.
    stacks = two_project_stacks
    api = stacks["api"]
    t = Template.from_stack(api)
    pids = [p.project_id for p in stacks["projects"]]
    tasks = f"{_BATCH}:job-definition/biffix-acme_task-*"
    job_definitions = f"{_BATCH}:job-definition/*"
    by_fn = []
    for fn in (api.backend_lambda, api.sweep_lambda):
        statements = _statements_for(t, api, fn.role)
        assert _lambda_env(api, fn)["BIFFIX_TASK_JOB_DEFINITION_PREFIX"] == "biffix-acme_task-"
        (register,) = [
            st
            for st in statements
            if st["Effect"] == "Allow" and tasks in _resources(st) and "Condition" in st
        ]
        assert _actions(register) == {"batch:RegisterJobDefinition", "batch:TagResource"}
        assert _resources(register) == [tasks]
        assert register["Condition"] == {
            "StringEquals": {"aws:RequestTag/biffix:project": pids},
            "ForAllValues:StringEquals": {"aws:TagKeys": ["biffix:project"]},
            "StringEqualsIfExists": {
                "batch:AWSLogsGroup": [f"/biffix/project-{pid}/batch" for pid in pids]
            },
        }
        denies = [
            st
            for st in statements
            if st["Effect"] == "Deny" and _actions(st) == {"batch:RegisterJobDefinition"}
        ]
        assert all(_resources(st) == [job_definitions] for st in denies), denies
        conditions = [st["Condition"] for st in denies]
        assert {"Bool": {"batch:Privileged": "true"}} in conditions, conditions
        (image,) = [c for c in conditions if "StringNotLike" in c]
        (registry,) = image["StringNotLike"].values()
        assert _render(registry) == (
            "{Ref:AWS::AccountId}.dkr.ecr.{Ref:AWS::Region}.{Ref:AWS::URLSuffix}/*"
        ), registry
        assert set(image["StringNotLike"]) == {"batch:Image"}
        assert len(denies) == 2
        # Finding a definition registered before needs DescribeJobDefinitions.
        assert "batch:DescribeJobDefinitions" in _actions_for_role(t, api, fn.role)
        by_fn.append((register, denies))
    # Both from the one `ControlPlaneGrants.launch`.
    assert by_fn[0] == by_fn[1]


def test_the_runtime_build_project_runs_the_repo_build_script(default_stacks: Stacks) -> None:
    build = default_stacks["build"]
    t = Template.from_stack(build)
    (props,) = [
        p["Properties"]
        for p in t.find_resources("AWS::CodeBuild::Project").values()
        if p["Properties"]["Name"] == "biffix-acme-runtime-build"
    ]
    # Named with the tenant's build-project prefix (the ingest stack routes its events by its
    # exact name, test_ingest).
    assert props["Name"].startswith(CONFIG.build_project_prefix)
    assert props["Source"]["Type"] == "NO_SOURCE"
    assert props["Environment"]["PrivilegedMode"] is False
    spec = json.loads(props["Source"]["BuildSpec"])
    exported = set(spec["env"]["exported-variables"])
    # The backend records the stored layer images a derive appends (variants::ensure).
    assert {"VARIANT_IMAGE", "RUNTIME_IMAGE_AMD64", "RUNTIME_IMAGE_ARM64"} <= exported
    unpack = spec["phases"]["build"]["commands"][0]
    packed = unpack.split()[1]
    script = gzip.decompress(base64.b64decode(packed)).decode()
    assert script == bundled_script(RUNTIME_BUILD_SCRIPT)
    assert script.startswith(COMMON_SCRIPT.read_text())
    granted = _actions_for_role(t, build, build.runtime_project.role)
    assert {"ecr:PutImage", "ecr:GetAuthorizationToken", "s3:PutObject", "s3:GetObject*"} <= granted
    statements = _statements_for(t, build, build.runtime_project.role)
    assert not any(_grants(st, "s3:DeleteObject") for st in statements), granted
    assert "BIFFIX_ARCHIVE_URI" in json.dumps(props["Environment"]["EnvironmentVariables"])


def test_backend_can_terminate_a_coordinator_on_cancel(default_stacks: Stacks) -> None:
    # Cancel terminates the run's coordinator job; TerminateJob authorizes on the job ARN only. The
    # backend (cancel) and the sweep (a cancel racing a parked run's launch) both get this from the
    # one `ControlPlaneGrants.launch` helper, so the two statements must be identical, not just
    # independently in-pattern (a future change to only one Lambda's grant must fail this test).
    api = default_stacks["api"]
    t = Template.from_stack(api)
    assert "batch:TerminateJob" in _actions_for_role(t, api, api.backend_lambda.role)
    (backend_term,) = [
        st
        for st in _statements_for(t, api, api.backend_lambda.role)
        if _actions(st) == {"batch:TerminateJob"}
    ]
    (sweep_term,) = [
        st
        for st in _statements_for(t, api, api.sweep_lambda.role)
        if _actions(st) == {"batch:TerminateJob"}
    ]
    assert backend_term == sweep_term, (backend_term, sweep_term)
    assert _resources(backend_term) == [BATCH_JOBS], backend_term


def test_backend_can_describe_jobs_for_ingest_claim_checks(default_stacks: Stacks) -> None:
    # POST /events reads a claimed job's `biffix:run` tag with DescribeJobs before accepting a
    # task_submitted; without the grant every claim is refused with a 503.
    api = default_stacks["api"]
    t = Template.from_stack(api)
    assert "batch:DescribeJobs" in _actions_for_role(t, api, api.backend_lambda.role)


def test_coordinator_role_can_describe_job_queues(default_stacks: Stacks) -> None:
    # The aws-batch executor fork walks queue → compute environment for EC2-vs-Fargate detection;
    # grant_coordinator (compute) attaches these to the api-owned coordinator role. Scope the
    # assertion to THAT role — a stack-wide union would pass if the backend role held them instead.
    api = default_stacks["api"]
    role = api.coordinator_roles[CONFIG.projects[0].id]
    granted = _actions_for_role(Template.from_stack(api), api, role)
    assert "batch:DescribeJobQueues" in granted
    assert "batch:DescribeComputeEnvironments" in granted
    # TerminateJob (kill orphaned sibling jobs so a failed run stops burning compute).
    assert "batch:TerminateJob" in granted


def _import_of(stacks: Stacks, stack: str, construct: IConstruct) -> str:
    """The prefix a cross-stack reference to `construct` (in `stacks[stack]`) renders with."""
    lid = _logical_id(_single_stacks(stacks)[stack], construct)
    return f"{{Import:{stack}:ExportsOutputRef{lid}"


def test_api_accepts_cli_client_multi_audience(default_stacks: Stacks) -> None:
    stacks = default_stacks
    api = stacks["api"]
    t = Template.from_stack(api)
    web = _import_of(stacks, "identity", stacks["identity"].user_pool_client)
    cli = _import_of(stacks, "identity", stacks["identity"].cli_user_pool_client)
    backend_id = _logical_id(api, api.backend_lambda)
    env = t.find_resources("AWS::Lambda::Function")[backend_id]["Properties"]["Environment"]
    assert _render(env["Variables"]["BIFFIX_CLI_CLIENT_ID"]).startswith(cli)
    assert "localhost:8976/callback" in env["Variables"]["BIFFIX_CLI_REDIRECT_URIS"]
    # The authorizer accepts exactly the web and the CLI clients' tokens.
    (authorizer,) = t.find_resources("AWS::ApiGatewayV2::Authorizer").values()
    audiences = [_render(a) for a in authorizer["Properties"]["JwtConfiguration"]["Audience"]]
    assert len(audiences) == 2, audiences
    assert audiences[0].startswith(web) and audiences[1].startswith(cli), audiences


def test_backend_receives_the_same_group_role_mapping(default_stacks: Stacks) -> None:
    stacks = default_stacks
    t = Template.from_stack(stacks["api"])
    t.has_resource_properties(
        "AWS::Lambda::Function",
        Match.object_like(
            {
                "Environment": Match.object_like(
                    {
                        "Variables": Match.object_like(
                            {"BIFFIX_GROUP_ROLE_MAP": json.dumps(RBAC_GROUP_ROLES, sort_keys=True)}
                        )
                    }
                )
            }
        ),
    )


def test_events_route_bypasses_cognito_and_backend_gets_the_signing_key(
    default_stacks: Stacks,
) -> None:
    # POST /events cannot carry a Cognito token (the coordinator has no user identity), so it skips
    # the JWT authorizer; the backend gets the capability signing-key ARN to verify the token.
    stacks = default_stacks
    api = stacks["api"]
    t = Template.from_stack(api)
    backend_id = _logical_id(api, api.backend_lambda)
    env = t.find_resources("AWS::Lambda::Function")[backend_id]["Properties"]["Environment"]
    secret = _render(env["Variables"]["BIFFIX_CAPABILITY_SECRET_ARN"])
    assert secret.startswith(
        _import_of(stacks, "persistence", stacks["persistence"].capability_secret)
    ), secret
    routes = t.find_resources("AWS::ApiGatewayV2::Route")
    events_routes = [r for r in routes.values() if r["Properties"]["RouteKey"] == "POST /events"]
    assert len(events_routes) == 1, routes
    assert "AuthorizerId" not in events_routes[0]["Properties"]
    assert events_routes[0]["Properties"].get("AuthorizationType", "NONE") == "NONE"


def test_ingest_url_is_api_local_not_cloudfront(default_stacks: Stacks) -> None:
    # The api/edge cycle cut: BIFFIX_INGEST_URL is the HTTP API endpoint (api-local), never derived
    # from the edge/CloudFront domain (which would make api depend on edge and edge on api).
    stacks = default_stacks
    t = Template.from_stack(stacks["api"])
    funcs = t.find_resources("AWS::Lambda::Function")
    backend = next(
        f
        for f in funcs.values()
        if "BIFFIX_INGEST_URL" in f["Properties"]["Environment"]["Variables"]
    )
    ingest_url = json.dumps(backend["Properties"]["Environment"]["Variables"]["BIFFIX_INGEST_URL"])
    assert "cloudfront" not in ingest_url.lower()
    # Intra-stack, so it renders as a GetAtt to the HTTP API's endpoint, not an execute-api literal.
    assert "HttpApi" in ingest_url and "ApiEndpoint" in ingest_url


# ── single-job mode ──


def _single_job_queue_arns(stacks: Stacks) -> dict[str, str]:
    """Each single-job queue-class's queue ARN as the api stack imports it, rendered."""
    api, compute = stacks["api"], stacks["compute"]
    return {
        qc.name: _render(api.resolve(compute.queues[qc.name].job_queue_arn))
        for qc in SINGLE_JOB_CLASSES
    }


def test_the_single_job_template_runs_the_whole_run_as_the_run_role(
    single_job_stacks: Stacks,
) -> None:
    api = single_job_stacks["api"]
    (project,) = single_job_stacks["projects"]
    t = Template.from_stack(api)
    jobdefs = t.find_resources("AWS::Batch::JobDefinition")
    single = api.single_job_job_definitions[project.project_id]
    tasks_mode = api.coordinator_job_definitions[project.project_id]
    props = jobdefs[_logical_id(api, single)]["Properties"]
    coordinator = jobdefs[_logical_id(api, tasks_mode)]["Properties"]
    assert props["JobDefinitionName"] == f"biffix-acme-coordinator-single-{project.project_id}"
    # The tasks-mode template's retries and timeout (the backend sizes the token by them).
    for key in ("RetryStrategy", "Timeout", "Type", "PlatformCapabilities"):
        assert props.get(key) == coordinator.get(key), key
    container = props["ContainerProperties"]
    assert container["Image"] == coordinator["ContainerProperties"]["Image"]
    assert container["Command"] == ["biffix-coordinator"]
    # The project's run base role, not its coordinator role.
    assert _render(api.resolve(container["JobRoleArn"])) == _render(
        api.resolve(project.run_base_role.role_arn)
    )
    assert container.get("Privileged") in (None, False)
    assert container["LinuxParameters"] == {
        "Devices": [
            {
                "ContainerPath": "/dev/fuse",
                "HostPath": "/dev/fuse",
                "Permissions": ["READ", "WRITE"],
            }
        ],
        "InitProcessEnabled": True,
    }
    # The instance-store scratch read-write, and the host's own state directory (where the
    # boothook writes the instance-store marker) read-only, so no run can remove or fake it.
    volumes = {v["Name"]: v["Host"]["SourcePath"] for v in container["Volumes"]}
    assert sorted(volumes.values()) == sorted([SCRATCH_DIR, HOST_STATE_DIR])
    mounts = {volumes[m["SourceVolume"]]: m for m in container["MountPoints"]}
    assert mounts == {
        path: {"ContainerPath": path, "ReadOnly": path == HOST_STATE_DIR, "SourceVolume": name}
        for name, path in volumes.items()
    }
    assert mounts[HOST_STATE_DIR]["ReadOnly"] is True
    assert mounts[SCRATCH_DIR]["ReadOnly"] is False
    assert posixpath.dirname(INSTANCE_STORE_MARKER) == HOST_STATE_DIR
    # A placeholder size: the backend asks for the whole host at submit.
    assert {r["Type"]: r["Value"] for r in container["ResourceRequirements"]} == {
        "VCPU": "1",
        "MEMORY": "2048",
    }
    env = {e["Name"]: e["Value"] for e in container["Environment"]}
    assert env == {
        "BIFFIX_REGION": CONFIG.region,
        "BIFFIX_TASK_LOG_GROUP": project.task_log_group_name,
        "BIFFIX_SCRATCH_ROOT": SCRATCH_DIR,
        "BIFFIX_INSTANCE_STORE_MARKER": INSTANCE_STORE_MARKER,
    }


def test_the_scratch_root_and_marker_are_one_value_everywhere(single_job_stacks: Stacks) -> None:
    # host_boot's constants are the one source: the template names them to the job, the host's
    # boothook gets the same, and the coordinator's own fallbacks (for a job definition that does
    # not set them) agree.
    api = single_job_stacks["api"]
    (project,) = single_job_stacks["projects"]
    jobdefs = Template.from_stack(api).find_resources("AWS::Batch::JobDefinition")
    single = api.single_job_job_definitions[project.project_id]
    container = jobdefs[_logical_id(api, single)]["Properties"]["ContainerProperties"]
    env = {e["Name"]: e["Value"] for e in container["Environment"]}
    assert env["BIFFIX_SCRATCH_ROOT"] == SCRATCH_DIR
    assert env["BIFFIX_INSTANCE_STORE_MARKER"] == INSTANCE_STORE_MARKER
    header = single_job_boot_header()
    assert f"BIFFIX_SCRATCH_DIR={SCRATCH_DIR}" in header
    assert f"BIFFIX_INSTANCE_STORE_MARKER={INSTANCE_STORE_MARKER}" in header
    coordinator = (REPO_ROOT / "docker/coordinator/lib/single_job.sh").read_text()
    assert f"${{BIFFIX_SCRATCH_ROOT:-{SCRATCH_DIR}}}" in coordinator
    assert f"${{BIFFIX_INSTANCE_STORE_MARKER:-{INSTANCE_STORE_MARKER}}}" in coordinator


def test_no_single_job_template_without_a_single_job_queue_class(default_stacks: Stacks) -> None:
    api = default_stacks["api"]
    assert api.single_job_job_definitions == {}
    env = _lambda_env(api, api.backend_lambda)
    assert "BIFFIX_SINGLE_JOB_QUEUES" not in env
    t = Template.from_stack(api)
    for fn in (api.backend_lambda, api.sweep_lambda):
        actions = _actions_for_role(t, api, fn.role)
        assert "ec2:DescribeInstanceTypes" not in actions


def test_backend_and_sweep_get_the_single_job_queues_in_config_order(
    single_job_stacks: Stacks,
) -> None:
    api = single_job_stacks["api"]
    arns = _single_job_queue_arns(single_job_stacks)
    for fn in (api.backend_lambda, api.sweep_lambda):
        raw = _lambda_env(api, fn)["BIFFIX_SINGLE_JOB_QUEUES"]
        rendered = _render(raw)
        # Compact JSON, the way the backend parses it: an array in deploy-config order (the first
        # is the default), so no JSON object's key order decides it.
        assert ": " not in rendered and ", " not in rendered, rendered
        assert json.loads(rendered) == [
            {"name": qc.name, "queue": arns[qc.name], "instance_types": qc.instances}
            for qc in SINGLE_JOB_CLASSES
        ]


def test_the_backend_may_launch_on_single_job_queues(single_job_stacks: Stacks) -> None:
    api = single_job_stacks["api"]
    t = Template.from_stack(api)
    queues = set(_single_job_queue_arns(single_job_stacks).values())
    coordinator = _render(api.resolve(single_job_stacks["compute"].coordinator_queue.job_queue_arn))
    for fn in (api.backend_lambda, api.sweep_lambda):
        statements = _statements_for(t, api, fn.role)
        submits = [st for st in statements if _actions(st) == {"batch:SubmitJob"}]
        # One statement per (queues, variants) pair, each keyed on the execution tag: tasks-mode
        # variants on the coordinator queue without it, single-job variants on the single-job
        # queues with it.
        by_resources = {tuple(sorted(_resources(st))): st.get("Condition") for st in submits}
        assert by_resources == {
            tuple(sorted([coordinator, VARIANT_JOB_DEFINITIONS, BATCH_JOBS])): {
                "Null": {"aws:RequestTag/biffix:execution": "true"}
            },
            tuple(sorted([*queues, SINGLE_JOB_VARIANT_JOB_DEFINITIONS, BATCH_JOBS])): {
                "StringEquals": {"aws:RequestTag/biffix:execution": "single_job"}
            },
        }
        (tag,) = [
            st
            for st in statements
            if _actions(st) == {"batch:TagResource"} and BATCH_JOBS in _resources(st)
        ]
        # The same tag conditions as the coordinator queue's, plus the execution tag (one value).
        assert set(_resources(tag)) == {BATCH_JOBS, coordinator, *queues}
        assert tag["Condition"]["ForAllValues:StringEquals"] == {
            "aws:TagKeys": ["biffix:run", "biffix:project", "biffix:execution"]
        }
        assert tag["Condition"]["StringEqualsIfExists"] == {
            "aws:RequestTag/biffix:execution": "single_job"
        }


def _submit_allowed(statements: list[Json], resources: list[str], tags: dict[str, str]) -> bool:
    """Whether a SubmitJob naming `resources` (queue, job definition, the new job) with request
    tags `tags` is allowed: IAM checks each resource on its own, against every statement, so each
    needs an Allow naming it (wildcards matched) whose conditions (the Null and StringEquals
    request-tag ones these statements use) hold."""

    def holds(condition: Json | None) -> bool:
        for op, keys in (condition or {}).items():
            for key, want in keys.items():
                tag = tags.get(key.removeprefix("aws:RequestTag/"))
                if op == "Null":
                    if (tag is None) != (want == "true"):
                        return False
                elif op == "StringEquals":
                    if tag not in (want if isinstance(want, list) else [want]):
                        return False
                else:
                    raise AssertionError(f"unexpected condition {op}")
        return True

    def allowed(resource: str) -> bool:
        return any(
            st["Effect"] == "Allow"
            and _grants(st, "batch:SubmitJob")
            and any(fnmatch.fnmatchcase(resource, r) for r in _resources(st))
            and holds(st.get("Condition"))
            for st in statements
        )

    return all(allowed(r) for r in resources)


def test_no_launcher_may_submit_a_crossed_pair(single_job_stacks: Stacks) -> None:
    # A single-job variant runs only on a single-job queue (tagged), a tasks-mode one only on the
    # coordinator queue (untagged): no tagging makes a crossed pair pass every resource check.
    api = single_job_stacks["api"]
    t = Template.from_stack(api)
    (single_queue, *_) = _single_job_queue_arns(single_job_stacks).values()
    coordinator = _render(api.resolve(single_job_stacks["compute"].coordinator_queue.job_queue_arn))
    rt = VARIANT_JOB_DEFINITIONS.removesuffix("*") + "abc:1"
    sj = SINGLE_JOB_VARIANT_JOB_DEFINITIONS.removesuffix("*") + "abc:1"
    job = BATCH_JOBS.removesuffix("*") + "0123"
    base = {"biffix:run": "r", "biffix:project": "p"}
    single = {**base, "biffix:execution": "single_job"}
    for fn in (api.backend_lambda, api.sweep_lambda):
        statements = _statements_for(t, api, fn.role)
        # The two real launches.
        assert _submit_allowed(statements, [coordinator, rt, job], base)
        assert _submit_allowed(statements, [single_queue, sj, job], single)
        # Every crossed pair, under either tagging, or the wrong tags on a right pair.
        for tags in (base, single, {**base, "biffix:execution": "other"}):
            assert not _submit_allowed(statements, [coordinator, sj, job], tags), tags
            assert not _submit_allowed(statements, [single_queue, rt, job], tags), tags
        assert not _submit_allowed(statements, [coordinator, rt, job], single)
        assert not _submit_allowed(statements, [single_queue, sj, job], base)
        (describe,) = [st for st in statements if "ec2:DescribeInstanceTypes" in _actions(st)]
        assert _actions(describe) == {"ec2:DescribeInstanceTypes"}
        assert _resources(describe) == ["*"]


def test_backend_and_sweep_register_single_job_variants_under_their_own_prefix(
    single_job_stacks: Stacks,
) -> None:
    # A single-job variant is named `biffix-<tenant>-coordinator-sj-…`, apart from the tasks-mode
    # `rt-` variants; the backend reads the prefix from the environment.
    api = single_job_stacks["api"]
    t = Template.from_stack(api)
    for fn in (api.backend_lambda, api.sweep_lambda):
        env = _lambda_env(api, fn)
        assert env["BIFFIX_VARIANT_JOB_DEFINITION_PREFIX"] == "biffix-acme-coordinator-rt-"
        assert (
            env["BIFFIX_SINGLE_JOB_VARIANT_JOB_DEFINITION_PREFIX"] == "biffix-acme-coordinator-sj-"
        )
        (register,) = [
            st
            for st in _statements_for(t, api, fn.role)
            if _actions(st) == {"batch:RegisterJobDefinition", "batch:TagResource"}
            and "Condition" not in st
        ]
        assert _resources(register) == [
            VARIANT_JOB_DEFINITIONS,
            SINGLE_JOB_VARIANT_JOB_DEFINITIONS,
        ]


def test_a_worker_queue_class_named_with_digits_only_is_refused(tmp_path: Path) -> None:
    # The queue map's first entry is the coordinator's default worker queue-class, and CDK's JSON
    # would put a digits-only name first. The loader refuses it (test_config_keys); a config built
    # in code is refused at synth.
    digits = dataclasses.replace(CONFIG.queue_classes[0], name="7", coordinator=False)
    bad = dataclasses.replace(CONFIG, queue_classes=[*CONFIG.queue_classes, digits])
    with pytest.raises(ValueError, match="digits only"):
        _build(tmp_path, bad)


def test_single_job_queues_are_neither_offered_nor_in_the_queue_map(
    single_job_stacks: Stacks,
) -> None:
    api = single_job_stacks["api"]
    workers = [qc.name for qc in SINGLE_JOB_CONFIG.worker_queue_classes]
    env = _lambda_env(api, api.backend_lambda)
    assert env["BIFFIX_QUEUE_CLASSES"].split(",") == workers
    (project,) = single_job_stacks["projects"]
    jobdefs = Template.from_stack(api).find_resources("AWS::Batch::JobDefinition")
    tasks_mode = api.coordinator_job_definitions[project.project_id]
    container = jobdefs[_logical_id(api, tasks_mode)]["Properties"]["ContainerProperties"]
    (queue_map,) = [
        e["Value"] for e in container["Environment"] if e["Name"] == "BIFFIX_QUEUE_CLASS_MAP"
    ]
    assert list(json.loads(_render(queue_map))) == workers


def test_backend_and_sweep_pass_the_single_job_template_s_roles(single_job_stacks: Stacks) -> None:
    # A single-job variant is a clone of the single-job template: it passes the template's
    # execution role and the run base role (already passed for task definitions).
    api = single_job_stacks["api"]
    t = Template.from_stack(api)
    (project,) = single_job_stacks["projects"]
    single = api.single_job_job_definitions[project.project_id]
    execution = _render(api.resolve(single.container.execution_role.role_arn))
    expected = _template_roles(api) | _run_roles(api, [project]) | {execution}
    assert len(expected) == 4
    for fn in (api.backend_lambda, api.sweep_lambda):
        passes = [st for st in _statements_for(t, api, fn.role) if _grants(st, "iam:PassRole")]
        assert {r for st in passes for r in _resources(st)} == expected


def test_the_run_role_pulls_images_only_with_single_job(
    default_stacks: Stacks, single_job_stacks: Stacks
) -> None:
    def pulls(stacks: Stacks) -> list[Json]:
        """The ECR statements of every api-stack policy attached to the project's run base role
        (which the api stack imports from the project's stack)."""
        api = stacks["api"]
        (project,) = stacks["projects"]
        role = _render(api.resolve(project.run_base_role.role_name))
        return [
            st
            for policy in _policies(Template.from_stack(api))
            if role in [_render(r) for r in policy["Properties"].get("Roles", [])]
            for st in policy["Properties"]["PolicyDocument"]["Statement"]
            if any(a.startswith("ecr:") for a in _actions(st))
        ]

    assert pulls(default_stacks) == []
    statements = pulls(single_job_stacks)
    (token,) = [st for st in statements if _actions(st) == {"ecr:GetAuthorizationToken"}]
    assert _resources(token) == ["*"]
    (pull,) = [st for st in statements if st is not token]
    assert _actions(pull) == {
        "ecr:BatchGetImage",
        "ecr:GetDownloadUrlForLayer",
        "ecr:BatchCheckLayerAvailability",
    }
    rendered = _resources(pull)
    assert len(rendered) == 2
    assert any("WorkflowRepo" in r for r in rendered), rendered
    assert any("BuildsRepo" in r for r in rendered), rendered
    # Never the coordinator repository.
    assert not any("CoordinatorRepo" in r for r in rendered), rendered
