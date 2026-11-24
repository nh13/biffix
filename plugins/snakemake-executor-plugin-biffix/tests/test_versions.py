"""The Snakemake the tests run is the one CI's matrix asked for."""

from __future__ import annotations

import os
import subprocess
import sys
from importlib.metadata import version

import pytest


def test_the_snakemake_under_test_is_the_one_asked_for() -> None:
    """CI names each job's Snakemake (BIFFIX_TEST_SNAKEMAKE: a release, or `locked`). A job that
    ran another one, as `uv run --with … pytest` does (the project's own pytest script, and its
    interpreter), would pass while testing nothing it claims to; the workflows the tests run use
    this interpreter's Snakemake too."""
    expected = os.environ.get("BIFFIX_TEST_SNAKEMAKE", "locked")
    if expected == "locked":
        pytest.skip("the locked Snakemake: nothing asked for another")
    assert version("snakemake") == expected
    spawned = subprocess.run(
        [
            sys.executable,
            "-c",
            "from importlib.metadata import version; print(version('snakemake'))",
        ],
        capture_output=True,
        text=True,
        check=True,
    )
    assert spawned.stdout.strip() == expected
