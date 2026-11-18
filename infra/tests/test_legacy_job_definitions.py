"""The engine job-definition cleanup: deregister only what this tenant's coordinators registered."""

import json
import subprocess

import pytest

from biffix_infra import legacy_job_definitions as cleanup
from biffix_infra.constructs.batch_access import PROJECT_TAG, RUN_TAG
from biffix_infra.legacy_job_definitions import JobDefinition, plan_cleanup

ROLE = "arn:aws:iam::111122223333:role/biffix-acme-project-demo-RunBaseRole-A"
OTHER_TENANT = "arn:aws:iam::111122223333:role/biffix-beta-project-demo-RunBaseRole-B"
PATH = "/biffix/acme/projects/"


def _def(name: str, tags: dict[str, str], role: str | None = ROLE, rev: int = 1) -> JobDefinition:
    arn = f"arn:aws:batch:us-east-1:111122223333:job-definition/{name}:{rev}"
    return JobDefinition(arn=arn, name=name, tags=tags, job_role=role)


def test_the_tags_are_the_ones_coordinators_registered_with() -> None:
    assert set(cleanup.BIFFIX_TAGS) == {PROJECT_TAG, RUN_TAG}


def test_only_this_tenant_s_tagged_engine_definitions_are_deregistered() -> None:
    nf = _def("nf-quay-io-biocontainers-samtools", {PROJECT_TAG: "demo", "nextflow.io/x": "y"})
    sm = _def("snakejob-def-align-0123", {RUN_TAG: "run-1", PROJECT_TAG: "demo"})
    sm_run_only = _def("snakejob-def-sort-4567", {RUN_TAG: "run-2"})
    definitions = [
        nf,
        sm,
        sm_run_only,
        # Another tenant's (its run role), and a definition not biffix's at all.
        _def("nf-other", {PROJECT_TAG: "demo"}, role=OTHER_TENANT),
        _def("nf-mine-untagged", {"nextflow.io/x": "y"}),
        _def("snakejob-def-untagged", {}, role=None),
        # Neither engine's name: the backend's task and coordinator definitions, or anyone's.
        _def("biffix-acme_task-sm-0123", {PROJECT_TAG: "demo"}),
        _def("biffix-acme-coordinator-demo", {PROJECT_TAG: "demo"}),
        _def("my-batch-job", {PROJECT_TAG: "demo"}),
    ]
    plan = plan_cleanup(definitions, {ROLE})
    assert plan.deregister == [nf, sm, sm_run_only]
    assert (plan.other_roles, plan.untagged) == (1, 2)


def _aws(
    monkeypatch: pytest.MonkeyPatch,
    definitions: list[dict[str, object]],
    roles: list[str],
    refuse: str | None = None,
) -> list[tuple[str, ...]]:
    """Answer `legacy_job_definitions._aws`: the project contexts naming `roles`, and
    `definitions` (the aws CLI's answer, every page already followed); record every call. A
    deregister of an ARN containing `refuse` fails."""
    calls: list[tuple[str, ...]] = []

    def aws(*args: str) -> dict[str, object]:
        calls.append(args)
        if args[:2] == ("ssm", "get-parameters-by-path"):
            contexts = [{"id": f"p{i}", "run_role_arn": r} for i, r in enumerate(roles)]
            return {
                "Parameters": [
                    {"Name": f"{PATH}{c['id']}", "Value": json.dumps(c)} for c in contexts
                ]
            }
        if args[:2] == ("batch", "describe-job-definitions"):
            # The CLI's own pagination, not a hand-rolled one.
            assert args[2:] == ("--status", "ACTIVE")
            return {"jobDefinitions": definitions}
        if args[:2] == ("batch", "deregister-job-definition"):
            if refuse and refuse in args[-1]:
                raise subprocess.CalledProcessError(254, ["aws", *args], stderr="ClientException")
            return {}
        raise AssertionError(args)

    monkeypatch.setattr(cleanup, "_aws", aws)
    return calls


def _detail(name: str, tags: dict[str, str], role: str = ROLE) -> dict[str, object]:
    return {
        "jobDefinitionArn": f"arn:aws:batch:us-east-1:111122223333:job-definition/{name}:1",
        "jobDefinitionName": name,
        "status": "ACTIVE",
        "tags": tags,
        "containerProperties": {"jobRoleArn": role},
    }


def _deregistered(calls: list[tuple[str, ...]]) -> list[str]:
    return [c[-1].split("/")[-1] for c in calls if c[1] == "deregister-job-definition"]


def test_main_deregisters_what_the_plan_names(
    monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    definitions = [
        _detail("nf-a", {PROJECT_TAG: "demo"}),
        _detail("unrelated", {}),
        _detail("snakejob-def-b", {RUN_TAG: "r"}),
        _detail("nf-c", {}),
        _detail("nf-d", {PROJECT_TAG: "demo"}, role=OTHER_TENANT),
    ]
    calls = _aws(monkeypatch, definitions, [ROLE])
    assert cleanup.main(["--projects-path", PATH]) == 0
    assert _deregistered(calls) == ["nf-a:1", "snakejob-def-b:1"]
    # The path as the backend reads it (no trailing slash), and one listing.
    assert ("--path", PATH.rstrip("/")) == calls[0][2:4]
    describes = [c for c in calls if c[1] == "describe-job-definitions"]
    assert len(describes) == 1
    assert "2 to deregister" in capsys.readouterr().out


def test_main_dry_run_deregisters_nothing(
    monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    calls = _aws(monkeypatch, [_detail("nf-a", {PROJECT_TAG: "demo"})], [ROLE])
    assert cleanup.main(["--projects-path", PATH, "--dry-run"]) == 0
    assert _deregistered(calls) == []
    assert "deregister arn:aws:batch:" in capsys.readouterr().out


def test_main_deregisters_nothing_without_this_tenant_s_run_roles(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    calls = _aws(monkeypatch, [_detail("nf-a", {PROJECT_TAG: "demo"})], [])
    assert cleanup.main(["--projects-path", PATH]) == 0
    assert [c for c in calls if c[0] == "batch"] == []


def test_main_goes_on_past_a_refusal_and_fails_at_the_end(
    monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    definitions = [_detail("nf-a", {PROJECT_TAG: "demo"}), _detail("nf-b", {PROJECT_TAG: "demo"})]
    calls = _aws(monkeypatch, definitions, [ROLE], refuse="nf-a")
    assert cleanup.main(["--projects-path", PATH]) == 1
    assert _deregistered(calls) == ["nf-a:1", "nf-b:1"]
    assert "Batch did not deregister" in capsys.readouterr().err


def test_main_shows_the_failed_call_and_aws_s_error(
    monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    def aws(*args: str) -> dict[str, object]:
        raise subprocess.CalledProcessError(254, ["aws", *args], stderr="AccessDenied")

    monkeypatch.setattr(cleanup, "_aws", aws)
    assert cleanup.main(["--projects-path", PATH]) == 1
    assert "aws ssm get-parameters-by-path: AccessDenied" in capsys.readouterr().err
