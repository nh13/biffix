"""Task job-definition retention: deregister only this tenant's task definitions whose image is
gone from its ECR."""

import subprocess

import pytest

from biffix_infra import task_job_definitions as retention
from biffix_infra.task_job_definitions import (
    ImageRef,
    Presence,
    TaskDefinition,
    parse_image,
    plan_retention,
)

PREFIX = "biffix-acme_task-"
REGISTRY = "111122223333.dkr.ecr.us-east-1.amazonaws.com"
GONE = f"{REGISTRY}/workflows/align@sha256:gone"
KEPT = f"{REGISTRY}/workflows/align@sha256:kept"


def _def(name: str, image: str | None) -> TaskDefinition:
    arn = f"arn:aws:batch:us-east-1:111122223333:job-definition/{name}:1"
    return TaskDefinition(arn=arn, name=name, image=image)


def test_an_image_is_parsed_only_from_the_tenant_s_registry() -> None:
    assert parse_image(GONE, REGISTRY) == ImageRef("workflows/align", digest="sha256:gone")
    assert parse_image(f"{REGISTRY}/coordinator:abc", REGISTRY) == ImageRef(
        "coordinator", tag="abc"
    )
    # Another account's or region's registry, a public image, or no digest or tag.
    assert parse_image("222233334444.dkr.ecr.us-east-1.amazonaws.com/w@sha256:x", REGISTRY) is None
    assert parse_image("111122223333.dkr.ecr.us-west-2.amazonaws.com/w@sha256:x", REGISTRY) is None
    assert parse_image("ubuntu:24.04", REGISTRY) is None
    assert parse_image(f"{REGISTRY}/workflows", REGISTRY) is None
    assert parse_image(f"{REGISTRY}/w@", REGISTRY) is None
    # A tag beside the digest is not part of the repository.
    assert parse_image(f"{REGISTRY}/workflows/align:1@sha256:d", REGISTRY) == ImageRef(
        "workflows/align", digest="sha256:d"
    )


def test_only_this_tenant_s_task_definitions_whose_image_is_gone_are_deregistered() -> None:
    gone_sm = _def(f"{PREFIX}sm-a", GONE)
    gone_nf = _def(f"{PREFIX}nf-b", GONE)
    definitions = [
        gone_sm,
        gone_nf,
        _def(f"{PREFIX}sm-c", KEPT),
        # Another image ECR could not answer for, one outside the registry, and none at all.
        _def(f"{PREFIX}sm-d", f"{REGISTRY}/workflows/flaky@sha256:x"),
        _def(f"{PREFIX}sm-e", "quay.io/biocontainers/samtools:1"),
        _def(f"{PREFIX}sm-f", None),
        # Not under the prefix: a sibling tenant `acme-task`'s, the coordinator's, the engines'.
        _def("biffix-acme-task_task-sm-a", GONE),
        _def("biffix-acme-coordinator-demo", GONE),
        _def("nf-align", GONE),
    ]
    looked_up: list[ImageRef] = []

    def lookup(ref: ImageRef) -> Presence:
        looked_up.append(ref)
        return {"sha256:gone": Presence.GONE, "sha256:kept": Presence.PRESENT}.get(
            ref.digest or "", Presence.UNKNOWN
        )

    plan = plan_retention(definitions, prefix=PREFIX, registry=REGISTRY, lookup=lookup)
    assert plan.deregister == [gone_sm, gone_nf]
    assert (plan.present, plan.foreign, plan.unknown) == (1, 2, 1)
    # Each image is looked up once, and only the tenant's own.
    assert sorted(r.digest or "" for r in looked_up) == ["sha256:gone", "sha256:kept", "sha256:x"]


