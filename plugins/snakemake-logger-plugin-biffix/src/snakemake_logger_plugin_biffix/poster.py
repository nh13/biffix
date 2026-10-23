"""POST biffix EngineEvents to the ingest route (the live path).

Uses only the stdlib (``urllib``) so the plugin has no HTTP dependency in the coordinator image.
Best-effort: a failed POST is logged and swallowed — the durable ``events.jsonl`` is the
archival backstop (the coordinator replays it at exit), and EventBridge is the tracking backbone,
so ingest loss never breaks a run.

Events are buffered and POSTed in batches by a background thread: a batch goes out once
``max_batch`` events are waiting (the route's cap) or ``flush_interval`` seconds after the previous
send, and :meth:`IngestPoster.close` drains whatever is left. One request per event made a
5,000-task DAG's start-up a request storm and blocked Snakemake's logging thread on every POST.

Delivery rules:

- A batch is bounded by event count (``max_batch``) AND by serialized size
  (:data:`MAX_BATCH_BYTES`), since the route answers 413 on either.
- A 413 or a 400 rejects the whole batch, so the batch is halved and each half resent until the
  offending event is alone; only that one event is then dropped (logged by kind and task key).
  401/403 apply to the whole token or run, so they drop the batch without splitting; a 401 (the
  token is invalid or expired) also stops every later send, as nf-biffix's poster does.
- A redirect is never followed (urllib would re-send the POST as a GET without its body, whose 2xx
  reads as delivered): a 3xx drops the batch as any other final failure.
- A 2xx can still refuse single events (a ``task_submitted`` whose Batch job claim the route could
  not verify): the answer's ``refused`` names each by its index in the batch, and the rest landed.
  A resend would be refused again, so each is logged (by kind, task key and reason) and dropped.
- Transient failures (connection errors, timeouts, protocol errors, 408, 429, 5xx) are retried
  with doubling backoff, or the response's ``Retry-After``. Nothing a send raises can kill the
  background flusher.
- :meth:`IngestPoster.close` has an overall deadline, and once a batch at close has exhausted its
  retries (ingest is unreachable) the rest of the drain is abandoned rather than paying the retry
  cost batch after batch. Events sent after close go out on a background thread, never on the
  caller's.
"""

from __future__ import annotations

import enum
import json
import logging
import threading
import time
import urllib.error
import urllib.parse
import urllib.request
from types import TracebackType
from typing import Protocol

logger = logging.getLogger(__name__)

