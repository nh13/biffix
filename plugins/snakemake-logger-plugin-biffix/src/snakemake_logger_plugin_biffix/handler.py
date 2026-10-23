# Portions of this module are adapted from snakesee (https://github.com/fg-labs/snakesee), its
# snakemake-logger-plugin-snakesee/src/snakemake_logger_plugin_snakesee/handler.py.
# snakesee is Copyright (c) 2024 Fulcrum Genomics LLC and licensed under the MIT License; see the
# NOTICE section of this package's README.md for its license text.
"""The biffix Snakemake logger plugin handler (Snakemake >= 9.0).

Adapted from snakesee's LogHandler (no snakesee dependency). Maps Snakemake LogEvents to biffix
``EngineEvent``s and dispatches each to two sinks: the ingest POST (live) and the durable
``events.jsonl`` (archival replay copy). It handles WORKFLOW_STARTED, JOB_INFO, JOB_FINISHED,
JOB_ERROR, PROGRESS, RULEGRAPH, the ERROR record of a MissingOutputException (a Batch job that
exited 0 without its outputs, which gets the engine's ``engine_task_verdict``), Snakemake's
"Trying to restart job N." message, and the remote-executor payload channel. A Batch task's
terminal state and per-attempt run/start facts come from AWS EventBridge, not the engine. A
LOCAL rule (run on the coordinator, never submitted to Batch) has no Batch terminal, so its engine
terminal, attempt numbers and retries are emitted here, and every finished job's S3 outputs
(``task_outputs``) are reported before its terminal.
Record-parsing quirks (rule_name vs rule, JOB_FINISHED's ``job_id`` vs ``jobid``) mirror what
snakesee learned from real records.
"""

from __future__ import annotations

import atexit
import logging
import os
import re
import time
from collections.abc import Iterable
from logging import LogRecord
from pathlib import Path
from typing import TypeGuard

from snakemake_interface_logger_plugins.base import LogHandlerBase
from snakemake_interface_logger_plugins.common import LogEvent

from snakemake_logger_plugin_biffix import events
from snakemake_logger_plugin_biffix.poster import MAX_OUTPUTS_PER_EVENT, IngestPoster
from snakemake_logger_plugin_biffix.remote_adapter import (
    batch_binding,
    batch_image,
    payload_from_record,
)
from snakemake_logger_plugin_biffix.settings import DEFAULT_EVENT_FILE, LogHandlerSettings
from snakemake_logger_plugin_biffix.writer import EventWriter

logger = logging.getLogger(__name__)


# The display annotation Snakemake appends to a logged file (snakemake/io/fmt.py `fmt_iofile`):
# " (<label>, <storage phrase>)", either part optional. The storage phrase differs by direction —
# the same S3 object logs as "... (send to storage)" as an output and "... (retrieve from storage)"
# as an input — so it must be stripped for an output to match its consumer's input.
_FILE_ANNOTATION = re.compile(
    r" \((?=[a-z])"
    r"(?:(?P<label>cached|pipe|service|nodelocal|update|before update|access: [^()]*)(?:, )?)?"
    r"(?:retrieve from storage|only provide storage uri|send to storage|in storage)?"
    r"\)$"
)

# The message Snakemake's scheduler logs (a plain record, no `event`) right after a failed job's
# JOB_ERROR when `--retries` will run it again (scheduling/job_scheduler.py `_handle_error`). It is
# the only signal, at failure time, that a retry follows. If upstream rewords it, the retry is
# still announced, just later: at the retried job's re-logged JOB_INFO.
_RESTART_MESSAGE = re.compile(r"Trying to restart job (\d+)\.")

