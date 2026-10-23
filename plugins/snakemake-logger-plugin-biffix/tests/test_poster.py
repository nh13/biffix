"""The ingest poster must send the batch envelope the backend `/events` route expects, batching
events rather than POSTing one request per event, and deliver as much as it can without ever
breaking or stalling the engine."""

from __future__ import annotations

import http.client
import io
import json
import logging
import threading
import time
import urllib.error
import urllib.request
from collections.abc import Callable, Iterator
from typing import Any, TypedDict

import pytest

from snakemake_logger_plugin_biffix import poster as poster_mod
from snakemake_logger_plugin_biffix.poster import MAX_ATTEMPTS, IngestPoster


@pytest.fixture(autouse=True)
def _no_leaked_sender_threads() -> Iterator[None]:
    """Fail a test that leaves a poster thread running. The recorder patches the module's
    ``_open``, so a leftover thread would post into the next test's recorder."""
    yield
    deadline = time.monotonic() + 5.0
    for thread in threading.enumerate():
        if thread.name.startswith("biffix-ingest"):
            thread.join(timeout=max(0.0, deadline - time.monotonic()))
    leaked = [t.name for t in threading.enumerate() if t.name.startswith("biffix-ingest")]
    assert not leaked, f"the test left poster threads running: {leaked}"


class _FakeResp:
    status = 202

    def __enter__(self) -> _FakeResp:
        return self

    def __exit__(self, *args: object) -> None:
        return None


Reject = Callable[[list[dict[str, object]], bytes], "Exception | None"]


def _sent_events(req: urllib.request.Request) -> list[dict[str, Any]]:
    """The events of a request the poster sends (its JSON body, parsed)."""
    assert isinstance(req.data, bytes), req.data
    events: list[dict[str, Any]] = json.loads(req.data.decode("utf-8"))["events"]
    return events


class _Sent(TypedDict):
    """A request the recorder accepted: its URL, Authorization header and JSON body (parsed,
    so its shape is read unchecked)."""

    url: str
    auth: str | None
    body: dict[str, Any]


class _Recorder:
    """A fake ``_open`` that records each accepted request and counts every call. It fails the
    first few calls with ``failures``, and any call ``reject(events, raw_body)`` returns an error
    for."""

    def __init__(
        self, failures: list[Exception] | None = None, reject: Reject | None = None
    ) -> None:
        self.requests: list[_Sent] = []
        self.sizes: list[int] = []
        self.calls = 0
        self._failures = list(failures or [])
        self._reject = reject
        self._lock = threading.Lock()

    def __call__(self, req: urllib.request.Request, _timeout: float = 0.0) -> _FakeResp:
        with self._lock:
            self.calls += 1
            if self._failures:
                raise self._failures.pop(0)
            data = req.data
            assert isinstance(data, bytes), data
            body = json.loads(data.decode("utf-8"))
            if self._reject is not None:
                err = self._reject(body["events"], data)
                if err is not None:
                    raise err
            self.sizes.append(len(data))
            self.requests.append(
                {"url": req.full_url, "auth": req.headers.get("Authorization"), "body": body}
            )
        return _FakeResp()

    @property
    def batches(self) -> list[list[dict[str, object]]]:
        return [r["body"]["events"] for r in self.requests]

    @property
    def delivered(self) -> list[dict[str, object]]:
        return [e for batch in self.batches for e in batch]


def _events(n: int) -> list[dict[str, object]]:
    return [{"run_id": "run-1", "kind": "task_created", "task_key": f"t{i}"} for i in range(n)]


def _bindings(n: int) -> list[dict[str, object]]:
    return [
        {"run_id": "run-1", "kind": "task_submitted", "task_key": f"s{i}", "batch_job_id": f"j{i}"}
        for i in range(n)
    ]


def _http_error(
    code: int, body: bytes = b"", headers: dict[str, str] | None = None
) -> urllib.error.HTTPError:
    return urllib.error.HTTPError(
        "https://x/api/events",
        code,
        "err",
        headers or {},  # type: ignore[arg-type]
        io.BytesIO(body),
    )


