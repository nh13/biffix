"""Event-builder + task_key + remote-adapter tests (no Snakemake needed)."""

from conftest import log_record

from snakemake_logger_plugin_biffix import events
from snakemake_logger_plugin_biffix.remote_adapter import (
    batch_binding,
    batch_image,
    payload_from_record,
)


def test_task_key_is_sorted_and_canonical() -> None:
    assert events.task_key("align", {"sample": "A", "lane": "1"}) == "align:lane=1,sample=A"
    assert events.task_key("all", None) == "all"


def test_task_key_does_not_escape_separators_in_values() -> None:
    # Pins the known, documented limitation (see events.task_key): the format is shared with every
    # other task_key producer and consumer, so a value containing ',' or '=' is joined verbatim
    # and can collide with a different wildcard set.
    assert events.task_key("r", {"a": "1,b=2"}) == "r:a=1,b=2"
    assert events.task_key("r", {"a": "1", "b": "2"}) == "r:a=1,b=2"


def test_task_submitted_shape() -> None:
    e = events.task_submitted(1.0, "r1", "align:sample=A", 1, "job-1")
    assert e["kind"] == "task_submitted"
    assert e["run_id"] == "r1"
    assert e["task_key"] == "align:sample=A"
    assert e["attempt"] == 1
    assert e["batch_job_id"] == "job-1"
    assert e["schema_version"] == 1
    assert "image" not in e
    assert events.task_submitted(1.0, "r1", "t", 1, "j", image="r/i@sha256:ab")["image"] == (
        "r/i@sha256:ab"
    )


def test_engine_task_verdict_shape() -> None:
    v = events.engine_task_verdict(1.0, "r1", "t", attempt=2, category="missing_output", reason="x")
    assert {k: v[k] for k in ("kind", "task_key", "verdict", "attempt", "category", "reason")} == {
        "kind": "engine_task_verdict",
        "task_key": "t",
        "verdict": "failed",
        "attempt": 2,
        "category": "missing_output",
        "reason": "x",
    }
    bare = events.engine_task_verdict(1.0, "r1", "t", reason="")
    assert not {"attempt", "category", "reason"} & bare.keys()
    cut = events.engine_task_verdict(1.0, "r1", "t", reason="y" * (events.REASON_MAX + 5))
    reason = cut["reason"]
    assert isinstance(reason, str)
    assert len(reason) == events.REASON_MAX and reason.endswith("…")


def test_remote_payload_image() -> None:
    assert batch_image({"image": "r/i@sha256:ab"}) == "r/i@sha256:ab"
    assert batch_image({}) is None
    assert batch_image({"image": ""}) is None
    assert batch_image({"image": 7}) is None


def test_engine_task_terminal_shape() -> None:
    s = events.task_succeeded(1.0, "r1", "all")
    assert s["kind"] == "task_succeeded" and s["task_key"] == "all"
    assert "batch_job_id" not in s and "attempt" not in s
    f = events.task_failed(1.0, "r1", "all")
    assert f["kind"] == "task_failed" and f["task_key"] == "all"
    assert "attempt" not in f
    # A retried local rule's terminals carry Snakemake's attempt number.
    assert events.task_succeeded(1.0, "r1", "all", attempt=2)["attempt"] == 2
    assert events.task_failed(1.0, "r1", "all", attempt=1)["attempt"] == 1


def test_run_progress_shape() -> None:
    e = events.run_progress(2.0, "r1", 3, 10)
    assert e["kind"] == "run_progress"
    assert e["completed"] == 3
    assert e["total"] == 10


def test_task_outputs_sorts_dedupes_and_omits_unset_fields() -> None:
    e = events.task_outputs(1.0, "r1", "align:sample=A", ["s3://b/z", "s3://b/a", "s3://b/z"])
    assert e["kind"] == "task_outputs"
    assert e["task_key"] == "align:sample=A"
    assert e["outputs"] == [{"s3_uri": "s3://b/a"}, {"s3_uri": "s3://b/z"}]
    assert "role" not in e and "attempt" not in e and "reused" not in e
    e2 = events.task_outputs(1.0, "r1", "all", ["s3://b/l"], role="log", attempt=2)
    assert e2["role"] == "log" and e2["attempt"] == 2
    assert "reused" not in e2


def test_remote_payload_extraction_and_binding() -> None:
    rec = log_record(
        remote_job_state={"schema_version": 2, "external_jobid": "job-9", "attempt": 2}
    )
    payload = payload_from_record(rec)
    assert payload is not None
    assert batch_binding(payload) == ("job-9", 2)


def test_remote_binding_rejects_unsupported_schema() -> None:
    assert batch_binding({"schema_version": 99, "external_jobid": "x"}) is None


def test_remote_binding_defaults_attempt() -> None:
    assert batch_binding({"schema_version": 1, "external_jobid": "job-1"}) == ("job-1", 1)


def test_remote_binding_coerces_bad_attempt_to_first() -> None:
    # attempt is 1-based; a bool (True == 1 but is not a real attempt), a non-int, and any int < 1
    # all coerce to the first attempt rather than leaking a nonsensical value onto the wire.
    base = {"schema_version": 1, "external_jobid": "job-2"}
    for bad in (True, False, "2", 2.0, 0, -3):
        assert batch_binding({**base, "attempt": bad}) == ("job-2", 1), f"attempt={bad!r}"
    # a genuine multi-attempt value is preserved.
    assert batch_binding({**base, "attempt": 3}) == ("job-2", 3)


def test_remote_binding_requires_external_jobid() -> None:
    # No external_jobid (or a falsy one) ⇒ no binding, whatever the attempt says.
    assert batch_binding({"schema_version": 1, "attempt": 2}) is None
    assert batch_binding({"schema_version": 1, "external_jobid": "", "attempt": 2}) is None


def test_accepts_executor_remote_job_state_payload() -> None:
    """Cross-repo contract guard: the pinned aws-batch executor (``EXECUTOR_SHA`` in
    docker/coordinator/wheels.env) emits its remote state under the ``remote_job_state`` record key,
    built by ``remote_state.build_payload``: ``schema_version`` 1, the ``external_jobid``,
    ``attempt`` = Snakemake's 1-based attempt on every phase, and AWS's own retryStrategy count
    separately as ``batch_attempts``. biffix MUST bind that exact shape; if it stops, child tasks
    never get a ``task_submitted`` binding and their attempts stay 0 (the run goes terminal but the
    task rows never leave ``running``). The Batch-internal count must never be read as the
    attempt."""
    executor_payload = {
        "schema_version": 1,
        "kind": "state",
        "jobid": 7,
        "executor": "aws-batch",
        "phase": "running",
        "remote_status": "RUNNING",
        "external_jobid": "abc-123-def456",
        "region": "us-east-1",
        "queued_at": 1000.0,
        "started_at": 1010.0,
        "queue": "arn:aws:batch:us-east-1:111111111111:job-queue/q",
        "log_stream": "biffix/default/abc",
        "attempt": 2,
        "batch_attempts": 3,
    }
    payload = payload_from_record(log_record(remote_job_state=executor_payload))
    assert payload is not None
    assert batch_binding(payload) == ("abc-123-def456", 2)
