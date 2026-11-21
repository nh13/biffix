"""The run's logs, uploaded under its own prefix (`<run prefix>logs/`) while the engine runs and
once at exit, so a run's engine log outlives the container and can be read without the job's
CloudWatch stream.

Usage: run_logs.py <s3 prefix> <state file> <deadline secs> [--file NAME=PATH]... \
           [--newest NAME=GLOB]... [--failed-tasks WORK_DIR] [--failure PATH STATUS] \
           [--failure-label TEXT] [--max-bytes N]

  --file NAME=PATH     upload PATH as <prefix>NAME, when it exists
  --newest NAME=GLOB   upload the most recently modified match of GLOB as <prefix>NAME
  --failed-tasks DIR   a LOCAL Nextflow work dir: upload the .command.* files of the tasks whose
                       .exitcode is not 0 (the MAX_FAILED_TASKS most recent) as
                       <prefix>tasks/<ab>/<hash>/<file>
  --failure PATH RC    write failure.txt: the engine's exit status and the end of PATH (its output)
  --failure-label TEXT what failed, when not the engine (failure.txt then says "TEXT (exit status
                       RC).")
  --max-bytes N        upload at most a file's last N bytes (default MAX_FILE_BYTES)

A file is uploaded only when it changed since the last upload (its identity, size and mtime, and
whether only its end was sent, recorded in the state file), and at most its last --max-bytes.
failure.txt goes first, then the named files, and only then the failed tasks' files, whose scan of
the work dir stops at half the deadline: a large work dir never keeps the rest from being sent.
Best-effort and bounded: a failed upload is logged and skipped, and the whole helper exits 0, at
the latest after <deadline secs>, so it never changes the run's outcome nor outlasts the time a
cancel leaves it.
"""

from __future__ import annotations

import argparse
import glob
import heapq
import json
import os
import secrets
import stat
import sys
import threading
import time
from collections.abc import Iterator
from contextlib import ExitStack, contextmanager
from dataclasses import dataclass
from pathlib import Path
from typing import BinaryIO

from s3_uri import split_prefix

# A log's last bytes, when it is longer: what explains an end is at the end. The periodic uploads
# pass less (lib/logs.sh): each re-sends a growing log whole, from memory.
MAX_FILE_BYTES = 64 * 1024 * 1024
MAX_FAILED_TASKS = 20
COMMAND_FILES = (".command.sh", ".command.run", ".command.out", ".command.err", ".command.log")
# failure.txt: the engine's last lines.
FAILURE_LINES, FAILURE_BYTES = 200, 64 * 1024


@dataclass(frozen=True)
class Upload:
    """One file to upload, as the key under the prefix."""

    name: str
    path: Path


def newest(pattern: str) -> Path | None:
    """The most recently modified file matching `pattern`, if any."""
    found: Path | None = None
    modified = -1
    for match in glob.glob(pattern):
        path = Path(match)
        try:
            st = _file_stat(path)
        except OSError:
            continue
        if found is None or st.st_mtime_ns > modified:
            found, modified = path, st.st_mtime_ns
    return found


@contextmanager
def _open_directory(path: Path) -> Iterator[int]:
    """Pin a directory and every ancestor without following links or accepting '..'."""
    parts = path.absolute().parts
    if ".." in parts:
        raise OSError(f"log path contains '..': {path}")
    directory_flags = os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW
    with ExitStack() as stack:
        directory = os.open(parts[0], directory_flags)
        stack.callback(os.close, directory)
        for part in parts[1:]:
            directory = os.open(part, directory_flags, dir_fd=directory)
            stack.callback(os.close, directory)
        yield directory


@contextmanager
def _open_regular(path: Path) -> Iterator[BinaryIO]:
    """Open a regular log without following links, including in its parent directories.

    Directory-relative opens pin each ancestor against concurrent link substitution. Nonblocking
    open lets fstat reject a FIFO without waiting for a writer. Workflow-owned logs are untrusted;
    symlinked ancestors and '..' are rejected, not resolved (including in the configured root).
    """
    if ".." in path.parts:
        raise OSError(f"log path contains '..': {path}")
    with _open_directory(path.parent) as directory, ExitStack() as stack:
        fd = os.open(path.name, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK, dir_fd=directory)
        stack.callback(os.close, fd)
        if not stat.S_ISREG(os.fstat(fd).st_mode):
            raise OSError(f"not a regular log file: {path}")
        with os.fdopen(fd, "rb", closefd=False) as fh:
            yield fh


def _file_stat(path: Path) -> os.stat_result:
    """Stat the same no-follow regular file that an upload would open."""
    with _open_regular(path) as fh:
        return os.fstat(fh.fileno())


