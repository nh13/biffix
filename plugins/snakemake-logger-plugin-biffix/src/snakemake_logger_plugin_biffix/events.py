"""The biffix ``EngineEvent`` wire shape.

These builders produce dicts that validate against ``schemas/wire.engine.schema.json`` (the
artifact ``biffix-core`` emits). The Rust ``wire::engine`` enum is the source of truth; this
module binds to it as a wire protocol. ``kind`` is the tag; ``task_key`` is
``rule:sorted(wildcards)`` (canonical, order-independent).

There is a builder for every kind the Snakemake handler emits: ``run_started``, ``run_progress``,
``run_dag``, ``task_created``, ``task_submitted``, ``task_retrying``, ``task_succeeded``,
``task_failed``, ``task_outputs``, ``engine_task_verdict``. The other schema kinds are not emitted
by this plugin:
``run_succeeded``/``run_failed`` (the coordinator that wraps the Snakemake invocation emits the run
terminal) and ``task_cached`` (the logger records carry no cache-hit signal: the cache decision
happens inside the executor, decoupled from the log events, so a cached rule is reported as
``task_succeeded``). The conformance guard (``test_schema_conformance.py``) makes every schema kind
either have a builder here or be listed there as not emitted, so a new kind cannot slip through.
"""

from __future__ import annotations

SCHEMA_VERSION = 1


def task_key(rule: str, wildcards: dict[str, str] | None) -> str:
    """Canonical stable logical-task identity: ``rule:sorted(wildcards)``.

    Known limitation: names and values are joined without escaping, so a wildcard value that
    itself contains ``,`` or ``=`` can collide with a different wildcard set (``{"a": "1,b=2"}``
    and ``{"a": "1", "b": "2"}`` both give ``r:a=1,b=2``). The format is shared with every other
    task_key producer and consumer (already-stored keys, the joins on them, the backend's
    rule-from-key parse), so it is kept as is, an accepted limitation until a real workflow hits
    it (escaping would be a contract change every producer and consumer adopts together);
    ``tests/test_events.py`` pins it. Snakemake's default wildcard regex allows these characters,
    so a workflow that needs distinct tasks for such values should constrain its wildcards."""
    if not wildcards:
        return rule
    inner = ",".join(f"{k}={wildcards[k]}" for k in sorted(wildcards))
    return f"{rule}:{inner}"


def _envelope(ts: float, run_id: str, body: dict[str, object]) -> dict[str, object]:
    return {"schema_version": SCHEMA_VERSION, "ts": ts, "run_id": run_id, **body}


def run_started(ts: float, run_id: str) -> dict[str, object]:
    return _envelope(ts, run_id, {"kind": "run_started"})


def run_progress(ts: float, run_id: str, completed: int, total: int) -> dict[str, object]:
    return _envelope(ts, run_id, {"kind": "run_progress", "completed": completed, "total": total})


def task_created(
    ts: float,
    run_id: str,
    tkey: str,
    group: str | None = None,
    deps: list[str] | None = None,
) -> dict[str, object]:
    body: dict[str, object] = {"kind": "task_created", "task_key": tkey}
    if group is not None:
        # The DAG node (rule) this task rolls up onto, for the run's graph view.
        body["group"] = group
    if deps:
        # The in-run tasks that produced this task's inputs (its upstream), sorted for stability.
        body["deps"] = sorted(set(deps))
    return _envelope(ts, run_id, body)


def run_dag(
    ts: float, run_id: str, nodes: list[str], edges: list[tuple[str, str]]
) -> dict[str, object]:
    """The run's group-level rule graph, emitted once. Folds as a set-union in the reducer."""
    return _envelope(
        ts,
        run_id,
        {
            "kind": "run_dag",
            "dag": {
                "nodes": [{"id": n} for n in nodes],
                "edges": [{"from": f, "to": t} for f, t in edges],
            },
        },
    )


