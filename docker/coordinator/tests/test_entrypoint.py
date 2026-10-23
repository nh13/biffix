"""The coordinator entrypoint (`docker/coordinator/entrypoint.sh`), run for real in a sandbox (a
`Job`): a stand-in runtime layer under a temporary `BIFFIX_RUNTIME_ROOT`, a work root in a temporary
directory (BIFFIX_WORK_ROOT), its libraries and helpers (lib/) in another (BIFFIX_LIB_DIR), a
stand-in boto3 over a directory (fakes/py), and a fake engine that reports `$FAKE_ENGINE_OUTPUT` as
its version, records its command line and environment, and exits with a chosen code. First the
runtime check, then the rest of the run.

The fake engines are symlinks to ONE script written once per session: macOS can stall the first
exec of every newly written executable for tens of seconds."""

from __future__ import annotations

import gzip
import hashlib
import io
import json
import os
import shutil
import signal
import subprocess
import tarfile
import threading
import time
from collections.abc import Callable
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

import pytest

SCRIPT = Path(__file__).resolve().parents[1] / "entrypoint.sh"
LIB = SCRIPT.parent / "lib"
FAKES = Path(__file__).parent / "fakes"
GROUP = "/biffix/biffix-dev-project-proj-a/batch"
# The engine release each fake runtime reports, as the version pins it.
VERSIONS = {"snakemake": "9.27.0", "nextflow": "25.10.4"}

_ENGINE = r"""#!/usr/bin/env bash
case "${1:-}" in --version|-v) echo "$FAKE_ENGINE_OUTPUT"; exit 0 ;; esac
printf '%s\n' "$@" > "$FAKE_ARGV"
for v in SNAKEMAKE_AWS_BATCH_JOB_TAGS SNAKEMAKE_AWS_BATCH_CONTAINER_IMAGE_MAP NXF_UUID \
  NXF_IGNORE_RESUME_HISTORY NXF_PLUGINS_DEFAULT BIFFIX_IMAGE_MAP_FILE PWD; do
  printf '%s=%s\n' "$v" "${!v-<unset>}"
done > "$FAKE_ENV"
ls .snakemake/metadata > "$FAKE_SEEN" 2>/dev/null || true
if [ -n "${FAKE_WRITE_METADATA:-}" ]; then
  mkdir -p .snakemake/metadata && echo new > .snakemake/metadata/new-output
fi
if [ -n "${FAKE_WAIT_FOR_TERM:-}" ]; then
  # Records the signal that stopped it. A signal ignored when bash starts cannot be trapped, so the
  # INT trap also shows the engine was started with SIGINT at its default.
  trap 'echo TERM > "$FAKE_TERMED"; exit 143' TERM
  trap 'echo INT > "$FAKE_TERMED"; exit 130' INT
  [ -z "${FAKE_IGNORE_STOP:-}" ] || trap '' TERM INT
  touch "$FAKE_READY"
  while :; do sleep 0.1; done
fi
exit "${FAKE_RC:-0}"
"""


@pytest.fixture(scope="session")
def engine_script(tmp_path_factory: pytest.TempPathFactory) -> Path:
    exe = tmp_path_factory.mktemp("recording-engine") / "engine"
    exe.write_text(_ENGINE)
    exe.chmod(0o755)
    return exe


def _source_tarball() -> bytes:
    buf = io.BytesIO()
    # mtime=0: the same bytes every time (gzip otherwise records the time it was written).
    with (
        gzip.GzipFile(fileobj=buf, mode="wb", mtime=0) as gz,
        tarfile.open(fileobj=gz, mode="w") as tar,
    ):
        data = b"rule all:\n    shell: 'true'\n"
        info = tarfile.TarInfo("Snakefile")
        info.size = len(data)
        tar.addfile(info, io.BytesIO(data))
    return buf.getvalue()


class Job:
    """One coordinator run's sandbox: a runtime layer, a work root, and a fake S3."""

    SOURCE_KEY = "workflow-sources/v/source.tar.gz"

    def __init__(self, root: Path, engine: str, engine_script: Path, version: str):
        self.root, self.engine, self.version = root, engine, version
        self.runtime = root / "runtime"
        rel = {"snakemake": "sm/bin/snakemake", "nextflow": "nf/bin/nextflow"}[engine]
        self.engine_exe = self.runtime / rel
        self.engine_exe.parent.mkdir(parents=True)
        self.engine_exe.symlink_to(engine_script)
        if engine == "nextflow":
            for plugin in ("nf-amazon-3.4.4", "nf-cloudcache-0.5.0", "nf-biffix-0.5.0"):
                (self.runtime / "nf/plugins" / plugin).mkdir(parents=True)
        self.work, self.s3, self.lib, self.out = (root / d for d in ("work", "s3", "lib", "out"))
        for d in (self.work, self.s3, self.lib, self.out):
            d.mkdir()
        # The image's /usr/local/lib/biffix: the script's libraries and helpers, copied (not
        # linked) so a test may replace one in its own sandbox.
        for f in LIB.iterdir():
            if f.is_file():
                shutil.copy(f, self.lib / f.name)
        tarball = _source_tarball()
        self.put("bucket", self.SOURCE_KEY, tarball)
        self.source_sha256 = hashlib.sha256(tarball).hexdigest()

    def put(self, bucket: str, key: str, body: bytes) -> None:
        path = self.s3 / bucket / key
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(body)

    def get(self, bucket: str, key: str) -> bytes:
        return (self.s3 / bucket / key).read_bytes()

    def source(self, **fields: str) -> str:
        return json.dumps(
            {"kind": "s3", "uri": f"s3://bucket/{self.SOURCE_KEY}", "sha256": self.source_sha256}
            | fields
        )

    def env(self, **overrides: str) -> dict[str, str]:
        version_output = (
            self.version if self.engine == "snakemake" else f"nextflow version {self.version}.1"
        )
        env = {
            "PATH": os.environ["PATH"],
            "HOME": str(self.root),
            "PYTHONPATH": str(FAKES / "py"),
            "FAKE_S3": str(self.s3),
            "FAKE_ENGINE_OUTPUT": version_output,
            "FAKE_ARGV": str(self.out / "argv"),
            "FAKE_ENV": str(self.out / "env"),
            "FAKE_SEEN": str(self.out / "seen"),
            "FAKE_READY": str(self.out / "ready"),
            "FAKE_TERMED": str(self.out / "termed"),
            "BIFFIX_RUN_ID": "run-test",
            "BIFFIX_ENTRYPOINT": "Snakefile",
            "BIFFIX_PROJECT_BUCKET": "bucket",
            "BIFFIX_PROJECT": "proj-a",
            "BIFFIX_TASK_LOG_GROUP": GROUP,
            "BIFFIX_REGION": "us-test-1",
            "BIFFIX_RUN_PREFIX": "runs/run-test/",
            "BIFFIX_ENGINE": self.engine,
            "BIFFIX_ENGINE_VERSION": self.version,
            "BIFFIX_RUNTIME_ROOT": str(self.runtime),
            "BIFFIX_WORK_ROOT": str(self.work),
            "BIFFIX_LIB_DIR": str(self.lib),
            "BIFFIX_WORKFLOW_SOURCE": self.source(),
            "BIFFIX_PARAMS": json.dumps({"sample": "SECRET-SAMPLE-ID"}),
            "BIFFIX_QUEUE_CLASS_MAP": json.dumps(
                {"coordinator-free-spot": "arn:queue/first", "cpu": "arn:queue/second"}
            ),
            "SNAKEMAKE_AWS_BATCH_JOB_ROLE": "arn:role/run",
        }
        return env | overrides

    def run(self, **overrides: str) -> subprocess.CompletedProcess[str]:
        for f in ("argv", "env", "seen"):
            (self.out / f).unlink(missing_ok=True)
        return subprocess.run(
            ["bash", str(SCRIPT)],
            env=self.env(**overrides),
            capture_output=True,
            text=True,
            timeout=60,
        )

    @property
    def argv(self) -> list[str]:
        return (self.out / "argv").read_text().splitlines()

    @property
    def engine_env(self) -> dict[str, str]:
        """The variables the fake engine records (`<unset>` for one it was not given)."""
        lines = (self.out / "env").read_text().splitlines()
        return dict(line.split("=", 1) for line in lines)