# The start of MissingOutputException's message (snakemake/exceptions.py), which the scheduler
# logs as an ERROR record (`exception="MissingOutputException"`) when a job that exited 0 lacks its
# declared outputs (scheduling/job_scheduler.py `_proceed`, after `job.postprocess`). The record
# carries no jobid field; the message is the only place Snakemake names it. Snakemake writes the id
# only when it is truthy (`if jobid:`), so job 0's reads "Job  completed ..." (no number).
_MISSING_OUTPUT_MESSAGE = re.compile(
    r"Job (?:(\d+) | )completed successfully, but some output files are missing\."
)
# The one other MissingOutputException Snakemake raises without a job id: a `touch()` output that
# does not exist (snakemake/io `_IOFile.touch`), on any job. Its id-less message names no job, so
# it is not taken for job 0's.
_TOUCH_MISSING = "shall be touched but does not exist"


def _as_list(files: object) -> Iterable[object]:
    """A job record's file field as an iterable: a non-empty str becomes a one-item list, None
    or bytes (or an empty str) none; anything else is returned for the caller to iterate."""
    if files is None or isinstance(files, (str, bytes)):
        return [files] if isinstance(files, str) and files else []
    return files if isinstance(files, Iterable) else []


def _texts(files: object) -> list[str]:
    """A job record's file field as display strings; None entries, or a field that cannot be
    iterated, yield none."""
    try:
        return [str(f) for f in _as_list(files) if f is not None]
    except TypeError:
        return []


def _paths(files: object) -> list[str]:
    """A job record's input/output files as bare paths/URIs. Snakemake logs a list of display
    strings (annotated per ``fmt_iofile``); None or anything unexpected yields none."""
    return [_FILE_ANNOTATION.sub("", text) for text in _texts(files) if text]


# Labels whose files never land in storage: a pipe is a FIFO between co-scheduled jobs, a service
# file is a socket/endpoint, and a nodelocal file stays on the node that wrote it.
_NON_STORAGE_LABELS = frozenset({"pipe", "service", "nodelocal"})

# The JOB_INFO record fields that list a job's produced files, in emission order. Each field name
# is also the `task_outputs` role its files are reported under.
_OUTPUT_ROLES: tuple[str, ...] = ("output", "log", "benchmark")


def _output_uris(files: object) -> list[str]:
    """The S3 URIs among a job record's produced files (outputs, logs or a benchmark).

    Only storage-backed files count: a bare path is a coordinator-local file that never reaches
    S3, and a pipe/service/nodelocal file is never sent to storage even under a storage provider.
    The label is read BEFORE stripping, since stripping discards it. ``files`` is a list of display
    strings, a single string (the benchmark), or None."""
    uris: list[str] = []
    for text in _texts(files):
        match = _FILE_ANNOTATION.search(text)
        if match is not None:
            if match.group("label") in _NON_STORAGE_LABELS:
                continue
            text = text[: match.start()]
        if text.startswith("s3://"):
            uris.append(text)
    return uris


# A Snakemake job id: a single job's int, or a group job's UUID string.
JobId = int | str


def _as_jobid(value: object) -> JobId | None:
    """A record's job id field, or None when it is absent (or not a job id at all)."""
    if isinstance(value, bool) or not isinstance(value, int | str):
        return None
    return value


def _is_jobid(jobid: object) -> TypeGuard[int]:
    """A single job's Snakemake id: an int (0 included), never a bool or a group job's UUID."""
    return isinstance(jobid, int) and not isinstance(jobid, bool)


