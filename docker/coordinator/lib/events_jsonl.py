"""The engine's durable events.jsonl, after the engine exits: archive it, and replay it to ingest.

Usage: events_jsonl.py upload <events.jsonl> <s3 uri>
       events_jsonl.py replay <events.jsonl>

Both are best-effort: a failure is logged and swallowed, never changing the run's outcome. The
replay reads the ingest route from BIFFIX_INGEST_URL and its token from BIFFIX_INGEST_TOKEN (the
environment, so the token never shows in a process listing).
"""

import json
import os
import sys
import time
import urllib.error
import urllib.request
from collections.abc import Iterator
from typing import Literal

# The route's limits and the delivery policy are the engine plugins' posters' (biffix_core::
# ingest_limits, written to schemas/ingest-limits.json; tests/test_lib.py fails if these differ).
# The route answers 413 over MAX_EVENTS events or ROUTE_MAX_BYTES of body, so chunks are cut by
# both, under the byte limit with headroom (MAX_BYTES). A 413 or 400 rejects a whole chunk, so it
# is halved until the refused event is alone and only that one is dropped, within a per-chunk
# request budget. A 2xx can still refuse single events (a task_submitted whose Batch job claim the
# route could not verify), named by index in the answer's `refused`; the rest landed, and each
# refused one is logged and dropped. A transient failure (a connection error, 408, 429, 5xx) is
# retried up to ATTEMPTS times, after the answer's Retry-After (at most MAX_RETRY_AFTER) or a
# doubling backoff from RETRY_BACKOFF.
MAX_EVENTS, ROUTE_MAX_BYTES = 500, 5 * 1024 * 1024
MAX_BYTES, SPLIT_BUDGET, ATTEMPTS = 4 * 1024 * 1024, 64, 3
RETRY_BACKOFF, MAX_RETRY_AFTER = 0.5, 30.0
# The replay's own: one request's timeout (it sends at exit, with no flush waiting on it) and the
# whole replay's deadline.
REQUEST_TIMEOUT, DEADLINE_SECS = 30, 300
HEAD, TAIL = b'{"events":[', b"]}"
# The route's answer once the run is cancelled (biffix-backend's api/events.rs): a cancel revokes
# the run's token, so every later chunk gets the same. Expected after a cancel, not a failure: the
# replay stops on it with one line, while any other 403 is logged as a failure.
CANCELLED = (403, "run is cancelled; capability token revoked")

# One event and its compact JSON.
Item = tuple[dict[str, object], bytes]
# How one chunk's POST ended (see Replay.post).
Outcome = Literal["sent", "split", "final", "down", "cancelled"]


class _NoRedirect(urllib.request.HTTPRedirectHandler):
    """Refuse to follow a redirect: urllib re-sends a redirected POST as a bodiless GET, whose 2xx
    would read as the events delivered. The 3xx is raised as an HTTPError instead (a final
    failure)."""

    # urllib passes (req, fp, code, msg, headers, newurl); refusing needs none of them.
    def redirect_request(self, *_args: object, **_kwargs: object) -> None:
        return None


OPENER = urllib.request.build_opener(_NoRedirect)


def upload(src: str, dest: str) -> None:
    """The archival replay copy, under the run's own prefix."""
    import boto3

    try:
        bucket, _, key = dest.removeprefix("s3://").partition("/")
        if not dest.startswith("s3://") or not bucket or not key:
            raise ValueError(f"not an s3 object uri: {dest}")
        boto3.client("s3").upload_file(src, bucket, key)
        print(f"biffix-coordinator: uploaded events.jsonl to s3://{bucket}/{key}", file=sys.stderr)
    except Exception as e:  # never fail the run on the archival copy
        print(f"biffix-coordinator: events.jsonl upload failed (swallowed): {e}", file=sys.stderr)