def _wait_for(cond: Callable[[], bool], within: float = 5.0) -> None:
    deadline = time.monotonic() + within
    while not cond() and time.monotonic() < deadline:
        time.sleep(0.01)


def _poster(
    *,
    max_batch: int = poster_mod.MAX_BATCH,
    max_batch_bytes: int = poster_mod.MAX_BATCH_BYTES,
    timeout: float = poster_mod.REQUEST_TIMEOUT,
    close_timeout: float = poster_mod.CLOSE_TIMEOUT,
    # A long interval keeps the timer out of the way unless a test is about the timer.
    flush_interval: float = 60.0,
) -> IngestPoster:
    return IngestPoster(
        "https://api.example/api",
        "tok-123",
        timeout=timeout,
        max_batch=max_batch,
        flush_interval=flush_interval,
        retry_backoff=0.0,
        close_timeout=close_timeout,
        max_batch_bytes=max_batch_bytes,
    )


def test_post_wraps_events_in_one_batch_envelope(monkeypatch: pytest.MonkeyPatch) -> None:
    recorder = _Recorder()
    monkeypatch.setattr("snakemake_logger_plugin_biffix.poster._open", recorder)

    poster = _poster()
    events = _events(3)
    for event in events:
        poster.post(event)
    assert recorder.requests == [], "post() buffers; nothing is sent until a flush"
    poster.close()

    # The backend expects IngestRequest { events: [...] }, not a bare event — one request here.
    assert len(recorder.requests) == 1
    assert recorder.requests[0]["url"] == "https://api.example/api/events"
    assert recorder.requests[0]["auth"] == "Bearer tok-123"
    assert recorder.batches == [events]


def test_batches_never_exceed_the_max_and_keep_order(monkeypatch: pytest.MonkeyPatch) -> None:
    recorder = _Recorder()
    monkeypatch.setattr("snakemake_logger_plugin_biffix.poster._open", recorder)

    poster = _poster(max_batch=4)
    events = _events(10)
    for event in events:
        poster.post(event)
    poster.close()

    assert all(len(batch) <= 4 for batch in recorder.batches)
    assert recorder.delivered == events


