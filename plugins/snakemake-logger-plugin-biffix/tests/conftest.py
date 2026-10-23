"""Shared fixtures: a real LogHandler whose two sinks are captured in memory."""

from __future__ import annotations

import atexit
import logging

import pytest
from snakemake_interface_logger_plugins.tests import MockOutputSettings

from snakemake_logger_plugin_biffix.handler import LogHandler
from snakemake_logger_plugin_biffix.poster import IngestPoster
from snakemake_logger_plugin_biffix.settings import LogHandlerSettings
from snakemake_logger_plugin_biffix.writer import EventWriter

BIFFIX_ENV = ("BIFFIX_RUN_ID", "BIFFIX_INGEST_URL", "BIFFIX_INGEST_TOKEN", "BIFFIX_EVENT_FILE")


def log_record(**fields: object) -> logging.LogRecord:
    """A log record carrying ``fields`` as attributes, as Snakemake's ``extra=`` records do."""
    return logging.makeLogRecord(fields)


class FakeWriter(EventWriter):
    """Stands in for EventWriter: records writes and truncates, touches no file."""

    def __init__(self) -> None:  # no super().__init__(): there is no file
        self.truncated = 0
        self.written: list[dict[str, object]] = []
        self.closed = False

    def write(self, event: dict[str, object]) -> None:
        self.written.append(event)

    def truncate(self) -> None:
        self.truncated += 1

    def close(self) -> None:
        self.closed = True


class FakePoster(IngestPoster):
    """Stands in for IngestPoster: records posts, sends nothing."""

    enabled = True

    def __init__(self) -> None:  # no super().__init__(): there is no sender
        self.posted: list[dict[str, object]] = []
        self.closed = False

    def post(self, event: dict[str, object]) -> None:
        self.posted.append(event)

    def close(self) -> None:
        self.closed = True


@pytest.fixture(autouse=True)
def _no_biffix_env(monkeypatch: pytest.MonkeyPatch) -> None:
    # The coordinator's env contract must not leak from the developer's shell into any test.
    for name in BIFFIX_ENV:
        monkeypatch.delenv(name, raising=False)


class RecordingHandler(LogHandler):
    """A LogHandler whose sinks are the in-memory fakes."""

    _writer: FakeWriter
    _poster: FakePoster
    # Every dispatched event, in order (the poster's list: each one reaches it).
    _dispatched: list[dict[str, object]]


def kinds(h: RecordingHandler) -> list[str]:
    """The kinds of the events ``h`` dispatched, in order."""
    return [str(e["kind"]) for e in h._dispatched]


def build_handler(**settings: str) -> RecordingHandler:
    """A LogHandler built through its real __post_init__ (so the tests share production's state
    initialisation), with both sinks swapped for in-memory fakes. Not registered with atexit."""
    h = RecordingHandler(MockOutputSettings(), LogHandlerSettings(**settings))
    atexit.unregister(h._cleanup)
    h._writer = FakeWriter()
    h._poster = FakePoster()
    return h


@pytest.fixture
def handler() -> RecordingHandler:
    """A run-"r1" handler whose dispatched events are listed in ``handler._dispatched``."""
    h = build_handler(run_id="r1")
    h._dispatched = h._poster.posted  # every dispatched event reaches the poster
    return h
