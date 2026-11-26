"""Forward coordinator output and seal small immutable archive segments.

Only the copier writes this private directory; the uploader removes sealed files. A storage
failure disables archival, never forwarding. Process death may lose an in-flight chunk; this is
best-effort capture, not a transactional transport.
"""

from __future__ import annotations

import argparse
import math
import os
import re
import select
import sys
import time
from pathlib import Path
from typing import BinaryIO

from capped_tee import forward

CHUNK = 64 * 1024
SEGMENT_BYTES = 4 * 1024 * 1024
SEALED = re.compile(r"[0-9]{6,}\.log\Z")


def warn(text: str) -> None:
    """Report directly to the original job log, tolerating a closed diagnostic stream."""
    try:
        print(f"biffix-coordinator: {text}", file=sys.stderr)
    except OSError:
        pass


class Segments:
    """One segment writer, reserving sequence numbers before creating their files."""

    def __init__(self, directory: str, segment_bytes: int, seal_secs: float, max_local: int):
        self.directory = Path(directory)
        self.segment_bytes, self.seal_secs, self.max_local = segment_bytes, seal_secs, max_local
        self.out: BinaryIO | None = None
        self.path: Path | None = None
        self.size = 0
        self.deadline: float | None = None
        counter = self.directory / ".next"
        self.number = int(counter.read_text()) if counter.exists() else 1
        if self.number < 1:
            raise ValueError("invalid segment counter")
        # A dead copier's already-written bytes are no longer mutable. Preserve those too.
        for path in self.directory.glob("*.log.open"):
            if SEALED.fullmatch(path.name.removesuffix(".open")):
                if path.stat().st_size:
                    path.rename(path.with_suffix(""))
                else:
                    path.unlink()
        self.prune()

    def _open(self) -> None:
        counter = self.directory / ".next.tmp"
        counter.write_text(str(self.number + 1))
        counter.replace(self.directory / ".next")
        self.path = self.directory / f"{self.number:06d}.log.open"
        self.out = self.path.open("xb", buffering=0)
        self.number += 1
        self.size = 0
        self.deadline = time.monotonic() + self.seal_secs

    def write(self, data: bytes) -> None:
        """Append a chunk, sealing at exact segment boundaries without extra buffering."""
        view = memoryview(data)
        while view:
            if self.out is None:
                self._open()
            assert self.out is not None
            count = self.out.write(view[: self.segment_bytes - self.size])
            if not count:
                raise OSError("segment write made no progress")
            self.size += count
            view = view[count:]
            if self.size == self.segment_bytes:
                self.seal()

    def seal(self) -> None:
        """Close and publish the current segment; an empty stream never creates one."""
        if self.out is None:
            return
        out, self.out = self.out, None
        out.close()
        assert self.path is not None
        self.path.rename(self.path.with_suffix(""))
        self.deadline = None
        self.prune()

    def prune(self) -> None:
        """Drop oldest queued segments at the disk cap; concurrent uploader removal is harmless."""
        paths = sorted(
            (p for p in self.directory.iterdir() if SEALED.fullmatch(p.name)),
            key=lambda p: int(p.stem),
        )
        for path in paths[: max(0, len(paths) - self.max_local)]:
            try:
                path.unlink()
            except FileNotFoundError:
                continue
            warn(f"coordinator archive backlog full: dropping {path.name}")

    def abandon(self) -> None:
        """Close a failed writer without attempting further storage operations."""
        out, self.out = self.out, None
        self.deadline = None
        if out is not None:
            try:
                out.close()
            except OSError:
                pass


def copy(
    directory: str,
    segment_bytes: int,
    seal_secs: float,
    max_local: int,
    *,
    input_fd: int = 0,
) -> None:
    """Forward input until EOF, sealing by size or age even when input becomes quiet."""
    segments: Segments | None = None
    try:
        segments = Segments(directory, segment_bytes, seal_secs, max_local)
    except (OSError, ValueError) as e:
        warn(f"coordinator archive disabled: {e}")
    to_stdout = True
    while True:
        timeout = None
        if segments is not None and segments.deadline is not None:
            timeout = max(0.0, segments.deadline - time.monotonic())
        readable, _, _ = select.select([input_fd], [], [], timeout)
        data = os.read(input_fd, CHUNK) if readable else None
        if data and to_stdout:
            to_stdout = forward(data)
        if segments is not None:
            try:
                if data:
                    segments.write(data)
                if data == b"" or (
                    segments.deadline is not None and time.monotonic() >= segments.deadline
                ):
                    segments.seal()
            except (OSError, ValueError) as e:
                warn(f"coordinator archive disabled: {e}")
                segments.abandon()
                segments = None
        if data == b"":
            break


def positive_int(text: str) -> int:
    """An integer limit greater than zero."""
    value = int(text)
    if value <= 0:
        raise argparse.ArgumentTypeError("must be positive")
    return value


def positive_seconds(text: str) -> float:
    """A finite, positive interval."""
    value = float(text)
    if not math.isfinite(value) or value <= 0:
        raise argparse.ArgumentTypeError("must be finite and positive")
    return value


def main(argv: list[str]) -> int:
    """Parse limits and forward stdin to stdout and the archive directory."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("directory")
    parser.add_argument("--segment-bytes", type=positive_int, default=SEGMENT_BYTES)
    parser.add_argument("--seal-secs", type=positive_seconds, default=60.0)
    parser.add_argument("--max-local", type=positive_int, default=64)
    args = parser.parse_args(argv)
    copy(args.directory, args.segment_bytes, args.seal_secs, args.max_local)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
