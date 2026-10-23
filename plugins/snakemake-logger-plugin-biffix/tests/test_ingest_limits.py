"""The poster's limits must equal the ones biffix-core defines.

biffix-core's ``ingest_limits`` is the one source of the ingest route's limits and the poster
policy both engine plugins follow; ``cargo xtask emit-schemas`` writes it to
``schemas/ingest-limits.json``. This reads that file (this test lives in the biffix repo next to
it; a missing file fails rather than skips) and checks every constant ``poster.py`` mirrors, and
that the file names no value this test does not check.
"""

import json
from pathlib import Path
from typing import Any

from snakemake_logger_plugin_biffix import poster

LIMITS = Path(__file__).resolve().parents[3] / "schemas" / "ingest-limits.json"


def _limits() -> dict[str, Any]:
    assert LIMITS.exists(), f"ingest limits not found at {LIMITS}"
    limits: dict[str, Any] = json.loads(LIMITS.read_text())
    return limits


def test_the_route_limits_match_biffix_core() -> None:
    route = _limits()["route"]
    assert set(route) == {"max_batch_events", "max_body_bytes"}
    assert poster.MAX_BATCH == route["max_batch_events"]
    assert poster.ROUTE_MAX_BODY_BYTES == route["max_body_bytes"]


def test_the_poster_policy_matches_biffix_core() -> None:
    policy = _limits()["poster"]
    # Seconds here, milliseconds in the file.
    want = {
        "max_batch_bytes": poster.MAX_BATCH_BYTES,
        "max_buffered_events": poster.MAX_BUFFERED,
        "max_attempts": poster.MAX_ATTEMPTS,
        "request_timeout_ms": poster.REQUEST_TIMEOUT * 1000,
        "flush_interval_ms": poster.FLUSH_INTERVAL * 1000,
        "retry_backoff_ms": poster.RETRY_BACKOFF * 1000,
        "max_retry_after_ms": poster.MAX_RETRY_AFTER * 1000,
        "close_timeout_ms": poster.CLOSE_TIMEOUT * 1000,
        "drop_warn_interval_ms": poster.DROP_WARN_INTERVAL * 1000,
        "split_post_budget": poster.SPLIT_POST_BUDGET,
        "max_outputs_per_event": poster.MAX_OUTPUTS_PER_EVENT,
        "keep_under_pressure": sorted(poster.KEEP_UNDER_PRESSURE),
    }
    got = dict(policy, keep_under_pressure=sorted(policy["keep_under_pressure"]))
    assert got == want


def test_the_poster_s_defaults_are_the_policy() -> None:
    p = poster.IngestPoster(None, None)
    assert p._timeout == poster.REQUEST_TIMEOUT
    assert p._max_batch == poster.MAX_BATCH
    assert p._flush_interval == poster.FLUSH_INTERVAL
    assert p._retry_backoff == poster.RETRY_BACKOFF
    assert p._close_timeout == poster.CLOSE_TIMEOUT
    assert p._max_batch_bytes == poster.MAX_BATCH_BYTES
