"""Regression coverage for bounded traversal and consistent, safely persisted upload marks."""

from __future__ import annotations

import json
import os
import subprocess
import sys
from collections.abc import Iterator
from contextlib import contextmanager
from pathlib import Path
from typing import BinaryIO

import pytest
from test_lib import FAKES, LIB, helper, module


@pytest.mark.parametrize("level", ["shard", "task"])
def test_scan_checks_deadline_between_unfinished_directories_and_keeps_candidates(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, level: str
) -> None:
    rl = module("run_logs")
    work = tmp_path / "work"
    task = work / "aa" / "0000"
    task.mkdir(parents=True)
    (task / ".exitcode").write_text("1")
    (task / ".command.err").write_text("failed")
    for i in range(30):
        (work / (f"z{i:02d}" if level == "shard" else "aa") / f"z{i:02d}").mkdir(parents=True)
    visited = 0
    real_scandir = os.scandir

    @contextmanager
    def scandir(path: str | Path | int) -> Iterator[Iterator[os.DirEntry[str]]]:
        nonlocal visited
        with real_scandir(path) as entries:
            ordered = sorted(entries, key=lambda entry: entry.name)

        def advancing_entries() -> Iterator[os.DirEntry[str]]:
            nonlocal visited
            for entry in ordered:
                visited += 1
                yield entry

        yield advancing_entries()

    def legacy_glob(path: Path, _pattern: str) -> Iterator[Path]:
        # Reproduce Python 3.11's selector: only existing exitcodes reach the caller.
        with os.scandir(path) as shards:
            for shard in shards:
                with os.scandir(shard.path) as tasks:
                    for entry in tasks:
                        exitcode = Path(entry.path) / ".exitcode"
                        if exitcode.exists():
                            yield exitcode

    monkeypatch.setattr(Path, "glob", legacy_glob)
    monkeypatch.setattr(os, "scandir", scandir)
    monkeypatch.setattr(rl.time, "monotonic", lambda: float(visited))
    uploads = rl.failed_tasks(work, stop_at=8)
    assert [upload.name for upload in uploads] == ["tasks/aa/0000/.command.err"]
    assert visited <= 9, "unfinished directories must not exhaust the remaining upload budget"


def test_main_disarms_its_deadline_when_uploads_finish(tmp_path: Path) -> None:
    """A completed invocation must not leave an os._exit timer armed in its caller."""
    result = subprocess.run(
        [
            sys.executable,
            "-c",
            "import sys, time; import run_logs; "
            "assert run_logs.main(['s3://b/logs/', sys.argv[1], '0.2']) == 0; "
            "time.sleep(0.4); print('caller survived')",
            str(tmp_path / "state"),
        ],
        env={
            "PATH": os.environ["PATH"],
            "PYTHONPATH": f"{LIB}{os.pathsep}{FAKES}",
            "FAKE_S3": str(tmp_path / "s3"),
        },
        capture_output=True,
        text=True,
        timeout=3,
    )
    assert result.returncode == 0, result.stderr
    assert result.stdout.strip() == "caller survived"
    assert "abandoning" not in result.stderr


def test_replaced_log_with_same_size_and_mtime_is_uploaded(tmp_path: Path) -> None:
    log, state, s3 = tmp_path / "trace", tmp_path / "state", tmp_path / "s3"
    log.write_text("old!")
    initial = log.stat()

    def upload() -> subprocess.CompletedProcess[str]:
        return helper(
            "run_logs", "s3://b/logs/", str(state), "3", "--file", f"trace.txt={log}",
            FAKE_S3=str(s3),
        )  # fmt: skip

    assert "uploaded 1" in upload().stderr
    replacement = tmp_path / "replacement"
    replacement.write_text("new!")
    os.utime(replacement, ns=(initial.st_atime_ns, initial.st_mtime_ns))
    replacement.replace(log)
    assert "uploaded 1" in upload().stderr
    assert (s3 / "b/logs/trace.txt").read_text() == "new!"
    assert "uploaded" not in upload().stderr


