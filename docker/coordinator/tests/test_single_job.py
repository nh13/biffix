"""Regression tests for cancellation during ECR login and safe Snakefile wrappers."""

from __future__ import annotations

import contextlib
import importlib.util
import json
import os
import signal
import subprocess
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from threading import Thread
from types import ModuleType

import pytest
from executables import python_executable
from processes import stop_group

LIB = Path(__file__).resolve().parents[1] / "lib"
FAKES = Path(__file__).parent / "fakes" / "py"


def single_job() -> ModuleType:
    """Load the helper without requiring lib/ to be a package."""
    spec = importlib.util.spec_from_file_location("single_job_helper", LIB / "single_job.py")
    assert spec is not None and spec.loader is not None
    mod = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = mod
    spec.loader.exec_module(mod)
    return mod


@pytest.mark.parametrize("raise_on_signal", [False, True], ids=["blocked-sdk", "sdk-error"])
def test_cancel_during_blocked_ecr_login_stops_before_pulling(
    tmp_path: Path, raise_on_signal: bool
) -> None:
    """A blocked SDK call must not hold the conversion alive or write a failure on cancel."""
    fake = tmp_path / "fake"
    fake.mkdir()
    started = tmp_path / "login-started"
    (fake / "boto3.py").write_text(
        "import os, signal, time\n"
        "from pathlib import Path\n"
        "def cancelled(*args): raise RuntimeError('private-ecr-token')\n"
        "class ECR:\n"
        "    def get_authorization_token(self):\n"
        + ("        signal.signal(signal.SIGTERM, cancelled)\n" if raise_on_signal else "")
        + "        marker = Path(os.environ['LOGIN_STARTED'])\n"
        "        pending = marker.with_suffix('.tmp')\n"
        "        pending.write_text(str(os.getpid()))\n"
        "        pending.replace(marker)\n"
        "        time.sleep(30)\n"
        "        raise RuntimeError('login failed after cancellation')\n"
        "def client(*args, **kwargs): return ECR()\n"
    )
    apptainer = tmp_path / "apptainer"
    apptainer.write_text('#!/bin/sh\ntouch "$PULL_STARTED"\nexit 1\n')
    apptainer.chmod(0o755)
    failure = tmp_path / "failure.txt"
    pull_started = tmp_path / "pull-started"
    proc = subprocess.Popen(
        [
            sys.executable,
            str(LIB / "single_job.py"),
            "convert",
            "",
            str(tmp_path / "images"),
            str(tmp_path / "logs"),
            str(failure),
            "123456789012.dkr.ecr.us-test-1.amazonaws.com/tool:1",
        ],
        env=os.environ
        | {
            "PATH": f"{tmp_path}:{os.environ['PATH']}",
            "PYTHONPATH": str(fake),
            "BIFFIX_REGION": "us-test-1",
            "LOGIN_STARTED": str(started),
            "PULL_STARTED": str(pull_started),
        },
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        start_new_session=True,
    )
    try:
        deadline = time.monotonic() + 5
        while not started.exists():
            assert proc.poll() is None, stop_group(proc)
            assert time.monotonic() < deadline, "ECR login did not start"
            time.sleep(0.01)
        login_pid = int(started.read_text())
        proc.send_signal(signal.SIGTERM)
        try:
            stdout, stderr = proc.communicate(timeout=2)
        except subprocess.TimeoutExpired:
            pytest.fail("SIGTERM did not interrupt the blocked ECR login")
        assert proc.returncode == 143, stderr
        assert not failure.exists()
        assert not pull_started.exists()
        assert "login failed" not in stdout + stderr
        assert "private-ecr-token" not in stdout + stderr
        with pytest.raises(ProcessLookupError):
            os.kill(login_pid, 0)
    finally:
        stop_group(proc)
        # The login runs in a session of its own, which killing the converter's group misses: on
        # a failed assertion it would sleep on. Its group is the login's pid (start_new_session).
        if started.exists():
            try:
                os.killpg(int(started.read_text()), signal.SIGKILL)
            except (ProcessLookupError, PermissionError):
                pass  # reaped by the converter, as the test asserts


