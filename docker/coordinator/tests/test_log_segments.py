"""Coordinator segments are immutable, retry-safe, bounded and lower priority than diagnostics."""

from __future__ import annotations

import os
import subprocess
import sys
from pathlib import Path

import pytest
from helpers import FAKES, LIB, bash, helper, module

NAME = "coordinator/attempt-1"


def upload(tmp_path: Path, *args: str, **env: str) -> subprocess.CompletedProcess[str]:
    return helper(
        "run_logs",
        "s3://b/logs/",
        str(tmp_path / "state"),
        "3",
        "--segments",
        f"{NAME}={tmp_path / 'segments'}",
        *args,
        FAKE_S3=str(tmp_path / "s3"),
        **env,
    )


def test_segments_are_uploaded_whole_then_removed(tmp_path: Path) -> None:
    segments = tmp_path / "segments"
    segments.mkdir()
    (segments / "000001.log").write_bytes(b"begin\x00middle\x1bend")
    (segments / "000002.log.open").write_bytes(b"still open")
    got = upload(tmp_path, "--max-bytes", "2")
    assert got.returncode == 0, got.stderr
    assert (tmp_path / "s3/b/logs" / NAME / "000001.log").read_bytes() == b"begin\x00middle\x1bend"
    assert not (segments / "000001.log").exists()
    assert (segments / "000002.log.open").read_bytes() == b"still open"
    assert not (tmp_path / "s3/b/logs" / NAME / "000002.log").exists()


def test_denied_segment_is_retried(tmp_path: Path) -> None:
    segments = tmp_path / "segments"
    segments.mkdir()
    segment = segments / "000001.log"
    segment.write_bytes(b"retained until acknowledged")
    first = upload(tmp_path, FAKE_S3_DENY_PUT="1")
    assert first.returncode == 0
    assert segment.read_bytes() == b"retained until acknowledged"
    second = upload(tmp_path)
    assert second.returncode == 0, second.stderr
    assert not segment.exists()
    assert (
        tmp_path / "s3/b/logs" / NAME / segment.name
    ).read_bytes() == b"retained until acknowledged"


@pytest.mark.parametrize("open_segment", [False, True])
def test_remote_commit_before_process_exit_is_not_published_again(
    tmp_path: Path,
    open_segment: bool,
) -> None:
    segments = tmp_path / "segments"
    segments.mkdir()
    segment = segments / ("000001.log.open" if open_segment else "000001.log")
    segment.write_bytes(b"original immutable payload")
    key = f"logs/{NAME}/000001.log"
    args = ("--final",) if open_segment else ()
    first = upload(tmp_path, *args, FAKE_S3_EXIT_AFTER_COMMIT=key)
    assert first.returncode == 0, first.stderr
    remote = tmp_path / "s3/b" / key
    assert remote.read_bytes() == b"original immutable payload"
    before = remote.stat()
    assert segment.exists(), "the killed uploader never acknowledged or removed its source"
    if open_segment:
        with segment.open("ab") as out:
            out.write(b"late bytes beyond final snapshot")
    second = upload(tmp_path, *args)
    assert second.returncode == 0, second.stderr
    assert "failed" not in second.stderr
    assert "uploaded 1 log file" in second.stderr
    assert remote.read_bytes() == b"original immutable payload"
    assert remote.stat().st_mtime_ns == before.st_mtime_ns
    assert remote.stat().st_ino == before.st_ino
    if not open_segment:
        assert not segment.exists()
    else:
        assert segment.read_bytes().endswith(b"late bytes beyond final snapshot")


def test_existing_different_object_is_never_overwritten_or_acknowledged(tmp_path: Path) -> None:
    segments = tmp_path / "segments"
    segments.mkdir()
    source = segments / "000001.log"
    source.write_bytes(b"new content")
    remote = tmp_path / "s3/b/logs" / NAME / source.name
    remote.parent.mkdir(parents=True)
    remote.write_bytes(b"old content")
    got = upload(tmp_path)
    assert got.returncode == 0
    assert remote.read_bytes() == b"old content"
    assert source.read_bytes() == b"new content"
    assert "failed" in got.stderr or "collision" in got.stderr


