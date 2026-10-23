"""Emitted engine events must conform to the checked-in wire schema — EXHAUSTIVELY.

The producer-conformance guard: the plugin binds to the schema biffix-core
emits (``schemas/wire.engine.schema.json``). Every ``kind`` the schema defines must either have a
builder sample here or be listed in ``NOT_EMITTED`` with the reason this plugin never sends it, so
a kind added to the schema fails the test until someone classifies it. Each sample must validate
against the schema AND use only property names the schema declares for its kind: the schema
tolerates unknown fields (additive-only contract), so validation alone would miss a misspelt or
renamed optional field. The same checks run over the events the handler actually dispatches for a
recorded sequence of Snakemake log records.

The schema file must exist (this test lives in the biffix repo next to it); a missing file fails
rather than skips.
"""

import json
from pathlib import Path
from typing import Any

import jsonschema
from conftest import RecordingHandler, log_record
from snakemake_interface_logger_plugins.common import LogEvent

from snakemake_logger_plugin_biffix import events

SCHEMA = Path(__file__).resolve().parents[3] / "schemas" / "wire.engine.schema.json"

# Schema kinds this plugin never emits, with the reason.
NOT_EMITTED = {
    "run_succeeded": "the coordinator wrapping the Snakemake invocation emits the run terminal",
    "run_failed": "the coordinator wrapping the Snakemake invocation emits the run terminal",
    "task_cached": "Snakemake's log records carry no cache-hit signal",
}


def _load_schema() -> dict[str, Any]:
    assert SCHEMA.exists(), f"engine wire schema not found at {SCHEMA}"
    schema: dict[str, Any] = json.loads(SCHEMA.read_text())
    return schema


def _variants(schema: dict[str, Any]) -> dict[str, dict[str, Any]]:
    """Each `kind`'s schema variant. schemars renders the tagged enum as `oneOf` variants each
    pinning its tag with `properties.kind.const == "<kind>"` (or `enum: ["<kind>"]`); collect them
    wherever they appear."""
    found: dict[str, dict[str, Any]] = {}

    def walk(node: object) -> None:
        if isinstance(node, dict):
            props = node.get("properties")
            if isinstance(props, dict):
                kind_prop = props.get("kind")
                if isinstance(kind_prop, dict) and "const" in kind_prop:
                    found[str(kind_prop["const"])] = node
                elif isinstance(kind_prop, dict) and isinstance(kind_prop.get("enum"), list):
                    for v in kind_prop["enum"]:
                        found[str(v)] = node
            for value in node.values():
                walk(value)
        elif isinstance(node, list):
            for item in node:
                walk(item)

    walk(schema)
    return found


def _check(
    sample: dict[str, Any], schema: dict[str, Any], variants: dict[str, dict[str, Any]]
) -> None:
    """Validate one event, and reject any property its kind's schema does not declare."""
    jsonschema.validate(sample, schema)
    kind = sample["kind"]
    assert kind in variants, f"{kind!r} is not a schema kind"
    declared = set(schema.get("properties", {})) | set(variants[kind].get("properties", {}))
    undeclared = set(sample) - declared
    assert not undeclared, f"{kind}: properties the schema does not declare: {sorted(undeclared)}"


# One sample per emitted kind, with every optional field set, so each field name is checked.
def _samples() -> dict[str, dict[str, Any]]:
    return {
        "run_started": events.run_started(1.0, "r1"),
        "run_progress": events.run_progress(1.0, "r1", 1, 3),
        "run_dag": events.run_dag(1.0, "r1", ["a", "b"], [("a", "b")]),
        "task_created": events.task_created(
            1.0, "r1", "merge", group="merge", deps=["align:sample=A", "align:sample=B"]
        ),
        "task_submitted": events.task_submitted(
            1.0, "r1", "align:sample=A", 1, "job-1", image="r.example/wf/align@sha256:" + "a" * 64
        ),
        "engine_task_verdict": events.engine_task_verdict(
            1.0, "r1", "align:sample=A", attempt=1, category="missing_output", reason="missing"
        ),
        "task_succeeded": events.task_succeeded(1.0, "r1", "all", attempt=2),
        "task_failed": events.task_failed(1.0, "r1", "all", attempt=1),
        "task_retrying": events.task_retrying(1.0, "r1", "align:sample=A", 1, 2),
        "task_outputs": events.task_outputs(
            1.0, "r1", "align:sample=A", ["s3://b/o.bam", "s3://b/o.bai"], role="output", attempt=1
        ),
    }


