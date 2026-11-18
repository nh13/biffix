"""Run-log collection tolerates disappearing files and never follows workflow-owned links."""

from __future__ import annotations

import builtins
import json
import os
import subprocess
import sys
from pathlib import Path

import pytest
from executables import python_executable
from test_lib import LIB, bash, helper, module


def test_collect_skips_a_vanished_newest_candidate(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    rl = module("run_logs")
    good, gone = tmp_path / "good.log", tmp_path / "gone.log"
    good.write_text("keep me")
    gone.write_text("gone")
    real_stat, real_open = Path.stat, os.open

    def stat(path: Path, *, follow_symlinks: bool = True) -> os.stat_result:
        if path == gone:
            raise FileNotFoundError(str(path))
        return real_stat(path, follow_symlinks=follow_symlinks)

    def open_fd(
        path: str | Path, flags: int, mode: int = 0o777, *, dir_fd: int | None = None
    ) -> int:
        if Path(path).name == gone.name:
            raise FileNotFoundError(str(path))
        return real_open(path, flags, mode, dir_fd=dir_fd)

    monkeypatch.setattr(Path, "stat", stat)
    monkeypatch.setattr(os, "open", open_fd)
    args = rl.parse(
        ["s3://b/logs/", str(tmp_path / "state"), "5", "--newest", f"engine.log={tmp_path}/*.log"]
    )
    assert [(item.name, item.path) for item in rl.collect(args)] == [("engine.log", good)]


def test_unreadable_failure_output_still_returns_the_status(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    rl = module("run_logs")
    output = tmp_path / "output"
    output.write_text("not readable")
    real_open = os.open

    def open_fd(
        path: str | Path, flags: int, mode: int = 0o777, *, dir_fd: int | None = None
    ) -> int:
        if Path(path).name == output.name:
            raise PermissionError(str(path))
        return real_open(path, flags, mode, dir_fd=dir_fd)

    def unreadable(*_args: object, **_kwargs: object) -> None:
        raise PermissionError(str(output))

    monkeypatch.setattr(os, "open", open_fd)
    monkeypatch.setattr(builtins, "open", unreadable)
    assert rl.failure_text(output, "2", "Setup failed") == "Setup failed (exit status 2).\n"


def _task(work: Path) -> Path:
    task = work / "ab" / "1234"
    task.mkdir(parents=True)
    (task / ".exitcode").write_text("1")
    (task / ".command.err").write_text("ordinary task error")
    return task


def _upload(tmp_path: Path, *args: str) -> Path:
    s3 = tmp_path / "s3"
    got = helper("run_logs", "s3://b/logs/", str(tmp_path / "state"), "3", *args, FAKE_S3=str(s3))
    assert got.returncode == 0, got.stderr
    return s3 / "b/logs"


def test_symlinked_task_file_is_skipped_without_losing_other_logs(tmp_path: Path) -> None:
    work = tmp_path / "work"
    task = _task(work)
    secret = tmp_path / "environ"
    secret.write_bytes(b"BIFFIX_INGEST_TOKEN=secret\0AWS_SECRET_ACCESS_KEY=secret\0")
    (task / ".command.out").symlink_to(secret)
    logs = _upload(tmp_path, "--failed-tasks", str(work))
    assert (logs / "tasks/ab/1234/.command.err").read_text() == "ordinary task error"
    assert not (logs / "tasks/ab/1234/.command.out").exists()


@pytest.mark.parametrize("component", ["work", "shard", "task", "exitcode"])
def test_symlinked_task_ancestors_and_exitcode_are_rejected(tmp_path: Path, component: str) -> None:
    work = tmp_path / "work"
    task = _task(work)
    link = {"work": work, "shard": task.parent, "task": task, "exitcode": task / ".exitcode"}[
        component
    ]
    target = tmp_path / "outside"
    link.rename(target)
    link.symlink_to(target, target_is_directory=component != "exitcode")
    named = tmp_path / "named.log"
    named.write_text("keep uploading")
    logs = _upload(tmp_path, "--failed-tasks", str(work), "--file", f"engine.log={named}")
    assert (logs / "engine.log").read_text() == "keep uploading"
    assert not (logs / "tasks").exists()


@pytest.mark.parametrize("component", ["file", "directory"])
def test_a_link_substituted_after_collection_cannot_be_read(tmp_path: Path, component: str) -> None:
    rl = module("run_logs")
    task = _task(tmp_path / "work")
    uploads = rl.failed_tasks(tmp_path / "work")
    assert len(uploads) == 1
    secret_dir = tmp_path / "secret"
    secret_dir.mkdir()
    (secret_dir / ".command.err").write_text("credentials")
    if component == "file":
        (task / ".command.err").unlink()
        (task / ".command.err").symlink_to(secret_dir / ".command.err")
    else:
        task.rename(task.with_name("original"))
        task.symlink_to(secret_dir, target_is_directory=True)
    with pytest.raises(OSError):
        rl.tail_bytes(uploads[0].path)


def test_symlinked_named_logs_and_failure_output_are_rejected(tmp_path: Path) -> None:
    secret = tmp_path / "environ"
    secret.write_text("private credentials")
    link = tmp_path / "engine.log"
    link.symlink_to(secret)
    good = tmp_path / "good.log"
    good.write_text("safe")
    logs = _upload(
        tmp_path,
        "--failure", str(link), "1",
        "--file", f"engine.log={link}",
        "--file", f"good.log={good}",
    )  # fmt: skip
    assert (logs / "failure.txt").read_text() == "The engine exited with status 1.\n"
    assert not (logs / "engine.log").exists()
    assert (logs / "good.log").read_text() == "safe"


def test_non_regular_logs_are_rejected_without_blocking(tmp_path: Path) -> None:
    fifo = tmp_path / "fifo"
    os.mkfifo(fifo)
    got = subprocess.run(
        [
            sys.executable,
            "-c",
            "import sys; from pathlib import Path; from run_logs import tail_bytes; "
            "tail_bytes(Path(sys.argv[1]))",
            str(fifo),
        ],
        env={"PATH": os.environ["PATH"], "PYTHONPATH": str(LIB)},
        capture_output=True,
        text=True,
        timeout=2,
    )
    assert got.returncode != 0
    assert "OSError" in got.stderr


@pytest.mark.parametrize("periodic", [False, True])
def test_uploaders_drop_secrets_but_preserve_ecs_provider_and_parent_environment(
    tmp_path: Path, periodic: bool
) -> None:
    capture = tmp_path / "capture.json"
    executable = tmp_path / "bin/python3"
    executable.parent.mkdir()
    python_executable(
        executable,
        "import json, os, time\n"
        "from pathlib import Path\n"
        # Widen the publication race so stopping a periodic child catches incomplete writes.
        "def slow_write(path, text):\n"
        "    with path.open('w') as stream:\n"
        "        stream.write(text[:1]); stream.flush()\n"
        "        time.sleep(0.2)\n"
        "        stream.write(text[1:])\n"
        "Path.write_text = slow_write\n"
        "capture = Path(os.environ['CAPTURE'])\n"
        "temporary = capture.with_suffix('.tmp')\n"
        "temporary.write_text(json.dumps(dict(os.environ)))\n"
        "temporary.replace(capture)\n",
    )
    launch = (
        "start_logs_sync\n"
        'for ((i=0; i<100; i++)); do [ ! -e "$CAPTURE" ] || break; sleep 0.05; done\n'
        "stop_logs_sync\n"
        if periodic
        else "upload_run_logs final\n"
    )
    secrets = {
        "BIFFIX_INGEST_TOKEN": "ingest-secret",
        "AWS_ACCESS_KEY_ID": "access-secret",
        "AWS_SECRET_ACCESS_KEY": "key-secret",
        "AWS_SESSION_TOKEN": "session-secret",
        "AWS_SECURITY_TOKEN": "legacy-session-secret",
    }
    provider = {
        "AWS_CONTAINER_CREDENTIALS_RELATIVE_URI": "/v2/credentials/task",
        "AWS_CONTAINER_CREDENTIALS_FULL_URI": "http://127.0.0.1/credentials",
        "AWS_CONTAINER_AUTHORIZATION_TOKEN": "provider-token",
        "AWS_REGION": "us-west-2",
    }
    got = bash(
        launch + 'printf "%s|%s" "$BIFFIX_INGEST_TOKEN" "$AWS_ACCESS_KEY_ID"',
        "logs",
        PATH=f"{executable.parent}:{os.environ['PATH']}",
        WORK=str(tmp_path), SRC=str(tmp_path), CAPTURE=str(capture),
        BIFFIX_PROJECT_BUCKET="b", BIFFIX_RUN_PREFIX="runs/r/", BIFFIX_LOGS_SYNC_SECS="1",
        **secrets, **provider,
    )  # fmt: skip
    assert got.returncode == 0, got.stderr
    assert got.stdout == "ingest-secret|access-secret"
    assert capture.exists(), got.stderr
    child = json.loads(capture.read_text())
    assert not secrets.keys() & child.keys()
    assert {key: child[key] for key in provider} == provider
