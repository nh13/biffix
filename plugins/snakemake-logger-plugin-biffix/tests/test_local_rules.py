"""Local-rule (engine-terminal) emission.

A rule that runs locally on the coordinator (an aggregation target like ``all``, or a localrule)
is never submitted to AWS Batch, so it has no EventBridge terminal. The engine is its only terminal
source: the handler must emit an engine ``task_succeeded``/``task_failed`` on finish for a jobid
that never got a Batch binding — but must NOT for a Batch-bound job (EventBridge owns that). A
local rule retried under ``--retries`` gets its attempt numbers and a ``task_retrying`` here too.
"""

from __future__ import annotations

import logging

from conftest import RecordingHandler, build_handler, kinds, log_record
from snakemake_interface_logger_plugins.common import LogEvent

from snakemake_logger_plugin_biffix.handler import _paths


def _restart_message(jobid: int) -> logging.LogRecord:
    # What Snakemake's scheduler logs (no `event`) when --retries will re-run a failed job.
    return log_record(msg=f"Trying to restart job {jobid}.")


def test_task_submitted_published_once_per_phase_storm(handler: RecordingHandler) -> None:
    # remote_job_state fires once per phase (queued/running/succeeded); the binding must publish
    # once, not three times, while the jobid is still marked batch-bound each time.
    h = handler
    h._job_info(log_record(jobid=8, rule_name="align", wildcards={"sample": "A"}), ts=1.0)
    payload = {"schema_version": 1, "external_jobid": "aws-8", "jobid": 8, "attempt": 1}
    for _phase in ("queued", "running", "succeeded"):
        h._handle_remote(log_record(jobid=8), payload, ts=2.0)
    (created,) = [e for e in h._dispatched if e["kind"] == "task_created"]
    submits = [e for e in h._dispatched if e["kind"] == "task_submitted"]
    assert len(submits) == 1
    assert submits[0]["batch_job_id"] == "aws-8"
    # The binding joins onto the task JOB_INFO created, wildcards included.
    assert submits[0]["task_key"] == created["task_key"] == "align:sample=A"
    assert 8 in h._job_attempts  # still marked bound despite the early return


def test_batch_retry_sequence_keeps_one_task_key(handler: RecordingHandler) -> None:
    # JOB_INFO → Batch attempt 1 → JOB_ERROR → restart → JOB_INFO → Batch attempt 2 → JOB_FINISHED.
    h = handler
    rec = log_record(
        jobid=4, rule_name="align", wildcards={"sample": "A"}, output=["s3://b/a (send to storage)"]
    )
    h._job_info(rec, ts=1.0)
    h._handle_remote(
        log_record(jobid=4), {"schema_version": 1, "external_jobid": "j1", "attempt": 1}, 2.0
    )
    h._job_error(log_record(jobid=4), ts=3.0)
    h.emit(_restart_message(4))
    h._job_info(rec, ts=4.0)
    h._handle_remote(
        log_record(jobid=4), {"schema_version": 1, "external_jobid": "j2", "attempt": 2}, 5.0
    )
    h._job_finished(log_record(job_id=4), ts=6.0)
    assert kinds(h) == [
        "task_created",
        "task_submitted",
        "task_created",
        "task_retrying",
        "task_submitted",
        "task_outputs",
    ]
    assert {e["task_key"] for e in h._dispatched} == {"align:sample=A"}
    retrying = h._dispatched[3]
    assert (retrying["prev_attempt"], retrying["next_attempt"]) == (1, 2)
    assert h._dispatched[4]["attempt"] == 2 and h._dispatched[4]["batch_job_id"] == "j2"
    assert h._dispatched[5]["attempt"] == 2