def test_login_credentials_only_reach_matching_registry_pulls(tmp_path: Path) -> None:
    """Credentials travel through the login pipe to ECR pulls, never to logs or other hosts."""
    registry = "123456789012.dkr.ecr.us-test-1.amazonaws.com"
    images = [f"{registry}/tool:1", "quay.io/tool:1"]
    apptainer = tmp_path / "apptainer"
    python_executable(
        apptainer,
        "import json, os, pathlib, sys\n"
        "image = sys.argv[3]\n"
        "credentials = {k: v for k, v in os.environ.items() "
        "if 'DOCKER_USERNAME' in k or 'DOCKER_PASSWORD' in k}\n"
        "pathlib.Path(os.environ['RECORDS'], image.rsplit('/', 1)[0].replace('/', '_'))"
        ".write_text(json.dumps(credentials))\n"
        "pathlib.Path(sys.argv[2]).touch()\n",
    )
    records = tmp_path / "records"
    records.mkdir()
    failure = tmp_path / "failure.txt"
    logs = tmp_path / "logs"
    result = subprocess.run(
        [
            sys.executable,
            str(LIB / "single_job.py"),
            "convert",
            "",
            str(tmp_path / "images"),
            str(logs),
            str(failure),
            *images,
        ],
        # Only what the fixture needs: a registry login the runner's own environment carries
        # (CI_DOCKER_PASSWORD, say) would be recorded too, and shown in an assertion's diff.
        env={
            "PATH": f"{tmp_path}:{os.environ['PATH']}",
            "PYTHONPATH": str(FAKES),
            "BIFFIX_REGION": "us-test-1",
            "FAKE_ECR_REGISTRY": registry,
            "FAKE_ECR_PASSWORD": "private-ecr-token",
            "APPTAINER_DOCKER_PASSWORD": "inherited-token",
            "SINGULARITY_DOCKER_USERNAME": "inherited-user",
            "RECORDS": str(records),
        },
        capture_output=True,
        text=True,
        timeout=10,
    )
    assert result.returncode == 0, result.stderr
    assert json.loads((records / f"docker:__{registry}").read_text()) == {
        "APPTAINER_DOCKER_USERNAME": "AWS",
        "APPTAINER_DOCKER_PASSWORD": "private-ecr-token",
    }
    assert json.loads((records / "docker:__quay.io").read_text()) == {}
    output = result.stdout + result.stderr + "".join(p.read_text() for p in logs.iterdir())
    assert "private-ecr-token" not in output
    assert "inherited-token" not in output
    assert not failure.exists()


def test_wrapper_never_overwrites_an_existing_entrypoint(tmp_path: Path) -> None:
    snakefile = tmp_path / ".biffix-container.smk"
    original = 'include: "rules/steps.smk"\n'
    snakefile.write_text(original)
    with pytest.raises(FileExistsError):
        single_job().snakefile_wrapper(
            str(snakefile), str(snakefile), "", "", "", False, memory_mib=61440
        )
    assert snakefile.read_text() == original


def test_unique_wrappers_are_siblings_and_preserve_relative_include(tmp_path: Path) -> None:
    snakefile = tmp_path / ".biffix-container.smk"
    original = 'include: "rules/steps.smk"\n'
    snakefile.write_text(original)
    helper = single_job()
    first = Path(helper.snakefile_wrapper(str(snakefile), "", "", "", "", False, memory_mib=61440))
    second = Path(helper.snakefile_wrapper(str(snakefile), "", "", "", "", False, memory_mib=61440))
    assert first != second and first != snakefile and second != snakefile
    assert first.parent == second.parent == snakefile.parent
    text = first.read_text()
    assert f"workflow.include({ascii(str(snakefile))}, overwrite_default_target=True)\n" in text
    assert text.count("workflow.include(") == 1
    assert (
        text.index("_biffix.configure_local_resources(workflow, 61440)")
        < text.index("workflow.include(")
        < text.index("_biffix.check_rule_images(")
    )
    # The budget and the checks are the main process's: a job's own Snakemake, in a task image
    # without the coordinator's helper, only includes the workflow.
    assert text.index("if workflow.is_main_process:") < text.index("import importlib.util")
    assert "\nworkflow.include(" in text
    assert "if _biffix is not None:\n    _biffix.check_rule_images(" in text
    assert snakefile.read_text() == original