def failed_tasks(work_dir: Path, stop_at: float | None = None) -> list[Upload]:
    """The .command.* files of the most recent tasks under `work_dir` that exited non-zero, of
    those found before `stop_at` (a time.monotonic() value; None: no limit)."""
    failed: list[tuple[int, Path]] = []
    for exitcode in _task_exitcodes(work_dir, stop_at):
        try:
            with _open_regular(exitcode) as fh:
                if fh.read(64).strip() != b"0":
                    heapq.heappush(failed, (os.fstat(fh.fileno()).st_mtime_ns, exitcode.parent))
                    if len(failed) > MAX_FAILED_TASKS:
                        heapq.heappop(failed)
        except OSError:
            continue
    failed.sort(reverse=True)
    uploads = []
    for _, task in failed[:MAX_FAILED_TASKS]:
        key = f"tasks/{task.parent.name}/{task.name}"
        for name in COMMAND_FILES:
            path = task / name
            try:
                _file_stat(path)
            except OSError:
                continue
            uploads.append(Upload(f"{key}/{name}", path))
    return uploads


def _task_exitcodes(work_dir: Path, stop_at: float | None) -> Iterator[Path]:
    """Visit both work-directory levels with a deadline, even for unfinished tasks."""

    def expired() -> bool:
        if stop_at is None or time.monotonic() < stop_at:
            return False
        print(
            f"biffix-coordinator: stopped scanning {work_dir} for failed tasks (out of time)",
            file=sys.stderr,
        )
        return True

    try:
        with _open_directory(work_dir) as root, os.scandir(root) as shards:
            for shard in shards:
                if expired():
                    return
                try:
                    fd = os.open(
                        shard.name, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=root
                    )
                    with ExitStack() as stack:
                        stack.callback(os.close, fd)
                        with os.scandir(fd) as tasks:
                            for task in tasks:
                                if expired():
                                    return
                                yield work_dir / shard.name / task.name / ".exitcode"
                except OSError:
                    continue
    except OSError:
        return


def failure_text(output: Path | None, status: str, label: str | None = None) -> str:
    """failure.txt: the status of what failed (`label`, by default the engine) and the last lines
    of its output."""
    tail = ""
    if output is not None:
        try:
            lines = tail_bytes(output, FAILURE_BYTES).decode(errors="replace").splitlines()
            tail = "\n".join(lines[-FAILURE_LINES:])
        except OSError:
            pass  # An unreadable log must not prevent the status-only failure.txt.
    head = (
        f"{label} (exit status {status})." if label else f"The engine exited with status {status}."
    )
    return f"{head} The end of its output:\n\n{tail}\n" if tail else f"{head}\n"


def tail_bytes(path: Path, max_bytes: int = MAX_FILE_BYTES) -> bytes:
    """The file's content, or its last `max_bytes`."""
    with _open_regular(path) as fh:
        return _tail(fh, max_bytes)


def _tail(fh: BinaryIO, max_bytes: int, size: int | None = None) -> bytes:
    """Read a bounded tail ending at the initial observation, not at a later append."""
    if size is None:
        size = os.fstat(fh.fileno()).st_size
    fh.seek(max(0, size - max_bytes))
    return fh.read(min(size, max_bytes))


class Uploader:
    """Uploads under the prefix, skipping what is unchanged since the last call (the state file)."""

    def __init__(self, uri: str, state_file: str, max_bytes: int = MAX_FILE_BYTES):
        import boto3
        from botocore.config import Config

        self.uri, self.state_file, self.max_bytes = uri, state_file, max_bytes
        self.bucket, self.prefix = split_prefix(uri)
        try:
            with _open_regular(Path(state_file)) as fh:
                loaded = json.load(fh)
            self.state: dict[str, str] = (
                loaded
                if isinstance(loaded, dict)
                and all(isinstance(k, str) and isinstance(v, str) for k, v in loaded.items())
                else {}
            )
        except (OSError, ValueError):
            self.state = {}
        self.s3 = boto3.client(
            "s3", config=Config(connect_timeout=5, read_timeout=15, retries={"max_attempts": 2})
        )
        self.sent = 0

    def failure(self, text: str) -> None:
        """Upload failure.txt."""
        try:
            self.s3.put_object(
                Bucket=self.bucket, Key=self.prefix + "failure.txt", Body=text.encode()
            )
            self.sent += 1
        except Exception as e:
            print(f"biffix-coordinator: failure.txt upload failed (skipped): {e}", file=sys.stderr)

    def send(self, uploads: list[Upload]) -> None:
        """Upload each file that changed since it was last sent whole (or its end, as now)."""
        for item in uploads:
            try:
                with _open_regular(item.path) as fh:
                    st = os.fstat(fh.fileno())
                    mark = f"{st.st_dev}:{st.st_ino}:{st.st_size}:{st.st_mtime_ns}"
                    if st.st_size > self.max_bytes:
                        # Only its end is sent: a later call with a larger cap sends it again.
                        mark += f":last {self.max_bytes}"
                    if self.state.get(item.name) == mark:
                        continue
                    body = _tail(fh, self.max_bytes, st.st_size)
                    after = os.fstat(fh.fileno())
                    stable = (
                        st.st_size == after.st_size
                        and st.st_mtime_ns == after.st_mtime_ns
                        and st.st_ctime_ns == after.st_ctime_ns
                        and len(body) == min(st.st_size, self.max_bytes)
                    )
                self.s3.put_object(
                    Bucket=self.bucket,
                    Key=self.prefix + item.name,
                    Body=body,
                )
                if stable:
                    self.state[item.name] = mark
                else:
                    self.state.pop(item.name, None)
                self.sent += 1
            except Exception as e:
                print(
                    f"biffix-coordinator: log upload of {item.name} failed (skipped): {e}",
                    file=sys.stderr,
                )
            # Recorded as it goes, so a later call skips what this one sent even if it is cut
            # short. A state file that cannot be written only means a later call sends again.
            try:
                _write_state(self.state_file, self.state)
            except OSError as e:
                print(
                    f"biffix-coordinator: could not record the logs sent ({self.state_file}): {e}",
                    file=sys.stderr,
                )

    def report(self) -> None:
        if self.sent:
            print(
                f"biffix-coordinator: uploaded {self.sent} log file(s) to {self.uri}",
                file=sys.stderr,
            )


