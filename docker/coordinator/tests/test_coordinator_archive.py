"""Real-entrypoint coverage of archival boundaries, outcomes, retries and shutdown."""

from __future__ import annotations

import fcntl
import os
import signal
import subprocess
import time
from pathlib import Path

import pytest
from test_entrypoint import Job, SingleJob, _coordinator
from test_entrypoint import apptainer_dir as apptainer_dir
from test_entrypoint import engine_script as engine_script
from test_entrypoint import nf_job as nf_job
from test_entrypoint import nf_single as nf_single
from test_entrypoint import sm_job as sm_job
from test_entrypoint import sm_single as sm_single


def archive(job: Job, attempt: int = 1) -> bytes:
    root = job.s3 / f"bucket/runs/run-test/logs/coordinator/attempt-{attempt}"
    return b"".join(p.read_bytes() for p in sorted(root.glob("*.log")))


def await_file(process: subprocess.Popen[str], path: Path) -> None:
    deadline = time.monotonic() + 15
    while not path.exists():
        assert process.poll() is None, f"coordinator exited {process.returncode} before {path.name}"
        assert time.monotonic() < deadline, f"no {path.name} handshake"
        time.sleep(0.02)


def wait_for_coordinator_exit(process: subprocess.Popen[str]) -> None:
    """Verify root exit before the enclosing fixture stops the remaining container processes.

    Descendants may retain output pipes beyond root exit. Let _coordinator own process-group
    cleanup exactly once, then read its cached output after leaving the context.
    """
    process.wait(timeout=15)


@pytest.mark.parametrize("status", [0, 7])
@pytest.mark.parametrize("engine", ["snakemake", "nextflow"])
def test_normal_outcome_archives_exact_capture_prefix(
    request: pytest.FixtureRequest,
    engine: str,
    status: int,
) -> None:
    job: Job = request.getfixturevalue("sm_job" if engine == "snakemake" else "nf_job")
    got = job.run(FAKE_RC=str(status), FAKE_WRITE_LOG="1", FAKE_LAST_WORDS="50")
    assert got.returncode == status, got.stderr
    captured = archive(job)
    assert captured, "no coordinator archive"
    assert got.stderr.startswith(captured.decode())
    assert captured.endswith(b"biffix-coordinator: no events.jsonl to upload\n")
    assert captured.count(b"engine says: boom\n") == 1
    assert captured.count(b"engine line 50\n") == 1
    assert got.stderr.count("engine says: boom\n") == 1
    assert b"SECRET-SAMPLE-ID" not in captured
    if status:
        assert b"engine line 50" in job.get("bucket", "runs/run-test/logs/failure.txt")
    else:
        assert not (job.s3 / "bucket/runs/run-test/logs/failure.txt").exists()


@pytest.mark.parametrize("status", [0, 7, 78])
def test_final_upload_is_owned_once(sm_job: Job, status: int) -> None:
    counter = sm_job.out / "final-uploads"
    helper = sm_job.lib / "run_logs.py"
    record = """    if "--max-bytes" not in sys.argv:
        with Path(os.environ["FINAL_UPLOADS"]).open("a") as calls:
            calls.write("final\\n")
"""
    helper.write_text(
        helper.read_text().replace(
            "    sys.exit(main(sys.argv[1:]))", record + "    sys.exit(main(sys.argv[1:]))"
        )
    )
    env = {"BIFFIX_ENTRYPOINT": ""} if status == 78 else {"FAKE_RC": str(status)}
    got = sm_job.run(FINAL_UPLOADS=str(counter), **env)
    assert got.returncode == status, got.stderr
    assert archive(sm_job)
    assert counter.read_text() == "final\n"


def test_inherited_writer_cannot_hold_coordinator_exit(sm_job: Job) -> None:
    events = sm_job.lib / "events.sh"
    events.write_text(
        events.read_text()
        + '\nupload_events_jsonl() { sleep 60 & printf "inherited writer\\n" >&2; }\n'
    )
    with _coordinator(sm_job.env()) as proc:
        wait_for_coordinator_exit(proc)
    _, stderr = proc.communicate(timeout=3)
    assert proc.returncode == 0, stderr
    assert archive(sm_job).endswith(b"inherited writer\n")
    assert "output archive drain incomplete; keeping live forwarding" in stderr