@pytest.mark.parametrize("when", ["before_tail", "during_read"])
def test_log_growth_during_read_does_not_save_a_stale_mark(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, when: str
) -> None:
    rl = module("run_logs")
    monkeypatch.syspath_prepend(str(FAKES))
    monkeypatch.setenv("FAKE_S3", str(tmp_path / "s3"))
    log = tmp_path / "engine.log"
    log.write_bytes(b"abcd")
    initial = log.stat()
    state = tmp_path / "state"
    uploader = rl.Uploader("s3://b/logs/", str(state), max_bytes=4)
    real_tail = rl._tail

    def tail(fh: BinaryIO, max_bytes: int, *args: object) -> bytes:
        if when == "before_tail":
            log.write_bytes(b"abcdefgh")
        body: bytes = real_tail(fh, max_bytes, *args)
        if when == "during_read":
            log.write_bytes(b"abcdefgh")
        return body

    monkeypatch.setattr(rl, "_tail", tail)
    uploader.send([rl.Upload("engine.log", log)])
    assert (tmp_path / "s3/b/logs/engine.log").read_bytes() == b"abcd"
    assert "engine.log" not in json.loads(state.read_text())
    # Restore the initial bytes and mtime in the same inode: no stale mark may skip this send.
    log.write_bytes(b"abcd")
    os.utime(log, ns=(initial.st_atime_ns, initial.st_mtime_ns))
    monkeypatch.setattr(rl, "_tail", real_tail)
    uploader.send([rl.Upload("engine.log", log)])
    assert uploader.sent == 2
    uploader.send([rl.Upload("engine.log", log)])
    assert uploader.sent == 2


@pytest.mark.parametrize("kind", ["fifo", "symlink", "ancestor", "invalid", "list", "values"])
def test_unsafe_or_invalid_state_does_not_prevent_uploads(tmp_path: Path, kind: str) -> None:
    state = tmp_path / "state"
    if kind == "fifo":
        os.mkfifo(state)
    elif kind == "symlink":
        target = tmp_path / "private"
        target.write_text('{"engine.log": "untrusted"}')
        state.symlink_to(target)
    elif kind == "ancestor":
        target = tmp_path / "private"
        target.mkdir()
        (target / "state").write_text('{"engine.log": "untrusted"}')
        (tmp_path / "link").symlink_to(target, target_is_directory=True)
        state = tmp_path / "link/state"
    else:
        state.write_text({"invalid": "{", "list": "[]", "values": '{"engine.log": 1}'}[kind])
    got = subprocess.run(
        [
            sys.executable, "-c",
            "import sys; from run_logs import Uploader; "
            "u = Uploader('s3://b/logs/', sys.argv[1]); "
            "assert u.state == {}, u.state; u.failure('status only')",
            str(state),
        ],
        env={"PATH": os.environ["PATH"], "PYTHONPATH": f"{LIB}{os.pathsep}{FAKES}",
             "FAKE_S3": str(tmp_path / "s3")},
        capture_output=True, text=True, timeout=2,
    )  # fmt: skip
    assert got.returncode == 0, got.stderr
    assert (tmp_path / "s3/b/logs/failure.txt").read_text() == "status only"


@pytest.mark.parametrize("component", ["destination", "temporary", "ancestor", "dotdot"])
def test_state_writer_cannot_modify_files_through_links(tmp_path: Path, component: str) -> None:
    rl = module("run_logs")
    private = tmp_path / "private"
    private.mkdir()
    secret = private / "state"
    secret.write_text("do not overwrite")
    state = tmp_path / "state"
    if component == "destination":
        state.symlink_to(secret)
    elif component == "temporary":
        state.with_name("state.tmp").symlink_to(secret)
    else:
        (tmp_path / "link").symlink_to(private, target_is_directory=True)
        state = tmp_path / ("link/state" if component == "ancestor" else "link/../private/state")
    try:
        rl._write_state(str(state), {"engine.log": "mark"})
    except OSError:
        assert component in ("ancestor", "dotdot")
    assert secret.read_text() == "do not overwrite"
    if component in ("destination", "temporary"):
        assert json.loads(state.read_text()) == {"engine.log": "mark"}