# The route's limits and this delivery policy are defined once, in biffix-core's `ingest_limits`,
# and written to `schemas/ingest-limits.json`; `tests/test_ingest_limits.py` fails if a value here
# differs from that file. Change them there first.
#
# The ingest route rejects a larger batch with 413.
MAX_BATCH = 500
# The route rejects a body over 5 MiB with 413. An event that alone is larger can never be
# delivered and is dropped without being sent.
ROUTE_MAX_BODY_BYTES = 5 * 1024 * 1024
# Batches are cut below the route's limit, leaving room for the envelope and for the Lambda
# payload limit (6 MB) should the body be base64-encoded on the way (4 MiB * 4/3 < 6 MB).
MAX_BATCH_BYTES = 4 * 1024 * 1024
# Events held while ingest is unreachable; beyond this, events are dropped (events.jsonl and the
# coordinator's replay still carry them). See KEEP_UNDER_PRESSURE for which ones go first.
MAX_BUFFERED = 20_000
# Attempts per batch for a transient failure (connection error, 408, 429, 5xx), with doubling
# backoff or the response's Retry-After.
MAX_ATTEMPTS = 3
# Per-request timeout (seconds).
REQUEST_TIMEOUT = 5.0
# The longest a batch waits for more events before it is sent (seconds).
FLUSH_INTERVAL = 1.0
# The first retry's backoff, doubling per attempt (seconds).
RETRY_BACKOFF = 0.5
# The longest a Retry-After is honoured for (seconds); the close deadline still cuts it short.
MAX_RETRY_AFTER = 30.0
# The overall time budget of close(): the flusher join plus the final drain.
CLOSE_TIMEOUT = 30.0
# At most one "buffer full" warning per this many seconds (with the running total).
DROP_WARN_INTERVAL = 30.0
# POSTs one batch may spend isolating the events the route rejects (413/400) by halving it. One bad
# event in 500 costs about 19; a route that rejects everything (a contract mismatch) stops here
# instead of costing ~2 requests per event.
SPLIT_POST_BUDGET = 64
# The most output refs one task_outputs event carries; a task's outputs beyond it go in further
# events (each its own fact, which the fold unions), so a huge glob output never makes one event
# over the route's body limit, dropped whole.
MAX_OUTPUTS_PER_EVENT = 1000
# When the buffer overflows, these kinds are kept and the oldest of the others dropped first:
# `task_submitted` carries the batch_job_id binding that joins the AWS Batch (EventBridge) stream
# to the engine's, and the run/task state transitions are small and are what would otherwise leave
# a task or run stuck in the UI until the exit replay (engine_task_verdict is one: it fails an
# attempt AWS reports SUCCEEDED). The bulky, descriptive events (task_created with its deps,
# run_dag, task_outputs, run_progress, which the next one supersedes) go first. If the buffer holds
# nothing but these kinds, the oldest of them are dropped.
KEEP_UNDER_PRESSURE = frozenset(
    {
        "task_submitted",
        "task_succeeded",
        "task_failed",
        "task_retrying",
        "task_cached",
        "run_started",
        "run_succeeded",
        "run_failed",
        "engine_task_verdict",
    }
)
# The most of a 2xx answer read: the summary, whose `refused` lists at most one entry per event.
MAX_ANSWER_BYTES = 1024 * 1024
# An overflow trims the buffer to this fraction of MAX_BUFFERED, so a full buffer is not re-scanned
# on every later post.
_TRIM_TO = 0.9
# The envelope the route expects: `IngestRequest { events: [...] }`.
_ENVELOPE_HEAD = b'{"events":['
_ENVELOPE_TAIL = b"]}"
_ENVELOPE_BYTES = len(_ENVELOPE_HEAD) + len(_ENVELOPE_TAIL)


class _NoRedirect(urllib.request.HTTPRedirectHandler):
    """Raise a 3xx as an HTTPError rather than follow it (see the module docs)."""

    # urllib passes (req, fp, code, msg, headers, newurl); refusing needs none of them.
    def redirect_request(self, *_args: object, **_kwargs: object) -> None:
        return None


_OPENER = urllib.request.build_opener(_NoRedirect)


class _Response(Protocol):
    """What the sender reads of a 2xx answer (an ``http.client.HTTPResponse``)."""

    def read(self, amt: int, /) -> bytes: ...

    def __enter__(self) -> _Response: ...

    def __exit__(
        self,
        exc_type: type[BaseException] | None,
        exc: BaseException | None,
        tb: TracebackType | None,
        /,
    ) -> None: ...


def _open(req: urllib.request.Request, timeout: float) -> _Response:
    """Send one request: urlopen, without following redirects."""
    # typeshed types an opener's answer as Any (its type depends on the URL scheme).
    resp: _Response = _OPENER.open(req, timeout=timeout)
    return resp


class _Outcome(enum.Enum):
    SENT = "sent"
    TRANSIENT = "transient"  # a retry may succeed
    SPLIT = "split"  # the route rejected the batch as a whole (413/400): halve it
    FINAL = "final"  # a retry cannot fix it (e.g. 401/403): drop the batch


# One event and its serialized form.
_Item = tuple[dict[str, object], bytes]