def test_local_rule_emits_engine_success_on_finish(handler: RecordingHandler) -> None:
    h = handler
    # JOB_INFO records the rule for a local job (jobid 5), which never gets a Batch binding.
    h._job_info(log_record(jobid=5, rule_name="all", wildcards=None), ts=1.0)
    assert kinds(h) == ["task_created"]
    # JOB_FINISHED for the never-bound jobid → engine task_succeeded with no batch_job_id.
    h._job_finished(log_record(job_id=5), ts=2.0)
    last = h._dispatched[-1]
    assert last["kind"] == "task_succeeded"
    assert last["task_key"] == "all"
    assert last["attempt"] == 1
    assert "batch_job_id" not in last
    assert 5 not in h._job_rules  # cleaned up


def test_local_rule_emits_engine_failure_on_error(handler: RecordingHandler) -> None:
    h = handler
    h._job_info(log_record(jobid=3, rule_name="all", wildcards=None), ts=1.0)
    h._job_error(log_record(jobid=3), ts=2.0)
    last = h._dispatched[-1]
    assert last["kind"] == "task_failed"
    assert last["task_key"] == "all"
    assert last["attempt"] == 1


def test_local_retry_is_announced_at_the_restart_message(handler: RecordingHandler) -> None:
    # JOB_INFO → JOB_ERROR → "Trying to restart job 3." → JOB_INFO → JOB_FINISHED: the retry is
    # announced right after the failure, so the reducer shows a retry gap, not a failure.
    h = handler
    rec = log_record(jobid=3, rule_name="agg", wildcards=None)
    h._job_info(rec, ts=1.0)
    h._job_error(log_record(jobid=3), ts=2.0)
    h.emit(_restart_message(3))
    assert kinds(h) == ["task_created", "task_failed", "task_retrying"]
    h._job_info(rec, ts=3.0)
    h._job_finished(log_record(job_id=3), ts=4.0)
    assert kinds(h) == [
        "task_created",
        "task_failed",
        "task_retrying",
        "task_created",
        "task_succeeded",
    ]
    failed, retrying, succeeded = h._dispatched[1], h._dispatched[2], h._dispatched[4]
    assert failed["attempt"] == 1
    assert (retrying["prev_attempt"], retrying["next_attempt"]) == (1, 2)
    assert succeeded["attempt"] == 2
    assert {e["task_key"] for e in h._dispatched} == {"agg"}
    # Nothing about the finished job is kept.
    assert 3 not in h._local_attempts and 3 not in h._pending_restarts


def test_local_retry_falls_back_to_the_relogged_job_info(handler: RecordingHandler) -> None:
    # Without the restart message (e.g. upstream reworded it), the retry's JOB_INFO announces it.
    h = handler
    rec = log_record(jobid=3, rule_name="agg", wildcards=None)
    h._job_info(rec, ts=1.0)
    h._job_error(log_record(jobid=3), ts=2.0)
    h._job_info(rec, ts=3.0)
    assert kinds(h) == ["task_created", "task_failed", "task_retrying", "task_created"]
    # Announced once: a later restart message for the same failure adds nothing.
    h.emit(_restart_message(3))
    assert kinds(h).count("task_retrying") == 1


def test_each_local_failure_carries_its_own_attempt(handler: RecordingHandler) -> None:
    # Two failed attempts must not share a dedup key (attempt-less, the second would be dropped).
    h = handler
    rec = log_record(jobid=0, rule_name="agg", wildcards=None)
    for ts in (1.0, 3.0):
        h._job_info(rec, ts=ts)
        h._job_error(log_record(jobid=0), ts=ts + 1)
        h.emit(_restart_message(0))
    h._job_info(rec, ts=5.0)
    h._job_error(log_record(jobid=0), ts=6.0)
    failures = [e["attempt"] for e in h._dispatched if e["kind"] == "task_failed"]
    retries = [
        (e["prev_attempt"], e["next_attempt"])
        for e in h._dispatched
        if e["kind"] == "task_retrying"
    ]
    assert failures == [1, 2, 3]
    assert retries == [(1, 2), (2, 3)]


