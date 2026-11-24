"""The process a job runs as, and what it reports."""

from __future__ import annotations

import logging
from pathlib import Path
from types import SimpleNamespace
from typing import cast

import pytest
from snakemake_interface_executor_plugins.jobs import JobExecutorInterface

from snakemake_executor_plugin_biffix.command import (
    apptainer_command,
    group_resource_args,
    host_command,
    job_env,
)
from snakemake_executor_plugin_biffix.report import emit_job_image, member_jobids


def test_a_job_with_an_image_runs_through_the_image_s_entrypoint() -> None:
    argv = apptainer_command("python -m snakemake x", Path("/i/t.img"), "/w/src", ["/scratch"])
    assert argv == [
        "apptainer", "run", "--cleanenv", "--no-eval", "--home", "/w/src", "--pwd", "/w/src",
        "--bind", "/scratch", "/i/t.img", "/bin/bash", "-c", "python -m snakemake x",
    ]  # fmt: skip
    assert host_command("python -m snakemake x") == ["/bin/bash", "-c", "python -m snakemake x"]


def test_a_contained_job_gets_only_the_variables_snakemake_passes_jobs() -> None:
    environ = {
        "PATH": "/bin",
        "BIFFIX_INGEST_TOKEN": "secret",
        "APPTAINERENV_BIFFIX_INGEST_TOKEN": "secret",
        "APPTAINER_CACHEDIR": "/c",
    }
    env = job_env(environ, {"AWS_REGION": "us-east-1", "BIFFIX_INGEST_TOKEN": "s"}, contained=True)
    # Apptainer itself keeps the caller's environment; the container gets the named variables.
    assert env == {
        "PATH": "/bin",
        "APPTAINER_CACHEDIR": "/c",
        "APPTAINERENV_AWS_REGION": "us-east-1",
    }


def test_a_contained_job_gets_no_container_variable_but_those_snakemake_passes() -> None:
    """Apptainer sets APPTAINERENV_<NAME>, and the legacy SINGULARITYENV_<NAME>, in the container
    even with --cleanenv: none inherited reaches it, the token's under either prefix included."""
    environ = {
        "PATH": "/bin",
        "APPTAINERENV_PATH": "/elsewhere",
        "APPTAINERENV_PYTHONHOME": "/py",
        "SINGULARITYENV_BIFFIX_INGEST_TOKEN": "secret",
        "SINGULARITYENV_X": "1",
        "APPTAINER_CACHEDIR": "/c",
        "SINGULARITY_CACHEDIR": "/s",
    }
    env = job_env(environ, {"AWS_REGION": "us-east-1"}, contained=True)
    assert env == {
        "PATH": "/bin",
        "APPTAINER_CACHEDIR": "/c",
        "SINGULARITY_CACHEDIR": "/s",
        "APPTAINERENV_AWS_REGION": "us-east-1",
    }


def test_a_job_on_the_host_gets_the_environment_without_the_ingest_token() -> None:
    environ = {
        "PATH": "/bin",
        "BIFFIX_INGEST_TOKEN": "secret",
        "APPTAINERENV_BIFFIX_INGEST_TOKEN": "secret",
        "SINGULARITYENV_BIFFIX_INGEST_TOKEN": "secret",
        "APPTAINERENV_Y": "2",
    }
    env = job_env(environ, {"X": "1"}, contained=False)
    # The token under no name; the host's other container variables are its own to keep.
    assert env == {"PATH": "/bin", "APPTAINERENV_Y": "2", "X": "1"}


def job(
    jobid: int | str, attempt: int = 1, members: list[int] | None = None
) -> JobExecutorInterface:
    """A job as the executor interface hands one over: a group job when it has `members`."""
    return cast(
        JobExecutorInterface,
        SimpleNamespace(
            jobid=jobid,
            attempt=attempt,
            is_group=lambda: members is not None,
            jobs=[SimpleNamespace(jobid=m) for m in members or []],
        ),
    )


def test_a_group_job_reports_its_jobs_ids_not_its_own() -> None:
    assert member_jobids(job(3)) == [3]
    assert member_jobids(job("uuid", members=[4, 5])) == [4, 5]


def test_each_job_s_image_is_reported_on_a_log_record(caplog: pytest.LogCaptureFixture) -> None:
    logger = logging.getLogger("test-report")
    with caplog.at_level(logging.INFO, logger="test-report"):
        emit_job_image(logger, job("uuid", attempt=2, members=[4, 5]), "reg.io/t@sha256:1")
        # A job without an image reports nothing.
        emit_job_image(logger, job(6), None)
    payloads = [record.remote_job_state for record in caplog.records]  # type: ignore[attr-defined]
    assert payloads == [
        {
            "schema_version": 1,
            "kind": "state",
            "jobid": jobid,
            "executor": "biffix",
            "phase": "queued",
            "image": "reg.io/t@sha256:1",
            "attempt": 2,
        }
        for jobid in (4, 5)
    ]
    # No external job id: what tells a reader the job never left the host.
    assert all("external_jobid" not in payload for payload in payloads)


def test_a_logger_that_takes_no_payload_is_left_alone() -> None:
    emit_job_image(object(), job(1), "reg.io/t@sha256:1")


def test_a_logger_whose_info_takes_no_payload_is_left_alone() -> None:
    """The interface declares its logger's `info(msg)` alone: reporting the image is best-effort,
    so such a logger gets nothing rather than fail the job."""
    got: list[str] = []

    class Logger:
        def info(self, msg: str) -> None:
            got.append(msg)

    emit_job_image(Logger(), job(1), "reg.io/t@sha256:1")
    assert got == []


def test_a_logger_that_is_not_a_logging_logger_still_gets_the_image() -> None:
    """The interface types the executor's logger as its own LoggerExecutorInterface, not as a
    logging.Logger: whatever has `info` taking `extra` gets each job's record."""
    got: list[tuple[str, dict[str, object]]] = []

    class Logger:
        def info(self, msg: str, *, extra: dict[str, object]) -> None:
            got.append((msg, extra))

    emit_job_image(Logger(), job(7), "reg.io/t@sha256:1")
    assert got == [
        (
            "job 7 runs in reg.io/t@sha256:1",
            {
                "remote_job_state": {
                    "schema_version": 1,
                    "kind": "state",
                    "jobid": 7,
                    "executor": "biffix",
                    "phase": "queued",
                    "image": "reg.io/t@sha256:1",
                    "attempt": 1,
                }
            },
        )
    ]


@pytest.mark.parametrize(
    ("is_group", "resources", "want"),
    [
        (True, {"mem_mb": 3000}, "--resources mem_mb=3000"),
        (True, {"mem_mb": 2500.0}, "--resources mem_mb=2500"),
        # A later Snakemake 9 holds each value in an object.
        (True, {"mem_mb": SimpleNamespace(value=1200)}, "--resources mem_mb=1200"),
        (False, {"mem_mb": 3000}, ""),
        (True, {}, ""),
        (True, {"mem_mb": 0}, ""),
        (True, {"mem_mb": "full"}, ""),
        (True, {"mem_mb": True}, ""),
    ],
)
def test_a_group_job_is_given_the_memory_reserved_for_it(
    is_group: bool, resources: dict[str, object], want: str
) -> None:
    assert group_resource_args(is_group, resources) == want
