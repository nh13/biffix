"""Single-job Snakemake regressions using the real engine, without pulling containers."""

from __future__ import annotations

import json
from collections.abc import Iterator
from contextlib import contextmanager
from pathlib import Path
from typing import BinaryIO

import pytest
from helpers import RealSnakemake, module


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
    # The profile's own limit survives, and its larger mem_mb does not lift the host's cap: with
    # 1500 MiB for jobs of about 1000 MB each, the two jobs cannot overlap (serial_workflow fails
    # if they do), as they could under the profile's 80000.
    sm = RealSnakemake(tmp_path)
    profile(sm, "resources:\n  license_tokens: 2\n  mem_mb: 80000\n")
    got = sm.run(
        serial_workflow("    resources: license_tokens=1\n"), "all", cpus=2, memory_mib=1500
    )
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
        assert "rule work: its Python run: block cannot execute in its assigned task image" in (
            got.stderr
        )
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
    run_logs = module("run_logs")
    images, logs = tmp_path / "images", tmp_path / "logs"
    images.mkdir()
    logs.mkdir()
    apptainer = tmp_path / "apptainer"
    # A log far longer than the tail the failure keeps.
    apptainer.write_text(
        '#!/bin/sh\nhead -c 100000 /dev/zero | tr "\\0" x\nprintf " final-diagnostic"\nexit 1\n'
    )
    apptainer.chmod(0o755)
    monkeypatch.setattr(sj, "FAILURE_TAIL_BYTES", 16)
    read: list[int] = []
    real_open = run_logs._open_regular

    class Counting:
        """A real file whose reads are counted (tail_bytes needs its fileno and seek)."""

        def __init__(self, fh: BinaryIO) -> None:
            self.fh = fh

        def __getattr__(self, name: str) -> object:
            return getattr(self.fh, name)

        def read(self, size: int = -1) -> bytes:
            got = self.fh.read(size)
            read.append(len(got))
            return got

    @contextmanager
    def counting_open(path: Path) -> Iterator[Counting]:
        with real_open(path) as fh:
            yield Counting(fh)

    # Count reads through the real no-follow opener, not a bypass of its file checks.
    monkeypatch.setattr(run_logs, "_open_regular", counting_open)
    conversion = sj.Conversion(str(apptainer), images, logs, {})
    conversion.pull("tool:1", {})
    assert conversion.failure.endswith("final-diagnostic")
    assert "xxxxxxxxxxxxxxxxxxxx" not in conversion.failure
    assert read and sum(read) <= 16, read


@pytest.mark.parametrize("profile_mem_mb", [None, 5000, 1000])
def test_older_snakemake_resources_get_the_host_cap_and_full_defaults(
    monkeypatch: pytest.MonkeyPatch, profile_mem_mb: int | None
) -> None:
    """A Snakemake 9 release before `snakemake.resources.Resource` keeps plain-number limits and a
    `DefaultResources`: the cap is the least of the host's and the profile's, both memory units are
    capped, and bare defaults become Snakemake's full ones."""
    from types import SimpleNamespace

    import snakemake

    class DefaultResources:
        def __init__(self, args: list[str] | None = None) -> None:
            self.args = args
            self.parsed = {"_cores": 1, "_nodes": 1, "tmpdir": "/tmp", "mem_mb": "full"}

    old = SimpleNamespace(DefaultResources=DefaultResources)
    monkeypatch.setattr(snakemake, "resources", old)
    bare = SimpleNamespace(args=["tmpdir=/tmp"], parsed={"_cores": 1, "_nodes": 1, "tmpdir": "/t"})
    settings = SimpleNamespace(nodes=100, default_resources=bare)
    limits: dict[str, int | None] = {"_cores": 2, "_nodes": 100}
    if profile_mem_mb is not None:
        limits["mem_mb"] = profile_mem_mb
    workflow = SimpleNamespace(resource_settings=settings, global_resources=limits)
    module("single_job").configure_local_resources(workflow, 1500)
    host_mb = 1500 * 1048576 // 1000000
    want_mb = host_mb if profile_mem_mb is None else min(profile_mem_mb, host_mb)
    assert limits == {"_cores": 2, "_nodes": None, "mem_mb": want_mb, "mem_mib": 1500}
    assert settings.nodes is None
    assert isinstance(settings.default_resources, DefaultResources)
    assert settings.default_resources.args == ["tmpdir=/tmp"]


def test_a_snakemake_without_the_resource_internals_is_refused() -> None:
    from types import SimpleNamespace

    with pytest.raises(RuntimeError, match="global_resources"):
        module("single_job").configure_local_resources(
            SimpleNamespace(resource_settings=SimpleNamespace()), 1500
        )