def test_unique_wrapper_parses_relative_includes_with_real_snakemake(tmp_path: Path) -> None:
    """The entrypoint retains relative includes and its default target without explicit targets."""
    pytest.importorskip("snakemake")
    workflow = tmp_path / "workflow-\U0001f9ec"
    rules = workflow / "rules"
    rules.mkdir(parents=True)
    snakefile = workflow / ".biffix-container.smk"
    original = 'rule all:\n    input: "done"\ninclude: "rules/steps.smk"\n'
    snakefile.write_text(original)
    (rules / "steps.smk").write_text(
        'rule step:\n    output: "done"\n    shell: "touch {output}"\n'
    )
    wrapper = single_job().snakefile_wrapper(
        str(snakefile), "", "", "", "", False, memory_mib=61440
    )
    result = subprocess.run(
        [
            sys.executable,
            "-m",
            "snakemake",
            "--snakefile",
            wrapper,
            "--cores",
            "1",
            "--dry-run",
        ],
        cwd=tmp_path,
        capture_output=True,
        text=True,
        timeout=30,
    )
    assert result.returncode == 0, result.stdout + result.stderr
    assert "rule all:" in result.stdout
    assert "rule step:" in result.stdout
    assert snakefile.read_text() == original


def test_local_rule_does_not_inherit_token_but_logger_still_authenticates(tmp_path: Path) -> None:
    """Removing the logger's environment scrub exposes the token to a real local shell."""
    received: list[tuple[str | None, bytes]] = []

    class Ingest(BaseHTTPRequestHandler):
        def do_POST(self) -> None:
            received.append(
                (
                    self.headers.get("Authorization"),
                    self.rfile.read(int(self.headers["Content-Length"])),
                )
            )
            self.send_response(200)
            self.end_headers()
            self.wfile.write(b"{}")

        def log_message(self, format: str, *args: object) -> None:
            pass

    server = ThreadingHTTPServer(("127.0.0.1", 0), Ingest)
    thread = Thread(target=server.serve_forever, daemon=True)
    thread.start()
    (tmp_path / "Snakefile").write_text(
        'localrules: all\nrule all:\n    output: "token.txt"\n'
        '    shell: "printf \'%s\' \\"${{BIFFIX_INGEST_TOKEN-}}\\" > {output}"\n'
    )
    try:
        result = subprocess.run(
            [sys.executable, "-m", "snakemake", "--cores", "1", "--logger", "biffix"],
            cwd=tmp_path,
            env=os.environ
            | {
                "BIFFIX_RUN_ID": "r1",
                "BIFFIX_INGEST_TOKEN": "private-ingest-token",
                "BIFFIX_INGEST_URL": f"http://127.0.0.1:{server.server_port}",
            },
            capture_output=True,
            text=True,
            timeout=30,
        )
        assert result.returncode == 0, result.stdout + result.stderr
        assert (tmp_path / "token.txt").read_text() == ""
        assert received
        assert all(auth == "Bearer private-ingest-token" for auth, _ in received)
        events = [event for _, body in received for event in json.loads(body)["events"]]
        assert any(event["kind"] == "run_started" for event in events)
    finally:
        server.shutdown()
        thread.join(timeout=2)
        server.server_close()


def test_wrapper_cli_prints_unique_path(tmp_path: Path) -> None:
    snakefile = tmp_path / ".biffix-container.smk"
    snakefile.touch()
    result = subprocess.run(
        [
            sys.executable,
            str(LIB / "single_job.py"),
            "snakefile-wrapper",
            str(snakefile),
            "",
            "",
            "",
            "",
            "no-check",
            "61440",
        ],
        capture_output=True,
        text=True,
        timeout=10,
    )
    assert result.returncode == 0, result.stderr
    wrapper = Path(result.stdout.strip())
    assert wrapper.is_file() and wrapper.parent == tmp_path and wrapper != snakefile