@pytest.fixture
def sm_job(tmp_path: Path, engine_script: Path) -> Job:
    return Job(tmp_path, "snakemake", engine_script, VERSIONS["snakemake"])


@pytest.fixture
def nf_job(tmp_path: Path, engine_script: Path) -> Job:
    return Job(tmp_path, "nextflow", engine_script, VERSIONS["nextflow"])


def _after(argv: list[str], flag: str) -> str:
    return argv[argv.index(flag) + 1]


# ── the runtime check, before anything is staged ──


@pytest.mark.parametrize(
    ("engine", "version_output", "pinned"),
    [
        ("snakemake", "9.27.0", "9.27.0"),
        ("nextflow", "nextflow version 25.10.4.11177", "25.10.4"),
    ],
)
def test_a_matching_runtime_passes_the_check(
    tmp_path: Path, engine_script: Path, engine: str, version_output: str, pinned: str
) -> None:
    job = Job(tmp_path, engine, engine_script, pinned)
    got = job.run(BIFFIX_RUNTIME_KEY="k", FAKE_ENGINE_OUTPUT=version_output)
    assert got.returncode == 0, got.stderr
    assert f"runtime {engine} {pinned} (key k)" in got.stderr
    assert job.argv  # the engine ran


@pytest.mark.parametrize(
    ("engine", "version_output", "pinned"),
    [
        ("snakemake", "9.26.0", "9.27.0"),
        ("nextflow", "nextflow version 25.10.3.11000", "25.10.4"),
    ],
)
def test_a_different_release_fails_with_ex_config(
    tmp_path: Path, engine_script: Path, engine: str, version_output: str, pinned: str
) -> None:
    job = Job(tmp_path, engine, engine_script, pinned)
    got = job.run(FAKE_ENGINE_OUTPUT=version_output)
    assert got.returncode == 78
    assert f"but the version pins {pinned}" in got.stderr
    assert not (job.out / "argv").exists() and not (job.work / "src").exists()


@pytest.mark.parametrize("engine", ["snakemake", "nextflow"])
def test_an_image_without_the_runtime_fails_with_ex_config(
    tmp_path: Path, engine_script: Path, engine: str
) -> None:
    job = Job(tmp_path, engine, engine_script, "9.27.0")
    job.engine_exe.unlink()
    got = job.run()
    assert got.returncode == 78
    assert "re-register the workflow" in got.stderr


@pytest.mark.parametrize(
    "name",
    [
        "BIFFIX_RUN_ID",
        "BIFFIX_ENTRYPOINT",
        "BIFFIX_PROJECT_BUCKET",
        "BIFFIX_PROJECT",
        "BIFFIX_TASK_LOG_GROUP",
        "BIFFIX_REGION",
        "BIFFIX_RUN_PREFIX",
        "BIFFIX_ENGINE_VERSION",
    ],
)
def test_every_required_variable_fails_with_ex_config(sm_job: Job, name: str) -> None:
    got = sm_job.run(**{name: ""})
    assert got.returncode == 78, got.stderr
    assert f"biffix-coordinator: {name} required" in got.stderr
    assert not (sm_job.out / "argv").exists() and not (sm_job.work / "src").exists()


def test_an_unknown_engine_fails_with_ex_config(sm_job: Job) -> None:
    got = sm_job.run(BIFFIX_ENGINE="cromwell")
    assert got.returncode == 78
    assert "unknown engine 'cromwell'" in got.stderr


# ── the rest of the script, driven end to end ──


def test_the_exit_code_is_the_engine_s_and_the_run_is_wired(sm_job: Job) -> None:
    got = sm_job.run(FAKE_RC="3")
    assert got.returncode == 3, got.stderr
    argv = sm_job.argv
    # The default worker queue is the map's first entry (the map holds worker classes only),
    # whatever it is called.
    assert _after(argv, "--aws-batch-job-queue") == "arn:queue/first"
    assert _after(argv, "--default-storage-prefix") == "s3://bucket/runs/run-test/"
    assert _after(argv, "--retries") == "3" and "--keep-going" in argv
    assert (sm_job.work / "src/Snakefile").exists()
    assert "matches its registered sha256" in got.stderr
    # The post-engine steps ran, and did not change the outcome.
    assert "no events.jsonl to upload" in got.stderr
    # Never traced: the job log does not carry the run's parameters.
    assert "SECRET-SAMPLE-ID" not in got.stderr
    assert f"biffix-coordinator: running run_in {sm_job.work}/src snakemake" in got.stderr


