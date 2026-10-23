# Portions of this module are adapted from snakesee (https://github.com/fg-labs/snakesee), its
# snakemake-logger-plugin-snakesee/src/snakemake_logger_plugin_snakesee/writer.py.
# snakesee is Copyright (c) 2024 Fulcrum Genomics LLC and licensed under the MIT License; see the
# NOTICE section of this package's README.md for its license text.
"""Durable ``events.jsonl`` writer (the archival replay copy).

Adapted from snakesee's EventWriter (no snakesee dependency). JSONL, one biffix EngineEvent dict per
line, appended as events are emitted. The coordinator uploads it and replays it through ingest as
just another producer (idempotent via the log's dedup, which the writer of record computes
downstream).

The file covers ONE Snakemake invocation: the handler truncates it at WORKFLOW_STARTED, so it never
mixes a previous invocation's stale events into this one. A coordinator retry is a fresh container,
and any events the previous attempt's replay did not deliver are not carried over.

Every event is flushed as it is written, so a crash of the Snakemake process loses at most the
line in flight. The event volume (one line per task-lifecycle transition) never makes per-event
flushing a bottleneck. No buffering knob exists, because buffering would defeat the file's purpose.
"""

from __future__ import annotations

import json
import threading
from pathlib import Path
from typing import IO


class EventWriter:
    def __init__(self, path: Path) -> None:
        self._path = Path(path)
        self._lock = threading.Lock()
        self._fh: IO[str] | None = None

    def _ensure_open(self) -> IO[str]:
        """The open append handle, opening it (and its parent directory) on first use."""
        if self._fh is None:
            self._path.parent.mkdir(parents=True, exist_ok=True)
            self._fh = open(self._path, "a", encoding="utf-8")
        return self._fh

    def write(self, event: dict[str, object]) -> None:
        line = json.dumps(event, separators=(",", ":"))
        with self._lock:
            fh = self._ensure_open()
            fh.write(line + "\n")
            fh.flush()

    def _close_locked(self) -> None:
        if self._fh is not None:
            self._fh.close()
            self._fh = None

    def truncate(self) -> None:
        """Clear stale events from a previous invocation (called on workflow_started)."""
        with self._lock:
            self._close_locked()
            self._path.parent.mkdir(parents=True, exist_ok=True)
            self._fh = open(self._path, "w", encoding="utf-8")

    def close(self) -> None:
        with self._lock:
            self._close_locked()
