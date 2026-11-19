"""A single-job run's task images.

In a single-job run every job runs on the coordinator's host, never as a Batch job, so no
``task_submitted`` names its image, and the container Snakemake runs is a local image file the
tenant reference cannot be recovered from. The coordinator writes ``{rule: image}`` to the file
``BIFFIX_TASK_IMAGES_FILE`` names while the workflow is parsed; each job's ``task_created`` names
its rule's image from it.
"""

from __future__ import annotations

import json
from pathlib import Path

import pytest
from conftest import RecordingHandler, build_handler, log_record

ALIGN = "123456789012.dkr.ecr.us-east-1.amazonaws.com/biffix/wf/align@sha256:" + "a" * 64


def _handler() -> RecordingHandler:
    h = build_handler(run_id="r1")
    h._dispatched = h._poster.posted
    return h


def _created(h: RecordingHandler) -> list[dict[str, object]]:
    return [e for e in h._dispatched if e["kind"] == "task_created"]


def test_a_mapped_rule_s_jobs_name_its_image_and_others_none(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    images = tmp_path / "task-images.json"
    monkeypatch.setenv("BIFFIX_TASK_IMAGES_FILE", str(images))
    # Built before the coordinator's wrapper Snakefile has written the file.
    h = _handler()
    images.write_text(json.dumps({"align": ALIGN}))
    h._job_info(log_record(jobid=1, rule_name="align", wildcards={"sample": "A"}), ts=1.0)
    h._job_info(log_record(jobid=2, rule_name="align", wildcards={"sample": "B"}), ts=1.1)
    h._job_info(log_record(jobid=3, rule_name="all"), ts=1.2)
    created = _created(h)
    assert [e.get("image") for e in created] == [ALIGN, ALIGN, None]
    assert "image" not in created[2]


def test_the_file_is_read_once(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    images = tmp_path / "task-images.json"
    images.write_text(json.dumps({"align": ALIGN}))
    monkeypatch.setenv("BIFFIX_TASK_IMAGES_FILE", str(images))
    h = _handler()
    h._job_info(log_record(jobid=1, rule_name="align"), ts=1.0)
    images.write_text(json.dumps({"align": "other@sha256:b"}))
    h._job_info(log_record(jobid=2, rule_name="align", wildcards={"sample": "B"}), ts=1.1)
    assert [e.get("image") for e in _created(h)] == [ALIGN, ALIGN]


@pytest.mark.parametrize("content", [None, "not json", "[1, 2]", '{"align": 3}'])
def test_a_missing_or_unreadable_file_means_no_images(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, content: str | None
) -> None:
    images = tmp_path / "task-images.json"
    if content is not None:
        images.write_text(content)
    monkeypatch.setenv("BIFFIX_TASK_IMAGES_FILE", str(images))
    h = _handler()
    h._job_info(log_record(jobid=1, rule_name="align"), ts=1.0)
    (created,) = _created(h)
    assert "image" not in created


@pytest.mark.parametrize(
    "raw, expected",
    [("1", 1), ("2", 2), ("", None), ("0", None), ("-1", None), ("no", None), ("4294967296", None)],
)
def test_every_fact_names_the_positive_coordinator_attempt(
    monkeypatch: pytest.MonkeyPatch, raw: str, expected: int | None
) -> None:
    monkeypatch.setenv("AWS_BATCH_JOB_ATTEMPT", raw)
    h = _handler()
    h._workflow_started(ts=0.5)
    h._job_info(log_record(jobid=1, rule_name="align"), ts=1.0)
    h._progress(log_record(done=1, total=2), ts=2.0)
    assert h._dispatched
    assert all(e.get("coordinator_attempt") == expected for e in h._dispatched)
    assert any(e["kind"] == "run_started" for e in h._dispatched)


def test_outside_a_single_job_run_no_image_is_named(handler: RecordingHandler) -> None:
    handler._job_info(log_record(jobid=1, rule_name="align"), ts=1.0)
    (created,) = _created(handler)
    assert "image" not in created
