"""The durable events.jsonl sink: EventWriter, and a real LogHandler writing through it."""

from __future__ import annotations

import atexit
import json
import logging
from pathlib import Path

import pytest
from conftest import FakePoster, log_record
from snakemake_interface_logger_plugins.base import LogHandlerBase
from snakemake_interface_logger_plugins.common import LogEvent
from snakemake_interface_logger_plugins.tests import MockOutputSettings, TestLogHandlerBase

from snakemake_logger_plugin_biffix.handler import LogHandler
from snakemake_logger_plugin_biffix.settings import LogHandlerSettings
from snakemake_logger_plugin_biffix.writer import EventWriter


def test_writer_appends_jsonl_and_truncates(tmp_path: Path) -> None:
    p = tmp_path / "events.jsonl"
    w = EventWriter(p)
    w.write({"kind": "run_started", "run_id": "r1"})
    w.write({"kind": "run_progress", "completed": 1, "total": 2})
    # No explicit flush: every write is flushed, so the file is readable straight away.
    lines = p.read_text().strip().splitlines()
    assert len(lines) == 2
    assert json.loads(lines[0])["kind"] == "run_started"

    w.truncate()
    assert p.read_text() == ""
    w.close()


# ── a real LogHandler (its own __post_init__, EventWriter and settings/env wiring) ──


class _PostingHandler(LogHandler):
    """A real LogHandler (its own EventWriter) whose poster is the in-memory fake."""

    _poster: FakePoster


def _real(tmp_path: Path, monkeypatch: pytest.MonkeyPatch, **settings: str) -> _PostingHandler:
    monkeypatch.chdir(tmp_path)  # the events file resolves against the cwd
    h = _PostingHandler(MockOutputSettings(), LogHandlerSettings(**settings))
    atexit.unregister(h._cleanup)
    h._poster = FakePoster()
    return h


def _lines(path: Path) -> list[dict[str, object]]:
    return [json.loads(line) for line in path.read_text().splitlines()]


def test_real_handler_writes_events_jsonl_and_posts(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    h = _real(tmp_path, monkeypatch, run_id="r1")
    h.emit(log_record(event=LogEvent.WORKFLOW_STARTED))
    h.emit(log_record(event=LogEvent.PROGRESS, done=1, total=2))
    h.close()
    written = _lines(tmp_path / ".biffix_events.jsonl")
    assert [e["kind"] for e in written] == ["run_started", "run_progress"]
    assert h._poster.posted == written
    assert h._poster.closed


def test_real_handler_without_run_id_writes_and_posts_nothing(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    h = _real(tmp_path, monkeypatch)
    h.emit(log_record(event=LogEvent.WORKFLOW_STARTED))
    h.emit(log_record(event=LogEvent.PROGRESS, done=1, total=2))
    h.close()
    assert not (tmp_path / ".biffix_events.jsonl").exists()  # not even created by truncate
    assert h._poster.posted == []


def test_a_write_failure_still_reaches_the_poster(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    h = _real(tmp_path, monkeypatch, run_id="r1")

    def fail(*_args: object) -> None:
        raise OSError("disk full")

    monkeypatch.setattr(h._writer, "write", fail)
    monkeypatch.setattr(h._writer, "truncate", fail)
    h.emit(log_record(event=LogEvent.WORKFLOW_STARTED))
    h.emit(log_record(event=LogEvent.PROGRESS, done=1, total=2))
    assert [e["kind"] for e in h._poster.posted] == ["run_started", "run_progress"]


def test_settings_win_over_env(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("BIFFIX_RUN_ID", "from-env")
    monkeypatch.setenv("BIFFIX_EVENT_FILE", "env.jsonl")
    h = _real(tmp_path, monkeypatch, run_id="from-settings", event_file="cli.jsonl")
    assert h._run_id == "from-settings"
    assert h.baseFilename == str(tmp_path / "cli.jsonl")


def test_env_supplies_run_id_and_event_file(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    # The coordinator's contract (entrypoint.sh): BIFFIX_EVENT_FILE names the file it uploads.
    monkeypatch.setenv("BIFFIX_RUN_ID", "r-env")
    monkeypatch.setenv("BIFFIX_EVENT_FILE", "events.jsonl")
    h = _real(tmp_path, monkeypatch)
    h.emit(log_record(event=LogEvent.WORKFLOW_STARTED))
    h.close()
    assert h._run_id == "r-env"
    assert [e["run_id"] for e in _lines(tmp_path / "events.jsonl")] == ["r-env"]


def test_a_writer_close_failure_is_logged_and_the_poster_still_drains(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, caplog: pytest.LogCaptureFixture
) -> None:
    h = _real(tmp_path, monkeypatch, run_id="r1")

    def fail() -> None:
        raise OSError("flush failed")

    monkeypatch.setattr(h._writer, "close", fail)
    with caplog.at_level(logging.WARNING, logger="snakemake_logger_plugin_biffix.handler"):
        h.close()
    assert any("events.jsonl close failed" in r.getMessage() for r in caplog.records)
    assert h._poster.closed


class TestBiffixLogHandlerInterface(TestLogHandlerBase):
    """The logger interface's own conformance suite, run against the real handler."""

    __test__ = True

    @pytest.fixture(autouse=True)
    def _in_tmp(self, tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
        monkeypatch.chdir(tmp_path)  # keep the events file out of the repo

    def get_log_handler_cls(self) -> type[LogHandlerBase]:
        return LogHandler

    def get_log_handler_settings(self) -> LogHandlerSettings:
        return LogHandlerSettings(run_id="r1")

    def _create_handler(self) -> LogHandlerBase:
        h = super()._create_handler()
        atexit.unregister(h._cleanup)  # type: ignore[attr-defined]
        h._poster = FakePoster()  # type: ignore[attr-defined]
        return h