def test_state_writer_exclusively_creates_temporary_file(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    rl = module("run_logs")
    monkeypatch.setattr(rl.secrets, "token_hex", lambda _size: "collision")
    secret = tmp_path / "secret"
    secret.write_text("private")
    temporary = tmp_path / ".state.collision.tmp"
    temporary.symlink_to(secret)
    with pytest.raises(FileExistsError):
        rl._write_state(str(tmp_path / "state"), {"log": "mark"})
    assert secret.read_text() == "private"
    assert temporary.is_symlink()
    assert not (tmp_path / "state").exists()


def test_state_writer_removes_temporary_file_after_publication_failure(tmp_path: Path) -> None:
    rl = module("run_logs")
    state = tmp_path / "state"
    state.mkdir()
    with pytest.raises(OSError):
        rl._write_state(str(state), {"log": "mark"})
    assert sorted(path.name for path in tmp_path.iterdir()) == ["state"]


def test_state_writer_pins_ancestors_against_link_substitution(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    rl = module("run_logs")
    parent = tmp_path / "parent"
    parent.mkdir()
    original = tmp_path / "original"
    private = tmp_path / "private"
    private.mkdir()
    (private / "state").write_text("private state")
    real_open, real_builtin_open = os.open, open
    swapped = False

    def swap() -> None:
        nonlocal swapped
        if not swapped:
            swapped = True
            parent.rename(original)
            parent.symlink_to(private, target_is_directory=True)

    def open_fd(
        path: str | Path, flags: int, mode: int = 0o777, *, dir_fd: int | None = None
    ) -> int:
        if flags & os.O_CREAT:
            swap()
        return real_open(path, flags, mode, dir_fd=dir_fd)

    def open_file(path: str, mode: str) -> object:
        if "w" in mode:
            swap()
        return real_builtin_open(path, mode)

    monkeypatch.setattr(os, "open", open_fd)
    monkeypatch.setattr("builtins.open", open_file)
    rl._write_state(str(parent / "state"), {"log": "mark"})
    assert (private / "state").read_text() == "private state"
    assert not (private / "state.tmp").exists()
    assert json.loads((original / "state").read_text()) == {"log": "mark"}
    assert sorted(path.name for path in original.iterdir()) == ["state"]


def test_an_attempt_that_writes_no_failure_removes_an_earlier_attempt_s_once(
    tmp_path: Path,
) -> None:
    """Batch retries a coordinator under the same run prefix (its host went away), so a failure.txt
    an earlier attempt uploaded would otherwise outlive a later attempt that succeeded or was
    cancelled, neither of which writes one. The first upload of an attempt that writes none
    removes it; later ones (the state file is the attempt's own) leave S3 alone."""
    s3, log = tmp_path / "s3", tmp_path / "engine.log"
    log.write_text("x")
    report = s3 / "b/runs/r/logs/failure.txt"
    report.parent.mkdir(parents=True)
    report.write_text("the earlier attempt's failure")
    state = tmp_path / "state.json"

    def upload(*extra: str) -> subprocess.CompletedProcess[str]:
        return helper(
            "run_logs", "s3://b/runs/r/logs/", str(state), "30", "--file", f"e.log={log}",
            *extra, FAKE_S3=str(s3),
        )  # fmt: skip

    got = upload()
    assert got.returncode == 0, got.stderr
    assert not report.exists()
    # Once per attempt: a report put back afterwards (by nothing in a real run) is left alone.
    report.write_text("put back")
    assert upload().returncode == 0
    assert report.read_text() == "put back"
    # An attempt that fails writes its own, over whatever is there.
    output = tmp_path / "output.log"
    output.write_text("boom\n")
    assert upload("--failure", str(output), "1").returncode == 0
    assert "boom" in report.read_text()


def test_a_failure_txt_that_cannot_be_removed_is_retried_by_the_next_upload(
    tmp_path: Path,
) -> None:
    s3 = tmp_path / "s3"
    report = s3 / "b/logs/failure.txt"
    report.parent.mkdir(parents=True)
    report.write_text("earlier")
    args = ["run_logs", "s3://b/logs/", str(tmp_path / "state.json"), "30"]
    got = helper(*args, FAKE_S3=str(s3), FAKE_S3_DENY_PUT="1")
    assert got.returncode == 0
    assert "removing an earlier attempt's failure.txt failed (skipped)" in got.stderr
    assert report.exists()
    assert helper(*args, FAKE_S3=str(s3)).returncode == 0
    assert not report.exists()