def _write_state(state_file: str, state: dict[str, str]) -> None:
    """Atomically replace state via an exclusive temporary file in a pinned, no-follow parent."""
    path = Path(state_file)
    with _open_directory(path.parent) as directory:
        tmp = f".{path.name}.{secrets.token_hex(16)}.tmp"
        fd = os.open(
            tmp, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600, dir_fd=directory
        )
        try:
            with ExitStack() as stack:
                stack.callback(os.close, fd)
                with os.fdopen(fd, "w", closefd=False) as fh:
                    json.dump(state, fh)
            os.replace(tmp, path.name, src_dir_fd=directory, dst_dir_fd=directory)
        finally:
            try:
                os.unlink(tmp, dir_fd=directory)
            except FileNotFoundError:
                pass


def _pair(value: str) -> tuple[str, str]:
    name, sep, path = value.partition("=")
    if not sep or not name or not path:
        raise argparse.ArgumentTypeError(f"not NAME=PATH: {value}")
    return name, path


def parse(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(prog="run_logs.py")
    parser.add_argument("uri")
    parser.add_argument("state_file")
    parser.add_argument("deadline", type=float)
    parser.add_argument("--file", type=_pair, action="append", default=[])
    parser.add_argument("--newest", type=_pair, action="append", default=[])
    parser.add_argument("--failed-tasks")
    parser.add_argument("--failure", nargs=2, metavar=("PATH", "STATUS"))
    parser.add_argument("--failure-label")
    parser.add_argument("--max-bytes", type=int, default=MAX_FILE_BYTES)
    return parser.parse_args(argv)


def collect(args: argparse.Namespace) -> list[Upload]:
    """The named files (--file, --newest) that exist now."""
    uploads = []
    for name, path in args.file:
        try:
            _file_stat(Path(path))
        except OSError:
            continue
        uploads.append(Upload(name, Path(path)))
    for name, pattern in args.newest:
        found = newest(pattern)
        if found is not None:
            uploads.append(Upload(name, found))
    return uploads


def main(argv: list[str]) -> int:
    args = parse(argv)
    started = time.monotonic()
    # The hard bound: whatever is still in flight at the deadline is abandoned.
    timer = threading.Timer(args.deadline, _give_up, args=(args.deadline,))
    timer.daemon = True
    timer.start()
    try:
        uploader = Uploader(args.uri, args.state_file, args.max_bytes)
        # First what a reader of a failed run wants, then the named files, which are cheap to
        # find; the failed tasks last, as finding them scans the whole work dir.
        if args.failure:
            path, status = args.failure
            uploader.failure(failure_text(Path(path) if path else None, status, args.failure_label))
        uploader.send(collect(args))
        if args.failed_tasks:
            stop_at = started + args.deadline / 2
            uploader.send(failed_tasks(Path(args.failed_tasks), stop_at))
        uploader.report()
    except Exception as e:  # never fail the run on its logs
        print(f"biffix-coordinator: log upload failed (swallowed): {e}", file=sys.stderr)
    finally:
        timer.cancel()
    return 0


def _give_up(deadline: float) -> None:
    print(
        f"biffix-coordinator: log upload still running after {deadline:g}s; abandoning it",
        file=sys.stderr,
    )
    sys.stderr.flush()
    os._exit(0)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