class IngestPoster:
    def __init__(
        self,
        url: str | None,
        token: str | None,
        timeout: float = REQUEST_TIMEOUT,
        max_batch: int = MAX_BATCH,
        flush_interval: float = FLUSH_INTERVAL,
        retry_backoff: float = RETRY_BACKOFF,
        close_timeout: float = CLOSE_TIMEOUT,
        max_batch_bytes: int = MAX_BATCH_BYTES,
    ) -> None:
        self._url = url.rstrip("/") + "/events" if url else None
        parts = urllib.parse.urlsplit(self._url or "")
        # What a warning names: host and path only (never the token, and no URL userinfo).
        self._where = f"{parts.hostname or ''}{parts.path}"
        self._token = token
        self._timeout = timeout
        self._max_batch = max_batch
        self._max_batch_bytes = max_batch_bytes
        self._flush_interval = flush_interval
        self._retry_backoff = retry_backoff
        self._close_timeout = close_timeout
        self._buffer: list[dict[str, object]] = []
        self._cond = threading.Condition()
        self._closed = False
        # Serializes sends so a close() drain never interleaves with the flusher's batch. Held only
        # around one batch's attempts, never across a split (it is not reentrant).
        self._send_lock = threading.Lock()
        self._thread: threading.Thread | None = None
        self._late_thread: threading.Thread | None = None
        # The monotonic time by which close() gives up; set by close(), seen by the flusher too.
        self._deadline: float | None = None
        # Circuit breaker: a batch exhausted its retries on transient failures and nothing has been
        # delivered since. At close this abandons the rest of the drain.
        self._down = False
        # The URL or token cannot form a request at all, or the route refused the token (401):
        # every later send would fail the same way.
        self._broken = False
        # Events the flusher had taken from the buffer and not finished with (for the close log).
        self._inflight = 0
        self._overflow_dropped = 0
        self._last_drop_warning: float | None = None
        self._abandoned = 0

    @property
    def enabled(self) -> bool:
        return bool(self._url and self._token)

    def post(self, event: dict[str, object]) -> None:
        """Queue one event for the next batch. Never blocks on the network."""
        if not self.enabled:
            return
        with self._cond:
            if self._closed:
                # A late event after close() (e.g. an atexit straggler) goes out on a background
                # thread, one attempt per batch, so the logging call never waits on the network.
                if self._broken or self._down:
                    self._abandoned += 1
                    return
                self._buffer.append(event)
                self._trim_locked()
                if self._late_thread is None:
                    self._late_thread = threading.Thread(
                        target=self._run_late, name="biffix-ingest-late", daemon=True
                    )
                    self._late_thread.start()
                return
            self._buffer.append(event)
            self._trim_locked()
            if self._thread is None:
                self._thread = threading.Thread(
                    target=self._run, name="biffix-ingest-poster", daemon=True
                )
                self._thread.start()
            if len(self._buffer) >= self._max_batch:
                self._cond.notify()

    def close(self) -> None:
        """Stop the flusher and send whatever is still buffered, within ``close_timeout``.

        Idempotent. Whatever is not delivered by the deadline, or once ingest is known to be
        unreachable, is abandoned (and counted in a warning): the coordinator's events.jsonl
        replay re-sends it.
        """
        with self._cond:
            if self._closed:  # later events belong to the late sender, not to a second drain
                return
            self._deadline = time.monotonic() + self._close_timeout
            # The close drain judges reachability afresh; its first exhausted batch trips it.
            self._down = False
            self._closed = True
            # What was posted before close is the close drain's, taken with the flag: an event
            # posted from here on goes to the late sender, which must never take these (it gives a
            # batch one attempt, and close() would return before they are sent).
            pending, self._buffer = self._buffer, []
            self._cond.notify()
            thread = self._thread
        if thread is not None and thread is not threading.current_thread():
            # The flusher honours the deadline between batches and clips each attempt's timeout to
            # it, so it winds down by then; the grace covers the last attempt's return.
            thread.join(timeout=max(0.0, self._remaining() or 0.0) + 1.0)
            if thread.is_alive():
                logger.warning(
                    "biffix ingest: the flusher was still sending %d event(s) at the close "
                    "deadline; abandoned them to the events.jsonl replay",
                    self._inflight,
                )
        try:
            self._send_all(pending)
        except Exception as e:  # noqa: BLE001 — close() must never raise into the engine
            logger.warning("biffix ingest: final flush failed (swallowed): %s", type(e).__name__)
        if self._abandoned or self._overflow_dropped:
            logger.warning(
                "biffix ingest: %d event(s) not delivered (%d abandoned at close or while ingest "
                "was unreachable, %d dropped from a full buffer); the coordinator's events.jsonl "
                "replay re-sends them",
                self._abandoned + self._overflow_dropped,
                self._abandoned,
                self._overflow_dropped,
            )

    # ── background threads ──

    def _run(self) -> None:
        while True:
            with self._cond:
                if not self._closed and len(self._buffer) < self._max_batch:
                    self._cond.wait(timeout=self._flush_interval)
                if self._closed:
                    return
                drained, self._buffer = self._buffer, []
                self._inflight = len(drained)
            try:
                self._send_all(drained)
            except Exception as e:  # noqa: BLE001 — one bad batch must never kill the flusher
                logger.warning(
                    "biffix ingest: flusher error (swallowed; %d event(s) dropped): %s",
                    len(drained),
                    type(e).__name__,
                )
            finally:
                self._inflight = 0

    def _run_late(self) -> None:
        while True:
            with self._cond:
                drained, self._buffer = self._buffer, []
                if not drained:
                    self._late_thread = None
                    return
            try:
                self._send_all(drained, late=True)
            except Exception as e:  # noqa: BLE001 — never raise out of a daemon thread
                logger.warning("biffix ingest: late send error (swallowed): %s", type(e).__name__)

    # ── the buffer ──

    def _trim_locked(self) -> None:
        """Drop events from a buffer over MAX_BUFFERED (see KEEP_UNDER_PRESSURE). Holds _cond."""
        if len(self._buffer) <= MAX_BUFFERED:
            return
        excess = len(self._buffer) - int(MAX_BUFFERED * _TRIM_TO)
        to_drop = excess
        kept: list[dict[str, object]] = []
        for event in self._buffer:
            if to_drop and event.get("kind") not in KEEP_UNDER_PRESSURE:
                to_drop -= 1
                continue
            kept.append(event)
        if to_drop:  # nothing expendable left: the oldest of the rest go
            del kept[:to_drop]
        self._buffer = kept
        self._overflow_dropped += excess
        now = time.monotonic()
        if self._last_drop_warning is None or now - self._last_drop_warning >= DROP_WARN_INTERVAL:
            self._last_drop_warning = now
            logger.warning(
                "biffix ingest buffer full (ingest unreachable?); %d event(s) dropped so far, "
                "oldest descriptive events first (the events.jsonl replay re-sends them)",
                self._overflow_dropped,
            )

    # ── sending ──

    def _remaining(self) -> float | None:
        return None if self._deadline is None else self._deadline - time.monotonic()

    def _abandoning(self, late: bool) -> bool:
        if self._broken:
            return True
        if late:  # sends after close() returned: the close deadline no longer applies
            return self._down
        if not self._closed:
            return False
        remaining = self._remaining()
        return self._down or (remaining is not None and remaining <= 0)

    def _encode(self, events: list[dict[str, object]]) -> list[_Item]:
        items: list[_Item] = []
        for event in events:
            try:
                data = json.dumps(event, separators=(",", ":")).encode("utf-8")
            except (TypeError, ValueError) as e:
                logger.warning(
                    "biffix ingest: dropped an unserializable %s event (%s): %s",
                    event.get("kind"),
                    event.get("task_key"),
                    type(e).__name__,
                )
                continue
            if len(data) + _ENVELOPE_BYTES > ROUTE_MAX_BODY_BYTES:
                logger.warning(
                    "biffix ingest: dropped a %s event (%s) of %d bytes, over the route's %d-byte "
                    "limit",
                    event.get("kind"),
                    event.get("task_key"),
                    len(data),
                    ROUTE_MAX_BODY_BYTES,
                )
                continue
            items.append((event, data))
        return items

    def _chunks(self, items: list[_Item]) -> list[list[_Item]]:
        """Cut items into batches of at most max_batch events and max_batch_bytes of body."""
        chunks: list[list[_Item]] = []
        chunk: list[_Item] = []
        size = _ENVELOPE_BYTES
        for item in items:
            extra = len(item[1]) + (1 if chunk else 0)  # the separating comma
            if chunk and (len(chunk) >= self._max_batch or size + extra > self._max_batch_bytes):
                chunks.append(chunk)
                chunk, size = [], _ENVELOPE_BYTES
                extra = len(item[1])
            chunk.append(item)
            size += extra
        if chunk:
            chunks.append(chunk)
        return chunks

    def _send_all(self, events: list[dict[str, object]], late: bool = False) -> None:
        for chunk in self._chunks(self._encode(events)):
            self._deliver(chunk, late, [SPLIT_POST_BUDGET])

    def _deliver(self, items: list[_Item], late: bool, budget: list[int]) -> None:
        """Send one batch; on a whole-batch rejection, halve it (``budget`` is shared per batch)."""
        if not items:
            return
        if self._abandoning(late):
            self._abandoned += len(items)
            return
        if budget[0] <= 0:
            logger.warning(
                "biffix ingest POST to %s: the route keeps rejecting this batch; dropped %d "
                "event(s) without isolating further",
                self._where,
                len(items),
            )
            return
        budget[0] -= 1
        body = _ENVELOPE_HEAD + b",".join(data for _, data in items) + _ENVELOPE_TAIL
        outcome, detail = self._send_with_retries(body, late)
        if outcome is _Outcome.SENT:
            self._log_refused(items, detail)
            return
        if outcome is _Outcome.SPLIT:
            if len(items) > 1:
                # The lock is released by now; each half takes it again.
                mid = len(items) // 2
                self._deliver(items[:mid], late, budget)
                self._deliver(items[mid:], late, budget)
                return
            event = items[0][0]
            logger.warning(
                "biffix ingest POST to %s rejected one %s event (%s), dropped: %s",
                self._where,
                event.get("kind"),
                event.get("task_key"),
                detail,
            )
            return
        # Never break the run on ingest failure — events.jsonl + EventBridge are the backstop.
        logger.warning(
            "biffix ingest POST of %d event(s) to %s failed (swallowed): %s",
            len(items),
            self._where,
            detail,
        )

    def _log_refused(self, items: list[_Item], answer: str) -> None:
        """Log each event a 2xx answer's ``refused`` names (see the module docs)."""
        try:
            refused = json.loads(answer).get("refused") or []
        except (ValueError, AttributeError):
            return  # not the summary (e.g. an older route's empty answer): nothing was refused
        if not isinstance(refused, list):
            return
        for entry in refused:
            # Only an entry the summary could hold: an object whose index (a JSON integer, never a
            # string or a bool) names an event of this batch. Anything else refuses nothing, as in
            # nf-biffix's poster and the coordinator's replay.
            index = entry.get("index") if isinstance(entry, dict) else None
            if not isinstance(index, int) or isinstance(index, bool):
                continue
            if not 0 <= index < len(items):
                continue
            event, reason = items[index][0], str(entry.get("reason", ""))
            logger.warning(
                "biffix ingest POST to %s refused one %s event (%s), dropped: %s",
                self._where,
                event.get("kind"),
                event.get("task_key"),
                reason,
            )

    def _send_with_retries(self, body: bytes, late: bool) -> tuple[_Outcome, str]:
        attempts = 1 if late else MAX_ATTEMPTS
        outcome, detail = _Outcome.TRANSIENT, "not attempted"
        with self._send_lock:
            for attempt in range(1, attempts + 1):
                remaining = None if late else self._remaining()
                if remaining is not None and remaining <= 0:
                    return _Outcome.TRANSIENT, "close deadline passed"
                timeout = self._timeout if remaining is None else min(self._timeout, remaining)
                outcome, detail, retry_after = self._post_once(body, timeout)
                if outcome is not _Outcome.TRANSIENT:
                    if outcome is _Outcome.SENT:
                        self._down = False
                    return outcome, detail
                if attempt < attempts:
                    backoff = (
                        min(retry_after, MAX_RETRY_AFTER)
                        if retry_after > 0
                        else self._retry_backoff * 2 ** (attempt - 1)
                    )
                    remaining = None if late else self._remaining()
                    time.sleep(backoff if remaining is None else max(0.0, min(backoff, remaining)))
        # Retries exhausted on transient failures: ingest looks unreachable.
        self._down = True
        return outcome, detail

    def _post_once(self, data: bytes, timeout: float) -> tuple[_Outcome, str, float]:
        """One POST; the outcome, for a failure a short description (never the token) and for a
        success the answer's body, and the response's Retry-After in seconds (0 when absent)."""
        try:
            req = urllib.request.Request(
                self._url,  # type: ignore[arg-type]
                data=data,
                method="POST",
                headers={
                    "Content-Type": "application/json",
                    "Authorization": f"Bearer {self._token}",
                },
            )
            # _open raises HTTPError for a 3xx/4xx/5xx, so returning is 2xx.
            with _open(req, timeout) as resp:
                return _Outcome.SENT, _answer(resp), 0.0
        except urllib.error.HTTPError as e:
            detail = f"HTTP {e.code}: {self._excerpt(e)}"
            if e.code in (408, 429) or e.code >= 500:
                return _Outcome.TRANSIENT, detail, _retry_after(e)
            if e.code in (400, 413):
                return _Outcome.SPLIT, detail, 0.0
            if e.code == 401:
                # The token is invalid or expired: no later send can succeed.
                self._mark_broken(f"ingest refused the capability token ({detail})")
            return _Outcome.FINAL, detail, 0.0
        except urllib.error.URLError as e:
            return _Outcome.TRANSIENT, f"{type(e).__name__}: {e.reason}", 0.0
        except ValueError as e:
            # A URL or token that cannot form a request (e.g. no scheme, a newline in the token).
            # Its message can quote the Authorization header, so only the type is logged.
            self._mark_broken(f"the ingest URL or token is invalid ({type(e).__name__})")
            return _Outcome.FINAL, type(e).__name__, 0.0
        except Exception as e:  # noqa: BLE001 — OSError, timeouts, http.client protocol errors
            return _Outcome.TRANSIENT, type(e).__name__, 0.0

    def _mark_broken(self, why: str) -> None:
        """Stop every later send (logged once): no request to this URL with this token can
        succeed."""
        if not self._broken:
            self._broken = True
            logger.warning(
                "biffix ingest disabled: %s; later events go only to the events file", why
            )

    def _excerpt(self, e: urllib.error.HTTPError) -> str:
        """The first bytes of an error response, which name what the route refused."""
        try:
            text = e.read(200).decode("utf-8", "replace")
        except Exception:  # noqa: BLE001
            text = ""
        text = " ".join(text.split()) or str(e.reason)
        if self._token:
            text = text.replace(self._token, "<token>")
        return text


def _answer(resp: _Response) -> str:
    """A 2xx response's body (the ingest summary), or "" when it cannot be read: the events were
    accepted either way."""
    try:
        body: bytes = resp.read(MAX_ANSWER_BYTES)
        return body.decode("utf-8", "replace")
    except Exception:  # noqa: BLE001
        return ""


def _retry_after(e: urllib.error.HTTPError) -> float:
    """The response's Retry-After in seconds (delta-seconds form only), else 0."""
    try:
        value = (e.headers.get("Retry-After") if e.headers is not None else None) or ""
        return max(0.0, float(int(value.strip())))
    except (ValueError, TypeError, AttributeError):
        return 0.0