def test_attempts_do_not_replace_each_other(sm_job: Job) -> None:
    first = sm_job.run(FAKE_WRITE_LOG="1", AWS_BATCH_JOB_ATTEMPT="1")
    assert first.returncode == 0, first.stderr
    original = archive(sm_job)
    assert original and b"attempt=1" in original
    second = sm_job.run(FAKE_RC="7", FAKE_WRITE_LOG="1", AWS_BATCH_JOB_ATTEMPT="2")
    assert second.returncode == 7, second.stderr
    assert archive(sm_job) == original
    assert b"attempt=2" in archive(sm_job, 2)


def test_pre_engine_failure_is_archived_without_an_engine_failure_report(sm_job: Job) -> None:
    got = sm_job.run(BIFFIX_ENTRYPOINT="")
    assert got.returncode == 78, got.stderr
    assert b"BIFFIX_ENTRYPOINT" in archive(sm_job)
    assert not (sm_job.s3 / "bucket/runs/run-test/logs/failure.txt").exists()


@pytest.mark.parametrize("missing", ["BIFFIX_PROJECT_BUCKET", "BIFFIX_RUN_PREFIX"])
def test_missing_destination_preserves_configuration_status(sm_job: Job, missing: str) -> None:
    got = sm_job.run(**{missing: ""})
    assert got.returncode == 78, got.stderr
    assert "unbound variable" not in got.stderr
    assert not (sm_job.s3 / "bucket/runs/run-test/logs").exists()


def test_dry_run_leaves_capture_off(sm_job: Job) -> None:
    got = sm_job.run(BIFFIX_DRY_RUN="1")
    assert got.returncode == 0, got.stderr
    assert "output archive run=" not in got.stderr
    assert archive(sm_job) == b""


@pytest.mark.parametrize("failure", ["deny", "deadline"])
def test_archive_upload_failure_preserves_engine_status(sm_job: Job, failure: str) -> None:
    env = (
        {"FAKE_S3_DENY_PUT": "1"}
        if failure == "deny"
        else {
            "FAKE_S3_SLOW_PUT": "2",
            "BIFFIX_LOGS_FINAL_SECS": "1",
        }
    )
    got = sm_job.run(FAKE_RC="7", FAKE_WRITE_LOG="1", **env)
    assert got.returncode == 7, got.stderr
    assert "output archive run=" in got.stderr
    assert "unbound variable" not in got.stderr
    if failure == "deadline":
        assert "abandoning" in got.stderr


@pytest.mark.parametrize("sig,expected", [(signal.SIGTERM, 143), (signal.SIGINT, 130)])
def test_cancel_keeps_status_and_archives(sm_job: Job, sig: signal.Signals, expected: int) -> None:
    with _coordinator(sm_job.env(FAKE_WAIT_FOR_TERM="1", FAKE_WRITE_LOG="1")) as proc:
        await_file(proc, sm_job.out / "ready")
        proc.send_signal(sig)
        wait_for_coordinator_exit(proc)
    _, stderr = proc.communicate(timeout=3)
    assert proc.returncode == expected, stderr
    assert (sm_job.out / "termed").read_text().strip() == "INT"
    assert b"engine says: boom\n" in archive(sm_job)
    assert b"stopped on the signal" in archive(sm_job)
    assert not (sm_job.s3 / "bucket/runs/run-test/logs/failure.txt").exists()


def gate_final_upload(job: Job) -> tuple[Path, Path]:
    """Gate the actual helper before its deadline starts, leaving its real upload implementation."""
    ready, release = job.out / "final-ready", job.out / "final-release"
    path = job.lib / "run_logs.py"
    text = path.read_text()
    gate = """    import time
    if "--max-bytes" not in sys.argv:
        Path(os.environ["FINAL_READY"]).touch()
        end = time.monotonic() + 10
        while not Path(os.environ["FINAL_RELEASE"]).exists():
            if time.monotonic() > end:
                raise RuntimeError("final upload gate was not released")
            time.sleep(0.01)
"""
    path.write_text(
        text.replace("    sys.exit(main(sys.argv[1:]))", gate + "    sys.exit(main(sys.argv[1:]))")
    )
    return ready, release