def test_a_full_batch_is_sent_without_waiting_for_the_interval(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    recorder = _Recorder()
    monkeypatch.setattr("snakemake_logger_plugin_biffix.poster._open", recorder)

    poster = _poster(max_batch=5)
    for event in _events(5):
        poster.post(event)
    _wait_for(lambda: bool(recorder.requests))
    assert recorder.batches == [_events(5)]
    poster.close()


def test_a_partial_batch_is_sent_after_the_flush_interval(monkeypatch: pytest.MonkeyPatch) -> None:
    recorder = _Recorder()
    monkeypatch.setattr("snakemake_logger_plugin_biffix.poster._open", recorder)

    poster = _poster(flush_interval=0.05)
    poster.post(_events(1)[0])
    _wait_for(lambda: bool(recorder.requests))
    assert recorder.batches == [_events(1)]
    poster.close()


@pytest.mark.parametrize(
    "failure",
    [
        _http_error(500),
        _http_error(503),
        _http_error(429),
        _http_error(408),
        urllib.error.URLError("refused"),
        TimeoutError("timed out"),
        http.client.BadStatusLine("GARBAGE"),
        http.client.IncompleteRead(b""),
    ],
)
def test_a_transient_failure_is_retried(
    monkeypatch: pytest.MonkeyPatch, failure: Exception
) -> None:
    recorder = _Recorder(failures=[failure])
    monkeypatch.setattr("snakemake_logger_plugin_biffix.poster._open", recorder)

    poster = _poster()
    poster.post(_events(1)[0])
    poster.close()

    assert recorder.batches == [_events(1)]
    assert recorder.calls == 2


@pytest.mark.parametrize(
    ("retry_after", "expected"),
    [("7", 7.0), ("3600", poster_mod.MAX_RETRY_AFTER), ("soon", 0.0)],
)
def test_retry_after_sets_the_backoff_up_to_a_cap(
    monkeypatch: pytest.MonkeyPatch, retry_after: str, expected: float
) -> None:
    recorder = _Recorder(failures=[_http_error(429, headers={"Retry-After": retry_after})])
    monkeypatch.setattr("snakemake_logger_plugin_biffix.poster._open", recorder)
    sleeps: list[float] = []
    monkeypatch.setattr(poster_mod.time, "sleep", lambda s: sleeps.append(s))

    poster = _poster()  # retry_backoff=0: without a usable Retry-After the backoff is 0
    poster.post(_events(1)[0])
    poster.close()

    assert recorder.batches == [_events(1)]
    # The send runs during close(), whose deadline (CLOSE_TIMEOUT) clips the sleep by a hair.
    assert len(sleeps) == 1 and sleeps[0] == pytest.approx(expected, abs=0.5)


def test_a_single_rejected_event_is_dropped_after_one_attempt(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    recorder = _Recorder(failures=[_http_error(400)])
    monkeypatch.setattr("snakemake_logger_plugin_biffix.poster._open", recorder)

    poster = _poster()
    poster.post(_events(1)[0])
    poster.close()  # must not raise

    assert recorder.requests == [], "a 400 on one event is final: it is dropped, not resent"
    assert recorder.calls == 1


@pytest.mark.parametrize("code", [401, 403])
def test_an_auth_error_drops_the_batch_without_splitting(
    monkeypatch: pytest.MonkeyPatch, code: int
) -> None:
    recorder = _Recorder(failures=[_http_error(code)])
    monkeypatch.setattr("snakemake_logger_plugin_biffix.poster._open", recorder)

    poster = _poster()
    for event in _events(4):
        poster.post(event)
    poster.close()

    assert recorder.requests == []
    assert recorder.calls == 1, "401/403 apply to the whole token or run: no retry, no split"


def test_a_401_stops_every_later_send(
    monkeypatch: pytest.MonkeyPatch, caplog: pytest.LogCaptureFixture
) -> None:
    # As nf-biffix's poster: a rejected token cannot succeed on any later batch.
    recorder = _Recorder(reject=lambda _events, _: _http_error(401))
    monkeypatch.setattr("snakemake_logger_plugin_biffix.poster._open", recorder)

    poster = _poster(max_batch_bytes=100)  # one event per batch
    with caplog.at_level(logging.WARNING):
        for event in _events(3):
            poster.post(event)
        poster.close()
        poster.post(_events(1)[0])  # a late one

    assert recorder.calls == 1
    assert poster._abandoned == 3
    disabled = [r for r in caplog.records if "biffix ingest disabled" in r.getMessage()]
    assert len(disabled) == 1
    assert "refused the capability token" in caplog.text and "tok-123" not in caplog.text


def test_persistent_failure_gives_up_without_raising(monkeypatch: pytest.MonkeyPatch) -> None:
    recorder = _Recorder(failures=[urllib.error.URLError("down")] * 10)
    monkeypatch.setattr("snakemake_logger_plugin_biffix.poster._open", recorder)

    poster = _poster()
    poster.post(_events(1)[0])
    poster.close()

    assert recorder.requests == []
    assert recorder.calls == MAX_ATTEMPTS


def test_a_413_splits_the_batch_until_every_event_is_delivered(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    # The route answers 413 for any batch over 3 events (standing in for its byte limit).
    recorder = _Recorder(reject=lambda events, _: _http_error(413) if len(events) > 3 else None)
    monkeypatch.setattr("snakemake_logger_plugin_biffix.poster._open", recorder)

    poster = _poster(max_batch=16)
    events = _events(16)
    for event in events:
        poster.post(event)
    poster.close()

    assert all(len(batch) <= 3 for batch in recorder.batches)
    assert recorder.delivered == events


def test_one_bad_event_does_not_take_its_batch_down(
    monkeypatch: pytest.MonkeyPatch, caplog: pytest.LogCaptureFixture
) -> None:
    def reject(events: list[dict[str, object]], _: bytes) -> Exception | None:
        if any(e["task_key"] == "t5" for e in events):
            return _http_error(400, b'event 5: unknown variant "tasq_failed" for kind')
        return None

    recorder = _Recorder(reject=reject)
    monkeypatch.setattr("snakemake_logger_plugin_biffix.poster._open", recorder)

    poster = _poster()
    events = _events(12)
    for event in events:
        poster.post(event)
    with caplog.at_level(logging.WARNING):
        poster.close()

    assert recorder.delivered == [e for e in events if e["task_key"] != "t5"]
    # The drop names the event, the endpoint and what the route said; never the token.
    assert "task_created event (t5)" in caplog.text
    assert "api.example/api/events" in caplog.text
    assert "unknown variant" in caplog.text
    assert "tok-123" not in caplog.text


class _AnsweringResp(_FakeResp):
    """A 2xx whose body is ``answer``."""

    def __init__(self, answer: bytes) -> None:
        self._answer = answer

    def read(self, _n: int = -1) -> bytes:
        return self._answer


def test_events_a_2xx_refuses_are_logged_once_and_not_resent(
    monkeypatch: pytest.MonkeyPatch, caplog: pytest.LogCaptureFixture
) -> None:
    # The route accepts the batch but refuses one claim on its own, naming it by its index.
    calls: list[list[dict[str, object]]] = []

    def urlopen(req: urllib.request.Request, _timeout: float = 0.0) -> _FakeResp:
        events = _sent_events(req)
        calls.append(events)
        answer = {
            "appended": len(events) - 1,
            "duplicates": 0,
            "refused": [{"index": 2, "reason": "Batch job job-9 is not a job of this run"}],
        }
        return _AnsweringResp(json.dumps(answer).encode())

    monkeypatch.setattr("snakemake_logger_plugin_biffix.poster._open", urlopen)
    poster = _poster()
    for event in _events(4):
        poster.post(event)
    with caplog.at_level(logging.WARNING):
        poster.close()

    assert len(calls) == 1, "a refused event is not resent: a resend is refused again"
    refusals = [r for r in caplog.records if "refused one" in r.getMessage()]
    assert len(refusals) == 1
    assert "refused one task_created event (t2)" in caplog.text
    assert "Batch job job-9 is not a job of this run" in caplog.text
    assert "tok-123" not in caplog.text


def test_a_2xx_without_a_summary_refuses_nothing(
    monkeypatch: pytest.MonkeyPatch, caplog: pytest.LogCaptureFixture
) -> None:
    for answer in [
        b"",
        b"not json",
        b"[1]",
        b'{"refused": [{"index": 99}]}',
        b'{"refused": [{"index": -1}]}',  # not the last event
        b'{"refused": [{"index": "0"}, {"index": true}, {"reason": "x"}, 0]}',
        b'{"refused": {"index": 0}}',
    ]:
        monkeypatch.setattr(
            "snakemake_logger_plugin_biffix.poster._open",
            lambda _req, _timeout=0.0, a=answer: _AnsweringResp(a),
        )
        poster = _poster()
        poster.post(_events(1)[0])
        with caplog.at_level(logging.WARNING):
            poster.close()
        assert "refused" not in caplog.text, answer


def test_a_route_that_rejects_everything_is_not_bisected_forever(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    recorder = _Recorder(reject=lambda _events, _: _http_error(400))
    monkeypatch.setattr("snakemake_logger_plugin_biffix.poster._open", recorder)

    poster = _poster()
    for event in _events(500):
        poster.post(event)
    poster.close()

    assert recorder.requests == []
    assert recorder.calls <= poster_mod.SPLIT_POST_BUDGET


def test_batches_are_bounded_by_serialized_size(monkeypatch: pytest.MonkeyPatch) -> None:
    recorder = _Recorder()
    monkeypatch.setattr("snakemake_logger_plugin_biffix.poster._open", recorder)

    limit = 1_000
    poster = _poster(max_batch_bytes=limit)
    events = [dict(e, deps=["x" * 40] * 3) for e in _events(30)]
    for event in events:
        poster.post(event)
    poster.close()

    assert len(recorder.batches) > 1
    assert all(size <= limit for size in recorder.sizes)
    assert recorder.delivered == events


def test_an_event_over_the_route_limit_is_dropped_unsent(monkeypatch: pytest.MonkeyPatch) -> None:
    recorder = _Recorder()
    monkeypatch.setattr("snakemake_logger_plugin_biffix.poster._open", recorder)
    monkeypatch.setattr(poster_mod, "ROUTE_MAX_BODY_BYTES", 500)

    poster = _poster()
    small, huge = _events(2)
    huge = dict(huge, deps=["y" * 100] * 10)
    poster.post(small)
    poster.post(huge)
    poster.close()

    assert recorder.batches == [[small]]


def test_any_exception_from_a_send_is_retried_as_transient(monkeypatch: pytest.MonkeyPatch) -> None:
    # _post_once turns a non-HTTP exception into a transient failure: the batch is retried, and
    # dropped once its attempts are spent.
    recorder = _Recorder(failures=[RuntimeError("boom")] * MAX_ATTEMPTS)
    monkeypatch.setattr("snakemake_logger_plugin_biffix.poster._open", recorder)

    poster = _poster(max_batch=2)
    first, second = _events(4)[:2], _events(4)[2:]
    for event in first:
        poster.post(event)
    _wait_for(lambda: recorder.calls >= MAX_ATTEMPTS)
    for event in second:
        poster.post(event)
    _wait_for(lambda: bool(recorder.requests))

    assert poster._thread is not None and poster._thread.is_alive()
    assert recorder.batches == [second]
    poster.close()


def test_the_flusher_survives_an_exception_out_of_a_batch(
    monkeypatch: pytest.MonkeyPatch, caplog: pytest.LogCaptureFixture
) -> None:
    # Past _post_once's own guard: an exception out of _send_all itself must not end the daemon
    # flusher, or every later event would wait for close().
    recorder = _Recorder()
    monkeypatch.setattr("snakemake_logger_plugin_biffix.poster._open", recorder)
    poster = _poster(max_batch=2)
    chunks, raised = poster._chunks, threading.Event()

    def chunks_once(items: list[poster_mod._Item]) -> list[list[poster_mod._Item]]:
        if not raised.is_set():
            raised.set()
            raise RuntimeError("boom")
        return chunks(items)

    monkeypatch.setattr(poster, "_chunks", chunks_once)
    first, second = _events(4)[:2], _events(4)[2:]
    with caplog.at_level(logging.WARNING):
        for event in first:
            poster.post(event)
        assert raised.wait(5.0)
        for event in second:
            poster.post(event)
        _wait_for(lambda: bool(recorder.requests))

    assert poster._thread is not None and poster._thread.is_alive()
    assert recorder.batches == [second]
    assert "flusher error (swallowed; 2 event(s) dropped): RuntimeError" in caplog.text
    poster.close()


def test_an_unusable_url_disables_posting_without_raising(caplog: pytest.LogCaptureFixture) -> None:
    poster = IngestPoster(
        "ingest.internal/api", "tok-secret", flush_interval=60.0, retry_backoff=0.0
    )
    with caplog.at_level(logging.WARNING):
        for event in _events(3):
            poster.post(event)
        poster.close()  # must not raise
        poster.post(_events(1)[0])  # nor may a late event
    assert "biffix ingest disabled" in caplog.text
    assert "tok-secret" not in caplog.text


def test_close_is_bounded_when_ingest_is_blackholed(monkeypatch: pytest.MonkeyPatch) -> None:
    def blackhole(_req: urllib.request.Request, timeout: float = 0.0) -> _FakeResp:
        time.sleep(timeout)
        raise TimeoutError("timed out")

    monkeypatch.setattr("snakemake_logger_plugin_biffix.poster._open", blackhole)
    poster = _poster(max_batch=5, timeout=0.5, close_timeout=1.0)
    for event in _events(200):
        poster.post(event)

    started = time.monotonic()
    poster.close()
    assert time.monotonic() - started < 3.0, "close() must honour its deadline"


def test_close_stops_retrying_once_ingest_is_known_down(
    monkeypatch: pytest.MonkeyPatch, caplog: pytest.LogCaptureFixture
) -> None:
    recorder = _Recorder(failures=[urllib.error.URLError("refused")] * 1000)
    monkeypatch.setattr("snakemake_logger_plugin_biffix.poster._open", recorder)

    # Nothing reaches the flusher's trigger, so close() drains everything, in size-cut batches.
    poster = _poster(max_batch=1000, max_batch_bytes=300)
    for event in _events(50):
        poster.post(event)
    with caplog.at_level(logging.WARNING):
        poster.close()

    assert recorder.calls == MAX_ATTEMPTS, "only the first batch pays for retries"
    assert "abandoned at close or while ingest was unreachable" in caplog.text


def test_close_is_idempotent_and_late_events_still_go_out(monkeypatch: pytest.MonkeyPatch) -> None:
    recorder = _Recorder()
    monkeypatch.setattr("snakemake_logger_plugin_biffix.poster._open", recorder)

    poster = _poster()
    poster.close()
    poster.close()
    late = _events(1)[0]
    poster.post(late)
    _wait_for(lambda: bool(recorder.requests))

    assert recorder.batches == [[late]]


def test_events_posted_before_close_are_the_close_drain_s(monkeypatch: pytest.MonkeyPatch) -> None:
    # A late event arriving while close() waits for the busy flusher starts the late sender, which
    # must take only what came after close: the events before it are the close drain's (its
    # retries, and sent before close() returns), not one late attempt's.
    started, release = threading.Event(), threading.Event()
    recorder = _Recorder()

    def send(req: urllib.request.Request, timeout: float = 0.0) -> _FakeResp:
        if [e["task_key"] for e in _sent_events(req)] == ["t0", "t1"]:
            started.set()
            release.wait(5.0)  # the flusher's batch, still sending when close() starts
        return recorder(req, timeout)

    monkeypatch.setattr("snakemake_logger_plugin_biffix.poster._open", send)
    poster = _poster(max_batch=2)
    taken: dict[str, list[str]] = {}
    send_all = poster._send_all

    def record(events: list[dict[str, object]], late: bool = False) -> None:
        name = threading.current_thread().name
        taken.setdefault(name, []).extend(str(e["task_key"]) for e in events)
        send_all(events, late)

    monkeypatch.setattr(poster, "_send_all", record)
    before: dict[str, object] = {"kind": "run_succeeded", "task_key": "b"}
    first, after = _events(2), _events(4)[3]
    for event in first:
        poster.post(event)
    assert started.wait(5.0)
    poster.post(before)
    closing = threading.Thread(target=poster.close, name="closing")
    closing.start()
    _wait_for(lambda: poster._closed)
    poster.post(after)
    _wait_for(lambda: "biffix-ingest-late" in taken)
    release.set()
    closing.join(5.0)
    _wait_for(lambda: len(recorder.delivered) == 4)

    assert taken["biffix-ingest-late"] == ["t3"], "the late sender took only what followed close"
    assert taken["closing"] == ["b"]
    assert {e["task_key"] for e in recorder.delivered} == {"t0", "t1", "b", "t3"}


def test_a_late_event_never_blocks_the_caller(monkeypatch: pytest.MonkeyPatch) -> None:
    def slow(_req: urllib.request.Request, _timeout: float = 0.0) -> _FakeResp:
        time.sleep(1.0)
        return _FakeResp()

    monkeypatch.setattr("snakemake_logger_plugin_biffix.poster._open", slow)
    poster = _poster()
    poster.close()
    started = time.monotonic()
    poster.post(_events(1)[0])
    assert time.monotonic() - started < 0.5


def test_overflow_keeps_the_bindings_and_drops_the_oldest_others(
    monkeypatch: pytest.MonkeyPatch, caplog: pytest.LogCaptureFixture
) -> None:
    recorder = _Recorder()
    monkeypatch.setattr("snakemake_logger_plugin_biffix.poster._open", recorder)
    monkeypatch.setattr(poster_mod, "MAX_BUFFERED", 10)

    poster = _poster(max_batch=1000)
    bindings = _bindings(3)
    created = _events(30)
    with caplog.at_level(logging.WARNING):
        for event in bindings + created:
            poster.post(event)
        poster.close()

    # Each overflow (11 events) trims to 9, dropping the oldest task_created events: 12 trims of
    # 2 over the 33 posts.
    assert recorder.delivered == bindings + created[-6:]
    assert poster._overflow_dropped == 24
    # A sustained overflow warns once (rate-limited), and close() logs the total.
    full = [r for r in caplog.records if "buffer full" in r.getMessage()]
    assert len(full) == 1
    assert "dropped from a full buffer" in caplog.text


def test_a_redirect_is_never_followed(caplog: pytest.LogCaptureFixture) -> None:
    # urllib re-sends a redirected POST as a GET without the events, whose 2xx read as delivered.
    from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

    methods: list[str] = []

    class Handler(BaseHTTPRequestHandler):
        def _answer(self, code: int) -> None:
            methods.append(self.command)
            self.send_response(code)
            if code == 302:
                self.send_header("Location", "/elsewhere")
            self.send_header("Content-Length", "0")
            self.end_headers()

        def do_POST(self) -> None:
            self.rfile.read(int(self.headers["Content-Length"]))
            self._answer(302)

        def do_GET(self) -> None:
            self._answer(200)

        def log_message(self, *args: object) -> None:
            pass

    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    serving = threading.Thread(target=server.serve_forever, daemon=True)
    serving.start()
    try:
        poster = IngestPoster(
            f"http://127.0.0.1:{server.server_address[1]}/api", "tok", flush_interval=60.0
        )
        poster.post(_events(1)[0])
        with caplog.at_level(logging.WARNING):
            poster.close()
    finally:
        server.shutdown()
        server.server_close()
    assert methods == ["POST"]
    assert "failed (swallowed): HTTP 302" in caplog.text


def test_an_overflow_of_kept_kinds_alone_drops_the_oldest(monkeypatch: pytest.MonkeyPatch) -> None:
    # Nothing expendable to drop: the buffer stays bounded, and the newest bindings survive.
    recorder = _Recorder()
    monkeypatch.setattr("snakemake_logger_plugin_biffix.poster._open", recorder)
    monkeypatch.setattr(poster_mod, "MAX_BUFFERED", 10)

    poster = _poster(max_batch=1000)
    bindings = _bindings(30)
    for event in bindings:
        poster.post(event)
        assert len(poster._buffer) <= 10
    poster.close()

    assert recorder.delivered == bindings[-10:]
    assert poster._overflow_dropped == 20


def test_a_trailing_slash_on_the_base_url_is_ignored(monkeypatch: pytest.MonkeyPatch) -> None:
    recorder = _Recorder()
    monkeypatch.setattr("snakemake_logger_plugin_biffix.poster._open", recorder)

    poster = IngestPoster("https://api.example/api/", "tok", flush_interval=60.0)
    poster.post(_events(1)[0])
    poster.close()

    assert recorder.requests[0]["url"] == "https://api.example/api/events"


def test_post_disabled_without_url_or_token(monkeypatch: pytest.MonkeyPatch) -> None:
    recorder = _Recorder()
    monkeypatch.setattr("snakemake_logger_plugin_biffix.poster._open", recorder)
    for poster in (IngestPoster(None, "tok"), IngestPoster("https://x/api", None)):
        poster.post({"run_id": "r"})
        poster.close()
    assert recorder.requests == [], "poster must no-op when url or token is missing"
