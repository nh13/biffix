"""Workflows run by a real Snakemake through the executor, with a stand-in for Apptainer."""

from __future__ import annotations

import os
import signal
import subprocess
import time
from pathlib import Path

import pytest
from conftest import DEFAULT, OTHER, TOOL, Run, file_of

# Each job writes the image file it ran from (the stand-in Apptainer's BIFFIX_TEST_IMAGE), or
# `host` for a job that ran without a container.
WRITE_IMAGE = 'shell: "echo ${{BIFFIX_TEST_IMAGE:-host}} > {output}"'


def test_each_job_runs_its_own_snakemake_in_its_image(run: Run) -> None:
    got = run.run(f"""rule all:
    input: "tool.txt", "default.txt"
rule tool:
    output: "tool.txt"
    resources: aws_batch_container_image="tool:1"
    {WRITE_IMAGE}
rule default:
    output: "default.txt"
    {WRITE_IMAGE}
""")
    assert got.returncode == 0, got.stderr
    assert run.output("tool.txt") == file_of(TOOL)
    assert run.output("default.txt") == file_of(DEFAULT)
    # `all` is a local rule, which Snakemake runs itself: two containers, each started through
    # the image's entrypoint, in the working directory, with a clean environment.
    calls = run.calls()
    assert len(calls) == 2
    src = str(run.src.resolve())
    for call in calls:
        assert call.startswith(f"run --cleanenv --no-eval --home {src} --pwd {src} {run.images}/")
        assert "/bin/bash -c python -m snakemake " in call and "--target-jobs" in call


def test_a_job_without_an_image_runs_on_the_host(run: Run) -> None:
    got = run.run(
        f'rule a:\n    output: "a.txt"\n    {WRITE_IMAGE}\n', default_image=None, image_map=False
    )
    assert got.returncode == 0, got.stderr
    assert run.output("a.txt") == "host"
    assert run.calls() == []


def test_a_group_job_runs_as_one_job_in_the_image_its_jobs_name(run: Run) -> None:
    """Two unconnected sets of jobs of one group are two group jobs, each in its own image; a job
    of the group that names none runs in its group job's."""
    got = run.run(f"""rule all:
    input: "post1.txt", "post2.txt"
rule pre:
    output: "pre{{n}}.txt"
    group: "g"
    resources: aws_batch_container_image=lambda wildcards: "tool:1" if wildcards.n == "1" else "other:2"
    {WRITE_IMAGE}
rule post:
    input: "pre{{n}}.txt"
    output: "post{{n}}.txt"
    group: "g"
    {WRITE_IMAGE}
""")  # noqa: E501
    assert got.returncode == 0, got.stderr
    assert {name: run.output(f"{name}.txt") for name in ("pre1", "post1", "pre2", "post2")} == {
        "pre1": file_of(TOOL),
        "post1": file_of(TOOL),
        "pre2": file_of(OTHER),
        "post2": file_of(OTHER),
    }
    # One container per group job, not per job.
    assert len(run.calls()) == 2


def test_a_run_block_runs_in_its_job_s_image(run: Run) -> None:
    got = run.run("""rule a:
    output: "a.txt"
    resources: aws_batch_container_image="tool:1"
    run:
        import os
        with open(output[0], "w") as out:
            out.write(os.environ.get("BIFFIX_TEST_IMAGE", "host"))
""")
    assert got.returncode == 0, got.stderr
    assert run.output("a.txt") == file_of(TOOL)


def test_a_job_gets_the_variables_snakemake_passes_and_never_the_ingest_token(run: Run) -> None:
    got = run.run(
        """envvars: "WORKFLOW_VAR"
rule a:
    output: "a.txt"
    shell: "echo ${{WORKFLOW_VAR:-}}:${{PASSED_VAR:-}}:${{OTHER_VAR:-}}:${{BIFFIX_INGEST_TOKEN:-}} > {output}"
""",  # noqa: E501
        "--envvars",
        "PASSED_VAR",
        env={
            "WORKFLOW_VAR": "w",
            "PASSED_VAR": "p",
            "OTHER_VAR": "o",
            "BIFFIX_INGEST_TOKEN": "secret",
        },
    )
    assert got.returncode == 0, got.stderr
    assert run.output("a.txt") == "w:p::"
    assert "secret" not in "".join(run.calls())