def task_submitted(
    ts: float,
    run_id: str,
    tkey: str,
    attempt: int,
    batch_job_id: str,
    image: str | None = None,
) -> dict[str, object]:
    """An attempt submitted to Batch: binds ``batch_job_id`` to the task. ``image`` is the
    container the Batch job runs, as the executor reports it; omitted when unknown."""
    body: dict[str, object] = {
        "kind": "task_submitted",
        "task_key": tkey,
        "attempt": attempt,
        "batch_job_id": batch_job_id,
    }
    if image:
        body["image"] = image
    return _envelope(ts, run_id, body)


# The longest ``reason`` an ``engine_task_verdict`` carries; a longer one is cut.
REASON_MAX = 1000


def engine_task_verdict(
    ts: float,
    run_id: str,
    tkey: str,
    attempt: int | None = None,
    category: str | None = None,
    reason: str | None = None,
) -> dict[str, object]:
    """Snakemake failed a Batch attempt that AWS may report succeeded (its declared outputs are
    missing after the job exited 0): ``verdict: failed``, which outranks AWS's success for the
    task. ``attempt`` is the one its ``task_submitted`` carried; ``category`` is the open
    ``TerminationCategory`` vocabulary (``missing_output``, ...); ``reason`` is Snakemake's message,
    cut to ``REASON_MAX`` characters. Absent fields are omitted."""
    body: dict[str, object] = {"kind": "engine_task_verdict", "task_key": tkey, "verdict": "failed"}
    if attempt is not None:
        body["attempt"] = attempt
    if category is not None:
        body["category"] = category
    if reason:
        body["reason"] = reason if len(reason) <= REASON_MAX else reason[: REASON_MAX - 1] + "…"
    return _envelope(ts, run_id, body)


def _terminal(
    ts: float, run_id: str, kind: str, tkey: str, attempt: int | None
) -> dict[str, object]:
    body: dict[str, object] = {"kind": kind, "task_key": tkey}
    if attempt is not None:
        body["attempt"] = attempt
    return _envelope(ts, run_id, body)


def task_succeeded(
    ts: float, run_id: str, tkey: str, attempt: int | None = None
) -> dict[str, object]:
    """Engine-reported success for a LOCAL rule (run on the coordinator, never submitted to Batch,
    so it has no Batch terminal). Carries no batch_job_id; the reducer records it as a task-level
    engine terminal. ``attempt`` is Snakemake's attempt number for the rule (it retries local rules
    under ``--retries``): it puts each attempt's terminal under its own dedup key. Batch tasks get
    their terminal from EventBridge, not this."""
    return _terminal(ts, run_id, "task_succeeded", tkey, attempt)


def task_failed(ts: float, run_id: str, tkey: str, attempt: int | None = None) -> dict[str, object]:
    """Engine-reported failure for a LOCAL rule (see [`task_succeeded`]). With ``attempt`` set, a
    retried rule's second failure is not dropped as a redelivery of the first, and the reducer can
    tell the failure a retry replaces from the retry's own failure."""
    return _terminal(ts, run_id, "task_failed", tkey, attempt)


def task_retrying(
    ts: float, run_id: str, tkey: str, prev_attempt: int, next_attempt: int
) -> dict[str, object]:
    return _envelope(
        ts,
        run_id,
        {
            "kind": "task_retrying",
            "task_key": tkey,
            "prev_attempt": prev_attempt,
            "next_attempt": next_attempt,
        },
    )


def task_outputs(
    ts: float,
    run_id: str,
    tkey: str,
    outputs: list[str],
    role: str | None = None,
    attempt: int | None = None,
) -> dict[str, object]:
    """The set of S3 objects one task attempt produced in one ``role`` (``output``, ``log``,
    ``benchmark``, ...; absent ⇒ ``output``). ``outputs`` are S3 URIs, emitted sorted and
    de-duplicated so the same set always serializes identically (the reducer folds it as a
    set-union keyed on ``s3_uri``).

    Never sets ``reused``: Snakemake's logger records carry no cache-hit signal, so the plugin
    cannot claim outputs came from a previous run."""
    body: dict[str, object] = {
        "kind": "task_outputs",
        "task_key": tkey,
        "outputs": [{"s3_uri": uri} for uri in sorted(set(outputs))],
    }
    if role is not None:
        body["role"] = role
    if attempt is not None:
        body["attempt"] = attempt
    return _envelope(ts, run_id, body)
