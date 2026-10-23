"""The public emit() → _emit dispatch layer: event routing, the remote-payload channel, and the
swallow-on-error contract (a logging handler must never raise into Snakemake). These exercise the
routing that test_local_rules bypasses by calling the private per-event methods directly.

emit() swallows every exception, so a test that expects "nothing happened" also asserts that no
warning was logged — otherwise "ignored" and "crashed and was swallowed" look the same."""

from __future__ import annotations

import logging

import pytest
from conftest import RecordingHandler, kinds, log_record
from snakemake_interface_logger_plugins.common import LogEvent

HANDLER_LOGGER = "snakemake_logger_plugin_biffix.handler"


def _warned(caplog: pytest.LogCaptureFixture) -> list[str]:
    return [
        r.getMessage()
        for r in caplog.records
        if r.name == HANDLER_LOGGER and r.levelno >= logging.WARNING
    ]


def test_needs_rulegraph(handler: RecordingHandler) -> None:
    # Snakemake logs RULEGRAPH only when some handler asks for it; run_dag depends on it.
    assert handler.needs_rulegraph is True


def test_emit_workflow_started_truncates_and_emits_run_started(handler: RecordingHandler) -> None:
    handler.emit(log_record(event=LogEvent.WORKFLOW_STARTED))
    assert handler._writer.truncated == 1
    assert kinds(handler) == ["run_started"]


def test_emit_progress_routes_to_run_progress(handler: RecordingHandler) -> None:
    handler.emit(log_record(event=LogEvent.PROGRESS, done=2, total=5))
    assert len(handler._dispatched) == 1
    event = handler._dispatched[0]
    assert event["kind"] == "run_progress"
    assert event["completed"] == 2
    assert event["total"] == 5
    assert event["run_id"] == "r1"


def test_emit_routes_job_lifecycle_for_jobid_zero(
    handler: RecordingHandler, caplog: pytest.LogCaptureFixture
) -> None:
    # Snakemake numbers jobs from 0, and JOB_FINISHED carries `job_id` (not `jobid`): jobid 0 is
    # often the local `all` rule, so a truthiness slip would drop its terminal.
    caplog.set_level(logging.WARNING, logger=HANDLER_LOGGER)
    handler.emit(log_record(event=LogEvent.JOB_INFO, jobid=0, rule_name="all", wildcards={}))
    handler.emit(log_record(event=LogEvent.JOB_FINISHED, job_id=0))
    assert kinds(handler) == ["task_created", "task_succeeded"]
    assert handler._dispatched[1]["task_key"] == "all"
    assert _warned(caplog) == []


def test_emit_routes_job_error_to_task_failed(
    handler: RecordingHandler, caplog: pytest.LogCaptureFixture
) -> None:
    caplog.set_level(logging.WARNING, logger=HANDLER_LOGGER)
    handler.emit(log_record(event=LogEvent.JOB_INFO, jobid=0, rule_name="all", wildcards={}))
    handler.emit(log_record(event=LogEvent.JOB_ERROR, jobid=0, rule_name="all"))
    assert kinds(handler) == ["task_created", "task_failed"]
    assert _warned(caplog) == []


def test_emit_routes_rulegraph_to_run_dag(handler: RecordingHandler) -> None:
    rulegraph = {
        "nodes": [{"rule": "a"}, {"rule": "b"}],
        "links": [{"sourcerule": "a", "targetrule": "b"}],
    }
    handler.emit(log_record(event=LogEvent.RULEGRAPH, rulegraph=rulegraph))
    assert kinds(handler) == ["run_dag"]


def test_emit_remote_payload_routes_to_task_submitted(handler: RecordingHandler) -> None:
    handler.emit(
        log_record(event=LogEvent.JOB_INFO, jobid=4, rule_name="align", wildcards={"sample": "A"})
    )
    payload = {"schema_version": 1, "external_jobid": "aws-1", "jobid": 4, "attempt": 1}
    handler.emit(log_record(remote_job_state=payload, jobid=4))
    submits = [e for e in handler._dispatched if e["kind"] == "task_submitted"]
    assert len(submits) == 1
    assert submits[0]["batch_job_id"] == "aws-1"
    assert submits[0]["task_key"] == "align:sample=A"
    assert 4 in handler._job_attempts


def test_emit_remote_payload_for_an_unknown_rule_binds_nothing(handler: RecordingHandler) -> None:
    # No JOB_INFO and no rule on the record: there is no task to bind. Nothing is published and
    # nothing recorded, so a later record for this job can still bind once its rule is known.
    payload = {"schema_version": 1, "external_jobid": "aws-1", "jobid": 4, "attempt": 1}
    handler.emit(log_record(remote_job_state=payload, jobid=4))
    assert handler._dispatched == []
    assert handler._job_attempts == {} and handler._emitted_submits == set()
    handler.emit(log_record(event=LogEvent.JOB_INFO, jobid=4, rule_name="align", wildcards={}))
    handler.emit(log_record(remote_job_state=payload, jobid=4))
    assert kinds(handler) == ["task_created", "task_submitted"]


def test_emit_remote_retry_publishes_retrying_then_submitted(handler: RecordingHandler) -> None:
    # The executor reports Snakemake's attempt number; a retry (attempt 2) is a new Batch job.
    handler._job_rules[4] = "align"
    payload = {"schema_version": 1, "external_jobid": "aws-2", "jobid": 4, "attempt": 2}
    handler.emit(log_record(remote_job_state=payload, jobid=4))
    assert kinds(handler) == ["task_retrying", "task_submitted"]
    retrying, submitted = handler._dispatched
    assert (retrying["prev_attempt"], retrying["next_attempt"]) == (1, 2)
    assert submitted["attempt"] == 2
    assert submitted["batch_job_id"] == "aws-2"
    assert retrying["task_key"] == submitted["task_key"] == "align"


def test_emit_remote_group_job_payload_is_ignored(
    handler: RecordingHandler, caplog: pytest.LogCaptureFixture
) -> None:
    # A group job carries a UUID string id with no rule to build a task_key from.
    caplog.set_level(logging.WARNING, logger=HANDLER_LOGGER)
    payload = {
        "schema_version": 1,
        "external_jobid": "aws-g",
        "jobid": "3a7c1e2f-0000-4444-8888-abcdef012345",
    }
    handler.emit(log_record(remote_job_state=payload))
    assert handler._dispatched == []
    assert handler._job_attempts == {}
    assert _warned(caplog) == []


def test_emit_swallows_exceptions_from_a_bad_record(
    handler: RecordingHandler, caplog: pytest.LogCaptureFixture
) -> None:
    # A non-numeric progress field makes int() raise inside _progress; emit() must swallow it
    # (logging must never break the workflow), dispatch nothing, and say so once.
    caplog.set_level(logging.WARNING, logger=HANDLER_LOGGER)
    handler.emit(log_record(event=LogEvent.PROGRESS, done="not-a-number", total=5))
    assert handler._dispatched == []
    swallowed = [m for m in _warned(caplog) if m == "biffix logger plugin: emit failed (swallowed)"]
    assert len(swallowed) == 1


def test_emit_ignores_records_with_no_event_or_payload(
    handler: RecordingHandler, caplog: pytest.LogCaptureFixture
) -> None:
    caplog.set_level(logging.WARNING, logger=HANDLER_LOGGER)
    handler.emit(log_record(event=None))
    handler.emit(log_record())  # neither .event nor a remote payload
    handler.emit(logging.LogRecord("snakemake", logging.INFO, "x.py", 1, "plain line", (), None))
    assert handler._dispatched == []
    assert handler._writer.truncated == 0
    assert _warned(caplog) == []