def test_a_job_on_the_host_does_not_get_the_ingest_token_either(run: Run) -> None:
    got = run.run(
        'rule a:\n    output: "a.txt"\n'
        '    shell: "echo ${{OTHER_VAR:-}}:${{BIFFIX_INGEST_TOKEN:-}} > {output}"\n',
        default_image=None,
        env={"OTHER_VAR": "o", "BIFFIX_INGEST_TOKEN": "secret"},
    )
    assert got.returncode == 0, got.stderr
    assert run.output("a.txt") == "o:"


def test_a_job_whose_image_cannot_run_fails_alone(run: Run) -> None:
    """An image the map lacks fails its job as any failed job: the others still run
    (--keep-going), and no container starts for it."""
    got = run.run(
        f"""rule all:
    input: "ok.txt", "unmapped.txt"
rule ok:
    output: "ok.txt"
    {WRITE_IMAGE}
rule unmapped:
    output: "unmapped.txt"
    resources: aws_batch_container_image="quay.io/x:1"
    {WRITE_IMAGE}
""",
        "--keep-going",
    )
    assert got.returncode != 0
    assert run.output("ok.txt") == file_of(DEFAULT)
    assert "unmapped: container image 'quay.io/x:1' is not in the container image map" in got.stderr
    assert not (run.src / "unmapped.txt").exists()
    assert len(run.calls()) == 1


def test_a_missing_image_file_fails_the_job(run: Run) -> None:
    (run.images / file_of(OTHER)).unlink()
    got = run.run(f"""rule missing:
    output: "missing.txt"
    resources: aws_batch_container_image="other:2"
    {WRITE_IMAGE}
""")
    assert got.returncode != 0
    assert f"missing: the image file of {OTHER}" in got.stderr
    assert "is missing: it was not converted" in got.stderr
    assert run.calls() == []


def test_a_job_s_outputs_reach_the_default_storage(run: Run, tmp_path: Path) -> None:
    """With a default storage provider and one directory of local copies for Snakemake and its
    jobs, a job finds its inputs there and its outputs and logs are published."""
    store, local = tmp_path / "store", tmp_path / "local"
    got = run.run(
        """rule all:
    input: "out/b.txt"
rule a:
    output: "out/a.txt"
    resources: aws_batch_container_image="tool:1"
    shell: "echo a > {output}"
rule b:
    input: "out/a.txt"
    output: "out/b.txt"
    log: "logs/b.log"
    shell: "cat {input} > {output}; echo b >> {output}; echo $BIFFIX_TEST_IMAGE > {log}"
""",
        "--default-storage-provider",
        "fs",
        "--default-storage-prefix",
        str(store),
        "--local-storage-prefix",
        str(local),
        "--remote-job-local-storage-prefix",
        str(local),
        "--biffix-bind",
        str(local),
    )
    assert got.returncode == 0, got.stderr
    assert (store / "out/a.txt").read_text().split() == ["a"]
    assert (store / "out/b.txt").read_text().split() == ["a", "b"]
    assert (store / "logs/b.log").read_text().strip() == file_of(DEFAULT)
    assert not (run.src / "out").exists()
    assert all(f"--bind {local} " in call for call in run.calls())


def test_a_failing_job_is_reported_and_retried(run: Run) -> None:
    got = run.run(
        """rule flaky:
    output: "a.txt"
    shell: "if [ -e tried ]; then touch {output}; else touch tried; exit 3; fi"
""",
        "--retries",
        "1",
    )
    assert got.returncode == 0, got.stderr
    assert "The job's process exited 1." in got.stderr
    assert "cluster/cloud log" not in got.stderr
    assert (run.src / "a.txt").exists()
    assert len(run.calls()) == 2


