"""The single-job acceptance fixtures (dev/smoke/single-job): their parameter schemas, the one image
Dockerfile both share, and that each workflow selects all three of its images. Their manifests are
checked with biffix's own parser (crates/biffix-workflow/tests/it/smoke_fixtures.rs), and the
fixtures run on real hosts (dev/smoke/single-job/README.md)."""

from __future__ import annotations

import json
import re
from pathlib import Path
from typing import Any

import pytest
from jsonschema.validators import validator_for

SMOKE = Path(__file__).resolve().parents[3] / "dev" / "smoke" / "single-job"
SCHEMAS = {
    "snakemake": SMOKE / "snakemake" / "config.schema.json",
    "nextflow": SMOKE / "nextflow" / "nextflow_schema.json",
}
WORKFLOWS = {
    "snakemake": SMOKE / "snakemake" / "Snakefile",
    "nextflow": SMOKE / "nextflow" / "main.nf",
}
COMMON = {"input_uri", "expected_machine", "validation_case", "expect_apptainer"}


def _schema(engine: str) -> dict[str, Any]:
    schema: dict[str, Any] = json.loads(SCHEMAS[engine].read_text())
    return schema


def _validator(engine: str) -> Any:
    schema = _schema(engine)
    cls = validator_for(schema)
    cls.check_schema(schema)
    return cls(schema)


def _params(engine: str) -> dict[str, object]:
    params: dict[str, object] = {"input_uri": "s3://test/input.txt", "expected_machine": "aarch64"}
    if engine == "nextflow":
        params["outdir"] = "s3://test/results/"
    return params


@pytest.mark.parametrize("engine", ["snakemake", "nextflow"])
def test_acceptance_parameters_follow_the_schema(engine: str) -> None:
    validator = _validator(engine)
    params = _params(engine)
    assert validator.is_valid(params)
    assert validator.is_valid({**params, "expected_machine": "x86_64"})
    assert validator.is_valid({**params, "expect_apptainer": False})
    for case in ("success", "retry", "fail", "cancel"):
        assert validator.is_valid({**params, "validation_case": case}), case
    for name in ("input_uri", "expected_machine"):
        assert not validator.is_valid({k: v for k, v in params.items() if k != name}), name
    assert not validator.is_valid({**params, "expected_machine": "arm64"})
    assert not validator.is_valid({**params, "input_uri": "https://test/input.txt"})
    assert not validator.is_valid({**params, "input_uri": "s3://test/"})
    assert not validator.is_valid({**params, "validation_case": "unknown"})
    assert not validator.is_valid({**params, "expect_apptainer": "false"})
    assert not validator.is_valid({**params, "unexpected": 1})
    if engine == "nextflow":
        assert not validator.is_valid({k: v for k, v in params.items() if k != "outdir"})


def test_both_fixtures_take_the_same_acceptance_parameters() -> None:
    snakemake, nextflow = (_schema(engine)["properties"] for engine in ("snakemake", "nextflow"))
    assert snakemake.keys() == COMMON
    assert nextflow.keys() == COMMON | {"outdir"}
    # Each common parameter is declared identically: type, constraint, default and description.
    for name in COMMON:
        assert snakemake[name] == nextflow[name], name
    assert snakemake["validation_case"]["enum"] == ["success", "retry", "fail", "cancel"]
    assert snakemake["expected_machine"]["enum"] == ["x86_64", "aarch64"]
    assert snakemake["input_uri"]["pattern"] == "^s3://[^/]+/.+"


def test_both_fixtures_build_their_images_from_one_pinned_base() -> None:
    snakemake, nextflow = (SMOKE / e / "images" / "Dockerfile" for e in ("snakemake", "nextflow"))
    # Copies, as each fixture is registered from its own directory: they must not drift apart.
    assert snakemake.read_bytes() == nextflow.read_bytes()
    dockerfile = snakemake.read_text()
    (base,) = re.findall(r"^FROM (\S+)$", dockerfile, re.MULTILINE)
    assert re.fullmatch(r"public\.ecr\.aws/docker/library/ubuntu:24\.04@sha256:[0-9a-f]{64}", base)
    assert "ARG ACCEPTANCE_IMAGE" in dockerfile
    assert "> /opt/biffix-acceptance-image" in dockerfile


@pytest.mark.parametrize("engine", ["snakemake", "nextflow"])
def test_the_tmp_probe_fails_its_task_and_is_unique_to_it(engine: str) -> None:
    # Under errexit, a failure of any command but the last in an `&&` list does not end the task,
    # so `touch x && rm x` would let a task with an unwritable /tmp pass. And the run's tasks share
    # one /tmp: a fixed probe name lets one task's `rm` remove another's file. So the probe is a
    # file of its own (mktemp), written and removed by commands that each fail the task.
    text = WORKFLOWS[engine].read_text()
    assert not re.search(r"touch [^\n]*&&", text)
    assert "probe=$(mktemp /tmp/.biffix-acceptance-probe.XXXXXX)" in text
    assert 'rm "$probe"' in text


def test_the_nextflow_input_is_staged_under_a_name_no_output_has() -> None:
    # An input_uri ending in first.txt would otherwise be staged as first.txt, the name FIRST
    # writes its output under, and its `cp` onto itself would fail.
    text = WORKFLOWS["nextflow"].read_text()
    assert "path data, stageAs: 'acceptance-input.txt'" in text


@pytest.mark.parametrize("engine", ["snakemake", "nextflow"])
def test_each_of_the_three_images_is_selected_by_a_task(engine: str) -> None:
    # An image no task names is never converted, so it would not count toward the conversion
    # measurement the three images are for.
    text = WORKFLOWS[engine].read_text()
    selects = 'aws_batch_container_image="{}"' if engine == "snakemake" else "container '{}'"
    for name in ("first", "second", "third"):
        assert selects.format(name) in text, name