def test_a_failed_pull_stops_another_pull_s_children(tmp_path: Path) -> None:
    """A failure stops each running pull's whole process group, not only the apptainer it ran:
    Apptainer's own children (mksquashfs) would otherwise keep converting after the run failed."""
    pidfile = tmp_path / "grandchild.pid"
    apptainer = tmp_path / "apptainer"
    apptainer.write_text(
        "#!/bin/sh\n"
        'case "$3" in\n'
        '  *slow*) echo $$ > "$PIDFILE.pgid.tmp"; mv "$PIDFILE.pgid.tmp" "$PIDFILE.pgid"\n'
        '    sleep 30 & echo $! > "$PIDFILE.tmp"; mv "$PIDFILE.tmp" "$PIDFILE"; wait ;;\n'
        '  *) while [ ! -f "$PIDFILE" ]; do sleep 0.01; done; exit 1 ;;\n'
        "esac\n"
    )
    apptainer.chmod(0o755)
    failure = tmp_path / "failure.txt"
    try:
        result = subprocess.run(
            [
                sys.executable,
                str(LIB / "single_job.py"),
                "convert",
                "",
                str(tmp_path / "images"),
                str(tmp_path / "logs"),
                str(failure),
                "quay.io/tool/slow:1",
                "quay.io/tool/bad:1",
            ],
            env=os.environ
            | {
                "PATH": f"{tmp_path}:{os.environ['PATH']}",
                "PIDFILE": str(pidfile),
                "BIFFIX_SIF_PARALLEL": "2",
            },
            capture_output=True,
            text=True,
            timeout=20,
        )
        assert result.returncode == 1, result.stderr
        assert "quay.io/tool/bad:1" in failure.read_text()
        grandchild = int(pidfile.read_text())
        deadline = time.monotonic() + 5
        while _alive(grandchild):
            assert time.monotonic() < deadline, "the stopped pull's child is still running"
            time.sleep(0.05)
    finally:
        _kill_published_group(pidfile)


def _kill_published_group(pidfile: Path) -> None:
    """Kill the process group a fake pull published in `<pidfile>.pgid` (its own pid: the converter
    starts each pull in a session of its own), whether or not the test got as far as reading it:
    a converter that timed out would otherwise leave the pull's `sleep` running. Never this
    test's own group."""
    try:
        pgid = int(Path(f"{pidfile}.pgid").read_text())
    except (OSError, ValueError):
        return
    if pgid != os.getpgrp():
        with contextlib.suppress(ProcessLookupError, PermissionError):
            os.killpg(pgid, signal.SIGKILL)


def test_a_failed_pull_kills_another_pull_s_child_that_ignores_sigterm(tmp_path: Path) -> None:
    """A stopped pull's child that ignores SIGTERM is killed, though the pull itself stopped (and
    was reaped) at once."""
    pidfile = tmp_path / "grandchild.pid"
    apptainer = tmp_path / "apptainer"
    apptainer.write_text(
        "#!/bin/sh\n"
        'case "$3" in\n'
        # The child publishes its pid once its trap is in place, so the failure's SIGTERM can
        # only reach it ignored.
        '  *slow*) sh -c \'trap "" TERM; echo $$ > "$PIDFILE.tmp"\n'
        '    mv "$PIDFILE.tmp" "$PIDFILE"; while :; do sleep 0.1; done\' & wait ;;\n'
        '  *) while [ ! -f "$PIDFILE" ]; do sleep 0.01; done; exit 1 ;;\n'
        "esac\n"
    )
    apptainer.chmod(0o755)
    failure = tmp_path / "failure.txt"
    try:
        result = subprocess.run(
            [
                sys.executable,
                str(LIB / "single_job.py"),
                "convert",
                "",
                str(tmp_path / "images"),
                str(tmp_path / "logs"),
                str(failure),
                "quay.io/tool/slow:1",
                "quay.io/tool/bad:1",
            ],
            env=os.environ
            | {
                "PATH": f"{tmp_path}:{os.environ['PATH']}",
                "PIDFILE": str(pidfile),
                "BIFFIX_SIF_PARALLEL": "2",
            },
            capture_output=True,
            text=True,
            timeout=20,
        )
        assert result.returncode == 1, result.stderr
        grandchild = int(pidfile.read_text())
        deadline = time.monotonic() + 5
        while _alive(grandchild):
            assert time.monotonic() < deadline, "the stopped pull's child is still running"
            time.sleep(0.05)
    finally:
        _kill_published(pidfile)


def _kill_published(pidfile: Path) -> None:
    """SIGKILL the process whose pid `pidfile` holds, if it was written and the process lives:
    read whatever became of the conversion (one that timed out, or failed before the test read
    the pid, leaves the child running in its pull's own process group)."""
    try:
        pid = int(pidfile.read_text())
    except (OSError, ValueError):
        return
    if _alive(pid):
        os.kill(pid, signal.SIGKILL)