def test_an_explicit_queue_class_the_map_lacks_fails(sm_job: Job) -> None:
    got = sm_job.run(BIFFIX_QUEUE_CLASS="gpu")
    assert got.returncode == 78
    assert "no worker queue resolved" in got.stderr


def test_snakemake_child_jobs_always_carry_the_run_and_project_tags(sm_job: Job) -> None:
    def tags(**env: str) -> str:
        assert sm_job.run(**env).returncode == 0
        return sm_job.engine_env["SNAKEMAKE_AWS_BATCH_JOB_TAGS"]

    assert tags() == "biffix:run=run-test,biffix:project=proj-a"
    assert tags(SNAKEMAKE_AWS_BATCH_JOB_TAGS="team=x") == (
        "team=x,biffix:run=run-test,biffix:project=proj-a"
    )
    # What the backend sends (the run tag) is kept as it is.
    sent = "biffix:run=run-test,team=x"
    assert tags(SNAKEMAKE_AWS_BATCH_JOB_TAGS=sent) == f"{sent},biffix:project=proj-a"
    both = "biffix:project=proj-a,biffix:run=run-test"
    assert tags(SNAKEMAKE_AWS_BATCH_JOB_TAGS=both) == both


def test_snakemake_tasks_get_spot_attempts_and_the_project_s_log_group(sm_job: Job) -> None:
    got = sm_job.run()
    assert got.returncode == 0, got.stderr
    assert _after(sm_job.argv, "--aws-batch-spot-attempts") == "5"
    assert _after(sm_job.argv, "--aws-batch-log-group") == GROUP
    assert "WARNING" not in got.stderr
    # BIFFIX_SPOT_ATTEMPTS overrides the default.
    got = sm_job.run(BIFFIX_SPOT_ATTEMPTS="2")
    assert got.returncode == 0, got.stderr
    assert _after(sm_job.argv, "--aws-batch-spot-attempts") == "2"


@pytest.mark.parametrize(
    ("value", "keep_going"),
    [("true", True), ("ON", True), ("1", True), ("false", False), ("No", False), ("OFF", False)],
)
def test_keep_going_is_a_case_insensitive_boolean(
    sm_job: Job, value: str, keep_going: bool
) -> None:
    got = sm_job.run(BIFFIX_KEEP_GOING=value)
    assert got.returncode == 0, got.stderr
    assert ("--keep-going" in sm_job.argv) is keep_going


def test_an_unknown_keep_going_fails_before_snakemake_runs(sm_job: Job) -> None:
    # It used to keep going, whatever the run asked for.
    got = sm_job.run(BIFFIX_KEEP_GOING="nope")
    assert got.returncode == 78
    assert "BIFFIX_KEEP_GOING is not a boolean" in got.stderr
    assert not (sm_job.out / "argv").exists()


@pytest.mark.parametrize(
    ("engine", "knob"),
    [
        ("snakemake", "BIFFIX_SPOT_ATTEMPTS"),
        ("snakemake", "BIFFIX_RETRIES"),
        ("snakemake", "BIFFIX_MAX_JOBS"),
        ("nextflow", "BIFFIX_SPOT_ATTEMPTS"),
        # Nextflow read a bad one as "no retries" and ran.
        ("nextflow", "BIFFIX_RETRIES"),
    ],
)
@pytest.mark.parametrize("value", ["many", "-1", "2.5"])
def test_a_non_numeric_count_fails_before_the_engine_runs(
    tmp_path: Path, engine_script: Path, engine: str, knob: str, value: str
) -> None:
    job = Job(tmp_path, engine, engine_script, VERSIONS[engine])
    got = job.run(**{knob: value})
    assert got.returncode == 78
    assert f"{knob} is not a number: {value}" in got.stderr
    assert not (job.out / "argv").exists()


def test_a_replaced_source_tarball_is_refused(sm_job: Job) -> None:
    sm_job.put("bucket", Job.SOURCE_KEY, _source_tarball() + b"tampered")
    got = sm_job.run()
    assert got.returncode == 78, got.stderr
    assert "refusing to run it" in got.stderr
    assert not (sm_job.out / "argv").exists()
    malformed = sm_job.run(BIFFIX_WORKFLOW_SOURCE=sm_job.source(sha256="nope"))
    assert malformed.returncode == 78
    assert "recorded sha256 is not a sha256: nope" in malformed.stderr


def test_a_tarball_without_a_recorded_digest_is_refused(sm_job: Job) -> None:
    source = json.dumps({"kind": "s3", "uri": f"s3://bucket/{Job.SOURCE_KEY}"})
    got = sm_job.run(BIFFIX_WORKFLOW_SOURCE=source)
    assert got.returncode == 78, got.stderr
    assert "records no sha256 for its source tarball" in got.stderr
    assert not (sm_job.out / "argv").exists()


def test_clone_credentials_are_never_logged(sm_job: Job) -> None:
    repo = "https://user:s3cr3t-token@127.0.0.1:9/x.git"
    got = sm_job.run(BIFFIX_WORKFLOW_SOURCE=json.dumps({"kind": "git", "repo": repo}))
    assert got.returncode != 0
    (line,) = [ln for ln in got.stderr.splitlines() if "biffix-coordinator: git clone" in ln]
    assert "https://127.0.0.1:9/x.git" in line
    # Nowhere in the job's log: not the coordinator's line, nor git's own error output.
    assert "s3cr3t-token" not in got.stderr + got.stdout


def _metadata_tarball(names: list[str]) -> bytes:
    buf = io.BytesIO()
    with tarfile.open(fileobj=buf, mode="w:gz") as tar:
        for name in names:
            info = tarfile.TarInfo(f"metadata/{name}")
            info.size = 3
            tar.addfile(info, io.BytesIO(b"old"))
    return buf.getvalue()


