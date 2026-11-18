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

A file is uploaded only when it changed since the last upload (its size and mtime, and whether only
its end was sent, recorded in the state file), and at most its last --max-bytes. failure.txt goes
first, then the named files, and only then the failed tasks' files, whose scan of the work dir
stops at half the deadline: a large work dir never keeps the rest from being sent. Best-effort and
bounded: a failed upload is logged and skipped, and the whole helper exits 0, at the latest after
<deadline secs>, so it never changes the run's outcome nor outlasts the time a cancel leaves it.
"""

from __future__ import annotations

import argparse
import glob
import json
import os
import sys
import threading
import time
from dataclasses import dataclass
from pathlib import Path

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
    files = [Path(p) for p in glob.glob(pattern) if os.path.isfile(p)]
    return max(files, key=lambda p: p.stat().st_mtime_ns, default=None)


def failed_tasks(work_dir: Path, stop_at: float | None = None) -> list[Upload]:
    """The .command.* files of the most recent tasks under `work_dir` that exited non-zero, of
    those found before `stop_at` (a time.monotonic() value; None: no limit)."""
    failed = []
    for exitcode in work_dir.glob("*/*/.exitcode"):
        if stop_at is not None and time.monotonic() > stop_at:
            print(
                f"biffix-coordinator: stopped scanning {work_dir} for failed tasks (out of time)",
                file=sys.stderr,
            )
            break
        try:
            if exitcode.read_text().strip() != "0":
                failed.append((exitcode.stat().st_mtime_ns, exitcode.parent))
        except OSError:
            continue
    failed.sort(reverse=True)
    uploads = []
    for _, task in failed[:MAX_FAILED_TASKS]:
        key = f"tasks/{task.parent.name}/{task.name}"
        uploads += [Upload(f"{key}/{f}", task / f) for f in COMMAND_FILES if (task / f).is_file()]
    return uploads


def failure_text(output: Path | None, status: str, label: str | None = None) -> str:
    """failure.txt: the status of what failed (`label`, by default the engine) and the last lines
    of its output."""
    tail = ""
    if output is not None and output.is_file():
        with open(output, "rb") as fh:
            fh.seek(max(0, output.stat().st_size - FAILURE_BYTES))
            lines = fh.read().decode(errors="replace").splitlines()[-FAILURE_LINES:]
        tail = "\n".join(lines)
    head = (
        f"{label} (exit status {status})." if label else f"The engine exited with status {status}."
    )
    return f"{head} The end of its output:\n\n{tail}\n" if tail else f"{head}\n"


def tail_bytes(path: Path, max_bytes: int = MAX_FILE_BYTES) -> bytes:
    """The file's content, or its last `max_bytes`."""
    with open(path, "rb") as fh:
        size = os.fstat(fh.fileno()).st_size
        fh.seek(max(0, size - max_bytes))
        return fh.read()


class Uploader:
    """Uploads under the prefix, skipping what is unchanged since the last call (the state file)."""

    def __init__(self, uri: str, state_file: str, max_bytes: int = MAX_FILE_BYTES):
        import boto3
        from botocore.config import Config

        self.uri, self.state_file, self.max_bytes = uri, state_file, max_bytes
        self.bucket, self.prefix = split_prefix(uri)
        try:
            with open(state_file) as fh:
                self.state: dict[str, str] = json.load(fh)
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
                st = item.path.stat()
                mark = f"{st.st_size}:{st.st_mtime_ns}"
                if st.st_size > self.max_bytes:
                    # Only its end is sent: a later call with a larger cap sends it again.
                    mark += f":last {self.max_bytes}"
                if self.state.get(item.name) == mark:
                    continue
                self.s3.put_object(
                    Bucket=self.bucket,
                    Key=self.prefix + item.name,
                    Body=tail_bytes(item.path, self.max_bytes),
                )
                self.state[item.name] = mark
                self.sent += 1
            except Exception as e:
                print(
                    f"biffix-coordinator: log upload of {item.name} failed (skipped): {e}",
                    file=sys.stderr,
                )
            # Recorded as it goes, so a later call skips what this one sent even if it is cut
            # short.
            _write_state(self.state_file, self.state)

    def report(self) -> None:
        if self.sent:
            print(
                f"biffix-coordinator: uploaded {self.sent} log file(s) to {self.uri}",
                file=sys.stderr,
            )


def _write_state(state_file: str, state: dict[str, str]) -> None:
    tmp = f"{state_file}.tmp"
    with open(tmp, "w") as fh:
        json.dump(state, fh)
    os.replace(tmp, state_file)


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
    uploads = [Upload(name, Path(path)) for name, path in args.file if os.path.isfile(path)]
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
