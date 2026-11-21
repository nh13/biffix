"""A cancel, on the executor's own state: real processes, no Snakemake."""

from __future__ import annotations

import asyncio
import os
import signal
import subprocess
import threading
import time
from collections.abc import Iterator
from pathlib import Path
from types import SimpleNamespace
from typing import Any

import pytest
from conftest import alive
from snakemake_interface_executor_plugins.executors.base import SubmittedJobInfo

import snakemake_executor_plugin_biffix as plugin
from snakemake_executor_plugin_biffix import Executor


def executor(*processes: subprocess.Popen[bytes]) -> Executor:
    """An Executor with only the state a cancel and a status check use, running `processes`."""
    got = object.__new__(Executor)
    got._processes = {p.pid: p for p in processes}
    got._processes_lock = threading.Lock()
    got._cancelled = False
    return got


_started: list[int] = []


@pytest.fixture(autouse=True)
def kill_what_a_test_started() -> Iterator[None]:
    """SIGKILL every job process group a test started, so a failing test (a cancel that left a
    member running) does not leave a process that ignores SIGTERM behind."""
    yield
    while _started:
        try:
            os.killpg(_started.pop(), signal.SIGKILL)
        except ProcessLookupError:
            pass


def job_process(script: str, ready: Path) -> subprocess.Popen[bytes]:
    """`script` in bash as the executor starts a job, a process group of its own; once `ready`
    exists, everything it starts is running."""
    process = subprocess.Popen(["/bin/bash", "-c", script], start_new_session=True)
    _started.append(process.pid)
    deadline = time.monotonic() + 30
    while not (ready.exists() and ready.read_text().strip()):
        assert time.monotonic() < deadline, f"{script} did not start"
        time.sleep(0.05)
    return process


