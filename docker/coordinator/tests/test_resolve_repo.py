"""`docker/coordinator/resolve-repo.sh`, the build-coordinator-image workflow's repository lookup,
run against a stand-in `aws` that prints `$FAKE_AWS_OUT`, exits `$FAKE_AWS_RC` and records its
arguments. The stand-in is written once per session (macOS can stall the first exec of every newly
written executable)."""

from __future__ import annotations

import os
import subprocess
from pathlib import Path

import pytest

SCRIPT = Path(__file__).resolve().parents[1] / "resolve-repo.sh"

_AWS = r"""#!/usr/bin/env bash
printf '%s\n' "$@" > "$FAKE_AWS_ARGV"
printf '%s\n' "${FAKE_AWS_OUT:-}"
exit "${FAKE_AWS_RC:-0}"
"""


@pytest.fixture(scope="session")
def aws_bin(tmp_path_factory: pytest.TempPathFactory) -> Path:
    bin_dir = tmp_path_factory.mktemp("fake-aws")
    exe = bin_dir / "aws"
    exe.write_text(_AWS)
    exe.chmod(0o755)
    return bin_dir


def _run(aws_bin: Path, tmp_path: Path, **env: str) -> tuple[subprocess.CompletedProcess[str], str]:
    argv = tmp_path / "argv"
    full = {
        "PATH": f"{aws_bin}{os.pathsep}{os.environ['PATH']}",
        "FAKE_AWS_ARGV": str(argv),
        "AWS_REGION": "us-west-2",
        **env,
    }
    proc = subprocess.run([str(SCRIPT)], env=full, capture_output=True, text=True, check=False)
    return proc, argv.read_text() if argv.exists() else ""


def test_exact_name_is_verified_and_printed(aws_bin: Path, tmp_path: Path) -> None:
    proc, argv = _run(aws_bin, tmp_path, COORDINATOR_REPO="my-repo", FAKE_AWS_OUT="my-repo")
    assert proc.returncode == 0, proc.stderr
    assert proc.stdout == "my-repo"
    assert "--repository-names\nmy-repo\n" in argv
    assert "--region\nus-west-2\n" in argv


def test_exact_name_wins_over_the_tenant(aws_bin: Path, tmp_path: Path) -> None:
    proc, argv = _run(
        aws_bin, tmp_path, COORDINATOR_REPO="my-repo", BIFFIX_TENANT="acme", FAKE_AWS_OUT="my-repo"
    )
    assert proc.returncode == 0, proc.stderr
    assert proc.stdout == "my-repo"
    assert "--repository-names" in argv


def test_missing_exact_name_fails(aws_bin: Path, tmp_path: Path) -> None:
    proc, _ = _run(aws_bin, tmp_path, COORDINATOR_REPO="gone", FAKE_AWS_RC="254")
    assert proc.returncode != 0
    assert "no ECR repository 'gone'" in proc.stderr


def test_tenant_resolves_the_one_anchored_match(aws_bin: Path, tmp_path: Path) -> None:
    name = "biffix-acme-data-coordinatorrepob4c9d7ce-abc123"
    proc, argv = _run(aws_bin, tmp_path, BIFFIX_TENANT="Acme", FAKE_AWS_OUT=name)
    assert proc.returncode == 0, proc.stderr
    assert proc.stdout == name
    # Anchored on the whole data-stack prefix (lowercased), never a bare substring.
    assert "starts_with(repositoryName, 'biffix-acme-data-coordinatorrepo')" in argv
    assert "contains(" not in argv


@pytest.mark.parametrize(
    ("out", "found"),
    [
        ("None", "0"),
        ("", "0"),
        ("biffix-acme-data-coordinatorrepoa-1\tbiffix-acme-data-coordinatorrepob-2", "2"),
    ],
)
def test_tenant_requires_exactly_one_match(
    aws_bin: Path, tmp_path: Path, out: str, found: str
) -> None:
    proc, _ = _run(aws_bin, tmp_path, BIFFIX_TENANT="acme", FAKE_AWS_OUT=out)
    assert proc.returncode != 0
    assert f"found {found}" in proc.stderr
    assert "set COORDINATOR_REPO" in proc.stderr


@pytest.mark.parametrize("tenant", ["a'b", "-acme", "acme data", "acme/x"])
def test_tenant_outside_the_name_alphabet_is_refused_before_any_call(
    aws_bin: Path, tmp_path: Path, tenant: str
) -> None:
    proc, argv = _run(aws_bin, tmp_path, BIFFIX_TENANT=tenant)
    assert proc.returncode != 0
    assert "not a valid tenant" in proc.stderr
    assert argv == ""


@pytest.mark.parametrize(
    ("env", "message"),
    [
        ({"AWS_REGION": ""}, "AWS_REGION is not set"),
        ({}, "set COORDINATOR_REPO"),
    ],
)
def test_missing_inputs_fail(
    aws_bin: Path, tmp_path: Path, env: dict[str, str], message: str
) -> None:
    proc, argv = _run(aws_bin, tmp_path, BIFFIX_TENANT="", COORDINATOR_REPO="", **env)
    assert proc.returncode != 0
    assert message in proc.stderr
    assert argv == ""