@pytest.mark.parametrize("status", [0, 7, 78])
def test_term_during_exit_finalizer_does_not_replace_saved_status(sm_job: Job, status: int) -> None:
    ready, release = gate_final_upload(sm_job)
    # An exit before the normal final-upload step exercises the EXIT owner, not only ENGINE_DONE.
    events = sm_job.lib / "events.sh"
    events.write_text(events.read_text() + '\nupload_events_jsonl() { exit "${FAKE_RC:-0}"; }\n')
    env = sm_job.env(FAKE_RC=str(status), FINAL_READY=str(ready), FINAL_RELEASE=str(release))
    if status == 78:
        env["BIFFIX_ENTRYPOINT"] = ""
    with _coordinator(env) as proc:
        await_file(proc, ready)
        proc.send_signal(signal.SIGTERM)
        release.touch()
        _, stderr = proc.communicate(timeout=15)
    _, stderr = proc.communicate(timeout=3)
    assert proc.returncode == status, stderr
    assert "received SIGTERM" in stderr
    assert archive(sm_job)


def test_single_job_archive_and_replay_precede_scratch_cleanup(sm_single: SingleJob) -> None:
    job = sm_single.job
    probe = job.out / "replayed"
    events = job.lib / "events.sh"
    events.write_text(
        events.read_text()
        + """
replay_events_jsonl() {
  [ -s "$WORK/snakemake.args" ] || return 91
  local files=("$FAKE_S3/bucket/runs/run-test/logs/coordinator/attempt-1/"*.log)
  [ -s "${files[0]}" ] || return 92
  printf 'archived before replay' > "$REPLAY_PROBE"
}
"""
    )
    got = sm_single.run_without_map(REPLAY_PROBE=str(probe))
    assert got.returncode == 0, got.stderr
    assert probe.read_text() == "archived before replay"
    assert not list(sm_single.scratch.iterdir())
    assert archive(job)


def test_finalizer_signal_skips_single_job_scratch_cleanup(sm_single: SingleJob) -> None:
    ready, release = gate_final_upload(sm_single.job)
    env = sm_single.job.env(**sm_single.env(FINAL_READY=str(ready), FINAL_RELEASE=str(release)))
    with _coordinator(env) as proc:
        await_file(proc, ready)
        proc.send_signal(signal.SIGTERM)
        release.touch()
        _, stderr = proc.communicate(timeout=15)
    _, stderr = proc.communicate(timeout=3)
    assert proc.returncode == 0, stderr
    assert "received SIGTERM" in stderr
    assert (sm_single.scratch / "run/src").exists()
    assert archive(sm_single.job)


@pytest.mark.parametrize("failure", ["marker", "lock", "wipe"])
def test_unowned_scratch_files_are_never_archived(sm_single: SingleJob, failure: str) -> None:
    work = sm_single.scratch / "run"
    work.mkdir()
    previous = b"previous run private configuration\n"
    for name in ("nextflow.config", ".nextflow.log", "snakemake.args", "Snakefile.biffix"):
        (work / name).write_bytes(previous)
    env = {}
    if failure == "marker":
        env["BIFFIX_INSTANCE_STORE_MARKER"] = str(sm_single.scratch / "missing-marker")
    elif failure == "wipe":
        single = sm_single.job.lib / "single_job.sh"
        single.write_text(single.read_text() + "\nclear_scratch() { return 1; }\n")
    with sm_single.marker.open("rb") as lock:
        if failure == "lock":
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        got = sm_single.run_without_map(**env)
    assert got.returncode == 78, got.stderr
    assert archive(sm_single.job), "this invocation's private capture must remain available"
    remote = sm_single.job.s3 / "bucket/runs/run-test/logs"
    assert not (remote / "nextflow.config").exists()
    assert all(previous not in path.read_bytes() for path in remote.rglob("*") if path.is_file())
    assert all(path.read_bytes() == previous for path in work.iterdir())


def test_unprepared_scratch_is_never_cleared_by_finalizer(sm_single: SingleJob) -> None:
    old = sm_single.scratch / "previous"
    old.write_text("untouched")
    got = sm_single.run_without_map(
        BIFFIX_INSTANCE_STORE_MARKER=str(sm_single.job.root / "missing")
    )
    assert got.returncode == 78, got.stderr
    assert old.read_text() == "untouched"
    assert b"instance store" in archive(sm_single.job)