def test_every_schema_kind_is_classified_and_every_sample_conforms() -> None:
    schema = _load_schema()
    variants = _variants(schema)
    samples = _samples()

    # Exhaustiveness: every schema kind is either emitted (sampled) or explicitly not emitted.
    schema_kinds = set(variants)
    assert schema_kinds, "failed to extract any kinds from the schema"
    classified = samples.keys() | NOT_EMITTED.keys()
    missing = schema_kinds - classified
    assert not missing, f"schema kinds neither sampled nor in NOT_EMITTED: {sorted(missing)}"
    extra = classified - schema_kinds
    assert not extra, f"kinds the schema does not define: {sorted(extra)}"
    assert not samples.keys() & NOT_EMITTED.keys(), "a kind is both sampled and NOT_EMITTED"

    for kind, sample in samples.items():
        assert sample["kind"] == kind
        _check(sample, schema, variants)


def test_events_the_handler_dispatches_conform(handler: RecordingHandler) -> None:
    # A local rule retried once, a Batch job retried once, and the run-level records.
    schema = _load_schema()
    variants = _variants(schema)
    h = handler
    wc = {"sample": "A"}
    records = [
        log_record(event=LogEvent.WORKFLOW_STARTED),
        log_record(
            event=LogEvent.RULEGRAPH,
            rulegraph={"nodes": [{"rule": "align"}, {"rule": "all"}], "links": []},
        ),
        log_record(
            event=LogEvent.JOB_INFO,
            jobid=1,
            rule_name="align",
            wildcards=wc,
            output=["s3://b/a.bam (send to storage)"],
            log=["s3://b/a.log (send to storage)"],
        ),
        log_record(
            remote_job_state={
                "schema_version": 1,
                "external_jobid": "j1",
                "attempt": 1,
                "image": "r.example/wf/align@sha256:" + "a" * 64,
            },
            jobid=1,
        ),
        log_record(
            event=LogEvent.ERROR,
            exception="MissingOutputException",
            msg="MissingOutputException in rule align:\nJob 1 completed successfully, but some "
            "output files are missing. Missing files after 5 seconds:\ns3://b/a.bam",
        ),
        log_record(msg="Trying to restart job 1."),
        log_record(
            event=LogEvent.JOB_INFO,
            jobid=1,
            rule_name="align",
            wildcards=wc,
            output=["s3://b/a.bam (send to storage)"],
        ),
        log_record(
            remote_job_state={"schema_version": 1, "external_jobid": "j2", "attempt": 2}, jobid=1
        ),
        log_record(event=LogEvent.JOB_FINISHED, job_id=1),
        log_record(
            event=LogEvent.JOB_INFO, jobid=0, rule_name="all", input=["s3://b/a.bam"], wildcards={}
        ),
        log_record(event=LogEvent.JOB_ERROR, jobid=0),
        log_record(msg="Trying to restart job 0."),
        log_record(event=LogEvent.JOB_INFO, jobid=0, rule_name="all", wildcards={}),
        log_record(event=LogEvent.JOB_FINISHED, job_id=0),
        log_record(event=LogEvent.PROGRESS, done=2, total=2),
    ]
    for record in records:
        h.emit(record)
    kinds = {str(e["kind"]) for e in h._dispatched}
    assert kinds == set(_samples()), f"sequence did not cover every emitted kind: {sorted(kinds)}"
    for event in h._dispatched:
        _check(event, schema, variants)