def _alive(pid: int) -> bool:
    """Whether `pid` is still running. A zombie is not: one whose parent exited before reaping it
    still answers a signal until init reaps it, which a runner may not do promptly."""
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    state = subprocess.run(["ps", "-o", "stat=", "-p", str(pid)], capture_output=True, text=True)
    return bool(state.stdout.strip()) and not state.stdout.strip().startswith("Z")


def test_a_failed_pull_stops_its_own_children(tmp_path: Path) -> None:
    """A pull whose apptainer exits nonzero has its own process group stopped too: a child it
    left (mksquashfs) is no longer among the running pulls the failure stops."""
    pidfile = tmp_path / "child.pid"
    apptainer = tmp_path / "apptainer"
    apptainer.write_text(
        '#!/bin/sh\necho $$ > "$PIDFILE.pgid.tmp"; mv "$PIDFILE.pgid.tmp" "$PIDFILE.pgid"\n'
        'sleep 30 & echo $! > "$PIDFILE.tmp"; mv "$PIDFILE.tmp" "$PIDFILE"\nexit 1\n'
    )
    apptainer.chmod(0o755)
    failure = tmp_path / "failure.txt"
    try:
        result = subprocess.run(
            [
                sys.executable,
                str(LIB / "single_job.py"),
                "convert",
                "",
                str(tmp_path / "images"),
                str(tmp_path / "logs"),
                str(failure),
                "quay.io/tool/bad:1",
            ],
            env=os.environ | {"PATH": f"{tmp_path}:{os.environ['PATH']}", "PIDFILE": str(pidfile)},
            capture_output=True,
            text=True,
            timeout=20,
        )
        assert result.returncode == 1, result.stderr
        assert "quay.io/tool/bad:1" in failure.read_text()
        child = int(pidfile.read_text())
        deadline = time.monotonic() + 5
        while _alive(child):
            assert time.monotonic() < deadline, "the failed pull's child is still running"
            time.sleep(0.05)
    finally:
        _kill_published_group(pidfile)


def test_a_failed_pull_kills_its_own_child_that_ignores_sigterm(tmp_path: Path) -> None:
    """A pull whose apptainer exits nonzero while a child of its own ignores SIGTERM: that child is
    killed once the grace period is up, before the conversion returns, rather than left running
    once its pull has left the running pulls the failure stops."""
    pidfile = tmp_path / "child.pid"
    apptainer = tmp_path / "apptainer"
    apptainer.write_text(
        "#!/bin/sh\n"
        'sh -c \'trap "" TERM; echo $$ > "$PIDFILE.tmp"\n'
        '  mv "$PIDFILE.tmp" "$PIDFILE"; while :; do sleep 0.1; done\' &\n'
        'while [ ! -f "$PIDFILE" ]; do sleep 0.01; done\n'
        "exit 1\n"
    )
    apptainer.chmod(0o755)
    failure = tmp_path / "failure.txt"
    try:
        result = subprocess.run(
            [
                sys.executable,
                str(LIB / "single_job.py"),
                "convert",
                "",
                str(tmp_path / "images"),
                str(tmp_path / "logs"),
                str(failure),
                "quay.io/tool/bad:1",
            ],
            env=os.environ | {"PATH": f"{tmp_path}:{os.environ['PATH']}", "PIDFILE": str(pidfile)},
            capture_output=True,
            text=True,
            timeout=20,
        )
        assert result.returncode == 1, result.stderr
        assert "quay.io/tool/bad:1" in failure.read_text()
        child = int(pidfile.read_text())
        # Killed before the conversion returned; only its reaping (by init) may lag.
        deadline = time.monotonic() + 1
        while _alive(child):
            assert time.monotonic() < deadline, "the failed pull's child is still running"
            time.sleep(0.05)
    finally:
        _kill_published(pidfile)