def read_events(src: str) -> list[Item]:
    """Each event of the file and its compact JSON, skipping blank lines, and logging and
    skipping malformed ones (a truncated last line, say) and any event too big for the route on
    its own."""
    items: list[Item] = []
    malformed = 0
    with open(src, errors="replace") as fh:
        for line in fh:
            line = line.strip()
            if not line:
                continue
            try:
                event = json.loads(line)
            except ValueError:
                malformed += 1
                continue
            if not isinstance(event, dict):
                malformed += 1
                continue
            data = json.dumps(event, separators=(",", ":")).encode()
            if len(data) + len(HEAD) + len(TAIL) > ROUTE_MAX_BYTES:
                kind = event.get("kind")
                print(
                    f"biffix-coordinator: events replay dropped a {kind} event over the route "
                    f"limit ({len(data)} bytes)",
                    file=sys.stderr,
                )
                continue
            items.append((event, data))
    if malformed:
        print(
            f"biffix-coordinator: events replay skipped {malformed} malformed line(s) of {src}",
            file=sys.stderr,
        )
    return items


def chunks(items: list[Item]) -> Iterator[list[Item]]:
    """`items` in order, cut into chunks of at most MAX_EVENTS events and MAX_BYTES of body."""
    chunk: list[Item] = []
    size = len(HEAD) + len(TAIL)
    for item in items:
        extra = len(item[1]) + (1 if chunk else 0)
        if chunk and (len(chunk) >= MAX_EVENTS or size + extra > MAX_BYTES):
            yield chunk
            chunk, size, extra = [], len(HEAD) + len(TAIL), len(item[1])
        chunk.append(item)
        size += extra
    if chunk:
        yield chunk


def refused_of(answer: str, n: int) -> list[tuple[int, str]]:
    """The (index, reason) of each event of an `n`-event chunk a 2xx answer refuses; none when the
    answer is not the ingest summary."""
    try:
        entries = json.loads(answer).get("refused") or []
    except (ValueError, AttributeError):
        return []
    out: list[tuple[int, str]] = []
    for entry in entries:
        try:
            index = entry["index"]
            if isinstance(index, int) and 0 <= index < n:
                out.append((index, str(entry.get("reason", ""))))
        except (KeyError, TypeError, AttributeError):
            continue
    return out


def retry_after_of(e: urllib.error.HTTPError) -> float:
    """The answer's Retry-After in seconds (delta-seconds form only, at most MAX_RETRY_AFTER),
    else 0."""
    try:
        value = (e.headers.get("Retry-After") if e.headers is not None else None) or ""
        return min(max(0.0, float(int(value.strip()))), MAX_RETRY_AFTER)
    except (ValueError, TypeError, AttributeError):
        return 0.0