def test_metadata_in_the_store_is_restored_and_kept_as_a_union(sm_job: Job) -> None:
    # An earlier run into the same (stable) output prefix left its metadata; a new, non-resume run
    # sees it, and what it syncs back holds both runs' records.
    key = "shared/.snakemake/biffix-metadata.tar.gz"
    sm_job.put("bucket", key, _metadata_tarball(["old-output"]))
    got = sm_job.run(BIFFIX_OUTPUT_PREFIX="s3://bucket/shared/", FAKE_WRITE_METADATA="1")
    assert got.returncode == 0, got.stderr
    assert (sm_job.out / "seen").read_text().split() == ["old-output"]
    with tarfile.open(fileobj=io.BytesIO(sm_job.get("bucket", key)), mode="r:gz") as tar:
        names = set(tar.getnames())
    assert {"metadata/old-output", "metadata/new-output"} <= names


def _cache_helper(job: Job, text: str) -> None:
    (job.lib / "snakemake_cache_rules.py").write_text(text)


def test_cache_rules_are_read_from_the_helper_s_marked_lines_only(sm_job: Job) -> None:
    _cache_helper(
        sm_job,
        "print('-' * 40)\nprint('BIFFIX_CACHE_RULE=align')\nprint('banner line')\n"
        "print('BIFFIX_CACHE_RULE=index')\n",
    )
    got = sm_job.run(BIFFIX_SNAKEMAKE_CACHE="s3://bucket/cache/")
    assert got.returncode == 0, got.stderr
    argv = sm_job.argv
    at = argv.index("--cache")
    assert argv[at + 1 : at + 3] == ["align", "index"]
    assert "-" * 40 not in argv and "banner line" not in argv


def test_a_failing_cache_helper_is_logged_and_the_run_goes_on(sm_job: Job) -> None:
    _cache_helper(sm_job, "import sys\nsys.exit('cannot parse the Snakefile: boom')\n")
    got = sm_job.run(BIFFIX_SNAKEMAKE_CACHE="s3://bucket/cache/")
    assert got.returncode == 0, got.stderr
    assert "cannot parse the Snakefile: boom" in got.stderr
    assert "could not read the workflow's cache: rules" in got.stderr
    assert "--cache" not in sm_job.argv


def _cancel(job: Job, **env: str) -> tuple[int, str, float]:
    """Run the job until its engine is up, SIGTERM it as a Batch cancel does, and return its exit
    code, its stderr and the seconds it took to exit after the signal."""
    proc = subprocess.Popen(
        ["bash", str(SCRIPT)],
        env=job.env(FAKE_WAIT_FOR_TERM="1", FAKE_WRITE_METADATA="1", **env),
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        start_new_session=True,  # its own process group, so a failed test can kill all of it
    )
    try:
        deadline = time.monotonic() + 30
        while not (job.out / "ready").exists():
            assert proc.poll() is None, proc.communicate()
            assert time.monotonic() < deadline, "the engine never started"
            time.sleep(0.05)
        signalled = time.monotonic()
        proc.send_signal(signal.SIGTERM)
        _, stderr = proc.communicate(timeout=30)
        took = time.monotonic() - signalled
    finally:
        if proc.poll() is None:
            os.killpg(proc.pid, signal.SIGKILL)
            proc.communicate()
    return proc.returncode, stderr, took


def test_a_cancel_stops_snakemake_at_once_and_the_cleanup_still_runs(sm_job: Job) -> None:
    # SIGINT, not SIGTERM: Snakemake takes SIGTERM as "finish the running jobs first", and SIGINT
    # as "cancel them (TerminateJob) and exit".
    rc, stderr, _ = _cancel(sm_job)
    assert rc == 143, stderr  # the cancel's, not the engine's (130)
    assert (sm_job.out / "termed").read_text().strip() == "INT"
    assert "received SIGTERM" in stderr
    assert "stopping the engine (SIGINT)" in stderr
    # The final metadata sync and the events.jsonl step ran after the engine stopped.
    assert (sm_job.s3 / "bucket/runs/run-test/.snakemake/biffix-metadata.tar.gz").exists()
    assert "no events.jsonl to upload" in stderr


def test_a_cancel_passes_nextflow_the_sigterm_it_stops_on(nf_job: Job) -> None:
    rc, stderr, _ = _cancel(nf_job)
    assert rc == 143, stderr
    assert (nf_job.out / "termed").read_text().strip() == "TERM"
    assert "no events.jsonl to upload" in stderr


def test_an_engine_that_does_not_stop_is_killed_in_time_for_the_cleanup(sm_job: Job) -> None:
    rc, stderr, took = _cancel(sm_job, FAKE_IGNORE_STOP="1", BIFFIX_STOP_GRACE_SECS="1")
    assert rc == 143, stderr
    assert "did not stop within 1s; killing it" in stderr
    assert took < 15, took
    assert (sm_job.s3 / "bucket/runs/run-test/.snakemake/biffix-metadata.tar.gz").exists()
    assert "no events.jsonl to upload" in stderr


def test_nextflow_config_escapes_every_value(nf_job: Job) -> None:
    # A `'` or a `\` in any value would otherwise end its Groovy string, and let the rest of the
    # value into the config.
    got = nf_job.run(
        BIFFIX_OUTPUT_PREFIX="s3://bucket/o'brien\\x/",
        BIFFIX_RUN_ID="run'1\\",
        BIFFIX_PROJECT="proj'a\\",
        BIFFIX_REGION="us-'east\\",
        SNAKEMAKE_AWS_BATCH_JOB_ROLE="arn:role/'r\\",
        BIFFIX_WORKER_IMAGE="reg/'img\\:1",
        BIFFIX_TASK_LOG_GROUP="/'group\\",
        BIFFIX_QUEUE_CLASS_MAP=json.dumps({"spot": "arn:queue/'q\\"}),
    )
    assert got.returncode == 0, got.stderr
    config = (nf_job.work / "nextflow.config").read_text().splitlines()
    for line in [
        "process.queue = 'arn:queue/\\'q\\\\'",
        "process.container = 'reg/\\'img\\\\:1'",
        "process.resourceLabels = ['biffix:run': 'run\\'1\\\\', 'biffix:project': 'proj\\'a\\\\']",
        "aws.region = 'us-\\'east\\\\'",
        "aws.batch.jobRole = 'arn:role/\\'r\\\\'",
        "aws.batch.logsGroup = '/\\'group\\\\'",
        "workDir = 's3://bucket/o\\'brien\\\\x/work'",
        "cloudcache { enabled = true; path = 's3://bucket/o\\'brien\\\\x/.nextflow/cache' }",
    ]:
        assert line in config, line