def _aws(
    monkeypatch: pytest.MonkeyPatch,
    details: list[dict[str, object]],
    ecr_errors: dict[str, str],
    refuse: str | None = None,
) -> list[tuple[str, ...]]:
    """Answer `task_job_definitions._aws`: `details` (the CLI's answer, every page followed), and
    describe-images failing with `ecr_errors[image id]` when there is one; record every call. A
    deregister of an ARN containing `refuse` fails."""
    calls: list[tuple[str, ...]] = []

    def aws(*args: str) -> dict[str, object]:
        calls.append(args)
        if args[:2] == ("batch", "describe-job-definitions"):
            assert args[2:] == ("--status", "ACTIVE")
            return {"jobDefinitions": details}
        if args[:2] == ("ecr", "describe-images"):
            error = ecr_errors.get(args[-1])
            if error:
                raise subprocess.CalledProcessError(254, ["aws", *args], stderr=error)
            return {"imageDetails": [{}]}
        if args[:2] == ("batch", "deregister-job-definition"):
            if refuse and refuse in args[-1]:
                raise subprocess.CalledProcessError(254, ["aws", *args], stderr="ClientException")
            return {}
        raise AssertionError(args)

    monkeypatch.setattr(retention, "_aws", aws)
    return calls


def _detail(name: str, image: str) -> dict[str, object]:
    return {
        "jobDefinitionArn": f"arn:aws:batch:us-east-1:111122223333:job-definition/{name}:1",
        "jobDefinitionName": name,
        "status": "ACTIVE",
        "containerProperties": {"image": image},
    }


def _deregistered(calls: list[tuple[str, ...]]) -> list[str]:
    return [c[-1].split("/")[-1] for c in calls if c[1] == "deregister-job-definition"]


def _args(*extra: str) -> list[str]:
    return ["--prefix", PREFIX, "--registry", REGISTRY, *extra]


def test_main_deregisters_a_definition_whose_image_or_repository_is_gone(
    monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    details = [
        _detail(f"{PREFIX}sm-a", GONE),
        _detail(f"{PREFIX}sm-b", f"{REGISTRY}/deleted@sha256:y"),
        _detail(f"{PREFIX}sm-c", KEPT),
        _detail(f"{PREFIX}sm-d", f"{REGISTRY}/workflows/align@sha256:denied"),
        _detail("nf-align", GONE),
    ]
    errors = {
        "imageDigest=sha256:gone": "An error occurred (ImageNotFoundException) ...",
        "imageDigest=sha256:y": "An error occurred (RepositoryNotFoundException) ...",
        "imageDigest=sha256:denied": "An error occurred (AccessDeniedException) ...",
    }
    calls = _aws(monkeypatch, details, errors)
    assert retention.main(_args()) == 0
    assert _deregistered(calls) == [f"{PREFIX}sm-a:1", f"{PREFIX}sm-b:1"]
    looked_up = [c for c in calls if c[:2] == ("ecr", "describe-images")]
    assert ("--repository-name", "workflows/align") == looked_up[0][2:4]
    out = capsys.readouterr().out
    assert "2 to deregister (image gone); kept: 1 with their image" in out
    assert "1 whose image could not be looked up" in out


def test_main_dry_run_deregisters_nothing(monkeypatch: pytest.MonkeyPatch) -> None:
    errors = {"imageDigest=sha256:gone": "ImageNotFoundException"}
    calls = _aws(monkeypatch, [_detail(f"{PREFIX}sm-a", GONE)], errors)
    assert retention.main(_args("--dry-run")) == 0
    assert _deregistered(calls) == []


def test_main_goes_on_past_a_refusal_and_fails_at_the_end(
    monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    details = [_detail(f"{PREFIX}sm-a", GONE), _detail(f"{PREFIX}nf-b", GONE)]
    errors = {"imageDigest=sha256:gone": "ImageNotFoundException"}
    calls = _aws(monkeypatch, details, errors, refuse="sm-a")
    assert retention.main(_args()) == 1
    assert _deregistered(calls) == [f"{PREFIX}sm-a:1", f"{PREFIX}nf-b:1"]
    assert "Batch did not deregister" in capsys.readouterr().err


def test_main_shows_the_failed_call_and_aws_s_error(
    monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    def aws(*args: str) -> dict[str, object]:
        raise subprocess.CalledProcessError(254, ["aws", *args], stderr="AccessDenied")

    monkeypatch.setattr(retention, "_aws", aws)
    assert retention.main(_args()) == 1
    assert "aws batch describe-job-definitions: AccessDenied" in capsys.readouterr().err