def test_restart_message_for_an_unknown_job_is_ignored(handler: RecordingHandler) -> None:
    h = handler
    h._job_info(log_record(jobid=3, rule_name="agg", wildcards=None), ts=1.0)
    h._job_error(log_record(jobid=3), ts=2.0)
    h.emit(_restart_message(9))
    h.emit(log_record(msg="Trying to restart job three."))
    assert "task_retrying" not in kinds(h)


def test_batch_bound_job_does_not_emit_engine_terminal(handler: RecordingHandler) -> None:
    h = handler
    h._job_info(log_record(jobid=7, rule_name="align", wildcards={"sample": "A"}), ts=1.0)
    h._job_attempts[7] = 1  # it ran on Batch — EventBridge is authoritative
    h._job_finished(log_record(job_id=7), ts=2.0)
    assert "task_succeeded" not in kinds(h)


def test_batch_bound_job_error_emits_no_engine_terminal_or_retry(handler: RecordingHandler) -> None:
    h = handler
    h._job_info(log_record(jobid=7, rule_name="align", wildcards={"sample": "A"}), ts=1.0)
    h._handle_remote(
        log_record(jobid=7), {"schema_version": 1, "external_jobid": "j", "attempt": 1}, 1.5
    )
    h._job_error(log_record(jobid=7), ts=2.0)
    h.emit(_restart_message(7))
    # EventBridge owns the Batch terminal, and the executor's attempt-2 binding the retry.
    assert "task_failed" not in kinds(h)
    assert "task_retrying" not in kinds(h)


def _missing_output_error(
    jobid: int | str, exception: str = "MissingOutputException"
) -> logging.LogRecord:
    # What the scheduler logs (print_exception) when a job that exited 0 lacks its outputs.
    return log_record(
        event=LogEvent.ERROR,
        exception=exception,
        msg=f'MissingOutputException in rule align in file "Snakefile", line 3:\nJob {jobid} '
        "completed successfully, but some output files are missing. Missing files after 5 "
        "seconds:\ns3://b/a.bam",
    )


def test_a_batch_job_missing_its_outputs_gets_a_verdict_on_its_attempt(
    handler: RecordingHandler,
) -> None:
    h = handler
    h._job_info(log_record(jobid=7, rule_name="align", wildcards={"sample": "A"}), ts=1.0)
    h._handle_remote(
        log_record(jobid=7), {"schema_version": 1, "external_jobid": "j", "attempt": 2}, 1.5
    )
    h.emit(_missing_output_error(7))
    (verdict,) = [e for e in h._dispatched if e["kind"] == "engine_task_verdict"]
    assert verdict["task_key"] == "align:sample=A"
    assert verdict["attempt"] == 2
    assert verdict["verdict"] == "failed"
    assert verdict["category"] == "missing_output"
    reason = verdict["reason"]
    assert isinstance(reason, str)
    assert reason.startswith("Job 7 completed successfully, but some output files")
    assert reason.endswith("s3://b/a.bam")


def test_no_verdict_for_a_local_job_an_unknown_job_or_another_error(
    handler: RecordingHandler,
) -> None:
    h = handler
    # Job 3 is local (no Batch binding); job 7 ran on Batch.
    h._job_info(log_record(jobid=3, rule_name="all", wildcards={}), ts=1.0)
    h._job_info(log_record(jobid=7, rule_name="align", wildcards={"sample": "A"}), ts=1.0)
    h._handle_remote(
        log_record(jobid=7), {"schema_version": 1, "external_jobid": "j", "attempt": 1}, 1.5
    )
    h.emit(_missing_output_error(3))
    h.emit(_missing_output_error(99))
    h.emit(_missing_output_error(7, exception="WorkflowError"))
    h.emit(_missing_output_error(""))  # job 0's (no id), but job 0 was never seen
    assert "engine_task_verdict" not in kinds(h)