def test_no_more_threads_run_at_once_than_the_cores_given(run: Run) -> None:
    """Snakemake schedules the jobs within --cores once nothing limits it by nodes (here
    --jobs is far above --cores; the biffix coordinator clears the nodes limit instead): with two
    cores, never more than two one-thread jobs at once, and a two-thread job alone. The wrapper
    here stands in for the coordinator's (whose own tests run the real one), so that this runs on
    every Snakemake release CI tests the plugin with."""
    snakefile = """rule all:
    input: expand("one{n}.txt", n=range(4)), "two.txt"
rule one:
    output: "one{n}.txt"
    shell: "d=$(mktemp -d active/XXXXXX); ls active | wc -l > {output}; sleep 0.5; rmdir $d"
rule two:
    output: "two.txt"
    threads: 2
    shell: "d=$(mktemp -d active/XXXXXX); ls active | wc -l > {output}; sleep 0.5; rmdir $d"
"""
    (run.src / "active").mkdir()
    (run.src / "Snakefile").write_text(snakefile)
    # `workflow.global_resources` is what the coordinator's wrapper Snakefile clears too.
    (run.src / "wrapper.smk").write_text(
        'workflow.global_resources["_nodes"] = None\ninclude: "Snakefile"\n'
    )
    command = run.command(snakefile, "--snakefile", "wrapper.smk", "all", cores=2)
    command[command.index("--jobs") + 1] = "100"
    got = subprocess.run(
        command, cwd=run.src, env=run.env(), capture_output=True, text=True, timeout=300
    )
    assert got.returncode == 0, got.stderr
    assert max(int(run.output(f"one{n}.txt")) for n in range(4)) <= 2
    assert run.output("two.txt") == "1"


def test_a_cancel_stops_every_job_and_what_it_started(run: Run, tmp_path: Path) -> None:
    """SIGINT is Snakemake's prompt stop (the biffix coordinator's cancel): the executor ends each
    job's whole session, not only the process it started."""
    pids = tmp_path / "pids"
    pids.mkdir()
    command = run.command(
        f"""rule all:
    input: "a.txt", "b.txt"
rule sleep:
    output: "{{name}}.txt"
    shell: "echo $$ > {pids}/{{wildcards.name}}; sleep 300 & echo $! > {pids}/{{wildcards.name}}.child; wait"
"""  # noqa: E501
    )
    process = subprocess.Popen(
        command,
        cwd=run.src,
        env=run.env(),
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
    )
    wanted = ["a", "a.child", "b", "b.child"]
    deadline = time.monotonic() + 120
    while time.monotonic() < deadline and not all(
        (pids / name).exists() and (pids / name).read_text().strip() for name in wanted
    ):
        assert process.poll() is None, process.communicate()[0]
        time.sleep(0.1)
    started = [int((pids / name).read_text()) for name in wanted]
    process.send_signal(signal.SIGINT)
    output, _ = process.communicate(timeout=60)
    assert process.returncode != 0, output
    deadline = time.monotonic() + 10
    while time.monotonic() < deadline and any(_alive(pid) for pid in started):
        time.sleep(0.1)
    assert not [pid for pid in started if _alive(pid)], output


def _alive(pid: int) -> bool:
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    return True


def test_the_executor_s_options_are_snakemake_s(run: Run) -> None:
    got = subprocess.run(
        [run.command("", "--help")[0], "-m", "snakemake", "--help"],
        cwd=run.src,
        env=run.env(),
        capture_output=True,
        text=True,
        timeout=120,
    )
    assert got.returncode == 0, got.stderr
    for option in (
        "--biffix-images-dir",
        "--biffix-image-map",
        "--biffix-default-image",
        "--biffix-bind",
    ):
        assert option in got.stdout


def test_the_images_directory_is_required(run: Run) -> None:
    command = run.command('rule a:\n    output: "a.txt"\n    shell: "touch {output}"\n')
    at = command.index("--biffix-images-dir")
    del command[at : at + 2]
    got = subprocess.run(
        command, cwd=run.src, env=run.env(), capture_output=True, text=True, timeout=120
    )
    assert got.returncode != 0
    assert "--biffix-images-dir" in got.stderr


def test_a_job_whose_container_cannot_start_fails_alone(run: Run) -> None:
    """No apptainer on PATH: the job with an image fails as any failed job, and one that needs
    no container still runs (--keep-going)."""
    (run.bin / "apptainer").unlink()
    got = run.run(
        f"""rule all:
    input: "contained.txt", "host.txt"
rule contained:
    output: "contained.txt"
    resources: aws_batch_container_image="tool:1"
    {WRITE_IMAGE}
rule host:
    output: "host.txt"
    {WRITE_IMAGE}
""",
        "--keep-going",
        default_image=None,
    )
    assert got.returncode != 0
    assert "could not start apptainer:" in got.stderr
    assert run.output("host.txt") == "host"
    assert not (run.src / "contained.txt").exists()


