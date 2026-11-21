"""A single-job run's task images.

In a single-job run every job runs on the coordinator's host, never as a Batch job, so no
``task_submitted`` names its image. The biffix executor plugin, which runs the jobs, reports each
job's image on a log record before Snakemake announces the job, and the job's ``task_created``
names it. A job no such record names (a local rule, which the engine runs itself) names none.
"""

from __future__ import annotations

import logging

import pytest
from conftest import RecordingHandler, build_handler, log_record
from snakemake_interface_logger_plugins.common import LogEvent

ALIGN = "123456789012.dkr.ecr.us-east-1.amazonaws.com/biffix/wf/align@sha256:" + "a" * 64


def _handler() -> RecordingHandler:
    h = build_handler(run_id="r1")
    h._dispatched = h._poster.posted
    return h


def _created(h: RecordingHandler) -> list[dict[str, object]]:
    return [e for e in h._dispatched if e["kind"] == "task_created"]


OTHER = "123456789012.dkr.ecr.us-east-1.amazonaws.com/biffix/wf/other@sha256:" + "b" * 64


def _reported(jobid: int, image: str, **extra: object) -> logging.LogRecord:
    """The biffix executor plugin's record for a job about to run on this host in ``image``."""
    payload = {
        "schema_version": 1,
        "kind": "state",
        "jobid": jobid,
        "executor": "biffix",
        "phase": "queued",
        "image": image,
        "attempt": 1,
        **extra,
    }
    return log_record(remote_job_state=payload)


def test_each_job_names_the_image_the_executor_reported_for_it() -> None:
    """Per job, not per rule: two jobs of one rule in two group jobs can run in two images."""
    h = _handler()
    h.emit(_reported(1, ALIGN))
    h.emit(_reported(2, OTHER))
    h.emit(log_record(event=LogEvent.JOB_INFO, jobid=1, rule_name="align", wildcards={"s": "A"}))
    h.emit(log_record(event=LogEvent.JOB_INFO, jobid=2, rule_name="align", wildcards={"s": "B"}))
    # A job nothing was reported for (a local rule, which the engine runs itself) names none.
    h.emit(log_record(event=LogEvent.JOB_INFO, jobid=3, rule_name="all"))
    created = _created(h)
    assert [e.get("image") for e in created] == [ALIGN, OTHER, None]
    assert "image" not in created[2]
    # Reporting an image binds no Batch job, so the engine stays the job's terminal source.
    assert [e["kind"] for e in h._dispatched] == ["task_created"] * 3
    h.emit(log_record(event=LogEvent.JOB_FINISHED, job_id=1))
    assert h._dispatched[-1]["kind"] == "task_succeeded"


def test_a_retried_job_names_the_image_reported_for_the_retry() -> None:
    h = _handler()
    h.emit(_reported(1, ALIGN))
    h.emit(log_record(event=LogEvent.JOB_INFO, jobid=1, rule_name="align"))
    h.emit(log_record(event=LogEvent.JOB_ERROR, jobid=1))
    # An image is named once: a second announcement with no report names none.
    h.emit(_reported(1, OTHER, attempt=2))
    h.emit(log_record(event=LogEvent.JOB_INFO, jobid=1, rule_name="align"))
    h.emit(log_record(event=LogEvent.JOB_INFO, jobid=1, rule_name="align"))
    assert [e.get("image") for e in _created(h)] == [ALIGN, OTHER, None]


@pytest.mark.parametrize(
    "raw, expected",
    [
        ("1", 1),
        ("2", 2),
        (" 2 ", 2),
        ("", None),
        ("0", None),
        ("-1", None),
        ("no", None),
        ("4294967296", None),
    ],
)
def test_every_fact_names_the_positive_coordinator_attempt(
    monkeypatch: pytest.MonkeyPatch, raw: str, expected: int | None
) -> None:
    monkeypatch.setenv("AWS_BATCH_JOB_ATTEMPT", raw)
    h = _handler()
    h._workflow_started(ts=0.5)
    h._job_info(log_record(jobid=1, rule_name="align"), ts=1.0)
    h._progress(log_record(done=1, total=2), ts=2.0)
    assert h._dispatched
    assert all(e.get("coordinator_attempt") == expected for e in h._dispatched)
    assert any(e["kind"] == "run_started" for e in h._dispatched)


def test_outside_a_single_job_run_no_image_is_named(handler: RecordingHandler) -> None:
    handler._job_info(log_record(jobid=1, rule_name="align"), ts=1.0)
    (created,) = _created(handler)
    assert "image" not in created
