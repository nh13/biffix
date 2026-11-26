"""The coordinator archive preserves bytes without putting storage on the forwarding path."""

from __future__ import annotations

import os
import subprocess
import sys
import time
from pathlib import Path

import pytest
from helpers import LIB, module


def run_copy(directory: Path, data: bytes, *args: str) -> subprocess.CompletedProcess[bytes]:
    return subprocess.run(
        [sys.executable, str(LIB / "segment_tee.py"), str(directory), *args],
        input=data,
        capture_output=True,
        timeout=10,
    )


def archived(directory: Path) -> bytes:
    return b"".join(p.read_bytes() for p in sorted(directory.glob("*.log")))


def test_size_and_eof_sealing_preserve_every_byte(tmp_path: Path) -> None:
    data = bytes(range(256)) * 1024 + b"last partial segment"
    got = run_copy(tmp_path, data, "--segment-bytes", "32768")
    assert got.returncode == 0, got.stderr
    assert got.stdout == data
    assert archived(tmp_path) == data
    assert len(list(tmp_path.glob("*.log"))) == 9
    assert not list(tmp_path.glob("*.open"))
    assert all(p.stat().st_size <= 32768 for p in tmp_path.glob("*.log"))


def test_empty_input_creates_no_segment(tmp_path: Path) -> None:
    got = run_copy(tmp_path, b"")
    assert got.returncode == 0, got.stderr
    assert not list(tmp_path.glob("*.log*"))


def test_age_seals_while_input_is_quiet(tmp_path: Path) -> None:
    process = subprocess.Popen(
        [sys.executable, str(LIB / "segment_tee.py"), str(tmp_path), "--seal-secs", "0.05"],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    try:
        assert process.stdin is not None
        process.stdin.write(b"quiet input")
        process.stdin.flush()
        deadline = time.monotonic() + 5
        while not list(tmp_path.glob("*.log")) and time.monotonic() < deadline:
            assert process.poll() is None, process.communicate()[1]
            time.sleep(0.01)
        assert archived(tmp_path) == b"quiet input"
        assert process.poll() is None
        stdout, stderr = process.communicate(b" then EOF", timeout=5)
        assert process.returncode == 0, stderr
        assert stdout == b"quiet input then EOF"
        assert archived(tmp_path) == stdout
    finally:
        if process.poll() is None:
            process.kill()
        process.communicate(timeout=5)


def test_restart_never_reuses_uploaded_number(tmp_path: Path) -> None:
    first = run_copy(tmp_path, b"first")
    assert first.returncode == 0, first.stderr
    first_path = next(tmp_path.glob("*.log"))
    assert first_path.name == "000001.log"
    first_path.unlink()  # Successful upload removed it; the producer must remember the number.
    second = run_copy(tmp_path, b"second")
    assert second.returncode == 0, second.stderr
    assert (tmp_path / "000002.log").read_bytes() == b"second"
    assert not first_path.exists()


def test_local_cap_drops_oldest_but_forwards_all_bytes(tmp_path: Path) -> None:
    got = run_copy(tmp_path, b"abcdefghijkl", "--segment-bytes", "4", "--max-local", "2")
    assert got.returncode == 0, got.stderr
    assert got.stdout == b"abcdefghijkl"
    assert archived(tmp_path) == b"efghijkl"
    assert sorted(p.name for p in tmp_path.glob("*.log")) == ["000002.log", "000003.log"]
    assert b"000001.log" in got.stderr and b"drop" in got.stderr.lower()


def test_unusable_directory_preserves_forwarding(tmp_path: Path) -> None:
    path = tmp_path / "not-a-directory"
    path.write_bytes(b"unchanged")
    got = run_copy(path, b"still forwarded")
    assert got.returncode == 0, got.stderr
    assert got.stdout == b"still forwarded"
    assert path.read_bytes() == b"unchanged"
    assert b"archive" in got.stderr.lower()


def test_write_failure_does_not_end_forwarding(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    mod = module("segment_tee")
    real_write = mod.Segments.write
    calls = 0

    def fail_second(self: object, data: bytes) -> None:
        nonlocal calls
        calls += 1
        if calls == 2:
            raise OSError("full volume")
        real_write(self, data)

    monkeypatch.setattr(mod.Segments, "write", fail_second)
    data = b"many chunks" * 20000
    # Use actual files as descriptors so this test cannot block on a full input/output pipe.
    source = tmp_path / "input"
    output = tmp_path / "output"
    source.write_bytes(data)
    with source.open("rb") as incoming, output.open("wb") as outgoing:
        monkeypatch.setattr(mod, "forward", lambda block: os.write(outgoing.fileno(), block) > 0)
        mod.copy(str(tmp_path), 4 * 1024 * 1024, 60, 64, input_fd=incoming.fileno())
    assert output.read_bytes() == data
    assert calls == 2  # Archival stopped after failure, not repeated for every input chunk.


def test_closed_stdout_still_archives(tmp_path: Path) -> None:
    process = subprocess.Popen(
        [sys.executable, str(LIB / "segment_tee.py"), str(tmp_path)],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    assert process.stdout is not None
    process.stdout.close()
    _, stderr = process.communicate(b"kept without a log reader", timeout=5)
    assert process.returncode == 0, stderr
    assert archived(tmp_path) == b"kept without a log reader"


@pytest.mark.parametrize(
    "args", [("--segment-bytes", "0"), ("--max-local", "-1"), ("--seal-secs", "nan")]
)
def test_invalid_limits_are_usage_errors(tmp_path: Path, args: tuple[str, str]) -> None:
    got = run_copy(tmp_path, b"", *args)
    assert got.returncode == 2
    assert b"error" in got.stderr.lower()