@pytest.mark.parametrize("unsafe", ["file", "directory", "fifo", "dotdot", "snapshot"])
def test_segments_do_not_follow_links_or_read_nonregular_files(tmp_path: Path, unsafe: str) -> None:
    segments = tmp_path / "segments"
    segments.mkdir()
    private = tmp_path / "private"
    private.mkdir()
    secret = private / "000001.log"
    secret.write_bytes(b"must not leak")
    if unsafe == "file":
        (segments / secret.name).symlink_to(secret)
    elif unsafe == "directory":
        segments.rmdir()
        segments.symlink_to(private, target_is_directory=True)
    elif unsafe == "fifo":
        os.mkfifo(segments / secret.name)
    elif unsafe == "snapshot":
        (segments / "000001.log.open").write_bytes(b"public")
        (segments / "000001.log.final").symlink_to(secret)
        got = upload(tmp_path, "--final")
        assert got.returncode == 0, got.stderr
        assert secret.read_bytes() == b"must not leak"
        assert not (tmp_path / "s3/b/logs" / NAME).exists()
        return
    else:
        # Replace the default path argument rather than normalize away '..'.
        got = helper(
            "run_logs",
            "s3://b/logs/",
            str(tmp_path / "state"),
            "1",
            "--segments",
            f"{NAME}={segments}/../private",
            FAKE_S3=str(tmp_path / "s3"),
        )
        assert got.returncode == 0
        assert not (tmp_path / "s3/b/logs" / NAME).exists()
        return
    got = upload(tmp_path)
    assert got.returncode == 0, got.stderr
    assert secret.read_bytes() == b"must not leak"
    assert not (tmp_path / "s3/b/logs" / NAME).exists()


@pytest.mark.parametrize("final", [False, True])
def test_diagnostic_priority_and_segment_order(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    final: bool,
) -> None:
    monkeypatch.syspath_prepend(str(FAKES))
    monkeypatch.setenv("FAKE_S3", str(tmp_path / "s3"))
    rl = module("run_logs")
    segments = tmp_path / "segments"
    segments.mkdir()
    for number in [1, 2, 3]:
        (segments / f"{number:06d}.log").write_text(f"segment {number}")
    (segments / "000004.log.open").write_text("final open")
    log = tmp_path / "engine.log"
    log.write_text("engine log")
    task = tmp_path / "work/aa/task"
    task.mkdir(parents=True)
    (task / ".exitcode").write_text("7")
    (task / ".command.err").write_text("task failure")
    probe = rl.Uploader("s3://b/logs/", str(tmp_path / "state"))
    client_type = type(probe.s3)
    real_put = client_type.put_object
    sent: list[str] = []

    def put(self: object, **kwargs: object) -> None:
        sent.append(str(kwargs["Key"]))
        real_put(self, **kwargs)

    monkeypatch.setattr(client_type, "put_object", put)
    args = [
        "s3://b/logs/",
        str(tmp_path / "state"),
        "3",
        "--failure",
        str(log),
        "7",
        "--file",
        f"engine.log={log}",
        "--failed-tasks",
        str(tmp_path / "work"),
        "--segments",
        f"{NAME}={segments}",
    ]
    if final:
        args.append("--final")
    assert rl.main(args) == 0
    assert sent[:3] == ["logs/failure.txt", "logs/engine.log", "logs/tasks/aa/task/.command.err"]
    numbers = [4, 3, 2, 1] if final else [1, 2, 3]
    assert sent[3:] == [f"logs/{NAME}/{n:06d}.log" for n in numbers]


@pytest.mark.parametrize("attempt", ["", "3"])
def test_logs_command_names_capture_and_independent_state(tmp_path: Path, attempt: str) -> None:
    got = bash(
        'logs_command final; printf "%s\\0" "${LOGS_CMD[@]}"',
        "logs",
        WORK=str(tmp_path / "work"),
        SRC=str(tmp_path / "src"),
        BIFFIX_PROJECT_BUCKET="bucket",
        BIFFIX_RUN_PREFIX="runs/r/",
        CAPTURE_DIR=str(tmp_path / "capture"),
        AWS_BATCH_JOB_ATTEMPT=attempt,
        RUN_LOGS_STATE_FILE=str(tmp_path / "capture/state"),
    )
    assert got.returncode == 0, got.stderr
    args = got.stdout.rstrip("\0").split("\0")
    assert args[2] == str(tmp_path / "capture/state")
    assert args[args.index("--segments") + 1] == (
        f"coordinator/attempt-{attempt or '1'}={tmp_path / 'capture'}"
    )
    assert "--final" in args
    assert "--failure" not in args


