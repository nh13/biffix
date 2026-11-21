"""Single-job Snakemake regressions using the real engine, without pulling containers."""

from __future__ import annotations

import json
import os
from collections.abc import Iterator
from contextlib import contextmanager
from pathlib import Path
from typing import BinaryIO

import pytest
from helpers import OTHER_IMAGE, TOOL_IMAGE, WORKER_IMAGE, RealSnakemake, bash, module


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


def test_single_job_forces_its_executor_but_preserves_profile_settings(tmp_path: Path) -> None:
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


class _Rule:
    """A rule as check_rule_images reads one."""

    norun = False
    is_template_engine = False
    _group = None

    def __init__(self, name: str, resource: object):
        self.name = name
        self.resources = {} if resource is None else {"aws_batch_container_image": resource}


class _Resource:
    """Snakemake's `Resource`, as newer Snakemake 9 releases wrap a rule's resource."""

    def __init__(self, value: object):
        self.value = value

    def is_evaluable(self) -> bool:
        return callable(self.value)


def _workflow(*rules: object) -> object:
    """A workflow as check_rule_images reads one: no rule is local."""
    from types import SimpleNamespace

    return SimpleNamespace(rules=list(rules), _localrules=set())


# The run's image map, as the backend writes one: the default image maps to itself, and the
# reserved key names it for people reading the map.
_CHECK_MAP = '{"tool:1": "reg.io/t@sha256:1", "reg.io/w@sha256:2": "reg.io/w@sha256:2", ' \
    '"$default": "reg.io/w@sha256:2"}'  # fmt: skip


@pytest.mark.parametrize("wrap", [False, True])
@pytest.mark.parametrize(
    ("resource", "problem"),
    [
        ("tool:1", None),
        ("docker.io/library/tool:1", None),
        (None, None),
        ("", "rule work: its image '' (aws_batch_container_image) is not an image reference;"),
        (3, "rule work: its image 3 (aws_batch_container_image) is not an image reference; set"),
        (lambda _wildcards: "tool:1", "rule work: its aws_batch_container_image is set by a func"),
        ("quay.io/x:1", "rule work: its image 'quay.io/x:1' is not in the run's image map; decl"),
        ("$default", "rule work: its image '$default' (aws_batch_container_image) is the map's"),
    ],
)
def test_a_rule_s_image_is_checked_whether_or_not_snakemake_wraps_it(
    tmp_path: Path, resource: object, problem: str | None, wrap: bool
) -> None:
    """Older Snakemake 9 releases keep a rule's raw resource values; newer ones wrap them. A rule
    is refused for what the executor would fail its jobs for (its job_image), a resource of ""
    included, which the AWS Batch executor refuses too."""
    from snakemake.exceptions import WorkflowError

    images = tmp_path / "images.json"
    images.write_text(_CHECK_MAP)
    value = _Resource(resource) if wrap and resource is not None else resource
    workflow = _workflow(_Rule("work", value))
    sj = module("single_job")
    if problem is None:
        sj.check_rule_images(workflow, str(images), str(tmp_path), "", False)
        # With the files checked, the image the rule names (none: the default one) must be there.
        named = "reg.io/t@sha256:1" if resource else "reg.io/w@sha256:2"
        with pytest.raises(WorkflowError, match=f"image {named}: its image file .* is missing"):
            sj.check_rule_images(workflow, str(images), str(tmp_path), "reg.io/w@sha256:2", True)
        (tmp_path / sj.image_file_name(named)).write_text("image file")
        sj.check_rule_images(workflow, str(images), str(tmp_path), "reg.io/w@sha256:2", True)
    else:
        for default_image in ("", "reg.io/w@sha256:2"):
            with pytest.raises(WorkflowError) as refused:
                sj.check_rule_images(workflow, str(images), str(tmp_path), default_image, False)
            assert problem in str(refused.value)


def test_a_default_image_the_map_does_not_name_is_refused(tmp_path: Path) -> None:
    """The executor looks the default image up in the map too: a rule that runs in it is refused
    when the map lacks it, as each of its jobs would be."""
    from snakemake.exceptions import WorkflowError

    images = tmp_path / "images.json"
    images.write_text('{"tool:1": "reg.io/t@sha256:1", "$default": "reg.io/w@sha256:2"}')
    with pytest.raises(
        WorkflowError, match="the default image 'reg.io/w@sha256:2' is not in the run"
    ):
        module("single_job").check_rule_images(
            _workflow(_Rule("work", None)), str(images), str(tmp_path), "reg.io/w@sha256:2", False
        )