def test_nextflow_config_wires_the_run(nf_job: Job) -> None:
    got = nf_job.run()
    assert got.returncode == 0, got.stderr
    config = (nf_job.work / "nextflow.config").read_text()
    assert (
        "process.resourceLabels = ['biffix:run': 'run-test', 'biffix:project': 'proj-a']" in config
    )
    # Spot reclaims are retried by Batch, not out of the errorStrategy's budget.
    assert "aws.batch.maxSpotAttempts = 5" in config
    assert nf_job.argv[:2] == ["-log", str(nf_job.work / ".nextflow.log")]
    # nf-biffix's package is outside Nextflow's own, so its DEBUG lines need asking for.
    assert nf_job.argv[2:4] == ["-debug", "io.github.nh13.biffix"]


def test_nextflow_runs_from_the_source_root_so_the_pipeline_s_config_is_read(nf_job: Job) -> None:
    # Nextflow reads the launch dir's nextflow.config: from the work root, a pipeline whose config
    # sits at the repo root above its entrypoint ran without its plugins {} (nf-schema).
    got = nf_job.run(BIFFIX_ENTRYPOINT="refprep/main.nf")
    assert got.returncode == 0, got.stderr
    assert Path(nf_job.engine_env["PWD"]).resolve() == (nf_job.work / "src").resolve()
    assert nf_job.argv[4:6] == ["run", str(nf_job.work / "src/refprep/main.nf")]
    assert _after(nf_job.argv, "-c") == str(nf_job.work / "nextflow.config")


def test_snakemake_runs_from_the_source_root_so_the_workflow_s_configfile_loads(
    sm_job: Job,
) -> None:
    # The standard layout (workflow/Snakefile, config/config.yaml at the root) reads its config with
    # `configfile: "config/config.yaml"`, relative to Snakemake's working directory. From the work
    # root it was not found and, with a --configfile given, skipped without a word.
    sm_job.put(
        "bucket",
        "src/workflow/Snakefile",
        b'configfile: "config/config.yaml"\nrule all:\n    shell: "true"\n',
    )
    sm_job.put("bucket", "src/config/config.yaml", b"tf_database_file: db.meme\n")
    got = sm_job.run(
        BIFFIX_WORKFLOW_SOURCE=json.dumps({"kind": "s3", "uri": "s3://bucket/src"}),
        BIFFIX_ENTRYPOINT="workflow/Snakefile",
        BIFFIX_TARGET="results/a.txt",
    )
    assert got.returncode == 0, got.stderr
    src = sm_job.work / "src"
    cwd = Path(sm_job.engine_env["PWD"]).resolve()
    assert cwd == src.resolve()
    assert (cwd / "config/config.yaml").read_text() == "tf_database_file: db.meme\n"
    assert _after(sm_job.argv, "--snakefile") == str(src / "workflow/Snakefile")
    # biffix's params, which override the workflow's config, are below the working directory too,
    # as Snakemake requires of every file it ships to a remote job; so is the whole source.
    configfile = Path(_after(sm_job.argv, "--configfile"))
    assert configfile.resolve().parent == cwd
    assert json.loads(configfile.read_text()) == {"sample": "SECRET-SAMPLE-ID"}
    env = {k: v for k, v in os.environ.items() if not k.startswith("GIT_")}
    shipped = subprocess.run(
        ["git", "ls-files", "."], cwd=cwd, capture_output=True, text=True, check=True, env=env
    ).stdout.split()
    assert shipped == [".biffix-config.json", "config/config.yaml", "workflow/Snakefile"]
    # The target is the source root's, as from a checkout.
    assert sm_job.argv[-1] == "results/a.txt"


def test_nextflow_tasks_log_to_the_project_s_own_group(nf_job: Job) -> None:
    # The job definition names the project's task log group; nf-amazon writes it into each task
    # job definition's awslogs-group.
    got = nf_job.run()
    assert got.returncode == 0, got.stderr
    config = (nf_job.work / "nextflow.config").read_text()
    assert f"aws.batch.logsGroup = '{GROUP}'" in config


def test_a_line_break_in_a_nextflow_config_value_is_refused(nf_job: Job) -> None:
    got = nf_job.run(BIFFIX_OUTPUT_PREFIX="s3://bucket/a\nprocess.executor = 'local'\n/")
    assert got.returncode == 78
    assert "cannot go into nextflow.config" in got.stderr
    assert not (nf_job.out / "argv").exists()


def _with_workflow_plugins(job: Job, *plugins: str) -> None:
    """The layer as the runtime build leaves it for a workflow that declares plugins: each unpacked
    next to biffix's, and listed in the nf/workflow-plugins marker."""
    for p in plugins:
        (job.runtime / "nf/plugins" / p.replace("@", "-")).mkdir()
    (job.runtime / "nf/workflow-plugins").write_text("".join(f"{p}\n" for p in plugins))


def test_a_layer_without_workflow_plugins_runs_nf_biffix_as_it_always_has(nf_job: Job) -> None:
    # No marker (a layer built before workflows could declare plugins, or one that declares none):
    # nf-biffix in the -c config's `plugins {}`, exactly as before, and no default plugins.
    got = nf_job.run()
    assert got.returncode == 0, got.stderr
    assert "plugins { id 'nf-biffix@0.5.0' }" in (nf_job.work / "nextflow.config").read_text()
    assert nf_job.engine_env["NXF_PLUGINS_DEFAULT"] == "<unset>"
    assert "-plugins" not in nf_job.argv
    # Such a layer was never required to have nf-amazon found by the coordinator.
    (nf_job.runtime / "nf/plugins/nf-amazon-3.4.4").rmdir()
    got = nf_job.run()
    assert got.returncode == 0, got.stderr


