"""The api stack, the hub: the coordinator role and job definition, the backend Lambda and
the HTTP API."""

import base64
import gzip
import json
import re
from pathlib import Path
from typing import Any

from aws_cdk.assertions import Match, Template
from constructs import IConstruct

from biffix_infra.app_graph import Stacks
from biffix_infra.config import DeployConfig
from biffix_infra.constructs.script_project import COMMON_SCRIPT, bundled_script
from biffix_infra.rbac import RBAC_GROUP_ROLES
from biffix_infra.stacks.api_stack import ApiStack
from biffix_infra.stacks.build_stack import RUNTIME_BUILD_SCRIPT
from tests.deploy_config_file import write_config
from tests.helpers import REPO_ROOT
from tests.synth.synth_helpers import (
    BATCH_JOBS,
    CONFIG,
    VARIANT_JOB_DEFINITIONS,
    _actions,
    _actions_for_role,
    _build,
    _context_parts,
    _grants,
    _lambda_env,
    _logical_id,
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


def test_the_backend_environment_stays_well_inside_lambda_s_4_kb(tmp_path: Path) -> None:
    # Lambda refuses a function whose environment (names + values) exceeds 4 KB, and only at
    # deploy: a real deploy failed at 4108 bytes. Synth a deploy that size (13
    # worker queue-classes, a custom domain, Google federation, two data sources) and bound the
    # backend's and the sweep's environments: literal values as they are, each
    # CloudFormation-resolved one (an ARN, an image URI) at 110 bytes (that deploy's
    # resolved values average 81, the longest 145), under a budget ~200 bytes short of the limit.
    # The projects' contexts live in SSM, not the environment, so adding a third and a fourth
    # project (each with a data source) does not grow it by a byte; each context fits a Standard
    # parameter (4 KB).
    arches = [f"arch-{a}" for a in ("c6a", "c7a", "c7i", "c7g", "c8g", "m7i", "m8a", "m8g")]
    arenas = ["arch-m8a-arena", "arch-m8g-arena", "arch-c8g64"]
    classes = [{"name": "coordinator", "capacity": "ON_DEMAND", "coordinator": True}] + [
        {"name": n, "capacity": "SPOT" if n != "cpu-ondemand" else "ON_DEMAND"}
        for n in ["cpu-spot", "cpu-ondemand", *arches, *arenas]
    ]
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
            measured[label] = sum(
                len(k) + (len(v) if isinstance(v, str) else 110) for k, v in env.items()
            )
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
    assert set(_resources(backend_submit)) == {queue, VARIANT_JOB_DEFINITIONS}


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
        # The coordinator role's own grant (the executor registers child job definitions) differs.
        if _actions(statement) == {"batch:RegisterJobDefinition", "batch:TagResource"}
    ]
    assert len(statements) == 2  # backend + sweep
    for statement in statements:
        assert _resources(statement) == [VARIANT_JOB_DEFINITIONS], statement
    # A clone passes only a project template's roles (its job and execution roles).
    expected = _template_roles(api)
    assert len(expected) == 2  # the one project's
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


def test_backend_and_sweep_pass_every_project_s_template_roles_and_no_run_role(
    two_project_stacks: Stacks,
) -> None:
    # A launch clones the run's project's template, so both Lambdas pass every project's
    # coordinator and execution roles; never a run role (Batch delivers it to the child jobs the
    # coordinator registers, which passes it itself).
    stacks = two_project_stacks
    api = stacks["api"]
    t = Template.from_stack(api)
    expected = _template_roles(api)
    assert len(expected) == 2 * len(stacks["projects"]) == 6
    for fn in (api.backend_lambda, api.sweep_lambda):
        passes = [st for st in _statements_for(t, api, fn.role) if _grants(st, "iam:PassRole")]
        passed = {r for st in passes for r in _resources(st)}
        assert passed == expected, passed
        assert not any("RunBaseRole" in r for r in passed), passed


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