def test_an_image_refusal_of_a_reason_the_check_does_not_know_keeps_the_plugin_s_message(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """A newer executor plugin may refuse an image for a reason the check has no wording of its
    own for: the plugin's message is kept, never reworded as another reason's."""
    from snakemake.exceptions import WorkflowError
    from snakemake_executor_plugin_biffix import images

    def refuse(_resources: object, job_name: str, *_args: object) -> str:
        raise images.ImageError("a new reason", job_name, "x:1", "its image x:1 is refused anew")

    monkeypatch.setattr(images, "job_image", refuse)
    with pytest.raises(WorkflowError) as refused:
        module("single_job").check_rule_images(
            _workflow(_Rule("work", "x:1")), "", str(tmp_path), "", False
        )
    message = str(refused.value)
    assert "rule work: its image x:1 is refused anew" in message
    assert "not an image reference" not in message and "not in the run's image map" not in message


def test_an_image_map_whose_spellings_disagree_cannot_be_used(tmp_path: Path) -> None:
    from snakemake.exceptions import WorkflowError

    images = tmp_path / "images.json"
    images.write_text('{"ubuntu": "reg.io/a@sha256:1", "ubuntu:latest": "reg.io/b@sha256:2"}')
    with pytest.raises(WorkflowError) as refused:
        module("single_job").check_rule_images(
            _workflow(_Rule("work", "ubuntu")), str(images), str(tmp_path), "", False
        )
    assert f"biffix: the run's image map {images} cannot be used: " in str(refused.value)
    assert "map to different images" in str(refused.value)


def test_without_a_map_a_rule_s_image_is_run_as_named(tmp_path: Path) -> None:
    """With no map the executor runs each image as named (no --biffix-image-map): so the check
    only looks for its file."""
    from snakemake.exceptions import WorkflowError

    sj = module("single_job")
    workflow = _workflow(_Rule("work", "quay.io/x:1"))
    sj.check_rule_images(workflow, "", str(tmp_path), "", False)
    with pytest.raises(WorkflowError, match="image quay.io/x:1: its image file .* is missing"):
        sj.check_rule_images(workflow, "", str(tmp_path), "", True)


@pytest.mark.parametrize(
    ("where", "attribute"),
    [
        ("workflow", "_localrules"),
        ("rule", "_group"),
        ("rule", "norun"),
        ("rule", "is_template_engine"),
    ],
)
def test_a_snakemake_without_what_the_check_reads_is_named(
    tmp_path: Path, where: str, attribute: str
) -> None:
    """The check reads Snakemake internals (which rules are local): a release that renamed one
    is refused with the attribute and the Snakemake release named, never a bare AttributeError
    from the wrapper Snakefile, nor a check skipped."""
    from snakemake.exceptions import WorkflowError

    if where == "workflow":
        workflow = _workflow(_Rule("work", "quay.io/x:1"))
        delattr(workflow, attribute)
    else:
        workflow = _workflow(_Bare("work", "quay.io/x:1", missing=attribute))
    with pytest.raises(WorkflowError) as refused:
        module("single_job").check_rule_images(workflow, "", str(tmp_path), "", False)
    message = str(refused.value)
    assert f"{where}s have no {attribute}" in message and "Snakemake 9." in message


class _Bare:
    """A rule as a Snakemake that renamed `missing` would hand check_rule_images."""

    def __init__(self, name: str, resource: object, missing: str):
        self.name = name
        self.resources = {"aws_batch_container_image": resource}
        for attribute, value in (("norun", False), ("is_template_engine", False), ("_group", None)):
            if attribute != missing:
                setattr(self, attribute, value)


def test_the_coordinator_and_the_executor_name_an_image_file_alike() -> None:
    """The coordinator converts each image to the file the executor plugin then runs it from."""
    from snakemake_executor_plugin_biffix.images import image_file_name

    for image in (TOOL_IMAGE, "123.dkr.ecr.r.amazonaws.com/w:1", "ubuntu", "a/b.sif", "a/b.sif:1"):
        assert module("single_job").image_file_name(image) == image_file_name(image)


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
        assert not sm.recorded.exists()


@pytest.mark.parametrize("local", [False, True])
def test_a_python_run_block_runs_in_its_job_s_image(tmp_path: Path, local: bool) -> None:
    """A job is its own Snakemake inside its image, as on AWS Batch, so a `run:` block's Python
    runs there too; a local rule's runs in the engine, as beside a Batch run."""
    sm = RealSnakemake(tmp_path)
    prefix = "localrules: work\n" if local else ""
    got = sm.run(
        prefix
        + """rule work:
    output: "done.txt"
    resources: aws_batch_container_image="tool:1"
    run:
        import os
        with open(output[0], "w") as out:
            out.write(os.environ.get("BIFFIX_TEST_IMAGE", "engine"))
""",
        "work",
        events=True,
    )
    assert got.returncode == 0, got.stderr
    if local:
        assert (sm.src / "done.txt").read_text() == "engine"
        assert not sm.recorded.exists()
        assert sm.task_images() == {"work": None}
    else:
        assert (sm.src / "done.txt").read_text() == sm.image_file(TOOL_IMAGE)
        assert sm.task_images() == {"work": TOOL_IMAGE}


# Each job writes the image file it ran from (the fake apptainer's BIFFIX_TEST_IMAGE; nothing for
# a job without a container).
_WRITE_IMAGE = 'shell: "echo ${{BIFFIX_TEST_IMAGE:-}} > {output}"'


def _ran_in(sm: RealSnakemake, *names: str) -> dict[str, str]:
    return {name: (sm.src / f"{name}.txt").read_text().strip() for name in names}


def test_an_ungrouped_rule_runs_its_own_image_or_the_default_one(tmp_path: Path) -> None:
    sm = RealSnakemake(tmp_path)
    workflow = f"""rule all:
    input: "own.txt", "default.txt"
rule own:
    output: "own.txt"
    resources: aws_batch_container_image="tool:1"
    {_WRITE_IMAGE}
rule default:
    output: "default.txt"
    {_WRITE_IMAGE}
"""
    got = sm.run(workflow, events=True)
    assert got.returncode == 0, got.stderr
    assert _ran_in(sm, "own", "default") == {
        "own": sm.image_file(TOOL_IMAGE),
        "default": sm.image_file(WORKER_IMAGE),
    }
    assert sm.task_images() == {"own": TOOL_IMAGE, "default": WORKER_IMAGE, "all": None}


def test_a_rule_without_an_image_runs_in_the_engine_when_there_is_no_default(
    tmp_path: Path,
) -> None:
    """As its Batch job would run in the coordinator's own image."""
    sm = RealSnakemake(tmp_path)
    got = sm.run(
        f'rule work:\n    output: "work.txt"\n    {_WRITE_IMAGE}\n', default_image="", events=True
    )
    assert got.returncode == 0, got.stderr
    assert _ran_in(sm, "work") == {"work": ""}
    assert not sm.recorded.exists()
    assert sm.task_images() == {"work": None}


def test_unconnected_jobs_of_one_group_run_as_group_jobs_of_their_own_images(
    tmp_path: Path,
) -> None:
    """On AWS Batch a group job is the jobs of a group that depend on each other, and runs in the
    image those jobs name. Two samples' jobs of one group are two group jobs, so they may name two
    images, and a job naming none runs in its own group job's image, not another sample's."""
    sm = RealSnakemake(tmp_path)
    workflow = f"""rule all:
    input: "post_a.txt", "post_b.txt"
rule pre_a:
    output: "pre_a.txt"
    group: "g"
    resources: aws_batch_container_image="tool:1"
    {_WRITE_IMAGE}
rule pre_b:
    output: "pre_b.txt"
    group: "g"
    resources: aws_batch_container_image="other:2"
    {_WRITE_IMAGE}
rule post:
    input: "pre_{{sample}}.txt"
    output: "post_{{sample}}.txt"
    group: "g"
    {_WRITE_IMAGE}
"""
    got = sm.run(workflow, cpus=2, events=True)
    assert got.returncode == 0, got.stderr
    tool, other = sm.image_file(TOOL_IMAGE), sm.image_file(OTHER_IMAGE)
    assert _ran_in(sm, "pre_a", "post_a", "pre_b", "post_b") == {
        "pre_a": tool,
        "post_a": tool,
        "pre_b": other,
        "post_b": other,
    }
    # One container per group job.
    assert sorted(sm.recorded.read_text().splitlines()) == sorted([tool, other])
    # The image is reported per job: `post` ran in a different image for each sample.
    assert sm.task_images() == {
        "pre_a": TOOL_IMAGE,
        "post:sample=a": TOOL_IMAGE,
        "pre_b": OTHER_IMAGE,
        "post:sample=b": OTHER_IMAGE,
        "all": None,
    }


def test_a_rule_the_run_does_not_use_does_not_decide_a_group_s_image(tmp_path: Path) -> None:
    sm = RealSnakemake(tmp_path)
    workflow = f"""rule all:
    input: "post.txt"
rule pre:
    output: "pre.txt"
    group: "g"
    resources: aws_batch_container_image="tool:1"
    {_WRITE_IMAGE}
rule post:
    input: "pre.txt"
    output: "post.txt"
    group: "g"
    {_WRITE_IMAGE}
rule unused:
    output: "unused.txt"
    group: "g"
    resources: aws_batch_container_image="other:2"
    {_WRITE_IMAGE}
"""
    got = sm.run(workflow)
    assert got.returncode == 0, got.stderr
    tool = sm.image_file(TOOL_IMAGE)
    assert _ran_in(sm, "pre", "post") == {"pre": tool, "post": tool}
    assert sm.recorded.read_text().splitlines() == [tool]


def test_a_group_job_whose_jobs_name_two_images_is_refused(tmp_path: Path) -> None:
    """Snakemake merges a group job's resources, and a string resource must be one value across
    its jobs: it refuses the group job, as it does for AWS Batch, and no job of it runs."""
    sm = RealSnakemake(tmp_path)
    workflow = f"""rule all:
    input: "post.txt"
rule pre:
    output: "pre.txt"
    group: "g"
    resources: aws_batch_container_image="tool:1"
    {_WRITE_IMAGE}
rule post:
    input: "pre.txt"
    output: "post.txt"
    group: "g"
    resources: aws_batch_container_image="other:2"
    {_WRITE_IMAGE}
"""
    got = sm.run(workflow)
    assert got.returncode != 0
    assert "Error grouping resources in group" in got.stderr
    assert "aws_batch_container_image" in got.stderr
    assert not sm.recorded.exists()
    assert not (sm.src / "pre.txt").exists()


def test_jobs_of_one_layer_of_a_group_job_must_all_name_its_image(tmp_path: Path) -> None:
    """Two jobs of a group job that could run side by side (neither needs the other), one naming
    an image and one naming none: Snakemake refuses to merge them, as it does for AWS Batch."""
    sm = RealSnakemake(tmp_path)
    workflow = f"""rule all:
    input: "post.txt"
rule named:
    output: "named.txt"
    group: "g"
    resources: aws_batch_container_image="tool:1"
    {_WRITE_IMAGE}
rule bare:
    output: "bare.txt"
    group: "g"
    {_WRITE_IMAGE}
rule post:
    input: "named.txt", "bare.txt"
    output: "post.txt"
    group: "g"
    resources: aws_batch_container_image="tool:1"
    {_WRITE_IMAGE}
"""
    got = sm.run(workflow, cpus=2)
    assert got.returncode != 0
    assert "Error grouping resources in group" in got.stderr
    assert "aws_batch_container_image" in got.stderr
    assert not sm.recorded.exists()
    assert not (sm.src / "named.txt").exists() and not (sm.src / "bare.txt").exists()


def test_a_local_rule_in_a_group_runs_in_its_group_job_s_image(tmp_path: Path) -> None:
    sm = RealSnakemake(tmp_path)
    workflow = f"""localrules: post
rule all:
    input: "post.txt"
rule pre:
    output: "pre.txt"
    group: "g"
    resources: aws_batch_container_image="tool:1"
    {_WRITE_IMAGE}
rule post:
    input: "pre.txt"
    output: "post.txt"
    group: "g"
    {_WRITE_IMAGE}
"""
    got = sm.run(workflow)
    assert got.returncode == 0, got.stderr
    tool = sm.image_file(TOOL_IMAGE)
    assert _ran_in(sm, "pre", "post") == {"pre": tool, "post": tool}


# Each job records when it ran (from just after it starts to just before it ends), as two lines of
# seconds in its output.
_NOW = "python3 -c 'import time; print(time.time())'"
_RECORD_INTERVAL = f'shell: "{_NOW} > {{output}}; sleep 0.6; {_NOW} >> {{output}}"'


def _most_threads_at_once(intervals: list[tuple[Path, int]]) -> int:
    """The most threads running at any one time, given each job's interval file and threads.
    Each interval lies within its job's run, so an overlap seen here is a real one."""
    events: list[tuple[float, int]] = []
    for path, threads in intervals:
        start, end = (float(line) for line in path.read_text().split())
        # At one instant, an end counts before a start.
        events += [(start, threads), (end, -threads)]
    running = most = 0
    for _, change in sorted(events, key=lambda e: (e[0], e[1])):
        running += change
        most = max(most, running)
    return most


def test_the_jobs_running_at_once_never_take_more_threads_than_the_host_has_cpus(
    tmp_path: Path,
) -> None:
    """Six one-thread jobs and a two-thread job on a host of two CPUs: never more than two
    threads at once, although a remote executor's --jobs alone would allow more; and two of the
    one-thread jobs do run side by side."""
    sm = RealSnakemake(tmp_path)
    workflow = f"""rule all:
    input: expand("one{{n}}.txt", n=range(6)), "two.txt"
rule one:
    output: "one{{n}}.txt"
    {_RECORD_INTERVAL}
rule two:
    output: "two.txt"
    threads: 2
    {_RECORD_INTERVAL}
"""
    profile(sm, "jobs: 100\n")
    got = sm.run(workflow, cpus=2)
    assert got.returncode == 0, got.stderr
    ones = [(sm.src / f"one{n}.txt", 1) for n in range(6)]
    assert _most_threads_at_once([*ones, (sm.src / "two.txt", 2)]) == 2
    assert _most_threads_at_once(ones) == 2


def test_a_group_job_takes_its_jobs_threads_of_the_host_s_cpus(tmp_path: Path) -> None:
    """A group job of two one-thread jobs that run side by side takes two CPUs: on a host of two,
    no other job runs beside it."""
    sm = RealSnakemake(tmp_path)
    workflow = f"""rule all:
    input: "post.txt", "alone.txt"
rule side:
    output: "side{{n}}.txt"
    group: "g"
    {_RECORD_INTERVAL}
rule post:
    input: "side1.txt", "side2.txt"
    output: "post.txt"
    group: "g"
    shell: "touch {{output}}"
rule alone:
    output: "alone.txt"
    {_RECORD_INTERVAL}
"""
    got = sm.run(workflow, cpus=2)
    assert got.returncode == 0, got.stderr
    sides = [(sm.src / f"side{n}.txt", 1) for n in (1, 2)]
    assert _most_threads_at_once([*sides, (sm.src / "alone.txt", 1)]) == 2
    # The group job's two jobs do run side by side, within the two CPUs it was given.
    assert _most_threads_at_once(sides) == 2


def test_a_failed_job_is_retried_and_its_output_reaches_the_engine(tmp_path: Path) -> None:
    sm = RealSnakemake(tmp_path)
    got = sm.run(
        """rule flaky:
    output: "done.txt"
    resources: aws_batch_container_image="tool:1"
    shell: "if [ -e tried ]; then touch {output}; else touch tried; echo job-said-this >&2; exit 3; fi"
""",  # noqa: E501
        "--retries",
        "1",
        events=True,
    )
    assert got.returncode == 0, got.stderr
    # A job's output is the engine's: it is what the run's logs and its failure quote.
    assert "job-said-this" in got.stderr
    assert (sm.src / "done.txt").exists()
    kinds = [json.loads(line)["kind"] for line in sm.events_file.read_text().splitlines()]
    assert [k for k in kinds if k.startswith("task_")] == [
        "task_created",
        "task_failed",
        "task_retrying",
        "task_created",
        "task_succeeded",
    ]
    # The retry reports its image again, before Snakemake announces it.
    created = [
        e
        for e in map(json.loads, sm.events_file.read_text().splitlines())
        if e["kind"] == "task_created"
    ]
    assert [e.get("image") for e in created] == [TOOL_IMAGE, TOOL_IMAGE]


def test_a_failed_group_job_fails_each_of_its_tasks(tmp_path: Path) -> None:
    sm = RealSnakemake(tmp_path)
    workflow = """rule all:
    input: "post.txt"
rule pre:
    output: "pre.txt"
    group: "g"
    shell: "touch {output}"
rule post:
    input: "pre.txt"
    output: "post.txt"
    group: "g"
    shell: "exit 3"
"""
    got = sm.run(workflow, events=True)
    assert got.returncode != 0
    events = [json.loads(line) for line in sm.events_file.read_text().splitlines()]
    failed = sorted(e["task_key"] for e in events if e["kind"] == "task_failed")
    assert failed == ["post", "pre"]


def test_a_runtime_without_the_executor_plugin_cannot_run_a_single_job_run(tmp_path: Path) -> None:
    """A runtime layer built before the plugin existed: the run is refused as misconfigured (78),
    rather than left to fail on an executor it does not know."""
    without = tmp_path / "bin"
    without.mkdir()
    python3 = without / "python3"
    python3.write_text("#!/bin/sh\nexit 3\n")
    python3.chmod(0o755)
    got = bash("require_single_job_executor", "snakemake", PATH=f"{without}:{os.environ['PATH']}")
    assert got.returncode == 78
    assert (
        "this Snakemake runtime has no biffix executor plugin (snakemake-executor-plugin-biffix)"
        in got.stderr
    )
    assert "re-register the workflow once the backend is at a release that pins it" in got.stderr


def test_an_executor_plugin_that_cannot_be_imported_is_not_taken_for_a_missing_one(
    tmp_path: Path,
) -> None:
    """A plugin that is there but fails to import: its own error is shown, and the run is refused
    for that rather than for an old runtime."""
    broken = tmp_path / "py" / "snakemake_executor_plugin_biffix"
    broken.mkdir(parents=True)
    (broken / "__init__.py").write_text("raise ImportError('a broken build: no such symbol')\n")
    got = bash("require_single_job_executor", "snakemake", PYTHONPATH=str(tmp_path / "py"))
    assert got.returncode == 78
    assert "a broken build: no such symbol" in got.stderr
    assert "is present but could not be imported" in got.stderr
    assert "has no biffix executor plugin" not in got.stderr
    # The real one passes.
    assert bash("require_single_job_executor", "snakemake").returncode == 0


def test_a_profile_s_envvars_reach_a_job_with_the_run_s_own(tmp_path: Path) -> None:
    """The variables a single-job run passes its jobs (the AWS credentials, the cache's) join a
    workflow profile's `envvars:`, as on the command line they would replace it."""
    sm = RealSnakemake(tmp_path)
    profile(sm, "envvars:\n  - PROFILE_VAR\n")
    got = sm.run(
        """rule work:
    output: "done.txt"
    shell: "echo ${{PROFILE_VAR:-}}:${{AWS_REGION:-}} > {output}"
""",
        "work",
        env={"PROFILE_VAR": "p", "AWS_REGION": "us-test-1"},
    )
    assert got.returncode == 0, got.stderr
    assert (sm.src / "done.txt").read_text().strip() == "p:us-test-1"


def test_a_profile_s_resource_scopes_are_kept_beside_the_host_s(tmp_path: Path) -> None:
    """Memory and disk are scoped to the whole host, on top of a profile's own scopes, which a
    --set-resource-scopes on the command line would replace."""
    sm = RealSnakemake(tmp_path)
    profile(sm, "set-resource-scopes:\n  gpu: local\n")
    got = sm.run(
        # The workflow's own scopes too, which the host's outrank (as a profile's would).
        """resource_scopes: mem_mb="local", disk_mb="local"
localrules: scopes
rule scopes:
    output: "scopes.txt"
    run:
        with open(output[0], "w") as out:
            scopes = workflow.resource_scopes
            out.write(" ".join(str(scopes.is_local(r)) for r in ("gpu", "mem_mb", "disk_mb")))
""",
        "scopes",
    )
    assert got.returncode == 0, got.stderr
    assert (sm.src / "scopes.txt").read_text() == "True False False"


def test_a_dry_run_forms_the_group_jobs_and_runs_nothing(tmp_path: Path) -> None:
    """A dry run converts no image (so none is looked for) and starts no job, but Snakemake still
    plans for the executor: it names the group job the run would form."""
    sm = RealSnakemake(tmp_path)
    workflow = f"""rule all:
    input: "post.txt"
rule pre:
    output: "pre.txt"
    group: "g"
    resources: aws_batch_container_image="tool:1"
    {_WRITE_IMAGE}
rule post:
    input: "pre.txt"
    output: "post.txt"
    group: "g"
    {_WRITE_IMAGE}
"""
    got = sm.run(workflow, "--dry-run", convert=False, env={"BIFFIX_DRY_RUN": "1"})
    assert got.returncode == 0, got.stderr
    assert "Group job g" in got.stdout + got.stderr
    assert not sm.recorded.exists() and not (sm.src / "pre.txt").exists()


def test_a_job_s_outputs_reach_the_run_s_storage(tmp_path: Path) -> None:
    """With a default storage provider (S3 in a run; here a directory), the engine fetches a
    job's inputs into the local copies it shares with its jobs, and each output and log is
    published to the storage."""
    sm = RealSnakemake(tmp_path)
    store = tmp_path / "store"
    workflow = """rule all:
    input: "out/c.txt"
rule a:
    output: "out/a.txt"
    resources: aws_batch_container_image="tool:1"
    shell: "echo a > {output}"
rule b:
    input: "out/a.txt"
    output: "out/b.txt"
    shell: "cat {input} > {output}; echo b >> {output}"
rule c:
    input: "out/b.txt", "out/a.txt"
    output: "out/c.txt"
    log: "logs/c.log"
    shell: "cat {input} > {output}; echo $BIFFIX_TEST_IMAGE > {log}"
"""
    got = sm.run(
        workflow,
        "--default-storage-provider",
        "fs",
        "--default-storage-prefix",
        str(store),
    )
    assert got.returncode == 0, got.stderr
    assert (store / "out/c.txt").read_text().split() == ["a", "b", "a"]
    assert (store / "out/a.txt").read_text().split() == ["a"]
    assert (store / "out/b.txt").read_text().split() == ["a", "b"]
    assert (store / "logs/c.log").read_text().strip() == sm.image_file(WORKER_IMAGE)
    # The local copies are on the work root, not in the source root.
    assert not (sm.src / "out").exists()
    assert (tmp_path / "storage").is_dir()


@pytest.mark.parametrize("defaults", ["disk_mb=345", "mem_mb=0"])
def test_explicit_partial_memory_defaults_remain_unchanged(defaults: str) -> None:
    from types import SimpleNamespace

    from snakemake.resources import Resources, ResourceScopes
    from snakemake.settings.types import ResourceSettings

    settings = ResourceSettings(
        cores=2, nodes=100, default_resources=Resources.parse([defaults], allow_expressions=True)
    )
    workflow = SimpleNamespace(
        resource_settings=settings,
        global_resources=Resources.from_mapping({"_cores": 2}),
        resource_scopes=ResourceScopes.defaults(),
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
    settings = SimpleNamespace(
        nodes=100, default_resources=bare, overwrite_resource_scopes={"gpu": "local"}
    )
    limits: dict[str, int | None] = {"_cores": 2, "_nodes": 100}
    if profile_mem_mb is not None:
        limits["mem_mb"] = profile_mem_mb
    scopes = {"mem_mb": "local", "gpu": "local"}
    workflow = SimpleNamespace(
        resource_settings=settings, global_resources=limits, resource_scopes=scopes
    )
    module("single_job").configure_local_resources(workflow, 1500)
    host_mb = 1500 * 1048576 // 1000000
    want_mb = host_mb if profile_mem_mb is None else min(profile_mem_mb, host_mb)
    assert limits == {"_cores": 2, "_nodes": None, "mem_mb": want_mb, "mem_mib": 1500}
    # The setting itself stays: Snakemake requires one of a remote executor.
    assert settings.nodes == 100
    assert isinstance(settings.default_resources, DefaultResources)
    assert settings.default_resources.args == ["tmpdir=/tmp"]
    # Memory and disk are the host's, beside the profile's own scopes: in both units, as such a
    # release takes `mem_mib` (and `disk_mib`) for resources of their own, whose scope a profile
    # could keep local.
    want_scopes = {
        "gpu": "local",
        "mem_mb": "global",
        "disk_mb": "global",
        "mem_mib": "global",
        "disk_mib": "global",
    }
    assert settings.overwrite_resource_scopes == want_scopes and scopes == want_scopes


def test_a_snakemake_without_the_resource_internals_is_refused() -> None:
    from types import SimpleNamespace

    with pytest.raises(RuntimeError, match="global_resources"):
        module("single_job").configure_local_resources(
            SimpleNamespace(resource_settings=SimpleNamespace()), 1500
        )