def test_a_layer_with_workflow_plugins_enables_biffix_s_without_replacing_the_pipeline_s(
    nf_job: Job,
) -> None:
    # A `plugins {}` block in the -c config would replace the pipeline's own list (nf-core's
    # nf-schema, say); the layer's nf-amazon and nf-biffix are Nextflow's defaults instead.
    _with_workflow_plugins(nf_job, "nf-schema@2.7.2")
    got = nf_job.run()
    assert got.returncode == 0, got.stderr
    assert "plugins" not in (nf_job.work / "nextflow.config").read_text()
    assert "-plugins" not in nf_job.argv
    assert nf_job.engine_env["NXF_PLUGINS_DEFAULT"] == "nf-amazon@3.4.4,nf-biffix@0.5.0"


def test_a_workflow_plugin_named_like_biffix_s_is_not_taken_for_it(nf_job: Job) -> None:
    # Registration refuses such ids; the lookup also matches only a version after `<id>-`.
    _with_workflow_plugins(nf_job, "nf-biffix-ext@1.0.0", "nf-amazon-x@2.0.0")
    got = nf_job.run()
    assert got.returncode == 0, got.stderr
    assert nf_job.engine_env["NXF_PLUGINS_DEFAULT"] == "nf-amazon@3.4.4,nf-biffix@0.5.0"


@pytest.mark.parametrize(
    ("plugin", "workflow_plugins"),
    [("nf-biffix", False), ("nf-biffix", True), ("nf-amazon", True)],
    ids=["nf-biffix-config", "nf-biffix-defaults", "nf-amazon-defaults"],
)
def test_nextflow_needs_exactly_one_of_biffix_s_plugins_in_the_layer(
    nf_job: Job, plugin: str, workflow_plugins: bool
) -> None:
    if workflow_plugins:
        _with_workflow_plugins(nf_job, "nf-schema@2.7.2")
    (nf_job.runtime / f"nf/plugins/{plugin}-9.9.9").mkdir()
    got = nf_job.run()
    assert got.returncode == 78
    assert f"exactly one {plugin} plugin" in got.stderr
    assert not (nf_job.out / "argv").exists()


def test_nextflow_refuses_a_layer_plugin_version_it_cannot_list(nf_job: Job) -> None:
    _with_workflow_plugins(nf_job, "nf-schema@2.7.2")
    (nf_job.runtime / "nf/plugins/nf-amazon-3.4.4").rename(
        nf_job.runtime / "nf/plugins/nf-amazon-3.4,4"
    )
    got = nf_job.run()
    assert got.returncode == 78
    assert "unusable version" in got.stderr
    assert not (nf_job.out / "argv").exists()


def test_a_nextflow_resume_names_the_prior_session_explicitly(nf_job: Job) -> None:
    # A bare -resume reads a local history this container never has, and runs from scratch.
    got = nf_job.run(BIFFIX_ENGINE_SESSION_ID="u1", BIFFIX_RESUME="1")
    assert got.returncode == 0, got.stderr
    assert _after(nf_job.argv, "-resume") == "u1"
    assert _after(nf_job.argv, "-name") == "run-test"
    env = nf_job.engine_env
    assert env["NXF_IGNORE_RESUME_HISTORY"] == "true"
    assert env["NXF_UUID"] == "<unset>"


def test_a_retried_coordinator_resumes_its_first_attempt_s_session(nf_job: Job) -> None:
    # Batch's second attempt of the coordinator job: the first attempt's finished tasks are in
    # the session's S3 cache, and are not run again.
    got = nf_job.run(BIFFIX_ENGINE_SESSION_ID="u1", AWS_BATCH_JOB_ATTEMPT="2")
    assert got.returncode == 0, got.stderr
    assert _after(nf_job.argv, "-resume") == "u1"
    env = nf_job.engine_env
    assert env["NXF_IGNORE_RESUME_HISTORY"] == "true"
    assert env["NXF_UUID"] == "<unset>"
    got = nf_job.run(BIFFIX_ENGINE_SESSION_ID="u1", AWS_BATCH_JOB_ATTEMPT="1")
    assert got.returncode == 0, got.stderr
    assert "-resume" not in nf_job.argv
    assert nf_job.engine_env["NXF_UUID"] == "u1"


def test_a_fresh_nextflow_run_uses_the_session_biffix_chose(nf_job: Job) -> None:
    got = nf_job.run(BIFFIX_ENGINE_SESSION_ID="u1")
    assert got.returncode == 0, got.stderr
    assert "-resume" not in nf_job.argv
    env = nf_job.engine_env
    assert env["NXF_UUID"] == "u1"
    assert env["NXF_IGNORE_RESUME_HISTORY"] == "<unset>"


def test_nextflow_retry_defaults_rank_below_the_pipeline_s_own_config(nf_job: Job) -> None:
    got = nf_job.run()
    assert got.returncode == 0, got.stderr
    defaults = (nf_job.work / ".nextflow-home/config").read_text()
    assert "process.errorStrategy = 'retry'" in defaults
    assert "process.maxRetries = 3" in defaults
    # The -c file outranks the pipeline's config, so the retry policy is not in it.
    assert _after(nf_job.argv, "-c") == str(nf_job.work / "nextflow.config")
    config = (nf_job.work / "nextflow.config").read_text()
    assert "errorStrategy" not in config and "maxRetries" not in config
    assert nf_job.run(BIFFIX_RETRIES="0").returncode == 0
    assert (nf_job.work / ".nextflow-home/config").read_text() == ""


# ── the rest of the env contract ──


MAP_URI = "s3://bucket/runs/run-test/images.json"


def test_the_image_map_is_handed_to_the_executor(sm_job: Job) -> None:
    sm_job.put("bucket", "runs/run-test/images.json", b'{"tool:1": "reg/tool@sha256:1"}')
    got = sm_job.run(BIFFIX_IMAGE_MAP_URI=MAP_URI)
    assert got.returncode == 0, got.stderr
    path = sm_job.engine_env["SNAKEMAKE_AWS_BATCH_CONTAINER_IMAGE_MAP"]
    assert path == str(sm_job.work / "images.json")
    assert json.loads(Path(path).read_text()) == {"tool:1": "reg/tool@sha256:1"}