def _bind_job_0(h: RecordingHandler, *, batch: bool) -> None:
    h._job_info(log_record(jobid=0, rule_name="merge", wildcards={}), ts=1.0)
    if batch:
        remote = {"schema_version": 1, "external_jobid": "j", "attempt": 1}
        h._handle_remote(log_record(jobid=0), remote, 1.5)


def test_job_0_s_id_less_message_gets_its_verdict(handler: RecordingHandler) -> None:
    # Snakemake writes the id only when it is truthy, so job 0's message has none.
    h = handler
    _bind_job_0(h, batch=True)
    h.emit(_missing_output_error(""))
    (verdict,) = [e for e in h._dispatched if e["kind"] == "engine_task_verdict"]
    assert verdict["task_key"] == "merge"
    assert verdict["attempt"] == 1
    reason = verdict["reason"]
    assert isinstance(reason, str)
    assert reason.startswith("Job  completed successfully, but some output files")


def test_no_verdict_for_a_local_job_0(handler: RecordingHandler) -> None:
    h = handler
    _bind_job_0(h, batch=False)
    h.emit(_missing_output_error(""))
    assert "engine_task_verdict" not in kinds(h)


def test_an_id_less_touch_failure_is_not_taken_for_job_0(handler: RecordingHandler) -> None:
    # A missing touch() output raises without a job id on any job: it names no job.
    h = handler
    _bind_job_0(h, batch=True)
    h.emit(
        log_record(
            event=LogEvent.ERROR,
            exception="MissingOutputException",
            msg="MissingOutputException in rule flag:\nJob  completed successfully, but some "
            "output files are missing. Output file done.txt of rule flag shall be touched but "
            "does not exist.",
        )
    )
    assert "engine_task_verdict" not in kinds(h)


def test_a_verdict_reason_never_carries_the_ingest_token() -> None:
    h = build_handler(run_id="r1", ingest_token="sekrit")
    h._job_info(log_record(jobid=7, rule_name="align", wildcards={}), ts=1.0)
    h._handle_remote(
        log_record(jobid=7), {"schema_version": 1, "external_jobid": "j", "attempt": 1}, 1.5
    )
    record = _missing_output_error(7)
    record.msg += "/sekrit.bam"
    h.emit(record)
    (verdict,) = [e for e in h._poster.posted if e["kind"] == "engine_task_verdict"]
    reason = verdict["reason"]
    assert isinstance(reason, str)
    assert "sekrit" not in reason and reason.endswith("<token>.bam")


def test_the_binding_names_the_image_the_executor_reports(handler: RecordingHandler) -> None:
    h = handler
    h._job_info(log_record(jobid=7, rule_name="align", wildcards={}), ts=1.0)
    h._job_info(log_record(jobid=8, rule_name="align", wildcards={"s": "B"}), ts=1.0)
    image = "123456789012.dkr.ecr.us-east-1.amazonaws.com/wf/align@sha256:" + "a" * 64
    h._handle_remote(
        log_record(jobid=7),
        {"schema_version": 1, "external_jobid": "j7", "attempt": 1, "image": image},
        2.0,
    )
    h._handle_remote(
        log_record(jobid=8), {"schema_version": 1, "external_jobid": "j8", "attempt": 1}, 2.0
    )
    submits = {e["batch_job_id"]: e for e in h._dispatched if e["kind"] == "task_submitted"}
    assert submits["j7"]["image"] == image
    assert "image" not in submits["j8"]


def test_unknown_jobid_on_finish_emits_nothing(handler: RecordingHandler) -> None:
    # A JOB_FINISHED whose rule was never recorded can't build a task_key → emit nothing.
    h = handler
    h._job_finished(log_record(job_id=99), ts=1.0)
    assert h._dispatched == []


