"""A copy of the engine's output, capped: stdin goes on to stdout as it comes, and to <file>, of
which only the end is kept.

Usage: capped_tee.py <file>

failure.txt quotes only the end of the engine's output (run_logs.py), and the copy is on the
coordinator's own disk (its root volume, or a single-job run's instance store), so once <file>
grows past ROTATE_AT bytes it is cut back to its last KEEP bytes. This is the file's only writer,
so nothing written meanwhile is lost. A stdout that goes away (a closed pipe) stops the
forwarding, never the copy.
"""

from __future__ import annotations

import os
import sys
from typing import BinaryIO

KEEP = 256 * 1024
ROTATE_AT = 1024 * 1024
CHUNK = 64 * 1024


def cut_to_end(path: str, keep: int) -> None:
    """Rewrite `path` as its last `keep` bytes."""
    with open(path, "rb") as fh:
        fh.seek(max(0, os.fstat(fh.fileno()).st_size - keep))
        tail = fh.read()
    tmp = f"{path}.tmp"
    with open(tmp, "wb") as fh:
        fh.write(tail)
    os.replace(tmp, path)


def forward(data: bytes) -> bool:
    """Write all of `data` to stdout; False once stdout is gone."""
    view = memoryview(data)
    try:
        while view:
            view = view[os.write(1, view) :]
    except OSError:
        return False
    return True


class CappedFile:
    """The copy: appended to, and cut back to its last `keep` bytes past `rotate_at`. A write that
    fails (a full volume) ends the copy, never the forwarding."""

    def __init__(self, path: str, keep: int, rotate_at: int):
        self.path, self.keep, self.rotate_at = path, keep, rotate_at
        self.out: BinaryIO | None = None
        self.size = 0
        try:
            self.out = open(path, "ab")
            self.size = self.out.tell()
        except OSError as e:
            self._give_up(e)

    def write(self, data: bytes) -> None:
        if self.out is None:
            return
        try:
            self.out.write(data)
            self.out.flush()
            self.size += len(data)
            if self.size > self.rotate_at:
                self.out.close()
                cut_to_end(self.path, self.keep)
                self.out = open(self.path, "ab")
                self.size = self.out.tell()
        except OSError as e:
            self._give_up(e)

    def close(self) -> None:
        out, self.out = self.out, None
        _close_quietly(out)

    def _give_up(self, e: OSError) -> None:
        print(f"biffix-coordinator: no copy of the engine's output kept: {e}", file=sys.stderr)
        # Dropped before it is closed: closing flushes what the failed write left buffered, which
        # fails the same way on a full volume.
        out, self.out = self.out, None
        _close_quietly(out)


def _close_quietly(out: BinaryIO | None) -> None:
    """Close `out` (None: nothing), ignoring the error a flush of what it buffered may raise."""
    if out is None:
        return
    try:
        out.close()
    except OSError:
        pass


def copy(path: str, keep: int = KEEP, rotate_at: int = ROTATE_AT) -> None:
    """Copy stdin to stdout and to `path` until stdin ends (see the module's docstring)."""
    out = CappedFile(path, keep, rotate_at)
    to_stdout = True
    try:
        while chunk := os.read(0, CHUNK):
            if to_stdout:
                to_stdout = forward(chunk)
            out.write(chunk)
    finally:
        out.close()


def main(argv: list[str]) -> int:
    if len(argv) != 1:
        print(__doc__, file=sys.stderr)
        return 2
    copy(argv[0])
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