def test_a_job_killed_by_a_signal_is_reported_so(run: Run) -> None:
    got = run.run("""rule killed:
    output: "a.txt"
    run:
        import os, signal
        os.killpg(os.getpgid(0), signal.SIGKILL)
""")
    assert got.returncode != 0
    assert "The job's process was stopped by signal 9." in got.stderr


@pytest.mark.parametrize(
    ("body", "why"),
    [('{"a": "x", "a": "y"}', "image 'a' is in the map twice"), (None, "No such file")],
)
def test_an_unreadable_image_map_stops_the_run_before_any_job(
    run: Run, body: str | None, why: str
) -> None:
    if body is None:
        run.map_file.unlink()
    else:
        run.map_file.write_text(body)
    got = run.run(f'rule a:\n    output: "a.txt"\n    {WRITE_IMAGE}\n')
    assert got.returncode != 0
    assert f"Failed to read the container image map {run.map_file}: " in got.stderr
    assert why in got.stderr
    assert run.calls() == []


def test_a_job_whose_image_resource_is_empty_fails_rather_than_run_on_the_host(run: Run) -> None:
    got = run.run(f"""rule empty:
    output: "a.txt"
    resources: aws_batch_container_image=""
    {WRITE_IMAGE}
""")
    assert got.returncode != 0
    assert "empty: container image '' is not an image reference" in got.stderr
    assert not (run.src / "a.txt").exists()
    assert run.calls() == []


def test_a_job_s_snakemake_starts_where_snakemake_did_not_in_the_workflow_s_workdir(
    run: Run,
) -> None:
    """A `workdir:` moves the main process; a job's own Snakemake reads the workflow and applies
    its `workdir:` again, so it starts where the main process started."""
    got = run.run('workdir: "wd"\nrule a:\n    output: "a.txt"\n    shell: "pwd > {output}"\n')
    assert got.returncode == 0, got.stderr
    assert Path(run.output("wd/a.txt")).resolve() == (run.src / "wd").resolve()
    assert not (run.src / "wd/wd").exists()


def test_a_group_job_s_snakemake_reads_workflow_cores_as_the_main_process_does(run: Run) -> None:
    """Every job's own Snakemake, a group job's included, gets the main process's --cores, once:
    a workflow that reads workflow.cores builds the same DAG in both. Here a grouped rule takes one
    input per core, which a group job's Snakemake given fewer cores would silently cut short."""
    got = run.run(
        """rule all:
    input: "merged.txt", "solo.txt"
rule chunk:
    output: "chunk{i}.txt"
    shell: "echo {wildcards.i} > {output}"
rule merge:
    input: expand("chunk{i}.txt", i=range(workflow.cores))
    output: "merged.txt"
    group: "g"
    shell: "cat {input} > {output}"
rule solo:
    output: "solo.txt"
    shell: "touch {output}"
""",
        cores=4,
    )
    assert got.returncode == 0, got.stderr
    assert run.output("merged.txt").split() == ["0", "1", "2", "3"]
    calls = run.calls()
    assert len([c for c in calls if "--local-groupid" in c]) == 1
    for call in calls:
        assert call.count(" --cores ") == 1 and " --cores 4 " in call, call


def test_a_variable_the_main_process_registers_reaches_every_job(run: Run) -> None:
    """A variable registered in Snakemake's main process (as the biffix coordinator's wrapper
    Snakefile registers the run's credentials) reaches each job, a group job's too, beside
    --envvars: on every Snakemake 9, though before 9.6 Snakemake itself passes a remote executor
    only --envvars."""
    got = run.run(
        """if workflow.is_main_process:
    workflow.register_envvars("REGISTERED_VAR")
rule all:
    input: "a.txt", "b.txt"
rule a:
    output: "a.txt"
    shell: "echo ${{REGISTERED_VAR:-}}:${{PASSED_VAR:-}} > {output}"
rule b:
    output: "b.txt"
    group: "g"
    shell: "echo ${{REGISTERED_VAR:-}}:${{PASSED_VAR:-}} > {output}"
""",
        "--envvars",
        "PASSED_VAR",
        env={"REGISTERED_VAR": "r", "PASSED_VAR": "p"},
    )
    assert got.returncode == 0, got.stderr
    assert run.output("a.txt") == run.output("b.txt") == "r:p"