class LogHandler(LogHandlerBase):
    """Emits biffix RunEvents from Snakemake log records."""

    def __post_init__(self) -> None:
        settings = self.settings
        if not isinstance(settings, LogHandlerSettings):
            settings = LogHandlerSettings()

        # run id / ingest wiring / events.jsonl name come from settings or the coordinator's env
        # (a non-empty setting wins).
        self._run_id = settings.run_id or os.environ.get("BIFFIX_RUN_ID", "")
        url = settings.ingest_url or os.environ.get("BIFFIX_INGEST_URL")
        token = settings.ingest_token or os.environ.get("BIFFIX_INGEST_TOKEN")
        self._poster = IngestPoster(url, token)
        # Scrubbed from any Snakemake text an event carries (a verdict's reason).
        self._token = token

        # Relative to the directory Snakemake starts in: Snakemake builds its logger handlers
        # before it applies `--directory`, and the logger interface exposes no workdir, so a
        # `--directory` run still writes the file here. The coordinator runs Snakemake from its
        # work dir and reads the file there (docker/coordinator/lib/events.sh `events_file`).
        event_file = (
            settings.event_file or os.environ.get("BIFFIX_EVENT_FILE") or DEFAULT_EVENT_FILE
        )
        path = Path.cwd() / event_file
        self._writer = EventWriter(path)
        self.baseFilename = str(path)

        # Say why the plugin is (partly) inactive — its Nextflow counterpart logs this, and silent
        # inertness is otherwise indistinguishable from "working" on a run whose events never land.
        # The plugin is loaded only when `--logger biffix` selects it, so this is not noise for
        # other users. The two states are genuinely different: without a run_id NOTHING is
        # emitted, but with a run_id and no ingest URL/token the durable events.jsonl IS still
        # written — only the POST is skipped.
        if not self._run_id:
            logger.warning(
                "biffix logger plugin: inactive — no run id (set BIFFIX_RUN_ID or "
                "--logger-biffix-run-id); no events emitted"
            )
        elif not self._poster.enabled:
            missing = [
                name
                for name, value in (
                    ("BIFFIX_INGEST_URL / --logger-biffix-ingest-url", url),
                    ("BIFFIX_INGEST_TOKEN", token),
                )
                if not value
            ]
            logger.info(
                "biffix logger plugin: run=%s but %s not set — writing local events.jsonl only, "
                "no ingest POST",
                self._run_id,
                " and ".join(missing),
            )
        else:
            logger.info("biffix logger plugin: tracking run=%s -> %s", self._run_id, url)

        self._init_state()
        atexit.register(self._cleanup)

    def _init_state(self) -> None:
        """The per-run record-translation state (every map is keyed by Snakemake jobid)."""
        # jobid → rule name, the fallback rule for a later record that lacks one.
        self._job_rules: dict[JobId, str] = {}
        # jobid → the task_key its JOB_INFO computed, so every later event joins on the same key.
        self._job_task_keys: dict[JobId, str] = {}
        # Output path → the task_key that produces it, for deriving each job's in-run upstream
        # (its inputs' producers). Never pruned: a downstream job may be scheduled long after its
        # producer finished. Snakemake logs a job's JOB_INFO only once its inputs' producers were
        # scheduled, so producers are always recorded first.
        self._output_producers: dict[str, str] = {}
        # (batch_job_id, attempt) pairs already published as task_submitted. The executor emits a
        # remote_job_state record once PER PHASE (queued/running/terminal), so a binding would
        # otherwise be POSTed 3x; the log dedupes them, but publishing once is cheaper.
        self._emitted_submits: set[tuple[str, int]] = set()
        # jobid → role → the S3 URIs the job will produce, captured at JOB_INFO and reported as
        # `task_outputs` at JOB_FINISHED (only a finished job's outputs are known to exist).
        self._job_outputs: dict[JobId, dict[str, list[str]]] = {}
        # jobid → the latest Batch attempt seen for it, so its outputs are credited to the attempt
        # that produced them. Membership is also what marks a jobid as Batch-bound: a jobid that
        # finishes WITHOUT an entry ran locally on the coordinator (an aggregation target, a
        # localrule) and has no Batch terminal, so the engine is its only terminal source.
        self._job_attempts: dict[JobId, int] = {}
        # Local-rule retries (Snakemake's --retries applies to local rules too). A retry keeps
        # the jobid, so these survive _forget_job at JOB_ERROR; JOB_FINISHED drops them.
        # jobid → the local attempt now running, for jobids that failed at least once (else 1).
        self._local_attempts: dict[JobId, int] = {}
        # jobid → (task_key, failed attempt) of a local failure whose retry is not yet announced.
        # An entry for a failure that is never retried stays, bounded by the number of failed
        # jobs.
        self._pending_restarts: dict[JobId, tuple[str, int]] = {}

    # ── interface properties ──
    @property
    def writes_to_stream(self) -> bool:
        return False

    @property
    def writes_to_file(self) -> bool:
        return True

    @property
    def has_filter(self) -> bool:
        return True

    @property
    def has_formatter(self) -> bool:
        return False

    @property
    def needs_rulegraph(self) -> bool:
        # Request the RULEGRAPH log event so we can emit the run's group-level rule graph (the DAG
        # view). Cheap: Snakemake logs one RULEGRAPH record per run.
        return True

    # ── emit ──
    def emit(self, record: LogRecord) -> None:
        # A logging handler must never break the workflow: any parse/sink failure is logged and
        # swallowed (the poster already self-guards; this also covers writer I/O and record
        # parsing such as int() on a non-numeric progress field).
        try:
            self._emit(record)
        except Exception:  # noqa: BLE001 — defensive: logging must not raise into Snakemake
            logger.warning("biffix logger plugin: emit failed (swallowed)", exc_info=True)

    def _emit(self, record: LogRecord) -> None:
        # No run_id ⇒ no event can be attributed: emit nothing (rather than orphaned events
        # carrying run_id=""), and do not create or wipe a file that will never be written.
        if not self._run_id:
            return
        ts = time.time()

        # The remote-executor payload channel carries the AWS Batch job id → publish the binding.
        payload = payload_from_record(record)
        if payload is not None:
            self._handle_remote(record, payload, ts)
            return

        event = getattr(record, "event", None)
        if event is None:
            # Only a local failure awaiting its retry makes a plain message worth parsing.
            if self._pending_restarts:
                self._maybe_restart(record, ts)
            return
        if event == LogEvent.WORKFLOW_STARTED:
            self._workflow_started(ts)
        elif event == LogEvent.JOB_INFO:
            self._job_info(record, ts)
        elif event == LogEvent.JOB_FINISHED:
            self._job_finished(record, ts)
        elif event == LogEvent.JOB_ERROR:
            self._job_error(record, ts)
        elif event == LogEvent.PROGRESS:
            self._progress(record, ts)
        elif event == LogEvent.RULEGRAPH:
            self._rulegraph(record, ts)
        elif event == LogEvent.ERROR:
            self._error(record, ts)

    # ── dispatch to both sinks ──
    def _dispatch(self, event: dict[str, object]) -> None:
        try:
            self._writer.write(event)
        except OSError:
            logger.warning("biffix logger plugin: event write failed (swallowed)", exc_info=True)
        self._poster.post(event)

    # ── record parsing (adapted from snakesee) ──
    def _extract_wildcards(self, record: LogRecord) -> dict[str, str] | None:
        wc = getattr(record, "wildcards", None)
        if wc is None:
            return None
        if hasattr(wc, "__dict__"):
            return {k: str(v) for k, v in vars(wc).items() if not k.startswith("_")}
        if isinstance(wc, dict):
            return {str(k): str(v) for k, v in wc.items()}
        return None

    def _record_rule(self, record: LogRecord, jobid: JobId | None) -> str | None:
        """The record's own rule name, else the one remembered for its jobid, else None."""
        rn = getattr(record, "rule_name", None) or getattr(record, "rule", None)
        if rn is None and jobid is not None:
            return self._job_rules.get(jobid)
        return None if rn is None else str(rn)

    def _workflow_started(self, ts: float) -> None:
        # A truncate failure (e.g. an unwritable cwd) must not also cost the live run_started,
        # matching _dispatch's tolerance of a failed write.
        try:
            self._writer.truncate()
        except OSError:
            logger.warning("biffix logger plugin: events.jsonl reset failed", exc_info=True)
        self._dispatch(events.run_started(ts, self._run_id))

    def _job_info(self, record: LogRecord, ts: float) -> None:
        jobid = _as_jobid(getattr(record, "jobid", None))
        rule = self._record_rule(record, jobid) or "unknown"
        task_key = events.task_key(rule, self._extract_wildcards(record))
        if jobid is not None:
            self._job_rules[jobid] = rule
            self._job_task_keys[jobid] = task_key
            # A retried local job re-logs JOB_INFO; announce its retry now if the scheduler's
            # restart message was not seen.
            self._announce_local_retry(jobid, ts)
        # Upstream = the in-run tasks that produce this job's inputs. An input no job in this run
        # produces (a source file, or an output left by an earlier run) has no producer — correctly
        # no edge.
        deps = [
            self._output_producers[path]
            for path in _paths(getattr(record, "input", None))
            if path in self._output_producers
        ]
        for path in _paths(getattr(record, "output", None)):
            self._output_producers[path] = task_key
        if jobid is not None:
            # A retry re-logs JOB_INFO for the same jobid; overwriting keeps the latest file set.
            self._job_outputs[jobid] = {
                role: uris
                for role in _OUTPUT_ROLES
                if (uris := _output_uris(getattr(record, role, None)))
            }
        # `group` = the rule name — the DAG node this task rolls up onto.
        self._dispatch(events.task_created(ts, self._run_id, task_key, group=rule, deps=deps))

    def _rulegraph(self, record: LogRecord, ts: float) -> None:
        """Emit the run's group-level rule graph from the RULEGRAPH log record. Snakemake's record
        carries ``rulegraph = {"nodes": [{"rule": name}], "links": [{"sourcerule", "targetrule"}]}``
        (workflow.log_rulegraph); links are upstream→downstream, matching our edge direction."""
        rg = getattr(record, "rulegraph", None)
        if not isinstance(rg, dict):
            return
        nodes = [n["rule"] for n in rg.get("nodes", []) if isinstance(n, dict) and n.get("rule")]
        edges = [
            (link["sourcerule"], link["targetrule"])
            for link in rg.get("links", [])
            if isinstance(link, dict) and link.get("sourcerule") and link.get("targetrule")
        ]
        if nodes or edges:
            self._dispatch(events.run_dag(ts, self._run_id, nodes, edges))

    def _handle_remote(self, record: LogRecord, payload: dict[str, object], ts: float) -> None:
        """A remote submit carries the AWS Batch job id — bind it via task_submitted."""
        binding = batch_binding(payload)
        if binding is None:
            return
        batch_job_id, attempt = binding
        # Use the SAME jobid the created-side keyed its maps under (the record's jobid, per
        # _job_info), falling back to the payload's only if the record lacks it. Reading
        # payload["jobid"] alone silently orphaned the binding when the payload omitted that key,
        # producing a task_submitted task_key that never matched the earlier task_created.
        jobid = _as_jobid(getattr(record, "jobid", None))
        if jobid is None:
            jobid = _as_jobid(payload.get("jobid"))
        # Only single jobs (int ids) have the rule/wildcards needed to build a task_key; a group job
        # carries a UUID string id and would be published as a bogus task. Group jobs are out of
        # scope, so skip them rather than invent a task.
        if not _is_jobid(jobid):
            return
        # The key JOB_INFO computed; else (its JOB_INFO was never recorded) the record's own rule
        # with no wildcards. With no rule at all there is no task to bind: publish nothing and
        # record nothing, so a later record for this jobid can still bind once the rule is known.
        task_key = self._job_task_keys.get(jobid)
        if task_key is None:
            rule = self._record_rule(record, jobid)
            if rule is None:
                return
            task_key = events.task_key(rule, None)
        # This jobid ran on Batch — remember it (idempotently) so _job_finished doesn't mistake it
        # for a local rule. The queued-phase record arrives well before JOB_FINISHED, so the
        # binding is recorded first even for a fast job. Keep the highest attempt: phase records
        # of an older attempt may still trail in.
        self._job_attempts[jobid] = max(attempt, self._job_attempts.get(jobid, attempt))
        # Publish the binding once per (job, attempt): the record fires per phase transition.
        submit_key = (batch_job_id, attempt)
        if submit_key in self._emitted_submits:
            return
        self._emitted_submits.add(submit_key)
        if attempt > 1:
            self._dispatch(events.task_retrying(ts, self._run_id, task_key, attempt - 1, attempt))
        self._dispatch(
            events.task_submitted(
                ts, self._run_id, task_key, attempt, batch_job_id, image=batch_image(payload)
            )
        )

    def _error(self, record: LogRecord, ts: float) -> None:
        """A Batch job Snakemake failed although it exited 0 (its declared outputs are missing):
        AWS reports it SUCCEEDED, so emit the engine's verdict, which fails the task anyway.

        Only a job with a Batch binding gets one (a local job's failure is not AWS's to report).
        No retry is announced here: the scheduler's "Trying to restart job N." message follows,
        and the retry's own Batch binding announces it (a new attempt), as for any Batch retry."""
        if getattr(record, "exception", None) != "MissingOutputException":
            return
        msg = getattr(record, "msg", None)
        if not isinstance(msg, str):
            return
        match = _MISSING_OUTPUT_MESSAGE.search(msg)
        if match is None:
            return
        if match.group(1) is None and _TOUCH_MISSING in msg:
            return
        jobid = int(match.group(1) or 0)
        attempt = self._job_attempts.get(jobid)
        tkey = self._task_key_for(jobid)
        if attempt is None or tkey is None:
            return
        reason = msg[match.start() :].strip()
        if self._token:
            reason = reason.replace(self._token, "<token>")
        self._dispatch(
            events.engine_task_verdict(
                ts, self._run_id, tkey, attempt=attempt, category="missing_output", reason=reason
            )
        )

    def _task_key_for(self, jobid: JobId) -> str | None:
        """The task_key JOB_INFO recorded for a jobid (Batch or local), or None when it was never
        recorded (no task_key can be built)."""
        if not _is_jobid(jobid):
            return None
        return self._job_task_keys.get(jobid)

    def _local_task_key(self, jobid: JobId) -> str | None:
        """The task_key of a jobid that ran LOCALLY (never got a Batch binding), or None.

        Returns None for a Batch job (its terminal is authoritative from EventBridge, not the
        engine) or when the rule was never recorded (can't build a task_key). This is the guard
        that keeps a local rule's engine terminal from double-reporting a Batch task."""
        return None if jobid in self._job_attempts else self._task_key_for(jobid)

    def _emit_task_outputs(self, jobid: JobId, ts: float) -> None:
        """Report a finished job's S3 outputs, one `task_outputs` per role (in chunks of at most
        MAX_OUTPUTS_PER_EVENT refs). Unlike the terminal, this is emitted for Batch AND local jobs:
        EventBridge knows a Batch job ended but not which objects it wrote, so the engine is the
        only source of the task → output mapping."""
        tkey = self._task_key_for(jobid)
        if tkey is None:
            return
        attempt = self._job_attempts.get(jobid)
        for role, uris in self._job_outputs.get(jobid, {}).items():
            unique = sorted(set(uris))
            for at in range(0, len(unique), MAX_OUTPUTS_PER_EVENT):
                chunk = unique[at : at + MAX_OUTPUTS_PER_EVENT]
                self._dispatch(
                    events.task_outputs(ts, self._run_id, tkey, chunk, role=role, attempt=attempt)
                )

    def _forget_job(self, jobid: JobId) -> None:
        """Drop a jobid's per-attempt state. The local-retry maps are NOT dropped here: a failed
        local job may be retried under the same jobid (see _job_finished)."""
        self._job_rules.pop(jobid, None)
        self._job_task_keys.pop(jobid, None)
        self._job_outputs.pop(jobid, None)
        self._job_attempts.pop(jobid, None)

    def _job_finished(self, record: LogRecord, ts: float) -> None:
        # Explicit None checks — a valid job id of 0 must not be treated as absent (which a
        # `job_id or jobid` truthiness fallback would do), or its map entries leak.
        jobid = _as_jobid(getattr(record, "job_id", None))
        if jobid is None:
            jobid = _as_jobid(getattr(record, "jobid", None))
        if jobid is None:
            return  # no job to report or forget
        # A Batch task's terminal is authoritative from EventBridge, so nothing is emitted for it
        # here. A LOCAL rule (never submitted to Batch — e.g. an aggregation target) has no Batch
        # terminal, so the engine is its only terminal source: emit an engine task_succeeded so its
        # task row can leave "running". Outputs go first so they are recorded before the terminal.
        self._emit_task_outputs(jobid, ts)
        tkey = self._local_task_key(jobid)
        if tkey is not None:
            attempt = self._local_attempts.get(jobid, 1)
            self._dispatch(events.task_succeeded(ts, self._run_id, tkey, attempt=attempt))
        self._forget_job(jobid)
        self._local_attempts.pop(jobid, None)
        self._pending_restarts.pop(jobid, None)

    def _job_error(self, record: LogRecord, ts: float) -> None:
        jobid = _as_jobid(getattr(record, "jobid", None))
        if jobid is None:
            return  # no job to report or forget
        # Same local-rule rationale as _job_finished, for the failure path. No task_outputs: a
        # failed job's declared outputs are not known to exist (Snakemake deletes partial ones).
        # JOB_ERROR is logged for EVERY failed attempt, including one --retries will re-run, and
        # carries no attempt number: the attempt is counted here, and the retry, if any, is
        # announced by _maybe_restart (or the retry's JOB_INFO), so the reducer shows the gap as
        # a retry rather than a failure.
        tkey = self._local_task_key(jobid)
        if tkey is not None:
            attempt = self._local_attempts.get(jobid, 1)
            self._dispatch(events.task_failed(ts, self._run_id, tkey, attempt=attempt))
            self._pending_restarts[jobid] = (tkey, attempt)
        self._forget_job(jobid)

    def _maybe_restart(self, record: LogRecord, ts: float) -> None:
        """Announce a local retry from Snakemake's "Trying to restart job N." message."""
        msg = getattr(record, "msg", None)
        if not isinstance(msg, str):
            return
        match = _RESTART_MESSAGE.fullmatch(msg.strip())
        if match is not None:
            self._announce_local_retry(int(match.group(1)), ts)

    def _announce_local_retry(self, jobid: JobId, ts: float) -> None:
        """Emit task_retrying(n → n+1) for a local failure awaiting its retry, once."""
        pending = self._pending_restarts.pop(jobid, None)
        if pending is None:
            return
        tkey, failed = pending
        self._local_attempts[jobid] = failed + 1
        self._dispatch(events.task_retrying(ts, self._run_id, tkey, failed, failed + 1))

    def _progress(self, record: LogRecord, ts: float) -> None:
        done = getattr(record, "done", 0)
        total = getattr(record, "total", 0)
        self._dispatch(events.run_progress(ts, self._run_id, int(done), int(total)))

    # ── lifecycle ──
    def close(self) -> None:
        self._cleanup()

    def _cleanup(self) -> None:
        if getattr(self, "_writer", None) is not None:
            try:
                self._writer.close()
            except Exception:  # noqa: BLE001 — logging must not raise into Snakemake
                logger.warning(
                    "biffix logger plugin: events.jsonl close failed (swallowed)", exc_info=True
                )
        # Drain the batched ingest POSTs; close() is idempotent, so the atexit re-run is harmless.
        if getattr(self, "_poster", None) is not None:
            try:
                self._poster.close()
            except Exception:  # noqa: BLE001 — logging must not raise into Snakemake
                logger.warning(
                    "biffix logger plugin: ingest close failed (swallowed)", exc_info=True
                )
