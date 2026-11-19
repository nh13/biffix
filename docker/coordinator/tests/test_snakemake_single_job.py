"""Single-job Snakemake regressions using the real engine, without pulling containers."""

from __future__ import annotations

import io
import json
from pathlib import Path

import pytest
from test_lib import RealSnakemake, module


@pytest.mark.parametrize("explicit_default", [False, True])
def test_wrapper_preserves_workflow_default_target(tmp_path: Path, explicit_default: bool) -> None:
    sm = RealSnakemake(tmp_path)
    prefix = (
        'rule other:\n    output: "other.txt"\n    shell: "touch {output}"\n'
        if explicit_default
        else ""
    )
    default = "    default_target: True\n" if explicit_default else ""
    got = sm.run(
        prefix
        + "rule chosen:\n"
        + default
        + '    output: "done.txt"\n    shell: "touch {output}"\n'
    )
    assert got.returncode == 0, got.stderr
    assert (sm.src / "done.txt").exists()
    assert not (sm.src / "other.txt").exists()


def test_explicit_target_still_overrides_workflow_default(tmp_path: Path) -> None:
    sm = RealSnakemake(tmp_path)
    got = sm.run(
        """rule first:
    output: "first.txt"
    shell: "touch {output}"
rule other:
    output: "other.txt"
    shell: "touch {output}"
""",
        "other",
    )
    assert got.returncode == 0, got.stderr
    assert (sm.src / "other.txt").exists()
    assert not (sm.src / "first.txt").exists()


def profile(sm: RealSnakemake, text: str) -> None:
    """Write the automatically loaded workflow profile."""
    path = sm.src / "profiles/default/config.yaml"
    path.parent.mkdir(parents=True)
    path.write_text(text)


def test_single_job_forces_local_executor_but_preserves_profile_settings(tmp_path: Path) -> None:
    sm = RealSnakemake(tmp_path)
    profile(
        sm, "executor: dryrun\nset-threads:\n  work: 2\nset-resources:\n  work:\n    mem_mb: 321\n"
    )
    got = sm.run(
        """rule work:
    output: "done.txt"
    shell: "echo {threads}:{resources.mem_mb} > {output}"
""",
        "work",
        cpus=2,
    )
    assert got.returncode == 0, got.stderr
    assert (sm.src / "done.txt").read_text().strip() == "2:321"


def serial_workflow(resource: str = "") -> str:
    """The lock makes overlapping jobs fail rather than relying on timing assertions."""
    return (
        """rule all:
    input: "a.txt", "b.txt"
rule work:
    output: "{name}.txt"
"""
        + resource
        + """    shell: "mkdir active; sleep 0.4; rmdir active; touch {output}"
"""
    )


@pytest.mark.parametrize("jobs", [2, 100])
def test_profile_jobs_cannot_disable_host_cpu_budget(tmp_path: Path, jobs: int) -> None:
    sm = RealSnakemake(tmp_path)
    profile(sm, f"jobs: {jobs}\n")
    got = sm.run(serial_workflow(), "all")
    assert got.returncode == 0, got.stderr
    assert (sm.src / "a.txt").exists() and (sm.src / "b.txt").exists()


def test_profile_global_resources_are_merged_with_host_memory_cap(tmp_path: Path) -> None:
    sm = RealSnakemake(tmp_path)
    profile(sm, "resources:\n  license_tokens: 1\n  mem_mb: 80000\n")
    got = sm.run(serial_workflow("    resources: license_tokens=1\n"), "all", cpus=2)
    assert got.returncode == 0, got.stderr
    assert (sm.src / "a.txt").exists() and (sm.src / "b.txt").exists()


def test_ordinary_rules_participate_in_host_memory_budget(tmp_path: Path) -> None:
    sm = RealSnakemake(tmp_path)
    got = sm.run(serial_workflow(), "all", cpus=2, memory_mib=1500)
    assert got.returncode == 0, got.stderr
    assert (sm.src / "a.txt").exists() and (sm.src / "b.txt").exists()


@pytest.mark.parametrize(
    "default_resources", ["", "default-resources:\n  mem_mb: 234\n  disk_mb: 345\n"]
)
def test_memory_defaults_preserve_explicit_and_profile_resources(
    tmp_path: Path, default_resources: str
) -> None:
    sm = RealSnakemake(tmp_path)
    if default_resources:
        profile(sm, default_resources)
    got = sm.run(
        """rule work:
    output: "done.txt"
    resources: mem_mb=123
    shell: "echo {resources.mem_mb}:{resources.disk_mb} > {output}"
""",
        "work",
    )
    assert got.returncode == 0, got.stderr
    assert (sm.src / "done.txt").read_text().strip() == (
        "123:345" if default_resources else "123:50000"
    )