def test_the_image_map_is_handed_to_nf_biffix(nf_job: Job) -> None:
    nf_job.put("bucket", "runs/run-test/images.json", b'{"tool:1": "reg/tool@sha256:1"}')
    got = nf_job.run(BIFFIX_IMAGE_MAP_URI=MAP_URI)
    assert got.returncode == 0, got.stderr
    assert nf_job.engine_env["BIFFIX_IMAGE_MAP_FILE"] == str(nf_job.work / "images.json")


@pytest.mark.parametrize("body", [None, b'["tool:1"]', b"not json"])
def test_an_unreadable_image_map_fails_with_ex_config(sm_job: Job, body: bytes | None) -> None:
    if body is not None:
        sm_job.put("bucket", "runs/run-test/images.json", body)
    got = sm_job.run(BIFFIX_IMAGE_MAP_URI="s3://bucket/runs/run-test/images.json")
    assert got.returncode == 78
    assert "could not read the image map" in got.stderr
    assert not (sm_job.out / "argv").exists()


def test_the_child_job_role_is_required(sm_job: Job) -> None:
    got = sm_job.run(SNAKEMAKE_AWS_BATCH_JOB_ROLE="")
    assert got.returncode == 78
    assert "SNAKEMAKE_AWS_BATCH_JOB_ROLE required" in got.stderr
    assert not (sm_job.out / "argv").exists()


def test_the_worker_image_is_snakemake_s_default_task_image(sm_job: Job) -> None:
    got = sm_job.run(BIFFIX_WORKER_IMAGE="reg/worker:1")
    assert got.returncode == 0, got.stderr
    assert _after(sm_job.argv, "--container-image") == "reg/worker:1"
    assert sm_job.run().returncode == 0
    assert "--container-image" not in sm_job.argv


def test_the_worker_image_is_nextflow_s_default_task_image(nf_job: Job) -> None:
    got = nf_job.run(BIFFIX_WORKER_IMAGE="reg/worker:1")
    assert got.returncode == 0, got.stderr
    config = nf_job.work / "nextflow.config"
    assert "process.container = 'reg/worker:1'" in config.read_text()
    assert nf_job.run().returncode == 0
    assert "process.container" not in config.read_text()


def test_a_snakemake_dry_run_submits_nothing(sm_job: Job) -> None:
    assert sm_job.run(BIFFIX_DRY_RUN="1").returncode == 0
    assert "--dry-run" in sm_job.argv
    assert sm_job.run().returncode == 0
    assert "--dry-run" not in sm_job.argv


def test_a_nextflow_dry_run_submits_nothing(nf_job: Job) -> None:
    assert nf_job.run(BIFFIX_DRY_RUN="1").returncode == 0
    assert "-preview" in nf_job.argv
    assert nf_job.run().returncode == 0
    assert "-preview" not in nf_job.argv


def _s3_objects(job: Job) -> set[str]:
    return {str(p.relative_to(job.s3)) for p in job.s3.rglob("*") if p.is_file()}


@pytest.mark.parametrize("engine", ["snakemake", "nextflow"])
def test_a_dry_run_writes_nothing_to_s3(tmp_path: Path, engine_script: Path, engine: str) -> None:
    job = Job(tmp_path, engine, engine_script, VERSIONS[engine])
    before = _s3_objects(job)
    events = job.work / "src/.biffix_events.jsonl"
    events.parent.mkdir(parents=True, exist_ok=True)
    events.write_text('{"kind": "task_created"}\n')
    got = job.run(BIFFIX_DRY_RUN="1", FAKE_WRITE_METADATA="1")
    assert got.returncode == 0, got.stderr
    # No metadata sync, no events.jsonl upload.
    assert _s3_objects(job) == before
    assert "dry run: not uploading events.jsonl" in got.stderr


def test_a_nextflow_dry_run_keeps_its_cache_and_bin_dir_local(nf_job: Job) -> None:
    config = nf_job.work / "nextflow.config"
    assert nf_job.run(BIFFIX_DRY_RUN="1").returncode == 0
    assert "cloudcache" not in config.read_text()
    assert "executor.disableRemoteBinDir = true" in config.read_text()
    assert nf_job.run().returncode == 0
    assert "cloudcache { enabled = true;" in config.read_text()
    assert "disableRemoteBinDir" not in config.read_text()


def test_the_target_is_what_snakemake_builds(sm_job: Job) -> None:
    assert sm_job.run(BIFFIX_TARGET="results/a.txt").returncode == 0
    assert sm_job.argv[-1] == "results/a.txt"


def test_the_target_is_the_nextflow_entry_workflow(nf_job: Job) -> None:
    assert nf_job.run(BIFFIX_TARGET="ALIGN").returncode == 0
    assert _after(nf_job.argv, "-entry") == "ALIGN"
    assert nf_job.run().returncode == 0
    assert "-entry" not in nf_job.argv


def test_the_job_and_retry_counts_override_the_defaults(sm_job: Job) -> None:
    got = sm_job.run(BIFFIX_MAX_JOBS="7", BIFFIX_RETRIES="0")
    assert got.returncode == 0, got.stderr
    assert _after(sm_job.argv, "--jobs") == "7"
    assert _after(sm_job.argv, "--retries") == "0"
    assert sm_job.run().returncode == 0
    assert _after(sm_job.argv, "--jobs") == "100"


def test_events_jsonl_is_archived_under_the_run_s_own_prefix(sm_job: Job) -> None:
    # A shared output prefix (or a resumed run's) holds other runs' outputs; the stream is its own.
    (sm_job.work / "src").mkdir()
    (sm_job.work / "src/.biffix_events.jsonl").write_text('{"kind": "task_created"}\n')
    got = sm_job.run(BIFFIX_OUTPUT_PREFIX="s3://bucket/shared/")
    assert got.returncode == 0, got.stderr
    assert sm_job.get("bucket", "runs/run-test/events.jsonl") == b'{"kind": "task_created"}\n'
    assert not list((sm_job.s3 / "bucket/shared").rglob("events.jsonl"))


