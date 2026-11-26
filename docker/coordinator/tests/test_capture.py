"""Exercise the actual Bash capture pipe, including failed and restarted copiers."""

from __future__ import annotations

import os
import shutil
import signal
import subprocess
import tempfile
from pathlib import Path

import pytest
from executables import python_executable
from helpers import LIB


def capture_directory(note: Path) -> Path | None:
    """Only return this harness's private capture directory, never an empty or arbitrary path."""
    if not note.exists() or not (text := note.read_text()):
        return None
    directory = Path(text)
    assert directory.is_absolute()
    assert directory.parent == Path("/tmp").resolve()
    assert directory.name.startswith("biffix-coordinator-logs.")
    assert directory.resolve() == directory
    return directory


def run_capture(
    tmp_path: Path, body: str, **env: str
) -> tuple[subprocess.CompletedProcess[str], bytes]:
    note = tmp_path / "capture-path"
    code = (
        'set -euo pipefail\n. "$LIB_DIR/capture.sh"\n'
        "await_file() { for ((i=0; i<250; i++)); do "
        '[ ! -f "$1" ] || return 0; sleep 0.02; done; return 1; }\n' + body
    )
    process = subprocess.Popen(
        ["bash", "-c", code],
        env={
            "PATH": os.environ["PATH"],
            "LIB_DIR": str(LIB),
            "NOTE": str(note),
            "BIFFIX_RUN_ID": "run-1",
            "AWS_BATCH_JOB_ID": "job-1",
            "AWS_BATCH_JOB_ATTEMPT": "2",
        }
        | env,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        start_new_session=True,
    )
    directory: Path | None = None
    try:
        stdout, stderr = process.communicate(timeout=12)
        if note.exists():
            directory = capture_directory(note)
        archive = (
            b""
            if directory is None
            else b"".join(p.read_bytes() for p in sorted(directory.glob("*.log*")) if p.is_file())
        )
        return subprocess.CompletedProcess(
            process.args, process.returncode, stdout, stderr
        ), archive
    finally:
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        process.communicate(timeout=3)
        if directory is None and note.exists():
            directory = capture_directory(note)
        if directory is not None:
            shutil.rmtree(directory, ignore_errors=True)


START = 'start_coordinator_capture\nprintf "%s" "${CAPTURE_DIR:-}" > "$NOTE"\n'


def test_capture_combines_streams_then_restores_descriptors(tmp_path: Path) -> None:
    got, archive = run_capture(
        tmp_path,
        START + 'printf "out\\n"; printf "err\\n" >&2\n'
        'finish_coordinator_capture\nfinish_coordinator_capture\nprintf "restored\\n"\nexit 7\n',
    )
    assert got.returncode == 7, got.stderr
    assert got.stdout == "restored\n"
    assert "run-1" in got.stderr and "attempt=2" in got.stderr and "job-1" in got.stderr
    assert got.stderr.endswith("out\nerr\n")
    assert archive == got.stderr.encode()


def test_more_than_pipe_capacity_is_not_duplicated_or_lost(tmp_path: Path) -> None:
    got, archive = run_capture(
        tmp_path,
        START + "python3 -c 'import sys; sys.stdout.write(\"x\" * 300000)'\n"
        "finish_coordinator_capture\n",
    )
    assert got.returncode == 0, got.stderr[-1000:]
    assert got.stderr.endswith("x" * 300000)
    assert archive == got.stderr.encode()


@pytest.mark.parametrize("failure", ["return 127", "return 23"])
def test_failed_copier_degrades_to_forwarding(tmp_path: Path, failure: str) -> None:
    got, _ = run_capture(
        tmp_path,
        f"python3() {{ {failure}; }}\n"
        + START
        + 'printf "survived\\n" >&2\nfinish_coordinator_capture\nexit 7\n',
    )
    assert got.returncode == 7, got.stderr
    assert got.stderr.endswith("survived\n")
    assert got.stderr.count("restarting") == 2
    assert "forwarding only" in got.stderr