def test_a_failed_pull_kills_another_pull_whose_apptainer_ignores_sigterm(tmp_path: Path) -> None:
    """A stopped pull whose apptainer process itself ignores SIGTERM is killed once the grace
    period after the SIGTERM is up: its worker, and so the conversion, would otherwise wait on it
    forever."""
    pidfile = tmp_path / "leader.pid"
    apptainer = tmp_path / "apptainer"
    apptainer.write_text(
        "#!/bin/sh\n"
        'case "$3" in\n'
        '  *slow*) trap \'\' TERM; echo $$ > "$PIDFILE.tmp"; mv "$PIDFILE.tmp" "$PIDFILE"\n'
        "    while :; do sleep 0.1; done ;;\n"
        '  *) while [ ! -f "$PIDFILE" ]; do sleep 0.01; done; exit 1 ;;\n'
        "esac\n"
    )
    apptainer.chmod(0o755)
    failure = tmp_path / "failure.txt"
    try:
        result = subprocess.run(
            [
                sys.executable,
                str(LIB / "single_job.py"),
                "convert",
                "",
                str(tmp_path / "images"),
                str(tmp_path / "logs"),
                str(failure),
                "quay.io/tool/slow:1",
                "quay.io/tool/bad:1",
            ],
            env=os.environ
            | {
                "PATH": f"{tmp_path}:{os.environ['PATH']}",
                "PIDFILE": str(pidfile),
                "BIFFIX_SIF_PARALLEL": "2",
            },
            capture_output=True,
            text=True,
            timeout=20,
        )
        assert result.returncode == 1, result.stderr
        assert "quay.io/tool/bad:1" in failure.read_text()
        assert not _alive(int(pidfile.read_text()))
    finally:
        if pidfile.exists() and _alive(int(pidfile.read_text())):
            os.killpg(int(pidfile.read_text()), signal.SIGKILL)