def test_a_prefix_source_stages_only_what_is_under_the_prefix(sm_job: Job) -> None:
    sm_job.put("bucket", "src/Snakefile", b"rule all:\n    shell: 'true'\n")
    sm_job.put("bucket", "src/config/x.yaml", b"a: 1\n")
    sm_job.put("bucket", "src2/stray", b"x")  # shares the string prefix, not the path
    got = sm_job.run(BIFFIX_WORKFLOW_SOURCE=json.dumps({"kind": "s3", "uri": "s3://bucket/src"}))
    assert got.returncode == 0, got.stderr
    assert "staged 2 file(s)" in got.stderr
    src = sm_job.work / "src"
    staged = sorted(str(p.relative_to(src)) for p in src.rglob("*") if p.is_file())
    assert staged == [".biffix-config.json", "Snakefile", "config/x.yaml"]


def test_an_empty_prefix_source_is_refused(sm_job: Job) -> None:
    source = json.dumps({"kind": "s3", "uri": "s3://bucket/nothing/"})
    got = sm_job.run(BIFFIX_WORKFLOW_SOURCE=source)
    assert got.returncode != 0
    assert "no objects found under s3://bucket/nothing/" in got.stderr
    assert not (sm_job.out / "argv").exists()


# ── the events.jsonl replay backstop ──


# An events.jsonl line: one RunEvent, as JSON.
Event = dict[str, object]

# A stand-in ingest route's answers: the status refusing a batch of events (of a byte size), or
# None to accept it; and an accepted batch's body.
Reject = Callable[[list[Event], int], int | None]
Answer = Callable[[list[Event]], bytes]


class _Ingest:
    """A stand-in ingest route: records each accepted batch; `reject(events, size)` returns the
    status code to refuse a request with (or None to accept it), and `answer(events)` the body of
    an accepted one."""

    def __init__(self, reject: Reject, answer: Answer = lambda _events: b"") -> None:
        self.batches: list[list[Event]] = []
        self.sizes: list[int] = []
        ingest = self

        class Handler(BaseHTTPRequestHandler):
            def do_POST(self) -> None:
                raw = self.rfile.read(int(self.headers["Content-Length"]))
                events = json.loads(raw)["events"]
                code = reject(events, len(raw))
                body = b""
                if code is None:
                    ingest.batches.append(events)
                    ingest.sizes.append(len(raw))
                    code = 202
                    body = answer(events)
                self.send_response(code)
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            def log_message(self, *args: object) -> None:
                pass

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        threading.Thread(target=self.server.serve_forever, daemon=True).start()
        self.url = f"http://127.0.0.1:{self.server.server_address[1]}/api"

    def close(self) -> None:
        self.server.shutdown()
        self.server.server_close()


def _replay(
    job: Job, events: list[Event], reject: Reject, answer: Answer = lambda _events: b""
) -> tuple[_Ingest, str]:
    (job.work / "src").mkdir(exist_ok=True)
    (job.work / "src/.biffix_events.jsonl").write_text(
        "".join(json.dumps(e) + "\n" for e in events)
    )
    ingest = _Ingest(reject, answer)
    try:
        got = job.run(BIFFIX_INGEST_URL=ingest.url, BIFFIX_INGEST_TOKEN="tok-secret")
    finally:
        ingest.close()
    assert got.returncode == 0, got.stderr
    assert "tok-secret" not in got.stderr
    return ingest, got.stderr


def _task(i: int, **extra: object) -> Event:
    return {"run_id": "run-test", "kind": "task_created", "task_key": f"t{i}", **extra}


def test_the_replay_splits_a_refused_chunk_and_drops_only_the_bad_event(sm_job: Job) -> None:
    events = [_task(i) for i in range(12)]

    def reject(batch: list[Event], _size: int) -> int | None:
        if len(batch) > 4:
            return 413
        return 400 if any(e["task_key"] == "t7" for e in batch) else None

    ingest, stderr = _replay(sm_job, events, reject)
    delivered = [e for batch in ingest.batches for e in batch]
    assert delivered == [e for e in events if e["task_key"] != "t7"]
    assert "dropped one task_created event (t7)" in stderr
    assert "replayed 11/12 events" in stderr


def test_the_replay_logs_and_drops_the_events_a_2xx_refuses(sm_job: Job) -> None:
    # The route accepts the chunk but refuses one claim on its own: it is not resent.
    events = [_task(i) for i in range(5)]

    def answer(batch: list[Event]) -> bytes:
        refused = [{"index": 3, "reason": "Batch job j is not a job of this run"}]
        return json.dumps(
            {"appended": len(batch) - 1, "duplicates": 0, "refused": refused}
        ).encode()

    ingest, stderr = _replay(sm_job, events, lambda _b, _s: None, answer)
    assert len(ingest.batches) == 1
    assert "refused one task_created event (t3), dropped: Batch job j is not a job" in stderr
    assert "replayed 4/5 events" in stderr


def test_the_replay_cuts_chunks_by_bytes_under_the_route_limit(sm_job: Job) -> None:
    limit = 5 * 1024 * 1024
    events = [_task(i, deps=["x" * 1024] * 1500) for i in range(3)]  # ~1.5 MiB each
    events.append(_task(3, deps=["y" * 1024] * 6000))  # over the limit on its own

    ingest, stderr = _replay(sm_job, events, lambda _b, size: 413 if size > limit else None)
    assert [e["task_key"] for batch in ingest.batches for e in batch] == ["t0", "t1", "t2"]
    assert len(ingest.batches) == 2 and all(size <= 4 * 1024 * 1024 for size in ingest.sizes)
    assert "over the route limit" in stderr


def test_the_replay_stops_once_ingest_is_unreachable(sm_job: Job) -> None:
    calls: list[int] = []

    def down(batch: list[Event], _size: int) -> int:
        calls.append(len(batch))
        return 503

    events = [_task(i, deps=["x" * 1024] * 1500) for i in range(6)]  # three chunks
    _, stderr = _replay(sm_job, events, down)
    assert len(calls) == 3, "the first chunk's retries, then no more chunks"
    assert "events replay stopped, ingest unreachable" in stderr
    assert "replayed 0/6 events" in stderr
