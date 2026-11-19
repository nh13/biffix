"""The log uploader's deadline belongs only to the active upload call."""

from __future__ import annotations

import os
import subprocess
import sys
from pathlib import Path

import pytest

LIB = Path(__file__).resolve().parents[1] / "lib"
FAKES = Path(__file__).parent / "fakes" / "py"


@pytest.mark.parametrize("uri", ["s3://b/logs/", "s3://b/missing-slash"])
def test_returning_from_log_upload_cancels_its_deadline(tmp_path: Path, uri: str) -> None:
    """A finished or handled-error upload must not exit its caller later."""
    got = subprocess.run(
        [
            sys.executable,
            "-c",
            "import run_logs, sys, time; "
            "assert run_logs.main(sys.argv[1:]) == 0; "
            "print('returned', flush=True); "
            "time.sleep(0.6); "
            "print('caller survived')",
            uri,
            str(tmp_path / "state.json"),
            "0.3",
        ],
        env={
            "PATH": os.environ["PATH"],
            "PYTHONPATH": os.pathsep.join((str(LIB), str(FAKES))),
            "FAKE_S3": str(tmp_path / "s3"),
        },
        capture_output=True,
        text=True,
        timeout=10,
    )
    assert got.returncode == 0, got.stderr
    assert got.stdout.splitlines() == ["returned", "caller survived"]
    assert "abandoning it" not in got.stderr