def test_staging_uploads_before_engine_and_term_without_child_archives(sm_job: Job) -> None:
    # Tune the sandbox helper, not production environment knobs, to avoid a minute-long test.
    copier = sm_job.lib / "segment_tee.py"
    copier.write_text(copier.read_text().replace("default=60.0", "default=0.05"))
    stage = sm_job.lib / "source.sh"
    stage.write_text(
        stage.read_text()
        + """
stage_workflow_source() {
  printf 'waiting for staging\\n' >&2
  : > "$STAGE_READY"
  while :; do sleep 0.02; done
}
"""
    )
    with _coordinator(
        sm_job.env(STAGE_READY=str(sm_job.out / "staging"), BIFFIX_LOGS_SYNC_SECS="1")
    ) as proc:
        await_file(proc, sm_job.out / "staging")
        deadline = time.monotonic() + 10
        while b"waiting for staging\n" not in archive(sm_job):
            assert proc.poll() is None
            assert time.monotonic() < deadline, "no periodic archive while staging"
            time.sleep(0.02)
        assert not (sm_job.out / "argv").exists()
        proc.send_signal(signal.SIGTERM)
        wait_for_coordinator_exit(proc)
    _, stderr = proc.communicate(timeout=3)
    assert proc.returncode == 143, stderr
    assert b"received SIGTERM" in archive(sm_job)


def test_engine_restart_picks_up_local_failed_task_uploads(nf_single: SingleJob) -> None:
    job = nf_single.job
    task = nf_single.scratch / "work/aa/task"
    env = job.env(
        **nf_single.env(
            FAKE_WAIT_FOR_TERM="1",
            FAKE_FAILED_TASK=str(task),
            BIFFIX_LOGS_SYNC_SECS="1",
        )
    )
    remote = job.s3 / "bucket/runs/run-test/logs/tasks/aa/task/.command.err"
    with _coordinator(env) as proc:
        await_file(proc, job.out / "ready")
        await_file(proc, remote)
        assert remote.read_text() == "task says: out of cheese\n"
        proc.send_signal(signal.SIGTERM)
        wait_for_coordinator_exit(proc)
    _, stderr = proc.communicate(timeout=3)
    assert proc.returncode == 143, stderr
    assert archive(job)


def test_killed_outer_copier_keeps_engine_status_and_later_output(sm_job: Job) -> None:
    copier = sm_job.lib / "segment_tee.py"
    code = """    note = Path(os.environ["COPIER_NOTE"])
    temp = note.with_suffix(".tmp")
    temp.write_text(f"{os.getpid()} {sys.argv[1]}")
    temp.replace(note)
"""
    copier.write_text(
        copier.read_text().replace(
            "    sys.exit(main(sys.argv[1:]))", code + "    sys.exit(main(sys.argv[1:]))"
        )
    )
    note = sm_job.out / "copier"
    with _coordinator(
        sm_job.env(FAKE_WAIT_FOR_TERM="1", FAKE_WRITE_LOG="1", COPIER_NOTE=str(note))
    ) as proc:
        await_file(proc, sm_job.out / "ready")
        await_file(proc, note)
        old_pid, directory_text = note.read_text().split(" ", 1)
        directory = Path(directory_text)
        assert directory.parent == Path("/tmp").resolve()
        assert directory.name.startswith("biffix-coordinator-logs.")
        deadline = time.monotonic() + 5
        while not any(b"engine says: boom" in p.read_bytes() for p in directory.glob("*.log.open")):
            assert time.monotonic() < deadline
            time.sleep(0.02)
        os.kill(int(old_pid), signal.SIGKILL)
        while note.read_text().split(" ", 1)[0] == old_pid:
            assert time.monotonic() < deadline
            time.sleep(0.02)
        proc.send_signal(signal.SIGTERM)
        wait_for_coordinator_exit(proc)
    _, stderr = proc.communicate(timeout=3)
    assert proc.returncode == 143, stderr
    assert "engine says: boom" in stderr
    assert b"stopped on the signal" in archive(sm_job)
