"""Nextflow's merged-config guard receives the coordinator's reserved task budget."""

from __future__ import annotations

import json
import os
import subprocess
import sys
from pathlib import Path

import pytest

LIB = Path(__file__).resolve().parents[1] / "lib"
BUDGET = ("BIFFIX_SINGLE_JOB_CPUS", "BIFFIX_SINGLE_JOB_TASK_MEMORY_MIB")


@pytest.mark.parametrize("mode,dry_run", [("single_job", ""), ("single_job", "1"), ("tasks", "")])
def test_nextflow_receives_the_task_budget_only_for_single_job(
    tmp_path: Path, mode: str, dry_run: str
) -> None:
    """The plugin must clamp to task memory, not total memory or workflow defaults."""
    # The fake engine reports only the budget variables, so a failure shows nothing else of the
    # environment it inherited.
    report_budget = (
        "import json, os; print(json.dumps({k: os.environ[k] for k in "
        f"{BUDGET!r} if k in os.environ}}))"
    )
    plugins = tmp_path / "plugins"
    (plugins / "nf-biffix-0.8.0").mkdir(parents=True)
    work = tmp_path / "work"
    work.mkdir()
    selectors = tmp_path / "selectors"
    selectors.write_text("")
    result = subprocess.run(
        [
            "bash",
            "-eu",
            "-c",
            r"""
source "$LIB/logs.sh"
source "$LIB/nextflow.sh"
is_single_job() { [ "$EXECUTION" = single_job ]; }
require_count() { :; }
start_logs_sync() { :; }
run_engine() { "$PYTHON" -c "$REPORT_BUDGET"; }
run_nextflow
""",
        ],
        # A budget the developer's own environment exports must not reach the tasks case.
        env={k: v for k, v in os.environ.items() if k not in BUDGET}
        | {
            "LIB": str(LIB),
            "REPORT_BUDGET": report_budget,
            "PYTHON": sys.executable,
            "EXECUTION": mode,
            "BIFFIX_EXECUTION": mode,
            "BIFFIX_DRY_RUN": dry_run,
            "WORK": str(work),
            "SRC": str(tmp_path),
            "STORE": "s3://bucket/run/",
            "PARAMS_JSON": "{}",
            "RUNTIME_ROOT": str(tmp_path / "runtime"),
            "NXF_HOME": str(tmp_path / "nxf"),
            "NXF_PLUGINS_DIR": str(plugins),
            "BIFFIX_ENTRYPOINT": "main.nf",
            "BIFFIX_REGION": "us-test-1",
            "BIFFIX_RUN_ID": "run-1",
            "BIFFIX_PROJECT": "project-1",
            "BIFFIX_TASK_LOG_GROUP": "tasks",
            "SNAKEMAKE_AWS_BATCH_JOB_QUEUE": "queue",
            "SNAKEMAKE_AWS_BATCH_JOB_ROLE": "role",
            "QUEUE_SELECTORS_FILE": str(selectors),
            "SCRATCH": str(tmp_path / "scratch"),
            "SJ_CPUS": "8",
            "SJ_TASK_MEMORY_MIB": "59392",
            "SJ_JVM_HEAP_MIB": "4096",
            "SJ_IMAGES": str(tmp_path / "images"),
            "SJ_BIN": str(tmp_path / "bin"),
        },
        capture_output=True,
        text=True,
        timeout=10,
    )
    assert result.returncode == 0, result.stderr
    engine_env = json.loads(result.stdout)
    if mode == "single_job":
        assert engine_env["BIFFIX_SINGLE_JOB_CPUS"] == "8"
        assert engine_env["BIFFIX_SINGLE_JOB_TASK_MEMORY_MIB"] == "59392"
    else:
        assert "BIFFIX_SINGLE_JOB_CPUS" not in engine_env
        assert "BIFFIX_SINGLE_JOB_TASK_MEMORY_MIB" not in engine_env