def test_killed_copier_restarts_without_breaking_writers(tmp_path: Path) -> None:
    shim = tmp_path / "bin" / "python3"
    shim.parent.mkdir()
    python_executable(
        shim,
        """\
import os
import sys
from pathlib import Path
Path(os.environ["COPIER_PID"]).write_text(str(os.getpid()))
os.execv(sys.executable, [sys.executable, *sys.argv[1:]])
""",
    )
    got, archive = run_capture(
        tmp_path,
        START + 'printf "before\\n" >&2\nawait_file "$COPIER_PID"\n'
        "for ((i=0; i<250; i++)); do "
        'if grep -q before "$CAPTURE_DIR"/*.log.open 2>/dev/null; then break; fi; '
        "sleep 0.02; done\n"
        'old=$(cat "$COPIER_PID"); kill -KILL "$old"\n'
        "for ((i=0; i<250; i++)); do "
        '[ "$(cat "$COPIER_PID")" = "$old" ] || break; sleep 0.02; done\n'
        '[ "$(cat "$COPIER_PID")" != "$old" ]\n'
        'await_file "$CAPTURE_DIR/000001.log"\n'
        'printf "after\\n" >&2\nfinish_coordinator_capture\nexit 7\n',
        PATH=f"{shim.parent}:{os.environ['PATH']}",
        COPIER_PID=str(tmp_path / "copier.pid"),
    )
    assert got.returncode == 7, got.stderr
    assert "before\n" in got.stderr and "after\n" in got.stderr
    assert b"before\n" in archive and b"after\n" in archive


@pytest.mark.parametrize("setup", ["mktemp() { return 1; }", "BIFFIX_DRY_RUN=1"])
def test_unavailable_or_dry_capture_leaves_descriptors_alone(tmp_path: Path, setup: str) -> None:
    got, archive = run_capture(
        tmp_path,
        setup + "\n" + START + '[ -z "${CAPTURE_DIR:-}" ]\n'
        'printf "out\\n"; printf "err\\n" >&2\nfinish_coordinator_capture\n',
    )
    assert got.returncode == 0, got.stderr
    assert got.stdout == "out\n" and got.stderr == "err\n"
    assert archive == b""


def test_inherited_writer_is_abandoned_not_killed(tmp_path: Path) -> None:
    got, archive = run_capture(
        tmp_path,
        START + "sleep 2 &\nholder=$!\nfinish_coordinator_capture\n"
        'kill -0 "$CAPTURE_PID"\nprintf "finished\\n"\nwait "$holder"\n',
    )
    assert got.returncode == 0, got.stderr
    assert got.stdout == "finished\n"
    assert archive


@pytest.mark.parametrize(
    "setup,marker_fd", [("exec 1>&-", 2), ("exec 2>&-", 1), ("ulimit -n 12", 2)]
)
def test_capture_descriptor_allocation_fails_open(
    tmp_path: Path, setup: str, marker_fd: int
) -> None:
    directory = Path(tempfile.mkdtemp(prefix="biffix-coordinator-logs.", dir="/tmp")).resolve()
    try:
        got, archived = run_capture(
            tmp_path,
            'mktemp() { printf "%s\\n" "$TEST_CAPTURE_DIR"; }\n'
            + setup
            + "\n"
            + START
            + f'printf "caller survived\\n" >&{marker_fd}\n'
            + '[ -z "${CAPTURE_DIR:-}${CAPTURE_PID:-}${ORIG_STDOUT_FD:-}${ORIG_STDERR_FD:-}" ]\n'
            + "finish_coordinator_capture\nexit 7\n",
            TEST_CAPTURE_DIR=str(directory),
        )
        assert got.returncode == 7, got.stderr
        assert "caller survived\n" in got.stdout + got.stderr
        assert archived == b""
        assert not directory.exists()
    finally:
        shutil.rmtree(directory, ignore_errors=True)
