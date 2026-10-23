"""The sweep Lambda, which launches runs parked until their images are ready."""

from aws_cdk.assertions import Template

from biffix_infra.app_graph import Stacks
from tests.synth.synth_helpers import (
    CONFIG,
    VARIANT_JOB_DEFINITIONS,
    _actions,
    _actions_for_role,
    _bucket_objects,
    _grants,
    _lambda_env,
    _logical_id,
    _resources,
    _statements_for,
)


def test_backend_and_sweep_resolve_tags_with_pull_rights_in_any_account(
    default_stacks: Stacks,
) -> None:
    # Pinning an ECR tag (registration, submit-time images, a parked run's images) uses
    # BatchGetImage, a pull permission, so another account's repository whose policy grants this
    # account pull only still resolves. DescribeImages is not granted: nothing calls it.
    api = default_stacks["api"]
    t = Template.from_stack(api)
    for statements in (
        _statements_for(t, api, api.backend_lambda.role),
        _statements_for(t, api, api.sweep_lambda.role),
    ):
        (pull,) = [st for st in statements if _actions(st) == {"ecr:BatchGetImage"}]
        assert _resources(pull) == ["arn:aws:ecr:*:*:repository/*"], pull
        # Wildcards count: `ecr:Describe*` or `ecr:*` would grant it too.
        assert not any(_grants(st, "ecr:DescribeImages") for st in statements)


def test_sweep_writes_only_run_provenance_and_inputs(default_stacks: Stacks) -> None:
    """The backend sweep writes each ended run's RO-Crate into its `provenance/` folder and a parked
    run's `inputs/images.json` at launch: those are its only S3 writes, and it gets no OIDC."""
    api = default_stacks["api"]
    t = Template.from_stack(api)
    statements = _statements_for(t, api, api.sweep_lambda.role)
    puts = {r for st in statements if _grants(st, "s3:PutObject") for r in _resources(st)}
    assert _bucket_objects(puts, default_stacks["projects"][0]) == {
        "biffix/runs/*/provenance/*",
        "biffix/runs/*/inputs/*",
    }, puts
    assert not any(_grants(st, "s3:DeleteObject") for st in statements)
    fn_id = _logical_id(api, api.sweep_lambda)
    env = t.find_resources("AWS::Lambda::Function")[fn_id]["Properties"]["Environment"]["Variables"]
    assert "BIFFIX_OIDC_ISSUER" not in env


def test_sweep_can_launch_parked_runs(default_stacks: Stacks) -> None:
    """A run parked in `preparing_images` is launched by the sweep, so it holds the API's launch
    grants: SubmitJob (coordinator queue + variant job definitions), TerminateJob (a cancel racing
    the launch), the capability secret (the run's ingest token), and the run's role and bucket.
    Both come from the one `ControlPlaneGrants.launch` helper, environment included."""
    api = default_stacks["api"]
    t = Template.from_stack(api)
    granted = _actions_for_role(t, api, api.sweep_lambda.role)
    assert {"batch:SubmitJob", "batch:TerminateJob"} <= granted, granted
    # The idle check lists the coordinator queue's jobs.
    assert "batch:ListJobs" in granted, granted
    # Nothing assumes the run role: Batch delivers it to the run's tasks (no `sts:*` or `*`).
    statements = _statements_for(t, api, api.sweep_lambda.role)
    assert not any(_grants(st, "sts:AssumeRole") for st in statements), granted
    assert "secretsmanager:GetSecretValue" in granted, granted
    statements = _statements_for(t, api, api.sweep_lambda.role)
    (submit,) = [st for st in statements if _actions(st) == {"batch:SubmitJob"}]
    assert VARIANT_JOB_DEFINITIONS in _resources(submit), submit
    assert not any(r.endswith("job-definition/*") for r in _resources(submit)), submit
    env = _lambda_env(api, api.sweep_lambda)
    backend_env = _lambda_env(api, api.backend_lambda)
    # The launch environment is the backend's, variable for variable (the run's project context
    # comes from the same SSM path, with the same default).
    for name in (
        "BIFFIX_CAPABILITY_SECRET_ARN",
        "BIFFIX_PROJECTS_PATH",
        "BIFFIX_DEFAULT_PROJECT",
        "BIFFIX_WORKER_IMAGE_REPO",
        "BIFFIX_COORDINATOR_QUEUE",
        "BIFFIX_INGEST_URL",
    ):
        assert name in env and env[name] == backend_env[name], name
    assert env["BIFFIX_PROJECTS_PATH"] == "/biffix/acme/projects/"
    assert env["BIFFIX_DEFAULT_PROJECT"] == CONFIG.projects[0].id


def test_sweep_runs_on_a_schedule_and_on_build_events(default_stacks: Stacks) -> None:
    """The sweep runs every 5 minutes (on the minutes that are multiples of 5, so the idle gate's
    hourly tick is minute 0), and a state change of one of this deploy's CodeBuild builds
    invokes it at once (so a finished import/derive launches a parked run promptly)."""
    api = default_stacks["api"]
    t = Template.from_stack(api)
    fn_id = _logical_id(api, api.sweep_lambda)
    rules = [
        r["Properties"]
        for r in t.find_resources("AWS::Events::Rule").values()
        if any(tgt["Arn"] == {"Fn::GetAtt": [fn_id, "Arn"]} for tgt in r["Properties"]["Targets"])
    ]
    assert [r["ScheduleExpression"] for r in rules if "ScheduleExpression" in r] == [
        "cron(0/5 * * * ? *)"
    ]
    assert [r["EventPattern"] for r in rules if "EventPattern" in r] == [
        {
            "source": ["aws.codebuild"],
            "detail-type": ["CodeBuild Build State Change"],
            "detail": {"project-name": CONFIG.build_project_names},
        }
    ]
    # EventBridge may invoke the sweep (one permission per rule).
    perms = [
        p["Properties"]
        for p in t.find_resources("AWS::Lambda::Permission").values()
        if p["Properties"]["FunctionName"] == {"Fn::GetAtt": [fn_id, "Arn"]}
        and p["Properties"]["Principal"] == "events.amazonaws.com"
    ]
    assert len(perms) == 2, perms