@pytest.mark.parametrize("source", ["none", "engine", "setup", "signal"])
def test_failure_source_is_explicit(tmp_path: Path, source: str) -> None:
    got = bash(
        'logs_command final; printf "%s\\0" "${LOGS_CMD[@]}"',
        "logs",
        WORK=str(tmp_path),
        SRC=str(tmp_path),
        BIFFIX_PROJECT_BUCKET="bucket",
        BIFFIX_RUN_PREFIX="runs/r/",
        rc="7",
        ENGINE_DONE="1" if source == "engine" else "0",
        FAILURE_LABEL="setup failed" if source == "setup" else "",
        FAILURE_FILE=str(tmp_path / "failure"),
        ENGINE_STOPPED_BY="143" if source == "signal" else "",
    )
    assert got.returncode == 0, got.stderr
    args = got.stdout.rstrip("\0").split("\0")
    assert "--segments" not in args
    if source in ["engine", "setup"]:
        index = args.index("--failure")
        assert args[index + 1 : index + 3] == [str(tmp_path / "failure"), "7"]
    else:
        assert "--failure" not in args


def test_vanished_segment_is_skipped_quietly(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    capsys: pytest.CaptureFixture[str],
) -> None:
    monkeypatch.syspath_prepend(str(FAKES))
    monkeypatch.setenv("FAKE_S3", str(tmp_path / "s3"))
    rl = module("run_logs")
    directory = tmp_path / "segments"
    directory.mkdir()
    segment = directory / "000001.log"
    segment.write_bytes(b"pruned concurrently")
    original = rl._segment_candidates

    def candidates(fd: int, final: bool) -> list[str]:
        found: list[str] = original(fd, final)
        segment.unlink()
        return found

    monkeypatch.setattr(rl, "_segment_candidates", candidates)
    uploader = rl.Uploader("s3://b/logs/", str(tmp_path / "state"))
    uploader.send_segments(NAME, directory)
    assert uploader.sent == 0
    assert "failed" not in capsys.readouterr().err


def test_deadline_during_segment_put_keeps_all_priority_artifacts(tmp_path: Path) -> None:
    segments = tmp_path / "segments"
    segments.mkdir()
    (segments / "000001.log").write_bytes(b"large low priority log")
    engine = tmp_path / "engine"
    engine.write_text("engine failure")
    task = tmp_path / "work/aa/task"
    task.mkdir(parents=True)
    (task / ".exitcode").write_text("7")
    (task / ".command.err").write_text("task failure")
    code = """
import sys
import time
import boto3
import run_logs
client = type(boto3.client("s3"))
original = client.put_object
def put(self: object, **kwargs: object) -> None:
    if kwargs.get("IfNoneMatch"):
        time.sleep(5)
    original(self, **kwargs)
client.put_object = put
sys.exit(run_logs.main(sys.argv[1:]))
"""
    got = subprocess.run(
        [
            sys.executable,
            "-c",
            code,
            "s3://b/logs/",
            str(tmp_path / "state"),
            "1",
            "--failure",
            str(engine),
            "7",
            "--file",
            f"engine.log={engine}",
            "--failed-tasks",
            str(tmp_path / "work"),
            "--segments",
            f"{NAME}={segments}",
        ],
        env={
            "PATH": os.environ["PATH"],
            "PYTHONPATH": f"{LIB}{os.pathsep}{FAKES}",
            "FAKE_S3": str(tmp_path / "s3"),
        },
        capture_output=True,
        text=True,
        timeout=4,
    )
    assert got.returncode == 0, got.stderr
    root = tmp_path / "s3/b/logs"
    assert (root / "failure.txt").is_file(), "failure report must precede slow segments"
    assert (root / "failure.txt").read_text().startswith("The engine exited with status 7")
    assert (root / "engine.log").read_bytes() == engine.read_bytes()
    assert (root / "tasks/aa/task/.command.err").read_text() == "task failure"
    assert not (root / NAME / "000001.log").exists()
    assert (segments / "000001.log").exists()
    assert "abandoning" in got.stderr
