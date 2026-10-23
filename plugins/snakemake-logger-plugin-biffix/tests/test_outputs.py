"""``task_outputs`` emission: which S3 objects each task produced.

JOB_INFO lists a job's outputs, logs and benchmark (as ``fmt_iofile`` display strings); the handler
remembers the storage-backed ones and reports them at JOB_FINISHED, one event per role, for Batch
and local jobs alike.
"""

from __future__ import annotations

from conftest import RecordingHandler, log_record

from snakemake_logger_plugin_biffix.handler import _output_uris
from snakemake_logger_plugin_biffix.poster import MAX_OUTPUTS_PER_EVENT


def _job_info(h: RecordingHandler, jobid: int, rule: str = "align", **fields: object) -> None:
    h._job_info(
        log_record(jobid=jobid, rule_name=rule, wildcards={"sample": "A"}, **fields), ts=1.0
    )


def _outputs_events(h: RecordingHandler) -> list[dict[str, object]]:
    return [e for e in h._dispatched if e["kind"] == "task_outputs"]


def test_finish_emits_one_event_per_role(handler: RecordingHandler) -> None:
    h = handler
    _job_info(
        h,
        1,
        output=["s3://b/a.bam (send to storage)", "s3://b/a.bai (send to storage)"],
        log=["s3://b/logs/a.log (send to storage)"],
        benchmark="s3://b/bench/a.tsv (send to storage)",  # a single string, not a list
    )
    h._job_finished(log_record(job_id=1), ts=2.0)
    by_role = {e["role"]: e for e in _outputs_events(h)}
    assert set(by_role) == {"output", "log", "benchmark"}
    assert by_role["output"]["outputs"] == [{"s3_uri": "s3://b/a.bai"}, {"s3_uri": "s3://b/a.bam"}]
    assert by_role["log"]["outputs"] == [{"s3_uri": "s3://b/logs/a.log"}]
    assert by_role["benchmark"]["outputs"] == [{"s3_uri": "s3://b/bench/a.tsv"}]
    for e in by_role.values():
        assert e["task_key"] == "align:sample=A"
        assert "attempt" not in e  # a local job's outputs carry no Batch attempt
        assert "reused" not in e


def test_a_role_s_outputs_are_cut_into_events_under_the_route_limit(
    handler: RecordingHandler,
) -> None:
    # As nf-biffix does: one event of every output of a huge glob would be over the route's body
    # limit, and dropped whole.
    h = handler
    n = 2 * MAX_OUTPUTS_PER_EVENT + 5
    uris = [f"s3://b/out/{i:05d} (send to storage)" for i in range(n)]
    _job_info(h, 1, output=uris + uris[:3])  # a repeat is reported once
    h._job_finished(log_record(job_id=1), ts=2.0)
    chunks: list[list[dict[str, str]]] = []
    for e in _outputs_events(h):
        outputs = e["outputs"]
        assert isinstance(outputs, list)
        chunks.append(outputs)
    assert [len(c) for c in chunks] == [MAX_OUTPUTS_PER_EVENT, MAX_OUTPUTS_PER_EVENT, 5]
    assert [r["s3_uri"] for c in chunks for r in c] == [f"s3://b/out/{i:05d}" for i in range(n)]


def test_outputs_precede_local_terminal(handler: RecordingHandler) -> None:
    h = handler
    _job_info(h, 1, rule="all", output=["s3://b/done (send to storage)"])
    h._job_finished(log_record(job_id=1), ts=2.0)
    assert [e["kind"] for e in h._dispatched] == ["task_created", "task_outputs", "task_succeeded"]


def test_output_uris_strip_annotations_and_skip_non_storage() -> None:
    assert _output_uris(
        [
            "s3://b/k (send to storage)",
            "s3://b/u (update, send to storage)",
            "s3://b/plain",
            "results/local.txt",  # coordinator-local, never reaches S3
            "s3://b/p (pipe)",
            "s3://b/s (service, send to storage)",
            "s3://b/n (nodelocal)",
            None,
        ]
    ) == ["s3://b/k", "s3://b/u", "s3://b/plain"]
    assert _output_uris("s3://b/bench.tsv (send to storage)") == ["s3://b/bench.tsv"]
    assert _output_uris(None) == []
    assert _output_uris(42) == []


def test_attempt_comes_from_the_remote_payload(handler: RecordingHandler) -> None:
    h = handler
    _job_info(h, 4, output=["s3://b/a.bam (send to storage)"])
    for attempt in (1, 2):
        payload = {"schema_version": 1, "external_jobid": f"aws-{attempt}", "attempt": attempt}
        h._handle_remote(log_record(jobid=4), payload, ts=1.5)
    # A trailing phase record of the older attempt must not lower it.
    payload = {"schema_version": 1, "external_jobid": "aws-1", "attempt": 1}
    h._handle_remote(log_record(jobid=4), payload, ts=1.6)
    h._job_finished(log_record(job_id=4), ts=2.0)
    (event,) = _outputs_events(h)
    assert event["attempt"] == 2
    # A Batch job still gets no engine terminal (EventBridge owns it).
    assert "task_succeeded" not in [e["kind"] for e in h._dispatched]


def test_rerun_job_info_overwrites_outputs(handler: RecordingHandler) -> None:
    h = handler
    _job_info(h, 1, output=["s3://b/old (send to storage)"])
    _job_info(h, 1, output=["s3://b/new (send to storage)"])
    h._job_finished(log_record(job_id=1), ts=2.0)
    (event,) = _outputs_events(h)
    assert event["outputs"] == [{"s3_uri": "s3://b/new"}]


def test_error_emits_no_outputs_and_forgets(handler: RecordingHandler) -> None:
    h = handler
    _job_info(h, 1, output=["s3://b/a.bam (send to storage)"])
    h._handle_remote(log_record(jobid=1), {"schema_version": 1, "external_jobid": "x"}, 1.5)
    h._job_error(log_record(jobid=1), ts=2.0)
    assert _outputs_events(h) == []
    # A Batch-bound job's failure is EventBridge's to report: no engine terminal.
    assert "task_failed" not in [e["kind"] for e in h._dispatched]
    assert 1 not in h._job_outputs and 1 not in h._job_attempts


def test_job_without_s3_outputs_emits_nothing(handler: RecordingHandler) -> None:
    h = handler
    _job_info(h, 1, output=["results/a.bam"], log=["logs/a.log"], benchmark=None)
    h._job_finished(log_record(job_id=1), ts=2.0)
    assert _outputs_events(h) == []


def test_state_forgotten_after_finish(handler: RecordingHandler) -> None:
    h = handler
    _job_info(h, 1, output=["s3://b/a.bam (send to storage)"])
    h._handle_remote(log_record(jobid=1), {"schema_version": 1, "external_jobid": "x"}, 1.5)
    h._job_finished(log_record(job_id=1), ts=2.0)
    assert 1 not in h._job_outputs and 1 not in h._job_attempts
    # A duplicate finish record emits nothing further.
    h._job_finished(log_record(job_id=1), ts=3.0)
    assert len(_outputs_events(h)) == 1