@pytest.mark.parametrize("resource", ["tool:1", "", None, 3, lambda _wildcards: "tool:1"])
def test_old_raw_image_resources_remain_supported(resource: object) -> None:
    sj = module("single_job")
    got = sj.rule_image("work", resource, False)
    if callable(resource):
        assert "set by a function" in got.problem
    elif resource == 3:
        assert "not a plain string" in got.problem
    else:
        assert got == sj.RuleImage("work", image=resource or None)


@pytest.mark.parametrize("grouped", [False, True])
def test_only_ungrouped_local_rules_skip_dynamic_image_validation(
    tmp_path: Path, grouped: bool
) -> None:
    sm = RealSnakemake(tmp_path)
    group = '    group: "g"\n' if grouped else ""
    got = sm.run(
        'localrules: work\nrule work:\n    output: "done.txt"\n'
        + group
        + '    resources: aws_batch_container_image=lambda wildcards: "not-mapped"\n'
        '    shell: "touch {output}"\n',
        "work",
    )
    if grouped:
        assert got.returncode != 0 and "set by a function" in got.stderr
    else:
        assert got.returncode == 0, got.stderr
        assert (sm.src / "done.txt").exists()
        assert json.loads(sm.task_images.read_text()) == {}


@pytest.mark.parametrize("local", [False, True])
def test_python_run_blocks_cannot_claim_an_image_they_do_not_run_in(
    tmp_path: Path, local: bool
) -> None:
    sm = RealSnakemake(tmp_path)
    prefix = "localrules: work\n" if local else ""
    got = sm.run(
        prefix
        + """rule work:
    output: "done.txt"
    run:
        open(str(output[0]), "w").write("done")
""",
        "work",
    )
    if local:
        assert got.returncode == 0, got.stderr
        assert (sm.src / "done.txt").exists()
        assert json.loads(sm.task_images.read_text()) == {}
    else:
        assert got.returncode != 0
        assert "run:" in got.stderr and "script:" in got.stderr
        assert not (sm.src / "done.txt").exists()
        assert not sm.task_images.exists()


@pytest.mark.parametrize("defaults", ["disk_mb=345", "mem_mb=0"])
def test_explicit_partial_memory_defaults_remain_unchanged(defaults: str) -> None:
    from types import SimpleNamespace

    from snakemake.resources import Resources
    from snakemake.settings.types import ResourceSettings

    settings = ResourceSettings(
        cores=2, nodes=100, default_resources=Resources.parse([defaults], allow_expressions=True)
    )
    workflow = SimpleNamespace(
        resource_settings=settings, global_resources=Resources.from_mapping({"_cores": 2})
    )
    module("single_job").configure_local_resources(workflow, 1500)
    values = settings._parsed_default_resources
    if defaults == "disk_mb=345":
        assert "mem_mb" not in values
        assert values["disk_mb"].value == 345
    else:
        assert values["mem_mb"].value == 0
    assert workflow.global_resources["mem_mb"].value == 1572
    assert workflow.global_resources["_nodes"].value is None


def test_failed_conversion_reads_only_the_bounded_log_tail(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    sj = module("single_job")
    images, logs = tmp_path / "images", tmp_path / "logs"
    images.mkdir()
    logs.mkdir()
    apptainer = tmp_path / "apptainer"
    apptainer.write_text('#!/bin/sh\nprintf "discard-this-prefix final-diagnostic"\nexit 1\n')
    apptainer.chmod(0o755)
    monkeypatch.setattr(sj, "FAILURE_TAIL_BYTES", 16)
    real_open = Path.open

    class BoundedReader(io.BytesIO):
        def read(self, size: int | None = -1) -> bytes:
            assert size is not None and 0 <= size <= 16, "failed conversion read an unbounded log"
            return super().read(size)

    def bounded_open(path: Path, mode: str = "r") -> object:
        if path.suffix == ".log" and mode == "rb":
            with real_open(path, "rb") as stream:
                return BoundedReader(stream.read())
        return real_open(path, mode)

    monkeypatch.setattr(Path, "open", bounded_open)
    conversion = sj.Conversion(str(apptainer), images, logs, {})
    conversion.pull("tool:1", {})
    assert conversion.failure.endswith("final-diagnostic")
    assert "discard-this-prefix" not in conversion.failure
