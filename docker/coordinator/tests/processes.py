"""Stopping the process trees the coordinator's tests start."""

from __future__ import annotations

import contextlib
import os
import signal
import subprocess


def stop_group(proc: subprocess.Popen[str]) -> str:
    """Kill the process group `proc` leads (started with `start_new_session=True`) and return its
    stderr, read for at most 10 s: killing only `proc` would leave its children (a `sleep`, a
    shim) holding its pipes, and a drain of them would wait for those to exit."""
    with contextlib.suppress(ProcessLookupError):
        os.killpg(proc.pid, signal.SIGKILL)
    try:
        _, stderr = proc.communicate(timeout=10)
    except subprocess.TimeoutExpired:
        return "<the process's output was still open 10 s after its process group was killed>"
    return stderr or ""