def test_the_main_thread_never_takes_the_conversion_s_lock(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """The SIGTERM handler takes the conversion's (non-reentrant) lock on the main thread, between
    any two of its bytecodes: were the main thread ever to hold it itself, a SIGTERM landing then
    would wait on it forever. A conversion whose pull fails, and so stops another pull whose child
    ignores SIGTERM until it is killed, takes it only on other threads."""
    sj = single_job()
    taken_on_main: list[bool] = []

    class RecordingLock:
        def __init__(self) -> None:
            self._lock = threading.Lock()

        def __enter__(self) -> None:
            taken_on_main.append(threading.current_thread() is threading.main_thread())
            self._lock.acquire()

        def __exit__(self, *_: object) -> None:
            self._lock.release()

    class Recording(sj.Conversion):  # type: ignore[name-defined]
        def __init__(self, *args: object) -> None:
            super().__init__(*args)
            self._lock = RecordingLock()

    pidfile = tmp_path / "grandchild.pid"
    apptainer = tmp_path / "apptainer"
    apptainer.write_text(
        "#!/bin/sh\n"
        'case "$3" in\n'
        # The child publishes its pid once its trap is in place, so the failure's SIGTERM can
        # only reach it ignored.
        '  *slow*) sh -c \'trap "" TERM; echo $$ > "$PIDFILE.tmp"\n'
        '    mv "$PIDFILE.tmp" "$PIDFILE"; while :; do sleep 0.1; done\' & wait ;;\n'
        '  *) while [ ! -f "$PIDFILE" ]; do sleep 0.01; done; exit 1 ;;\n'
        "esac\n"
    )
    apptainer.chmod(0o755)
    monkeypatch.setattr(sj, "Conversion", Recording)
    monkeypatch.setattr(sj, "STOP_GRACE_SECONDS", 0.5)
    monkeypatch.syspath_prepend(str(LIB))
    monkeypatch.setenv("PATH", f"{tmp_path}:{os.environ['PATH']}")
    monkeypatch.setenv("PIDFILE", str(pidfile))
    monkeypatch.setenv("BIFFIX_SIF_PARALLEL", "2")
    failure = tmp_path / "failure.txt"
    try:
        got = sj.convert(
            "",
            str(tmp_path / "images"),
            str(tmp_path / "logs"),
            str(failure),
            ["quay.io/tool/slow:1", "quay.io/tool/bad:1"],
        )
        assert got == 1
        assert "quay.io/tool/bad:1" in failure.read_text()
        assert taken_on_main and not any(taken_on_main)
    finally:
        _kill_published(pidfile)


def test_a_second_sigterm_returns_while_the_first_holds_the_lock(tmp_path: Path) -> None:
    """The handler runs on the main thread, between any two of its bytecodes: a second SIGTERM can
    arrive while the first handler holds the (non-reentrant) lock in fail(), and must return
    rather than wait on that lock forever."""
    conversion = single_job().Conversion("apptainer", tmp_path, tmp_path, {})
    conversion.stopped_by = signal.SIGTERM
    conversion._lock.acquire()
    try:
        second = Thread(target=conversion.stop, args=(signal.SIGTERM,), daemon=True)
        second.start()
        second.join(1)
        assert not second.is_alive(), "a second SIGTERM waited on the first handler's lock"
    finally:
        conversion._lock.release()


def test_a_failure_that_cannot_be_written_still_fails_the_conversion(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    """The failure file may sit on a full or missing filesystem: the conversion still reports the
    failure (on stderr, exit 1) rather than raising in place of it."""
    unwritable = tmp_path / "missing" / "failure.txt"
    assert single_job()._failed(str(unwritable), "an image cannot be had") == 1
    err = capsys.readouterr().err
    assert "biffix-coordinator: an image cannot be had" in err
    assert "could not write" in err


def test_a_cancel_after_a_pull_s_wait_still_kills_its_child_that_ignores_sigterm(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """A SIGTERM that lands after a pull's apptainer has exited (with its image), but before its
    worker takes it out of the running pulls, claims its group: a child it left that ignores
    SIGTERM is killed once the grace period is up, before the pull returns."""
    sj = single_job()
    monkeypatch.syspath_prepend(str(LIB))
    monkeypatch.setattr(sj, "STOP_GRACE_SECONDS", 0.5)
    pidfile = tmp_path / "child.pid"
    apptainer = tmp_path / "apptainer"
    apptainer.write_text(
        "#!/bin/sh\n"
        'sh -c \'trap "" TERM; echo $$ > "$PIDFILE.tmp"\n'
        '  mv "$PIDFILE.tmp" "$PIDFILE"; while :; do sleep 0.1; done\' &\n'
        'while [ ! -f "$PIDFILE" ]; do sleep 0.01; done\n'
        'echo image > "$2"\n'
    )
    apptainer.chmod(0o755)
    images, logs = tmp_path / "images", tmp_path / "logs"
    images.mkdir()
    logs.mkdir()
    conversion = sj.Conversion(str(apptainer), images, logs, {"PIDFILE": str(pidfile)})
    supervise = conversion._supervise

    def then_cancelled(proc: subprocess.Popen[bytes]) -> bytes:
        output: bytes = supervise(proc)
        conversion.stop(signal.SIGTERM)
        return output

    monkeypatch.setattr(conversion, "_supervise", then_cancelled)
    try:
        conversion.pull("quay.io/tool/ok:1", {})
        assert conversion.stopped_by == signal.SIGTERM
        child = int(pidfile.read_text())
        deadline = time.monotonic() + 1
        while _alive(child):
            assert time.monotonic() < deadline, "the cancelled pull's child is still running"
            time.sleep(0.05)
    finally:
        _kill_published(pidfile)


def test_a_pull_whose_log_cannot_be_read_still_fails_the_conversion_and_cleans_up(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """A failed pull whose log cannot be read for its failure still records the failure (naming
    the image and its status), leaves the running pulls, and removes its partial image: the read
    error must not cut the cleanup short."""
    if str(LIB) not in sys.path:
        monkeypatch.syspath_prepend(str(LIB))
    import run_logs

    def unreadable(*_args: object) -> bytes:
        raise OSError("the log went away")

    monkeypatch.setattr(run_logs, "tail_bytes", unreadable)
    apptainer = tmp_path / "apptainer"
    apptainer.write_text('#!/bin/sh\necho partial > "$2"\nexit 1\n')
    apptainer.chmod(0o755)
    images, logs = tmp_path / "images", tmp_path / "logs"
    images.mkdir()
    logs.mkdir()
    conversion = single_job().Conversion(str(apptainer), images, logs, dict(os.environ))
    conversion.pull("quay.io/tool/bad:1", {})
    assert conversion.failed.is_set()
    assert "could not convert the task image quay.io/tool/bad:1" in conversion.failure
    assert "(apptainer pull exited 1)" in conversion.failure
    assert "the log went away" in conversion.failure
    assert not conversion._running
    assert list(images.iterdir()) == []