@pytest.fixture(autouse=True)
def short_grace(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(plugin, "CANCEL_GRACE_SECONDS", 0.5)


def test_a_job_that_ignores_sigterm_is_killed(tmp_path: Path) -> None:
    pid = tmp_path / "pid"
    process = job_process(f"trap '' TERM; echo $$ > {pid}; while :; do sleep 0.1; done", pid)
    executor(process).cancel_jobs([])
    assert process.returncode == -signal.SIGKILL


def test_what_a_job_started_is_killed_after_the_job_itself_stopped_on_sigterm(
    tmp_path: Path,
) -> None:
    """The job's process (its Snakemake, say) stops on SIGTERM and is reaped; a tool it started
    that ignores SIGTERM is still in the job's process group, and is killed with it."""
    child = tmp_path / "child"
    process = job_process(
        f"(trap '' TERM; while :; do sleep 0.1; done) & echo $! > {child}; wait", child
    )
    survivor = int(child.read_text())
    executor(process).cancel_jobs([])
    assert process.returncode == -signal.SIGTERM
    deadline = time.monotonic() + 5
    while alive(survivor) and time.monotonic() < deadline:
        time.sleep(0.05)
    assert not alive(survivor)


def test_a_group_that_stopped_on_sigterm_is_not_sent_sigkill(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """Only a group with a process still in it after the grace period is killed: once a group is
    empty its id may be reused, by a process that is none of this run's."""
    pid = tmp_path / "pid"
    process = job_process(f"echo $$ > {pid}; while :; do sleep 0.1; done", pid)
    sent: list[tuple[int, int]] = []
    signal_group = plugin._signal_group

    def recording(pgid: int, signum: int) -> None:
        sent.append((pgid, signum))
        signal_group(pgid, signum)

    monkeypatch.setattr(plugin, "_signal_group", recording)
    executor(process).cancel_jobs([])
    assert process.returncode == -signal.SIGTERM
    assert sent == [(process.pid, signal.SIGTERM)]


def _check(running: Executor, job_info: SubmittedJobInfo) -> list[SubmittedJobInfo]:
    async def collect() -> list[SubmittedJobInfo]:
        return [j async for j in running.check_active_jobs([job_info])]

    return asyncio.run(collect())


def _reported(running: Executor) -> list[tuple[str, Any]]:
    """Record the executor's reports to the scheduler rather than make them."""
    reports: list[tuple[str, Any]] = []

    def success(job_info: SubmittedJobInfo) -> None:
        reports.append(("success", job_info))

    def error(_job_info: SubmittedJobInfo, msg: str | None = None, **_: object) -> None:
        reports.append(("error", msg))

    setattr(running, "report_job_success", success)  # noqa: B010
    setattr(running, "report_job_error", error)  # noqa: B010
    return reports


def test_a_job_a_cancel_stopped_is_not_reported_as_failed() -> None:
    """After a cancel the scheduler takes no more results: a job the cancel stopped did not fail,
    and is not logged as a failed job."""
    process = subprocess.Popen(["/bin/bash", "-c", "exit 3"], start_new_session=True)
    process.wait()
    running = executor(process)
    reports = _reported(running)
    job_info = SubmittedJobInfo(job=SimpleNamespace(), aux={"process": process})  # type: ignore[arg-type]
    running._cancelled = True
    assert _check(running, job_info) == []
    assert reports == []
    # Before a cancel, the same process is a failed job.
    running._cancelled = False
    assert _check(running, job_info) == []
    assert reports == [("error", "The job's process exited 3.")]


def test_a_failed_job_s_process_group_is_stopped_before_it_is_reported(tmp_path: Path) -> None:
    """A job whose Snakemake failed (SIGKILLed, say) while a tool it started is still running is
    reported failed only once that tool is stopped (SIGTERM, then SIGKILL after the grace period):
    a retry would otherwise start beside it, and a later cancel could no longer find its group.
    That is not a cancel: the executor keeps running jobs."""
    child = tmp_path / "child"
    process = job_process(
        f"(trap '' TERM; while :; do sleep 0.1; done) & echo $! > {child}; kill -KILL $$", child
    )
    survivor = int(child.read_text())
    process.wait()
    running = executor(process)
    reports = _reported(running)
    # Whether the tool was still running when the failure was reported, not only afterwards.
    alive_at_report: list[bool] = []
    report_error = running.report_job_error

    def error(job_info: SubmittedJobInfo, msg: str | None = None, **kwargs: object) -> None:
        alive_at_report.append(alive(survivor))
        report_error(job_info, msg=msg, **kwargs)

    setattr(running, "report_job_error", error)  # noqa: B010
    job_info = SubmittedJobInfo(job=SimpleNamespace(), aux={"process": process})  # type: ignore[arg-type]
    assert _check(running, job_info) == []
    assert alive_at_report == [False]
    assert not alive(survivor)
    assert reports == [("error", "The job's process was stopped by signal 9.")]
    assert running._processes == {} and not running._cancelled


def test_a_failed_job_whose_group_outlives_sigkill_is_reported_saying_so(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """A process SIGKILL does not stop at once (in uninterruptible sleep, say) cannot be waited
    for without end: once KILL_WAIT_SECONDS are up the failure is reported, saying a process of
    its group was still running, so a retry beside it is not a silent one."""
    child = tmp_path / "child"
    process = job_process(
        f"(trap '' TERM; while :; do sleep 0.1; done) & echo $! > {child}; kill -KILL $$", child
    )
    process.wait()
    real_signal = plugin._signal_group
    # The SIGKILL never lands, as for a process in uninterruptible sleep.
    monkeypatch.setattr(
        plugin,
        "_signal_group",
        lambda pgid, signum: None if signum == signal.SIGKILL else real_signal(pgid, signum),
    )
    monkeypatch.setattr(plugin, "KILL_WAIT_SECONDS", 0.2)
    running = executor(process)
    reports = _reported(running)
    job_info = SubmittedJobInfo(job=SimpleNamespace(), aux={"process": process})  # type: ignore[arg-type]
    assert _check(running, job_info) == []
    ((kind, msg),) = reports
    assert kind == "error"
    assert msg.startswith("The job's process was stopped by signal 9.")
    assert f"process group {process.pid}" in msg and "still running" in msg


def test_a_cancelled_group_that_outlives_sigkill_is_logged(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """A cancel does not wait without end on a process SIGKILL does not stop at once: it logs
    that the group still has a process once KILL_WAIT_SECONDS are up."""
    ready = tmp_path / "ready"
    process = job_process(f"trap '' TERM; echo up > {ready}; while :; do sleep 0.1; done", ready)
    real_signal = plugin._signal_group
    monkeypatch.setattr(
        plugin,
        "_signal_group",
        lambda pgid, signum: None if signum == signal.SIGKILL else real_signal(pgid, signum),
    )
    monkeypatch.setattr(plugin, "KILL_WAIT_SECONDS", 0.2)
    running = executor(process)
    logged: list[str] = []
    running.logger = SimpleNamespace(error=logged.append)  # type: ignore[assignment]
    waiter = threading.Thread(target=running.cancel_jobs, args=([],))
    waiter.start()
    waiter.join(timeout=5)
    still_waiting = waiter.is_alive()  # read before the kill below would release it
    os.killpg(process.pid, signal.SIGKILL)  # what the suppressed SIGKILL would have done
    waiter.join(timeout=5)
    process.wait()
    assert not still_waiting, "the cancel waited on the surviving group"
    assert logged == [
        f"A process of the cancelled job's process group {process.pid} was still running "
        "0.2 s after SIGKILL."
    ]