def test_rulegraph_emits_run_dag(handler: RecordingHandler) -> None:
    # The RULEGRAPH log record carries Snakemake's rule graph; the handler turns it into a run_dag
    # event (nodes as {"id": rule}, edges as {"from": sourcerule, "to": targetrule}).
    h = handler
    record = log_record(
        rulegraph={
            "nodes": [{"rule": "align"}, {"rule": "sort"}],
            "links": [{"source": 0, "target": 1, "sourcerule": "align", "targetrule": "sort"}],
        }
    )
    h._rulegraph(record, ts=1.0)
    dags = [e for e in h._dispatched if e["kind"] == "run_dag"]
    assert len(dags) == 1
    dag = dags[0]["dag"]
    assert isinstance(dag, dict)
    assert {n["id"] for n in dag["nodes"]} == {"align", "sort"}
    assert dag["edges"] == [{"from": "align", "to": "sort"}]


def test_task_created_carries_rule_group(handler: RecordingHandler) -> None:
    h = handler
    h._job_info(log_record(jobid=9, rule_name="align", wildcards={"sample": "A"}), ts=1.0)
    created = [e for e in h._dispatched if e["kind"] == "task_created"]
    assert created and created[0]["group"] == "align"


def test_job_deps_are_the_in_run_producers_of_its_inputs(handler: RecordingHandler) -> None:
    # align A and B produce BAMs; merge consumes both plus a source file no job produces.
    h = handler
    h._job_info(
        log_record(
            jobid=1, rule_name="align", wildcards={"sample": "A"}, input=["a.fq"], output=["a.bam"]
        ),
        ts=1.0,
    )
    h._job_info(
        log_record(
            jobid=2, rule_name="align", wildcards={"sample": "B"}, input=["b.fq"], output=["b.bam"]
        ),
        ts=1.0,
    )
    h._job_info(
        log_record(
            jobid=3,
            rule_name="merge",
            wildcards=None,
            input=["b.bam", "a.bam", "ref.fa"],
            output=["merged.bam"],
        ),
        ts=2.0,
    )
    created = {e["task_key"]: e for e in h._dispatched if e["kind"] == "task_created"}
    assert "deps" not in created["align:sample=A"]  # no in-run upstream: key omitted
    assert created["merge"]["deps"] == ["align:sample=A", "align:sample=B"]


def test_job_without_file_lists_has_no_deps(handler: RecordingHandler) -> None:
    h = handler
    h._job_info(log_record(jobid=1, rule_name="all", wildcards=None), ts=1.0)
    assert "deps" not in h._dispatched[0]


def test_storage_annotations_do_not_hide_a_dependency(handler: RecordingHandler) -> None:
    # With S3 default storage Snakemake logs the same object as "... (send to storage)" when a job
    # outputs it and "... (retrieve from storage)" when the next job inputs it.
    h = handler
    h._job_info(
        log_record(
            jobid=1,
            rule_name="hello",
            wildcards=None,
            input=[],
            output=["s3://b/runs/r1/hello.txt (send to storage)"],
        ),
        ts=1.0,
    )
    h._job_info(
        log_record(
            jobid=2,
            rule_name="world",
            wildcards=None,
            input=["s3://b/runs/r1/hello.txt (retrieve from storage)"],
            output=["s3://b/runs/r1/world.txt (send to storage)"],
        ),
        ts=2.0,
    )
    world = [e for e in h._dispatched if e["task_key"] == "world"][0]
    assert world["deps"] == ["hello"]


def test_file_annotation_stripping_keeps_real_parentheses() -> None:
    assert _paths(["a.txt (pipe)", "b.txt (cached, in storage)", "c.txt (access: sequential)"]) == [
        "a.txt",
        "b.txt",
        "c.txt",
    ]
    # A filename that merely ends in parentheses is left alone.
    assert _paths(["report (Final).pdf", "x (1)"]) == ["report (Final).pdf", "x (1)"]
    # A single string is one path; None, bytes and a non-iterable are none.
    assert _paths("a.txt (pipe)") == ["a.txt"]
    assert _paths(None) == [] and _paths(b"a") == [] and _paths(42) == []
