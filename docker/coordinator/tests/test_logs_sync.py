"""Periodic log uploads reject busy loops, survive failures, and stop before the final PUT."""

from __future__ import annotations

import os
import signal
import subprocess
from pathlib import Path

import pytest
from executables import python_executable
from test_lib import LIB

_UPLOAD_SCRIPT = """\
import os
import signal
import sys
import time
from pathlib import Path

work = Path(os.environ['WORK'])

def publish(name: str, text: str = 'ready') -> None:
    temporary = work / (name + '.tmp')
    temporary.write_text(text)
    temporary.replace(work / name)

if Path(os.environ['BIFFIX_TEST_ARGV0']).name == 'sleep':
    if sys.argv[1] == '120':
        publish('sleeping')
    os.execv('/bin/sleep', ['sleep', *sys.argv[1:]])

if '--max-bytes' not in sys.argv:
    publish('final', 'complete' if (work / 'ended').exists() else 'overlap')
    sys.exit(0)

mode = os.environ['UPLOAD_MODE']
if mode == 'delayed-stop':
    def stop(_signum: int, _frame: object) -> None:
        publish('terminating')
        time.sleep(0.4)
        publish('ended')
        sys.exit(0)
    signal.signal(signal.SIGTERM, stop)
    publish('started', str(os.getpid()))
    while True:
        time.sleep(0.05)
elif not (work / 'first').exists():
    publish('first')
    if mode == 'killed':
        os.kill(os.getpid(), signal.SIGKILL)
    sys.exit(23)
else:
    publish('retried')
"""


def _run(tmp_path: Path, code: str, **env: str) -> subprocess.CompletedProcess[str]:
    """Run the real shell library and always clean up its complete process group."""
    executable = tmp_path / "bin" / "python3"
    executable.parent.mkdir()
    python_executable(executable, _UPLOAD_SCRIPT)
    (executable.parent / "sleep").symlink_to(executable)
    script = (
        'set -euo pipefail\n. "$LIB_DIR/common.sh"\n. "$LIB_DIR/logs.sh"\n'
        # A bounded handshake avoids guessing when the background uploader has started.
        "await_file() {\n"
        "  for ((i=0; i<250; i++)); do\n"
        '    [ ! -e "$WORK/$1" ] || return 0\n'
        "    sleep 0.02\n"
        "  done\n"
        "  return 1\n"
        "}\n" + code
    )
    process = subprocess.Popen(
        ["bash", "-c", script],
        env={
            "PATH": f"{executable.parent}:{os.environ['PATH']}",
            "LIB_DIR": str(LIB),
            "WORK": str(tmp_path),
            "SRC": str(tmp_path),
            "BIFFIX_PROJECT_BUCKET": "bucket",
            "BIFFIX_RUN_PREFIX": "runs/r/",
            "BIFFIX_LOGS_SYNC_SECS": "1",
        }
        | env,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        start_new_session=True,
    )
    try:
        stdout, stderr = process.communicate(timeout=10)
        return subprocess.CompletedProcess(process.args, process.returncode, stdout, stderr)
    finally:
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        process.communicate(timeout=3)


@pytest.mark.parametrize("interval", ["0", "00", "-1", "1.5", "invalid"])
def test_periodic_interval_must_be_positive(tmp_path: Path, interval: str) -> None:
    got = _run(
        tmp_path,
        'start_logs_sync\nstop_logs_sync\nprintf "accepted"\n',
        BIFFIX_LOGS_SYNC_SECS=interval,
    )
    assert got.returncode == 78, got.stderr
    assert "BIFFIX_LOGS_SYNC_SECS" in got.stderr
    assert "accepted" not in got.stdout


def test_stop_reaps_delayed_term_uploader_before_final_upload(tmp_path: Path) -> None:
    got = _run(
        tmp_path,
        "start_logs_sync\n"
        "await_file started\n"
        "stop_logs_sync\n"
        '[ ! -e "$WORK/ended" ] || printf "reaped before return\\n"\n'
        "upload_run_logs final\n"
        'printf "parent survived\\n"\n',
        UPLOAD_MODE="delayed-stop",
    )
    assert got.returncode == 0, got.stderr
    assert "reaped before return" in got.stdout
    assert (tmp_path / "final").read_text() == "complete"
    assert "parent survived" in got.stdout


@pytest.mark.parametrize(("mode", "status"), [("failed", 23), ("killed", 137)])
def test_periodic_upload_retries_after_child_failure(
    tmp_path: Path, mode: str, status: int
) -> None:
    got = _run(
        tmp_path,
        'start_logs_sync\nawait_file retried\nstop_logs_sync\nprintf "retried\\n"\n',
        UPLOAD_MODE=mode,
    )
    assert got.returncode == 0, got.stderr
    assert got.stdout == "retried\n"
    assert f"periodic log upload exited with status {status}; retrying next interval" in got.stderr


def test_stop_interrupts_sleep_without_starting_an_uploader(tmp_path: Path) -> None:
    got = _run(
        tmp_path,
        'start_logs_sync\nawait_file sleeping\nstop_logs_sync\nprintf "stopped\\n"\n',
        BIFFIX_LOGS_SYNC_SECS="120",
    )
    assert got.returncode == 0, got.stderr
    assert got.stdout == "stopped\n"
    assert not (tmp_path / "first").exists()


def test_term_between_child_launch_and_pid_assignment_reaps_child(tmp_path: Path) -> None:
    got = _run(
        tmp_path,
        # Inherit DEBUG into the loop to deliver TERM in the launch/assignment window.
        "set -T\n"
        'trap \'if [[ $BASH_COMMAND == "child=\\$!" ]]; then '
        'kill -TERM "$BASHPID"; fi\' DEBUG\n'
        "start_logs_sync\n"
        'wait "$LOGS_PID"\n'
        'printf "stopped\\n"\n',
        BIFFIX_LOGS_SYNC_SECS="120",
    )
    assert got.returncode == 0, got.stderr
    assert got.stdout == "stopped\n"


def test_dry_run_starts_no_uploader_and_preserves_parent(tmp_path: Path) -> None:
    got = _run(
        tmp_path,
        "start_logs_sync\n"
        '[ -z "$LOGS_PID" ]\n'
        "stop_logs_sync\n"
        "upload_run_logs final\n"
        'printf "dry\\n"\n',
        BIFFIX_DRY_RUN="1",
    )
    assert got.returncode == 0, got.stderr
    assert got.stdout == "dry\n"
    assert not (tmp_path / "final").exists()