class Replay:
    """One replay of a file's events to `url`, giving up on ingest once it is unreachable or the
    deadline passes."""

    def __init__(self, url: str, token: str, deadline: float) -> None:
        self.url, self.token, self.deadline = url, token, deadline
        self.sent = 0
        # The POSTs the current chunk may still spend on splits (reset per top-level chunk).
        self.budget = SPLIT_BUDGET
        # Why ingest is unreachable, once it is.
        self.down: str | None = None
        # Whether the route answered that the run is cancelled.
        self.cancelled = False

    def post(self, chunk: list[Item]) -> tuple[Outcome, str]:
        """One chunk, retrying transient failures: sent / split / final / down / cancelled, and a
        detail (for sent, the answer's body)."""
        body = HEAD + b",".join(d for _, d in chunk) + TAIL
        detail = ""
        for attempt in range(ATTEMPTS):
            retry_after = 0.0
            remaining = self.deadline - time.monotonic()
            if remaining <= 0:
                return "down", "replay deadline passed"
            req = urllib.request.Request(
                self.url,
                data=body,
                method="POST",
                headers={
                    "Content-Type": "application/json",
                    "Authorization": f"Bearer {self.token}",
                },
            )
            try:
                with OPENER.open(req, timeout=min(REQUEST_TIMEOUT, remaining)) as resp:
                    try:
                        return "sent", resp.read(1024 * 1024).decode("utf-8", "replace")
                    except Exception:  # accepted either way; only the refusals go unread
                        return "sent", ""
            except urllib.error.HTTPError as e:
                try:
                    excerpt = " ".join(e.read(200).decode("utf-8", "replace").split())
                except Exception:
                    excerpt = ""
                # Match the server's answer as sent; redact only what gets logged.
                cancelled = (e.code, excerpt) == CANCELLED
                excerpt = excerpt.replace(self.token, "<token>") or str(e.reason)
                detail = f"HTTP {e.code}: {excerpt}"
                if cancelled:
                    return "cancelled", detail
                if e.code in (400, 413):
                    return "split", detail
                if e.code not in (408, 429) and e.code < 500:
                    return "final", detail
                retry_after = retry_after_of(e)
            except urllib.error.URLError as e:
                detail = f"{type(e).__name__}: {e.reason}"
            except Exception as e:  # never quote the request (a header error can carry the token)
                detail = type(e).__name__
            if attempt + 1 < ATTEMPTS:
                backoff = retry_after or RETRY_BACKOFF * 2**attempt
                time.sleep(min(backoff, max(0, self.deadline - time.monotonic())))
        return "down", detail

    def deliver(self, chunk: list[Item]) -> None:
        if self.down is not None or self.cancelled:
            return
        if self.budget <= 0:
            print(
                f"biffix-coordinator: events replay dropped {len(chunk)} event(s) the route keeps "
                "rejecting",
                file=sys.stderr,
            )
            return
        self.budget -= 1
        outcome, detail = self.post(chunk)
        if outcome == "sent":
            refused = refused_of(detail, len(chunk))
            for index, reason in refused:
                kind, key = chunk[index][0].get("kind"), chunk[index][0].get("task_key")
                reason = reason.replace(self.token, "<token>")
                print(
                    f"biffix-coordinator: events replay: the route refused one {kind} event "
                    f"({key}), dropped: {reason}",
                    file=sys.stderr,
                )
            self.sent += len(chunk) - len(refused)
        elif outcome == "split" and len(chunk) > 1:
            mid = len(chunk) // 2
            self.deliver(chunk[:mid])
            self.deliver(chunk[mid:])
        elif outcome == "split":
            kind, key = chunk[0][0].get("kind"), chunk[0][0].get("task_key")
            print(
                f"biffix-coordinator: events replay dropped one {kind} event ({key}): {detail}",
                file=sys.stderr,
            )
        elif outcome == "final":
            print(
                f"biffix-coordinator: events replay chunk of {len(chunk)} failed (swallowed): "
                f"{detail}",
                file=sys.stderr,
            )
        elif outcome == "cancelled":
            self.cancelled = True
        else:  # ingest is unreachable: stop rather than pay the retries chunk after chunk
            self.down = detail


def replay(src: str, url: str, token: str) -> None:
    """Re-POST every recorded event to `url`/events, as an at-least-once backstop."""
    deadline = time.monotonic() + DEADLINE_SECS
    items = read_events(src)
    if not items:
        return
    run = Replay(url.rstrip("/") + "/events", token, deadline)
    for chunk in chunks(items):
        run.budget = SPLIT_BUDGET
        run.deliver(chunk)
        if run.cancelled:
            print(
                "biffix-coordinator: events replay: run cancelled; the server keeps what it "
                "already has",
                file=sys.stderr,
            )
            return
        if run.down is not None:
            print(
                f"biffix-coordinator: events replay stopped, ingest unreachable (swallowed): "
                f"{run.down}",
                file=sys.stderr,
            )
            break
    print(f"biffix-coordinator: replayed {run.sent}/{len(items)} events", file=sys.stderr)


if __name__ == "__main__":
    match sys.argv[1:]:
        case ["upload", src, dest]:
            upload(src, dest)
        case ["replay", src]:
            replay(src, os.environ["BIFFIX_INGEST_URL"], os.environ["BIFFIX_INGEST_TOKEN"])
        case _:
            sys.exit(__doc__)
