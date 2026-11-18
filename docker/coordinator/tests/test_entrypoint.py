"""The coordinator entrypoint (`docker/coordinator/entrypoint.sh`), run for real in a sandbox (a
`Job`): a stand-in runtime layer under a temporary `BIFFIX_RUNTIME_ROOT`, a work root in a temporary
directory (BIFFIX_WORK_ROOT), its libraries and helpers (lib/) in another (BIFFIX_LIB_DIR), a
stand-in boto3 over a directory (fakes/py), and a fake engine that reports `$FAKE_ENGINE_OUTPUT` as
its version, records its command line and environment, and exits with a chosen code. First the
runtime check, then the rest of the run.

The fake engines are symlinks to ONE script written once per session: macOS can stall the first
exec of every newly written executable for tens of seconds."""

from __future__ import annotations

import ast
import contextlib
import gzip
import hashlib
import io
import json
import os
import shutil
import signal
import subprocess
import sys
import tarfile
import threading
import time
from collections.abc import Callable, Iterator
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

import pytest
from processes import stop_group

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
  NXF_IGNORE_RESUME_HISTORY NXF_PLUGINS_DEFAULT BIFFIX_IMAGE_MAP_FILE \
  SNAKEMAKE_AWS_BATCH_JOB_DEFINITION_MAP BIFFIX_JOB_DEFINITION_MAP_FILE \
  SNAKEMAKE_AWS_BATCH_JOB_ROLE PWD NXF_OPTS NXF_APPTAINER_NEW_PID_NAMESPACE APPTAINER_CACHEDIR \
  APPTAINER_TMPDIR APPTAINER_DOCKER_PASSWORD BIFFIX_TASK_IMAGES_FILE PATH; do
  printf '%s=%s\n' "$v" "${!v-<unset>}"
done > "$FAKE_ENV"
ls .snakemake/metadata > "$FAKE_SEEN" 2>/dev/null || true
if [ -n "${FAKE_WRITE_METADATA:-}" ]; then
  mkdir -p .snakemake/metadata && echo new > .snakemake/metadata/new-output
fi
# The engine's logs, its output, and a failed local task's files.
if [ -n "${FAKE_WRITE_LOG:-}" ]; then
  mkdir -p .snakemake/log && echo "snakemake log" > .snakemake/log/2026-11-18T000000.snakemake.log
  [ "${1:-}" != -log ] || echo "nextflow log" > "$2"
  echo "engine says: boom" >&2
fi
[ -z "${FAKE_TRACE:-}" ] || echo "task_id hash" > trace-20261118-00000000.txt
if [ -n "${FAKE_LAST_WORDS:-}" ]; then
  for i in $(seq 1 "$FAKE_LAST_WORDS"); do echo "engine line $i"; done
fi
if [ -n "${FAKE_FAILED_TASK:-}" ]; then
  mkdir -p "$FAKE_FAILED_TASK" && echo 1 > "$FAKE_FAILED_TASK/.exitcode"
  echo "task says: out of cheese" > "$FAKE_FAILED_TASK/.command.err"
fi
# The files the engine holds open beyond stdio, as "<device> <inode>" lines.
[ -z "${FAKE_OPEN_FILES:-}" ] || python3 -c '
import os, sys
with open(sys.argv[1], "w") as out:
    for fd in range(3, 256):
        try:
            st = os.fstat(fd)
        except OSError:
            continue
        print(st.st_dev, st.st_ino, file=out)' "$FAKE_OPEN_FILES"
# What the engine sees of a single-job run's scratch root, which is cleared at exit.
[ -z "${FAKE_KEEP:-}" ] || cp -R "$BIFFIX_SCRATCH_ROOT/." "$FAKE_KEEP/"
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
        ("nextflow", "nextflow version 26.04.9.14305", "26.04.9"),
        ("nextflow", "nextflow version 26.10.5.14654", "26.10.5"),
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
        ("nextflow", "nextflow version 26.04.9.14305", "26.10.5"),
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
    # Without a job definition map (a local run) the executor registers its own definitions, with
    # the project's log group and run role.
    assert _after(sm_job.argv, "--aws-batch-log-group") == GROUP
    assert _after(sm_job.argv, "--aws-batch-job-role") == "arn:role/run"
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
        ("snakemake", "BIFFIX_CORES"),
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


@contextlib.contextmanager
def _coordinator(env: dict[str, str]) -> Iterator[subprocess.Popen[str]]:
    """The coordinator, started in a process group of its own. On the way out the whole group is
    killed, whether or not the coordinator itself has exited (a fake engine or pull it left behind
    would hold its pipes open), and its output collected within a bound."""
    proc = subprocess.Popen(
        ["bash", str(SCRIPT)],
        env=env,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        start_new_session=True,
    )
    try:
        yield proc
    finally:
        stop_group(proc)


def _cancel(job: Job, **env: str) -> tuple[int, str, float]:
    """Run the job until its engine is up, SIGTERM it as a Batch cancel does, and return its exit
    code, its stderr and the seconds it took to exit after the signal."""
    with _coordinator(job.env(FAKE_WAIT_FOR_TERM="1", FAKE_WRITE_METADATA="1", **env)) as proc:
        deadline = time.monotonic() + 30
        while not (job.out / "ready").exists():
            assert proc.poll() is None, stop_group(proc)
            assert time.monotonic() < deadline, "the engine never started"
            time.sleep(0.05)
        signalled = time.monotonic()
        proc.send_signal(signal.SIGTERM)
        _, stderr = proc.communicate(timeout=30)
        took = time.monotonic() - signalled
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


NF_ASSIGNMENT = json.dumps({"by_label": {"process_high": "cpu"}, "by_name": {"ALIGN": "cpu"}})


def test_nextflow_tasks_go_to_the_queues_the_version_assigns(nf_job: Job) -> None:
    got = nf_job.run(BIFFIX_QUEUE_CLASS="cpu", BIFFIX_QUEUE_ASSIGNMENT=NF_ASSIGNMENT)
    assert got.returncode == 0, got.stderr
    config = (nf_job.work / "nextflow.config").read_text().splitlines()
    # The default first, then the selectors, which outrank it (and the pipeline's own queue under
    # the same selector: this file is read after the pipeline's).
    at = config.index("process.queue = 'arn:queue/second'")
    assert config[at + 1 : at + 3] == [
        "process { withLabel: 'process_high' { queue = 'arn:queue/second' } }",
        "process { withName: 'ALIGN' { queue = 'arn:queue/second' } }",
    ]
    # A dry run keeps them, and still runs every process locally.
    assert nf_job.run(BIFFIX_DRY_RUN="1", BIFFIX_QUEUE_ASSIGNMENT=NF_ASSIGNMENT).returncode == 0
    dry = (nf_job.work / "nextflow.config").read_text().splitlines()
    assert "process { withName: '.*' { executor = 'local' } }" in dry
    assert "process { withName: 'ALIGN' { queue = 'arn:queue/second' } }" in dry


def test_a_queue_assignment_the_map_cannot_resolve_fails_before_the_engine(nf_job: Job) -> None:
    got = nf_job.run(BIFFIX_QUEUE_ASSIGNMENT=json.dumps({"by_label": {"gpu": "gpu"}}))
    assert got.returncode == 78
    assert "queue-class 'gpu' (by_label.gpu) has no queue" in got.stderr
    assert not (nf_job.out / "argv").exists()


def test_snakemake_rules_go_to_the_queues_the_version_assigns(sm_job: Job) -> None:
    got = sm_job.run(
        BIFFIX_QUEUE_ASSIGNMENT=json.dumps({"by_name": {"align": "cpu", "call": "cpu"}}),
        BIFFIX_TARGET="all",
    )
    assert got.returncode == 0, got.stderr
    argv = sm_job.argv
    assert _after(argv, "--aws-batch-job-queue") == "arn:queue/first"
    # The executor's rule queue map, by path; never --set-resources, which would drop the workflow
    # profile's own set-resources.
    rule_queues = Path(_after(argv, "--aws-batch-rule-queues"))
    assert json.loads(rule_queues.read_text()) == {
        "align": "arn:queue/second",
        "call": "arn:queue/second",
    }
    assert "--set-resources" not in argv and argv[-1] == "all"
    # Without an assignment, no map at all.
    assert sm_job.run().returncode == 0
    assert "--aws-batch-rule-queues" not in sm_job.argv


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
        (job.runtime / "nf/plugins" / p.replace("@", "-")).mkdir(exist_ok=True)
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
    defaults = (nf_job.work / ".nextflow-home/config").read_text()
    assert "errorStrategy" not in defaults and "maxRetries" not in defaults
    # biffix never turns the trace on: its task metrics need `ps` and more in every task image.
    assert nf_job.run().returncode == 0
    assert "trace" not in (nf_job.work / ".nextflow-home/config").read_text()


# ── the rest of the env contract ──


MAP_URI = "s3://bucket/runs/run-test/images.json"
DEFINITIONS_URI = "s3://bucket/runs/run-test/job-definitions.json"
DEFINITIONS = {"reg/tool@sha256:1": "arn:aws:batch:us-test-1:1:job-definition/biffix-t_task-sm-a:1"}


def _put_maps(job: Job) -> dict[str, str]:
    """The run's image map and job definition map in the fake S3, and the env naming them."""
    job.put("bucket", "runs/run-test/images.json", b'{"tool:1": "reg/tool@sha256:1"}')
    job.put("bucket", "runs/run-test/job-definitions.json", json.dumps(DEFINITIONS).encode())
    return {"BIFFIX_IMAGE_MAP_URI": MAP_URI, "BIFFIX_JOB_DEFINITION_MAP_URI": DEFINITIONS_URI}


def test_the_image_map_is_handed_to_the_executor(sm_job: Job) -> None:
    got = sm_job.run(**_put_maps(sm_job))
    assert got.returncode == 0, got.stderr
    path = sm_job.engine_env["SNAKEMAKE_AWS_BATCH_CONTAINER_IMAGE_MAP"]
    assert path == str(sm_job.work / "images.json")
    assert json.loads(Path(path).read_text()) == {"tool:1": "reg/tool@sha256:1"}


def test_the_image_map_is_handed_to_nf_biffix(nf_job: Job) -> None:
    got = nf_job.run(**_put_maps(nf_job))
    assert got.returncode == 0, got.stderr
    assert nf_job.engine_env["BIFFIX_IMAGE_MAP_FILE"] == str(nf_job.work / "images.json")


@pytest.mark.parametrize("body", [None, b'["tool:1"]', b"not json"])
def test_an_unreadable_image_map_fails_with_ex_config(sm_job: Job, body: bytes | None) -> None:
    env = _put_maps(sm_job)
    (sm_job.s3 / "bucket/runs/run-test/images.json").unlink()
    if body is not None:
        sm_job.put("bucket", "runs/run-test/images.json", body)
    got = sm_job.run(**env)
    assert got.returncode == 78
    assert "could not read the image map" in got.stderr
    assert not (sm_job.out / "argv").exists()


def test_snakemake_runs_every_job_on_biffix_s_job_definitions(sm_job: Job) -> None:
    got = sm_job.run(**_put_maps(sm_job))
    assert got.returncode == 0, got.stderr
    path = sm_job.engine_env["SNAKEMAKE_AWS_BATCH_JOB_DEFINITION_MAP"]
    assert path == str(sm_job.work / "job-definitions.json")
    assert json.loads(Path(path).read_text()) == DEFINITIONS
    # The definitions carry the job role and the task log group, and the executor refuses either
    # beside the map: neither is passed, and the role's env var (which it reads too) is cleared.
    assert "--aws-batch-job-role" not in sm_job.argv
    assert "--aws-batch-log-group" not in sm_job.argv
    assert sm_job.engine_env["SNAKEMAKE_AWS_BATCH_JOB_ROLE"] == "<unset>"
    # Nor is a privileged container ever asked for.
    assert "--aws-batch-privileged" not in sm_job.argv


def test_nextflow_runs_every_task_on_biffix_s_job_definitions(nf_job: Job) -> None:
    got = nf_job.run(**_put_maps(nf_job))
    assert got.returncode == 0, got.stderr
    path = nf_job.engine_env["BIFFIX_JOB_DEFINITION_MAP_FILE"]
    assert path == str(nf_job.work / "job-definitions.json")
    assert json.loads(Path(path).read_text()) == DEFINITIONS
    config = (nf_job.work / "nextflow.config").read_text()
    # The job role is the definitions'; the log group is still where nf-amazon reads task logs.
    assert "aws.batch.jobRole" not in config
    assert f"aws.batch.logsGroup = '{GROUP}'" in config


def test_an_image_map_without_a_job_definition_map_fails_with_ex_config(sm_job: Job) -> None:
    env = _put_maps(sm_job)
    del env["BIFFIX_JOB_DEFINITION_MAP_URI"]
    got = sm_job.run(**env)
    assert got.returncode == 78
    assert "without BIFFIX_JOB_DEFINITION_MAP_URI" in got.stderr
    assert "deploy the backend at the same release as this coordinator image" in got.stderr
    assert not (sm_job.out / "argv").exists()


@pytest.mark.parametrize("body", [None, b'["arn:jd"]', b"not json"])
def test_an_unreadable_job_definition_map_fails_with_ex_config(
    sm_job: Job, body: bytes | None
) -> None:
    env = _put_maps(sm_job)
    (sm_job.s3 / "bucket/runs/run-test/job-definitions.json").unlink()
    if body is not None:
        sm_job.put("bucket", "runs/run-test/job-definitions.json", body)
    got = sm_job.run(**env)
    assert got.returncode == 78
    assert "could not read the job definition map" in got.stderr
    assert not (sm_job.out / "argv").exists()


def test_with_job_definitions_no_child_job_role_is_needed(sm_job: Job) -> None:
    got = sm_job.run(**_put_maps(sm_job), SNAKEMAKE_AWS_BATCH_JOB_ROLE="")
    assert got.returncode == 0, got.stderr


def test_with_job_definitions_nextflow_needs_no_child_job_role(nf_job: Job) -> None:
    got = nf_job.run(**_put_maps(nf_job), SNAKEMAKE_AWS_BATCH_JOB_ROLE="")
    assert got.returncode == 0, got.stderr
    assert "aws.batch.jobRole" not in (nf_job.work / "nextflow.config").read_text()


def test_a_job_definition_map_without_an_image_map_is_not_read(sm_job: Job) -> None:
    # The backend writes the two together; a definition map alone (no image map to resolve task
    # images through) leaves the executor registering its own definitions, as a local run does.
    env = _put_maps(sm_job)
    del env["BIFFIX_IMAGE_MAP_URI"]
    got = sm_job.run(**env)
    assert got.returncode == 0, got.stderr
    assert sm_job.engine_env["SNAKEMAKE_AWS_BATCH_JOB_DEFINITION_MAP"] == "<unset>"
    assert sm_job.engine_env["BIFFIX_JOB_DEFINITION_MAP_FILE"] == "<unset>"
    assert _after(sm_job.argv, "--aws-batch-job-role") == "arn:role/run"


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


def _s3_objects(job: Job) -> dict[str, bytes]:
    """Every object in the fake S3, by key, with its content: a write that replaces an object is
    a change too."""
    return {str(p.relative_to(job.s3)): p.read_bytes() for p in job.s3.rglob("*") if p.is_file()}


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
    # Off, not just unset: a pipeline's own `cloudcache.enabled = true` with no path would take
    # the local work dir as its path, which Nextflow refuses before any task starts.
    assert "cloudcache.enabled = false" in config.read_text().splitlines()
    assert "cloudcache {" not in config.read_text()
    assert "executor.disableRemoteBinDir = true" in config.read_text()
    assert nf_job.run().returncode == 0
    assert "cloudcache { enabled = true;" in config.read_text()
    assert "disableRemoteBinDir" not in config.read_text()


def test_a_nextflow_dry_run_previews_with_a_local_work_dir_and_executor(nf_job: Job) -> None:
    # -preview still opens the work dir (a PutObject on S3), and the AWS Batch executor needs an
    # S3 one: a dry run previews locally.
    config = nf_job.work / "nextflow.config"
    assert nf_job.run(BIFFIX_DRY_RUN="1").returncode == 0
    dry = config.read_text().splitlines()
    assert "process.executor = 'local'" in dry
    # The pipeline's own selectors and directives outrank process.executor; this one, last, wins.
    assert "process { withName: '.*' { executor = 'local' } }" in dry
    assert f"workDir = '{nf_job.work}/nf-work'" in dry
    assert not any("awsbatch" in line or line.startswith("workDir = 's3://") for line in dry)
    assert nf_job.run().returncode == 0
    real = config.read_text().splitlines()
    assert "process.executor = 'awsbatch'" in real
    assert "workDir = 's3://bucket/runs/run-test/work'" in real
    # Everything else is the same config.
    differ = ("process.executor", "process { withName: '.*'", "workDir")
    unchanged = [line for line in real if not line.startswith(differ)]
    local_cache = ["cloudcache.enabled = false", "executor.disableRemoteBinDir = true"]
    assert [line for line in dry if not line.startswith(differ)] == [
        local
        for line in unchanged
        for local in (local_cache if line.startswith("cloudcache") else [line])
    ]


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


def test_snakemake_gets_cores_so_a_workflow_can_read_workflow_cores(sm_job: Job) -> None:
    # Without --cores, `threads: workflow.cores` fails the parse. It also caps every job's threads
    # (a Batch job's vCPUs), so it is a one-job ceiling, not the coordinator's own vCPUs.
    assert sm_job.run().returncode == 0
    assert _after(sm_job.argv, "--cores") == "64"
    assert _after(sm_job.argv, "--jobs") == "100"
    assert sm_job.run(BIFFIX_CORES="16").returncode == 0
    assert _after(sm_job.argv, "--cores") == "16"
    assert sm_job.run(BIFFIX_DRY_RUN="1").returncode == 0
    assert _after(sm_job.argv, "--cores") == "64"


def test_zero_cores_fail_before_snakemake_runs(sm_job: Job) -> None:
    got = sm_job.run(BIFFIX_CORES="0")
    assert got.returncode == 78
    assert "BIFFIX_CORES must be at least 1: 0" in got.stderr
    assert not (sm_job.out / "argv").exists()


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
    status code to refuse a request with (or None to accept it), `answer(events)` the body of an
    accepted one, and `refusal` the body of a refused one."""

    def __init__(
        self, reject: Reject, answer: Answer = lambda _events: b"", refusal: bytes = b""
    ) -> None:
        self.refused = 0
        self.batches: list[list[Event]] = []
        self.sizes: list[int] = []
        ingest = self

        class Handler(BaseHTTPRequestHandler):
            def do_POST(self) -> None:
                raw = self.rfile.read(int(self.headers["Content-Length"]))
                events = json.loads(raw)["events"]
                code = reject(events, len(raw))
                body = refusal
                if code is not None:
                    ingest.refused += 1
                else:
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
    job: Job,
    events: list[Event],
    reject: Reject,
    answer: Answer = lambda _events: b"",
    refusal: bytes = b"",
    token: str = "tok-secret",
) -> tuple[_Ingest, str]:
    (job.work / "src").mkdir(exist_ok=True)
    (job.work / "src/.biffix_events.jsonl").write_text(
        "".join(json.dumps(e) + "\n" for e in events)
    )
    ingest = _Ingest(reject, answer, refusal)
    try:
        got = job.run(BIFFIX_INGEST_URL=ingest.url, BIFFIX_INGEST_TOKEN=token)
    finally:
        ingest.close()
    assert got.returncode == 0, got.stderr
    assert token not in got.stderr
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


def test_the_replay_of_a_cancelled_run_stops_quietly(sm_job: Job) -> None:
    # A cancel revokes the run's token: the route refuses every chunk the same way, by design.
    events = [_task(i, deps=["x" * 1024] * 1500) for i in range(6)]  # three chunks
    refusal = b"run is cancelled; capability token revoked"
    ingest, stderr = _replay(sm_job, events, lambda _b, _s: 403, refusal=refusal)
    assert ingest.refused == 1, "no retry, no split, no further chunk"
    assert "events replay: run cancelled; the server keeps what it already has" in stderr
    assert "failed" not in stderr and "replayed" not in stderr


def test_a_cancel_is_recognized_when_the_token_occurs_in_the_refusal(sm_job: Job) -> None:
    # The match is on the server's answer, not on its redacted copy.
    events = [_task(i) for i in range(3)]
    refusal = b"run is cancelled; capability token revoked"
    ingest, stderr = _replay(sm_job, events, lambda _b, _s: 403, refusal=refusal, token="revoked")
    assert ingest.refused == 1
    assert "events replay: run cancelled; the server keeps what it already has" in stderr
    assert "failed" not in stderr


def test_any_other_403_is_still_a_failure(sm_job: Job) -> None:
    events = [_task(i) for i in range(3)]
    refusal = b"capability token does not authorize the run these events claim"
    _, stderr = _replay(sm_job, events, lambda _b, _s: 403, refusal=refusal)
    assert "events replay chunk of 3 failed (swallowed): HTTP 403: capability token" in stderr
    assert "run cancelled" not in stderr
    assert "replayed 0/3 events" in stderr


# ── the run's logs (both modes) ──


def _logs(job: Job) -> dict[str, bytes]:
    """What the run uploaded under its logs/ prefix, by name."""
    root = job.s3 / "bucket/runs/run-test/logs"
    if not root.exists():
        return {}
    return {str(p.relative_to(root)): p.read_bytes() for p in root.rglob("*") if p.is_file()}


def test_a_nextflow_run_uploads_its_log_and_generated_config(nf_job: Job) -> None:
    got = nf_job.run(FAKE_WRITE_LOG="1")
    assert got.returncode == 0, got.stderr
    logs = _logs(nf_job)
    assert logs["nextflow.log"] == b"nextflow log\n"
    assert logs["nextflow.config"] == (nf_job.work / "nextflow.config").read_bytes()
    assert b"process.maxRetries" in logs["nextflow-defaults.config"]
    # biffix asks for no trace, so there is none.
    assert "trace.txt" not in logs
    # A run that succeeded has no failure to explain.
    assert "failure.txt" not in logs


def test_a_trace_the_pipeline_turned_on_is_uploaded(nf_job: Job) -> None:
    got = nf_job.run(FAKE_TRACE="1")
    assert got.returncode == 0, got.stderr
    assert _logs(nf_job)["trace.txt"] == b"task_id hash\n"


def test_failure_txt_quotes_the_engine_s_very_last_lines(sm_job: Job) -> None:
    # The copy of the engine's output has caught up before failure.txt is written.
    got = sm_job.run(FAKE_LAST_WORDS="100000", FAKE_RC="2")
    assert got.returncode == 2, got.stderr
    failure = _logs(sm_job)["failure.txt"].decode()
    assert failure.startswith("The engine exited with status 2.")
    assert failure.rstrip().endswith("engine line 100000")
    # The copy is kept to its end, not the whole output.
    assert (sm_job.work / "engine-output.log").stat().st_size < 1024 * 1024


def test_a_failed_snakemake_run_uploads_the_end_of_its_output(sm_job: Job) -> None:
    got = sm_job.run(FAKE_WRITE_LOG="1", FAKE_RC="3")
    assert got.returncode == 3, got.stderr
    logs = _logs(sm_job)
    assert logs["snakemake.log"] == b"snakemake log\n"
    assert b"--executor\naws-batch\n" in logs["snakemake.args"]
    failure = logs["failure.txt"].decode()
    assert failure.startswith("The engine exited with status 3.")
    assert "engine says: boom" in failure
    # The job's own log still has all of the engine's output.
    assert "engine says: boom" in got.stderr


def test_a_cancelled_run_uploads_its_logs_but_no_failure(sm_job: Job) -> None:
    rc, stderr, _ = _cancel(sm_job, FAKE_WRITE_LOG="1")
    assert rc == 143, stderr
    logs = _logs(sm_job)
    assert "snakemake.log" in logs and "failure.txt" not in logs


def _earlier_attempt_failed(job: Job) -> None:
    """The failure.txt an earlier attempt of the run uploaded before its host went away: Batch
    retries the coordinator under the same run prefix."""
    report = job.s3 / "bucket/runs/run-test/logs/failure.txt"
    report.parent.mkdir(parents=True, exist_ok=True)
    report.write_text("the earlier attempt's failure")


def test_a_cancelled_retry_does_not_show_the_earlier_attempt_s_failure(sm_job: Job) -> None:
    _earlier_attempt_failed(sm_job)
    rc, stderr, _ = _cancel(sm_job, FAKE_WRITE_LOG="1", AWS_BATCH_JOB_ATTEMPT="2")
    assert rc == 143, stderr
    logs = _logs(sm_job)
    assert "snakemake.log" in logs and "failure.txt" not in logs


def test_a_retry_that_succeeds_does_not_show_the_earlier_attempt_s_failure(sm_job: Job) -> None:
    _earlier_attempt_failed(sm_job)
    got = sm_job.run(FAKE_WRITE_LOG="1", AWS_BATCH_JOB_ATTEMPT="2")
    assert got.returncode == 0, got.stderr
    logs = _logs(sm_job)
    assert "snakemake.log" in logs and "failure.txt" not in logs


def test_the_logs_are_uploaded_while_the_engine_runs(nf_job: Job) -> None:
    env = nf_job.env(FAKE_WAIT_FOR_TERM="1", FAKE_WRITE_LOG="1", BIFFIX_LOGS_SYNC_SECS="1")
    with _coordinator(env) as proc:
        deadline = time.monotonic() + 30
        while not (nf_job.out / "ready").exists() or "nextflow.log" not in _logs(nf_job):
            assert proc.poll() is None, stop_group(proc)
            assert time.monotonic() < deadline, "no logs while the engine ran"
            time.sleep(0.1)
        assert not (nf_job.out / "termed").exists(), "the engine is still running"
        proc.send_signal(signal.SIGTERM)
        proc.communicate(timeout=30)
    assert proc.returncode == 143


# ── single-job runs ──


_APPTAINER = r"""#!/usr/bin/env bash
case "${1:-}" in --version|version) echo "apptainer version 1.5.5"; exit 0 ;; esac
if [ "${1:-}" = pull ]; then
  dest=$2 ref=$3
  printf '%s %s %s\n' "$ref" "${APPTAINER_DOCKER_USERNAME:--}" "${APPTAINER_DOCKER_PASSWORD:--}" \
    >> "$FAKE_PULLS"
  mkdir -p "$FAKE_RUNNING" && touch "$FAKE_RUNNING/$$"
  # FAKE_PULL_BARRIER=N: the first N pulls wait (10 s at most) until N have started, so a slow
  # second start cannot hide an overlap; the count is recorded after it.
  if [ -n "${FAKE_PULL_BARRIER:-}" ]; then
    mkdir -p "$FAKE_RUNNING.started" && touch "$FAKE_RUNNING.started/$$"
    for _ in $(seq 100); do
      [ "$(ls "$FAKE_RUNNING.started" | wc -l)" -ge "$FAKE_PULL_BARRIER" ] && break
      sleep 0.1
    done
  fi
  ls "$FAKE_RUNNING" | wc -l | tr -d ' ' >> "$FAKE_CONCURRENCY"
  # A hanging pull names its process group (its own pid: the converter starts each pull in a
  # session of its own) in $FAKE_RUNNING.hung, created whole, for a test to check and clean up.
  case " ${FAKE_PULL_HANG:-} " in *" $ref "*)
    mkdir -p "$FAKE_RUNNING.hung" && touch "$FAKE_RUNNING.hung/$$"; sleep 60 ;;
  esac
  sleep "${FAKE_PULL_SECS:-0}"
  rm -f "$FAKE_RUNNING/$$"
  case " ${FAKE_PULL_FAIL:-} " in *" $ref "*)
    # FAKE_PULL_FAIL_AFTER_HANG: fail only once a hanging pull has started (10 s at most), so the
    # failure has a pull in flight to stop.
    if [ -n "${FAKE_PULL_FAIL_AFTER_HANG:-}" ]; then
      for _ in $(seq 100); do
        [ -n "$(ls "$FAKE_RUNNING.hung" 2>/dev/null)" ] && break
        sleep 0.1
      done
    fi
    echo "FATAL: unauthorized: $ref"; exit 255 ;;
  esac
  echo "image file of $ref" > "$dest"
  exit 0
fi
printf '%s\n' "$@" "APPTAINER_BINDPATH=${APPTAINER_BINDPATH-}" \
  "APPTAINERENV_OMP_NUM_THREADS=${APPTAINERENV_OMP_NUM_THREADS-<unset>}" > "$FAKE_APPTAINER_ARGV"
"""

ECR = "123456789012.dkr.ecr.us-test-1.amazonaws.com"
TOOL = f"{ECR}/biffix-t/tool@sha256:{'1' * 64}"
WORKER = f"{ECR}/biffix-t/wf@sha256:{'2' * 64}"
PUBLIC = "quay.io/public/thing:1"
SJ_MAP = {
    "tool:1": TOOL,
    "docker.io/library/tool:1": TOOL,
    TOOL: TOOL,
    "quay.io/public/thing:1": PUBLIC,
    WORKER: WORKER,
    "$default": WORKER,
}


@pytest.fixture(scope="session")
def apptainer_dir(tmp_path_factory: pytest.TempPathFactory) -> Path:
    bin_dir = tmp_path_factory.mktemp("fake-apptainer")
    exe = bin_dir / "apptainer"
    exe.write_text(_APPTAINER)
    exe.chmod(0o755)
    return bin_dir


def _hung_pulls(job: Job) -> list[int]:
    """The process groups of the hanging pulls that started (FAKE_PULL_HANG)."""
    hung = job.out / "running.hung"
    return [int(p.name) for p in hung.iterdir()] if hung.exists() else []


def _group_members(pgid: int) -> list[str]:
    """The process states (ps's STAT) of process group `pgid`'s members."""
    listing = subprocess.run(
        ["ps", "-A", "-o", "pgid=,stat="], capture_output=True, text=True, check=True
    ).stdout
    return [
        fields[1]
        for fields in (line.split() for line in listing.splitlines())
        if len(fields) >= 2 and fields[0] == str(pgid)
    ]


def _group_gone(pgid: int, within: float = 5) -> bool:
    """Whether process group `pgid` has no live process, waiting up to `within` seconds for its
    members to exit. A zombie is not live: signal 0 still finds one, but a killed member that
    nothing reaps (a container's PID 1 may never) runs nothing."""
    deadline = time.monotonic() + within
    while True:
        if all(state.startswith("Z") for state in _group_members(pgid)):
            return True
        if time.monotonic() >= deadline:
            return False
        time.sleep(0.05)


def test_a_group_of_only_zombies_is_gone() -> None:
    """A killed pull nothing has reaped yet (a container's PID 1 may never) runs nothing."""
    zombie = subprocess.Popen(["true"], start_new_session=True)  # its own group, never waited on
    try:
        deadline = time.monotonic() + 5
        # The state's first letter only: Linux's ps flags a session leader too ("Zs").
        while True:
            states = _group_members(zombie.pid)
            if [state[:1] for state in states] == ["Z"]:
                break
            assert time.monotonic() < deadline, f"the child never became a zombie: {states}"
            time.sleep(0.05)
        assert _group_gone(zombie.pid, within=0.2)
    finally:
        zombie.wait()
    live = subprocess.Popen(["sleep", "30"], start_new_session=True)
    try:
        assert not _group_gone(live.pid, within=0.2)
    finally:
        live.kill()
        live.wait()


def _kill_hung_pulls(job: Job) -> None:
    """Kill every hanging pull's group: the converter starts each in a session of its own, which
    killing the coordinator's group misses."""
    for pgid in _hung_pulls(job):
        with contextlib.suppress(ProcessLookupError, PermissionError):
            os.killpg(pgid, signal.SIGKILL)


def _sif(image: str) -> str:
    """The image file name Nextflow looks for (lib/single_job.py image_file_name)."""
    return image.replace(":", "-").replace("/", "-") + ".img"


class SingleJob:
    """A Job run as a single-job run: a scratch root, a fake apptainer and ECR, and a copy of the
    scratch root as the engine saw it (`kept`), since it is cleared at exit."""

    def __init__(self, job: Job, apptainer_dir: Path):
        self.job = job
        self.scratch = job.root / "scratch"
        self.kept = job.root / "kept"
        self.scratch.mkdir()
        # What the single-job hosts' boot script writes, outside the scratch root, once it has
        # mounted the instance store there (the job sees it read-only).
        self.marker = job.root / "host/instance-store"
        self.marker.parent.mkdir()
        self.marker.write_text("")
        self.kept.mkdir()
        self.apptainer_dir = apptainer_dir

    def env(self, **overrides: str) -> dict[str, str]:
        out = self.job.out
        env = {
            "BIFFIX_EXECUTION": "single_job",
            "BIFFIX_SINGLE_JOB_CPUS": "8",
            "BIFFIX_SINGLE_JOB_MEMORY_MIB": "65536",
            "BIFFIX_SCRATCH_ROOT": str(self.scratch),
            "BIFFIX_INSTANCE_STORE_MARKER": str(self.marker),
            "BIFFIX_OOM_SCORE_ADJ_FILE": str(out / "oom_score_adj"),
            # Neither is needed: no task is a Batch job.
            "BIFFIX_QUEUE_CLASS_MAP": "",
            "SNAKEMAKE_AWS_BATCH_JOB_ROLE": "",
            "PATH": f"{self.apptainer_dir}:{os.environ['PATH']}",
            "FAKE_ECR_REGISTRY": ECR,
            "FAKE_ECR_PASSWORD": "ecr-SECRET-pw",
            "FAKE_PULLS": str(out / "pulls"),
            "FAKE_RUNNING": str(out / "running"),
            "FAKE_CONCURRENCY": str(out / "concurrency"),
            "FAKE_APPTAINER_ARGV": str(out / "apptainer-argv"),
            "FAKE_KEEP": str(self.kept),
        }
        return env | overrides

    def run(self, **overrides: str) -> subprocess.CompletedProcess[str]:
        """The run, with the image map SJ_MAP."""
        self.job.put("bucket", "runs/run-test/images.json", json.dumps(SJ_MAP).encode())
        return self.run_without_map(BIFFIX_IMAGE_MAP_URI=MAP_URI, **overrides)

    def run_without_map(self, **overrides: str) -> subprocess.CompletedProcess[str]:
        """The run, with no image map unless `overrides` names one."""
        for f in ("pulls", "concurrency"):
            (self.job.out / f).unlink(missing_ok=True)
        return self.job.run(**self.env(**overrides))

    @property
    def pulls(self) -> list[list[str]]:
        path = self.job.out / "pulls"
        return [line.split(" ") for line in path.read_text().splitlines()] if path.exists() else []


@pytest.fixture
def sm_single(sm_job: Job, apptainer_dir: Path) -> SingleJob:
    return SingleJob(sm_job, apptainer_dir)


@pytest.fixture
def nf_single(nf_job: Job, apptainer_dir: Path) -> SingleJob:
    return SingleJob(nf_job, apptainer_dir)


# Stands for a PATH with every tool on the host's but apptainer ([`_path_without_apptainer`]).
PATH_WITHOUT_APPTAINER = "<the host's PATH without apptainer>"


def _path_without_apptainer(root: Path) -> str:
    """A directory linking each command the host's PATH finds (its first match, as a shell would)
    except apptainer: bash and the setup tools stay wherever they live, even beside an apptainer
    (a /usr/bin that has both), and a developer's own apptainer cannot satisfy the check. Each
    link's target is absolute, so a relative PATH entry still names the tool it does here."""
    bin_dir = root / "path-without-apptainer"
    bin_dir.mkdir()
    for entry in os.environ["PATH"].split(os.pathsep):
        directory = Path(entry).absolute()
        if not entry or not directory.is_dir():
            continue
        try:
            tools = list(directory.iterdir())
        except OSError:  # a directory the user may search but not list (execute-only)
            continue
        for tool in tools:
            link = bin_dir / tool.name
            if tool.name == "apptainer" or link.exists() or link.is_symlink():
                continue
            try:
                runnable = tool.is_file() and os.access(tool, os.X_OK)
            except OSError:  # one the host keeps from this user (macOS has some in /usr/sbin)
                continue
            if runnable:
                link.symlink_to(tool)
    return str(bin_dir)


@pytest.mark.skipif(os.geteuid() == 0, reason="root lists a directory whatever its mode")
def test_a_path_without_apptainer_skips_a_directory_it_cannot_list(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    hidden = tmp_path / "execute-only"
    hidden.mkdir()
    (hidden / "tool").touch(mode=0o755)
    shown = tmp_path / "listed"
    shown.mkdir()
    (shown / "other").touch(mode=0o755)
    monkeypatch.setenv("PATH", os.pathsep.join([str(hidden), str(shown)]))
    hidden.chmod(0o100)
    try:
        bin_dir = Path(_path_without_apptainer(tmp_path))
    finally:
        hidden.chmod(0o755)
    assert sorted(p.name for p in bin_dir.iterdir()) == ["other"]


@pytest.mark.parametrize(
    ("env", "why"),
    [
        ({"BIFFIX_SINGLE_JOB_CPUS": ""}, "BIFFIX_SINGLE_JOB_CPUS required"),
        ({"BIFFIX_SINGLE_JOB_MEMORY_MIB": ""}, "BIFFIX_SINGLE_JOB_MEMORY_MIB required"),
        ({"BIFFIX_SINGLE_JOB_CPUS": "lots"}, "BIFFIX_SINGLE_JOB_CPUS is not a number: lots"),
        ({"BIFFIX_SINGLE_JOB_MEMORY_MIB": "64G"}, "BIFFIX_SINGLE_JOB_MEMORY_MIB is not a number"),
        ({"BIFFIX_SINGLE_JOB_CPUS": "0"}, "BIFFIX_SINGLE_JOB_CPUS must be at least 1"),
        ({"BIFFIX_EXECUTION": "cluster"}, "unknown execution mode BIFFIX_EXECUTION='cluster'"),
        ({"BIFFIX_RESUME": "1"}, "a single-job run cannot be resumed"),
        ({"BIFFIX_SCRATCH_ROOT": "/"}, "the scratch root must be an absolute path other than /"),
        ({"BIFFIX_SCRATCH_ROOT": "//"}, "the scratch root must be an absolute path other than /"),
        (
            {"BIFFIX_SCRATCH_ROOT": "/scratch/.."},
            "the scratch root must be an absolute path other than /",
        ),
        ({"BIFFIX_SCRATCH_ROOT": "scratch"}, "the scratch root must be an absolute path"),
        ({"BIFFIX_SIF_PARALLEL": "0"}, "BIFFIX_SIF_PARALLEL must be at least 1: 0"),
        ({"BIFFIX_SIF_PARALLEL": "two"}, "BIFFIX_SIF_PARALLEL is not a number: two"),
        ({"BIFFIX_SCRATCH_ROOT": "/nonexistent/scratch"}, "is not a writable directory"),
        ({"PATH": PATH_WITHOUT_APPTAINER}, "has no apptainer: a single-job run needs an x86_64"),
        # Too small to leave the tasks a GiB beside the engine's reserve.
        ({"BIFFIX_SINGLE_JOB_MEMORY_MIB": "3000"}, "leaves 952 MiB for tasks"),
    ],
)
def test_a_misconfigured_single_job_run_fails_with_ex_config(
    sm_single: SingleJob, env: dict[str, str], why: str, tmp_path: Path
) -> None:
    if env.get("PATH") == PATH_WITHOUT_APPTAINER:
        env = {**env, "PATH": _path_without_apptainer(tmp_path)}
        assert not (Path(env["PATH"]) / "apptainer").exists()
        assert (Path(env["PATH"]) / "bash").exists()
    got = sm_single.run(**env)
    assert got.returncode == 78, got.stderr
    assert why in got.stderr
    assert not (sm_single.job.out / "argv").exists()


def test_the_engine_does_not_inherit_the_scratch_lock(sm_single: SingleJob) -> None:
    # Workflow code holding the lock's descriptor could release it (flock LOCK_UN), and a second
    # coordinator on the host would then wipe this run's scratch root.
    opened = sm_single.job.out / "open-files"
    got = sm_single.run(FAKE_OPEN_FILES=str(opened))
    assert got.returncode == 0, got.stderr
    held = opened.read_text().splitlines()
    assert held, "the probe lists at least its own output file"
    marker = sm_single.marker.stat()
    assert f"{marker.st_dev} {marker.st_ino}" not in held


def test_a_zero_padded_single_job_size_is_read_as_decimal(sm_single: SingleJob) -> None:
    # Bash arithmetic would read 04000 as octal 2048, leaving the tasks nothing beside Snakemake's
    # 2 GiB reserve; 08 is not octal at all.
    got = sm_single.run(BIFFIX_SINGLE_JOB_CPUS="08", BIFFIX_SINGLE_JOB_MEMORY_MIB="04000")
    assert got.returncode == 0, got.stderr
    assert "single-job run: 8 vCPUs, 4000 MiB" in got.stderr
    assert _after(sm_single.job.argv, "--cores") == "8"


def test_the_scratch_root_is_cleared_before_the_run_stages_anything(sm_single: SingleJob) -> None:
    # A previous run's data on this reused host, and an image file its workflow could have changed.
    (sm_single.scratch / "old-run/src").mkdir(parents=True)
    (sm_single.scratch / "old-run/src/secret.txt").write_text("another project's data")
    (sm_single.scratch / ".hidden").write_text("x")
    images = sm_single.scratch / "apptainer/images"
    images.mkdir(parents=True)
    (images / _sif(TOOL)).write_text("tampered")
    got = sm_single.run()
    assert got.returncode == 0, got.stderr
    kept = sm_single.kept
    assert not (kept / "old-run").exists() and not (kept / ".hidden").exists()
    assert (
        kept / "apptainer/images" / _sif(TOOL)
    ).read_text() == f"image file of docker://{TOOL}\n"
    # And it is cleared again at exit.
    assert list(sm_single.scratch.iterdir()) == []


def test_a_scratch_root_that_is_not_the_instance_store_is_refused(sm_single: SingleJob) -> None:
    sm_single.marker.unlink()
    (sm_single.scratch / "host-file").write_text("the host's own")
    got = sm_single.run()
    assert got.returncode == 78, got.stderr
    assert f"is not the host's instance store (no marker {sm_single.marker})" in got.stderr
    assert not (sm_single.job.out / "argv").exists()
    # Nothing is wiped.
    assert (sm_single.scratch / "host-file").read_text() == "the host's own"


def test_the_scratch_root_is_canonicalized(sm_single: SingleJob) -> None:
    # Spelled with `..` and repeated slashes, it is still the one directory that is wiped.
    (sm_single.scratch / "old").write_text("x")
    spelled = f"{sm_single.scratch}/../{sm_single.scratch.name}//"
    got = sm_single.run(BIFFIX_SCRATCH_ROOT=spelled)
    assert got.returncode == 0, got.stderr
    assert sm_single.job.engine_env["PATH"].startswith(f"{sm_single.scratch.resolve()}/run/bin:")
    assert list(sm_single.scratch.iterdir()) == []


def test_every_task_image_is_converted_before_the_engine_starts(sm_single: SingleJob) -> None:
    got = sm_single.run(
        BIFFIX_WORKER_IMAGE=WORKER,
        BIFFIX_SIF_PARALLEL="2",
        FAKE_PULL_BARRIER="2",
        FAKE_PULL_SECS="0.5",
    )
    assert got.returncode == 0, got.stderr
    # Each distinct tenant image once, into the image file Nextflow would look for.
    pulls = sm_single.pulls
    assert sorted(p[0] for p in pulls) == sorted(f"docker://{i}" for i in (TOOL, WORKER, PUBLIC))
    pulled = {p[0]: p[1:] for p in pulls}
    assert sorted(p.name for p in (sm_single.kept / "apptainer/images").iterdir()) == sorted(
        _sif(i) for i in (TOOL, WORKER, PUBLIC)
    )
    # The ECR login goes to the region's ECR only, and is never logged nor left to the engine.
    assert pulled[f"docker://{TOOL}"] == ["AWS", "ecr-SECRET-pw"]
    assert pulled[f"docker://{PUBLIC}"] == ["-", "-"]
    assert "ecr-SECRET-pw" not in got.stderr + got.stdout
    assert sm_single.job.engine_env["APPTAINER_DOCKER_PASSWORD"] == "<unset>"
    # Several at once, never more than asked for.
    assert max(int(n) for n in (sm_single.job.out / "concurrency").read_text().split()) == 2
    # No task job definitions: none was asked for, none fetched.
    assert sm_single.job.engine_env["SNAKEMAKE_AWS_BATCH_JOB_DEFINITION_MAP"] == "<unset>"
    # Apptainer's own cache and build space are on the instance store.
    assert sm_single.job.engine_env["APPTAINER_CACHEDIR"] == str(
        sm_single.scratch / "apptainer/cache"
    )


def test_a_task_image_that_cannot_be_pulled_fails_the_run_at_once(sm_single: SingleJob) -> None:
    started = time.monotonic()
    try:
        got = sm_single.run(
            FAKE_PULL_FAIL=f"docker://{PUBLIC}",
            FAKE_PULL_FAIL_AFTER_HANG="1",
            FAKE_PULL_HANG=f"docker://{TOOL}",
            FAKE_PULL_SECS="0",
        )
        took = time.monotonic() - started
        # The failure came while the other pull was in flight, and stopped it.
        (hung,) = _hung_pulls(sm_single.job)
        assert _group_gone(hung), "the pull in flight outlived the failure"
    finally:
        _kill_hung_pulls(sm_single.job)
    # A run failure, not a misconfiguration, naming the image; the other pulls are stopped.
    assert got.returncode == 1, got.stderr
    assert f"could not convert the task image {PUBLIC}" in got.stderr
    assert f"FATAL: unauthorized: docker://{PUBLIC}" in got.stderr
    assert took < 30, took
    assert not (sm_single.job.out / "argv").exists()
    # Its reason is in the run's logs, as the conversion's failure, not the engine's.
    failure = _logs(sm_single.job)["failure.txt"].decode()
    assert failure.startswith("Converting the task images failed (exit status 1).")
    assert PUBLIC in failure


def test_a_cancel_stops_the_image_conversion_at_once(sm_single: SingleJob) -> None:
    sm_single.job.put("bucket", "runs/run-test/images.json", json.dumps(SJ_MAP).encode())
    env = sm_single.job.env(
        **sm_single.env(BIFFIX_IMAGE_MAP_URI=MAP_URI, FAKE_PULL_HANG=f"docker://{TOOL}")
    )
    try:
        with _coordinator(env) as proc:
            deadline = time.monotonic() + 30
            while not _hung_pulls(sm_single.job):
                assert proc.poll() is None, stop_group(proc)
                assert time.monotonic() < deadline, "the conversion never started"
                time.sleep(0.05)
            signalled = time.monotonic()
            proc.send_signal(signal.SIGTERM)
            _, stderr = proc.communicate(timeout=30)
            took = time.monotonic() - signalled
        (hung,) = _hung_pulls(sm_single.job)
        assert _group_gone(hung), "the pull in flight outlived the cancel"
    finally:
        _kill_hung_pulls(sm_single.job)
    # Stopped well within Batch's stop timeout, as a cancel, not a failure.
    assert proc.returncode == 143, stderr
    assert took < 10, took
    assert "stopping the task images' conversion (SIGTERM)" in stderr
    assert "the task image conversion was stopped" in stderr
    assert not (sm_single.job.out / "argv").exists()
    assert "failure.txt" not in _logs(sm_single.job)
    # A stopped run leaves its scratch to the next run to clear.
    assert "leaving" in stderr and (sm_single.scratch / "run").exists()


def test_no_ecr_login_fails_the_run(sm_single: SingleJob) -> None:
    got = sm_single.run(FAKE_ECR_DENY="1")
    assert got.returncode == 1, got.stderr
    assert "could not log in to ECR in us-test-1" in got.stderr
    assert sm_single.pulls == []


def test_a_single_job_dry_run_converts_nothing_and_writes_nothing(sm_single: SingleJob) -> None:
    sm_single.job.put("bucket", "runs/run-test/images.json", json.dumps(SJ_MAP).encode())
    before = _s3_objects(sm_single.job)
    got = sm_single.run_without_map(BIFFIX_IMAGE_MAP_URI=MAP_URI, BIFFIX_DRY_RUN="1")
    assert got.returncode == 0, got.stderr
    assert "--dry-run" in sm_single.job.argv
    assert sm_single.pulls == []
    assert _s3_objects(sm_single.job) == before


def test_single_job_snakemake_runs_every_job_here_in_its_image(sm_single: SingleJob) -> None:
    got = sm_single.run(BIFFIX_WORKER_IMAGE=WORKER)
    assert got.returncode == 0, got.stderr
    argv = sm_single.job.argv
    scratch = sm_single.scratch
    assert _after(argv, "--cores") == "8"
    # 64 GiB less Snakemake's 4 GiB reserve, as the one memory resource Snakemake 9.27 takes.
    at = argv.index("--resources")
    assert argv[at + 1 : at + 3] == ["mem_mb=64424", "--sdm"]
    assert _after(argv, "--sdm") == "apptainer"
    assert _after(argv, "--apptainer-prefix") == str(scratch / "apptainer/snakemake")
    assert "--apptainer-args=--cleanenv" in argv
    assert _after(argv, "--local-storage-prefix") == str(scratch / "storage")
    assert _after(argv, "--default-storage-prefix") == "s3://bucket/runs/run-test/"
    assert "--retries" in argv and "--keep-going" in argv and "--logger" in argv
    assert _after(argv, "--executor") == "local"
    for batch_only in ("--jobs", "--container-image", "--aws-batch-job-queue"):
        assert batch_only not in argv
    assert not any(a.startswith("--aws-batch") for a in argv)
    # Tasks start through biffix's apptainer wrapper.
    assert sm_single.job.engine_env["PATH"].startswith(f"{scratch}/run/bin:")


def test_single_job_snakemake_sets_every_rule_s_image_in_a_wrapper_snakefile(
    sm_single: SingleJob,
) -> None:
    got = sm_single.run(BIFFIX_WORKER_IMAGE=WORKER)
    assert got.returncode == 0, got.stderr
    src = sm_single.kept / "run/src"
    wrapper = Path(_after(sm_single.job.argv, "--snakefile"))
    assert wrapper.parent == sm_single.scratch / "run/src"
    assert wrapper.name.startswith(".biffix-container-") and wrapper.suffix == ".smk"
    text = (src / wrapper.name).read_text()
    calls = [node for node in ast.walk(ast.parse(text)) if isinstance(node, ast.Call)]
    include = next(
        node
        for node in calls
        if isinstance(node.func, ast.Attribute) and node.func.attr == "include"
    )
    assert ast.literal_eval(include.args[0]) == str(sm_single.scratch / "run/src/Snakefile")
    assert [(arg.arg, ast.literal_eval(arg.value)) for arg in include.keywords] == [
        ("overwrite_default_target", True)
    ]
    task_images = sm_single.scratch / "run/task-images.json"
    images = sm_single.scratch / "apptainer/images"
    map_file = sm_single.scratch / "run/images.json"
    containers = next(
        node
        for node in calls
        if isinstance(node.func, ast.Attribute) and node.func.attr == "set_rule_containers"
    )
    assert [ast.literal_eval(arg) for arg in containers.args[1:]] == [
        str(map_file),
        str(images),
        WORKER,
        str(task_images),
        True,
    ]
    # Nothing is linked into the source: no `container:` value is read.
    assert not any(p.is_symlink() for p in src.rglob("*"))
    # The logger plugin finds each rule's image there.
    assert sm_single.job.engine_env["BIFFIX_TASK_IMAGES_FILE"] == str(task_images)
    # The engine config biffix generated is in the run's logs.
    logs = _logs(sm_single.job)
    assert logs["Snakefile.biffix"].decode() == text
    assert b"--sdm\napptainer\n" in logs["snakemake.args"]


def test_single_job_snakemake_without_a_default_image_runs_such_rules_here(
    sm_single: SingleJob,
) -> None:
    got = sm_single.run()
    assert got.returncode == 0, got.stderr
    wrapper = Path(_after(sm_single.job.argv, "--snakefile"))
    text = (sm_single.kept / "run/src" / wrapper.name).read_text()
    calls = [node for node in ast.walk(ast.parse(text)) if isinstance(node, ast.Call)]
    containers = next(
        node
        for node in calls
        if isinstance(node.func, ast.Attribute) and node.func.attr == "set_rule_containers"
    )
    assert ast.literal_eval(containers.args[2]) == str(sm_single.scratch / "apptainer/images")
    assert ast.literal_eval(containers.args[3]) == ""


def test_a_single_job_run_without_an_image_map_pulls_only_its_worker_image(
    sm_single: SingleJob,
) -> None:
    got = sm_single.run_without_map(BIFFIX_WORKER_IMAGE=WORKER)
    assert got.returncode == 0, got.stderr
    assert [p[0] for p in sm_single.pulls] == [f"docker://{WORKER}"]
    assert sm_single.job.engine_env["BIFFIX_IMAGE_MAP_FILE"] == "<unset>"


def test_a_single_job_run_with_neither_an_image_map_nor_a_worker_image_pulls_nothing(
    sm_single: SingleJob,
) -> None:
    got = sm_single.run_without_map()
    assert got.returncode == 0, got.stderr
    assert sm_single.pulls == []


def test_a_single_job_dry_run_checks_no_image_file(sm_single: SingleJob) -> None:
    got = sm_single.run(BIFFIX_DRY_RUN="1")
    assert got.returncode == 0, got.stderr
    assert Path(_after(sm_single.job.argv, "--snakefile")).name.startswith(".biffix-container-")
    # The run converts no image, so the wrapper is told not to look for their files.
    wrapper = Path(_after(sm_single.job.argv, "--snakefile"))
    text = (sm_single.kept / "run/src" / wrapper.name).read_text()
    assert text.rstrip().endswith(", False)")


def test_snakemake_parses_the_command_line_of_a_single_job_run(sm_single: SingleJob) -> None:
    pytest.importorskip("snakemake")
    got = sm_single.run(BIFFIX_WORKER_IMAGE=WORKER, BIFFIX_TARGET="all")
    assert got.returncode == 0, got.stderr
    # The real Snakemake (the dev dependency, with the storage and logger plugins the command line
    # names) parses it: no option it refuses, no two memory resources.
    parsed = subprocess.run(
        [sys.executable, "-c",
         "import sys; from snakemake.cli import parse_args; "
         "_, a = parse_args(sys.argv[1:]); "
         "print(sorted(t.name for t in a.rerun_triggers), sorted(a.resources), a.targets, "
         "sorted(m.name for m in a.software_deployment_method))",
         *sm_single.job.argv],
        capture_output=True, text=True, timeout=120,
    )  # fmt: skip
    assert parsed.returncode == 0, parsed.stderr
    assert parsed.stdout.split("\n")[0] == (
        "['CODE', 'INPUT', 'MTIME', 'PARAMS'] ['mem_mb'] ['all'] ['APPTAINER']"
    )


def test_single_job_nextflow_runs_every_task_here_through_apptainer(nf_single: SingleJob) -> None:
    got = nf_single.run(BIFFIX_WORKER_IMAGE=WORKER, BIFFIX_ENGINE_SESSION_ID="u1")
    assert got.returncode == 0, got.stderr
    scratch = nf_single.scratch
    config = (nf_single.kept / "run/nextflow.config").read_text().splitlines()
    for line in [
        "process.executor = 'local'",
        "process { withName: '.*' { executor = 'local' } }",
        f"process.container = '{WORKER}'",
        "aws.region = 'us-test-1'",
        "apptainer.enabled = true",
        "apptainer.ociAutoPull = false",
        f"apptainer.libraryDir = '{scratch}/apptainer/images'",
        f"apptainer.cacheDir = '{scratch}/apptainer/nextflow-cache'",
        f"workDir = '{scratch}/work'",
        # A pipeline's own cloud cache would index the local work dir, which Nextflow refuses.
        "cloudcache.enabled = false",
        "plugins { id 'nf-biffix@0.5.0' }",
        *(
            f"{engine}.enabled = false"
            for engine in ("docker", "podman", "singularity", "conda", "wave", "fusion")
        ),
    ]:
        assert line in config, line
    for batch_only in ("awsbatch", "aws.batch", "process.queue", "cloudcache {", "resourceLabels"):
        assert not any(batch_only in line for line in config), batch_only
    # The job's size, as the lowest-priority defaults; the JVM heap capped and kept out of it.
    defaults = (nf_single.kept / "run/.nextflow-home/config").read_text().splitlines()
    for line in [
        "process.errorStrategy = 'retry'",
        "executor.cpus = 8",
        "executor.memory = '59392 MB'",
        "process.resourceLimits = [cpus: 8, memory: '59392 MB']",
        "process.ext.singularity_pull_docker_container = true",
    ]:
        assert line in defaults, line
    # No trace: its task metrics would need `ps` and more in every task image.
    assert not any("trace" in line for line in defaults)
    env = nf_single.job.engine_env
    assert env["NXF_OPTS"] == "-Xmx4096m"
    assert env["NXF_APPTAINER_NEW_PID_NAMESPACE"] == "false"
    assert env["PATH"].startswith(f"{scratch}/run/bin:")
    assert env["NXF_UUID"] == "u1"
    assert "-resume" not in nf_single.job.argv


@pytest.mark.parametrize("dry_run", [False, True])
def test_a_local_work_dir_keeps_a_workflow_s_nf_cloudcache_from_loading(
    nf_single: SingleJob, dry_run: bool
) -> None:
    # On Nextflow 25.10 and 26.04 a loaded nf-cloudcache takes the cache over whatever
    # `cloudcache.enabled` says and refuses a run with no cloud path. A single-job run (and a dry
    # run, whose work dir is local too) names the workflow's other plugins, and nf-biffix, in a
    # -c `plugins {}`, which replaces the pipeline's list; the defaults still add biffix's.
    job = nf_single.job
    _with_workflow_plugins(job, "nf-schema@2.7.2", "nf-cloudcache@0.5.0", "nf-validation@1.1.4")
    got = job.run(BIFFIX_DRY_RUN="1") if dry_run else nf_single.run(BIFFIX_WORKER_IMAGE=WORKER)
    assert got.returncode == 0, got.stderr
    config = (
        (job.work if dry_run else nf_single.kept / "run").joinpath("nextflow.config").read_text()
    )
    assert (
        "plugins {\n  id 'nf-schema@2.7.2'\n  id 'nf-validation@1.1.4'\n  id 'nf-biffix@0.5.0'\n}"
        in config
    )
    assert "nf-cloudcache" not in config
    assert job.engine_env["NXF_PLUGINS_DEFAULT"] == "nf-amazon@3.4.4,nf-biffix@0.5.0"


@pytest.mark.parametrize("dry_run", [False, True])
def test_a_workflow_plugin_with_a_control_character_fails_a_local_work_dir_run(
    nf_single: SingleJob, dry_run: bool
) -> None:
    # The refusal fails the run (78), not just the command substitution quoting it: the config
    # never gets an `id` line with nothing after it.
    job = nf_single.job
    _with_workflow_plugins(job, "nf-schema@2.7.2", "nf-cloudcache@0.5.0")
    listing = job.runtime / "nf/workflow-plugins"
    listing.write_text(listing.read_text() + "nf-validation@1.1.4\r\n")
    got = job.run(BIFFIX_DRY_RUN="1") if dry_run else nf_single.run(BIFFIX_WORKER_IMAGE=WORKER)
    assert got.returncode == 78, got.stderr
    # The captured stderr reads the `\r` as a line break (universal newlines).
    assert "the workflow plugin nf-validation@1.1.4\n has a control character" in got.stderr
    config = (job.work if dry_run else nf_single.kept / "run").joinpath("nextflow.config")
    lines = config.read_text().splitlines() if config.exists() else []
    assert [line for line in lines if line.startswith("  id") and not line.endswith("'")] == []
    assert not (job.out / "argv").exists()


def test_a_workflow_s_nf_cloudcache_still_loads_where_the_work_dir_is_s3(nf_job: Job) -> None:
    # A real Batch run enables the cloud cache, with its S3 path: the pipeline's list stays whole.
    _with_workflow_plugins(nf_job, "nf-schema@2.7.2", "nf-cloudcache@0.5.0")
    got = nf_job.run()
    assert got.returncode == 0, got.stderr
    assert "plugins" not in (nf_job.work / "nextflow.config").read_text()


@pytest.mark.skipif(os.geteuid() == 0, reason="root reads a file whatever its mode")
def test_an_unreadable_workflow_plugins_list_fails_a_local_work_dir_run(
    nf_single: SingleJob,
) -> None:
    # A list the coordinator cannot read might name nf-cloudcache: the run is refused as
    # misconfigured, not started with the pipeline's own list.
    _with_workflow_plugins(nf_single.job, "nf-schema@2.7.2", "nf-cloudcache@0.5.0")
    listing = nf_single.job.runtime / "nf/workflow-plugins"
    listing.chmod(0)
    try:
        got = nf_single.run(BIFFIX_WORKER_IMAGE=WORKER)
    finally:
        listing.chmod(0o644)
    assert got.returncode == 78, got.stderr
    assert "cannot read the runtime layer's workflow plugin list" in got.stderr


def test_a_local_work_dir_leaves_a_workflow_without_nf_cloudcache_its_own_list(
    nf_single: SingleJob,
) -> None:
    _with_workflow_plugins(nf_single.job, "nf-schema@2.7.2")
    got = nf_single.run(BIFFIX_WORKER_IMAGE=WORKER)
    assert got.returncode == 0, got.stderr
    assert "plugins" not in (nf_single.kept / "run/nextflow.config").read_text()


def test_a_retried_single_job_nextflow_run_starts_afresh(nf_single: SingleJob) -> None:
    got = nf_single.run(BIFFIX_ENGINE_SESSION_ID="u1", AWS_BATCH_JOB_ATTEMPT="2")
    assert got.returncode == 0, got.stderr
    assert "-resume" not in nf_single.job.argv
    assert nf_single.job.engine_env["NXF_UUID"] == "u1"
    assert nf_single.job.engine_env["NXF_IGNORE_RESUME_HISTORY"] == "<unset>"


def test_a_single_job_nextflow_run_uploads_its_failed_tasks_files(nf_single: SingleJob) -> None:
    task = nf_single.scratch / "work/ab/cdef0123"
    got = nf_single.run(FAKE_FAILED_TASK=str(task), FAKE_WRITE_LOG="1", FAKE_RC="1")
    assert got.returncode == 1, got.stderr
    logs = _logs(nf_single.job)
    assert logs["tasks/ab/cdef0123/.command.err"] == b"task says: out of cheese\n"
    assert "engine says: boom" in logs["failure.txt"].decode()
    assert "nextflow.log" in logs and "nextflow.config" in logs


def test_the_apptainer_wrapper_raises_each_task_s_oom_score(
    nf_single: SingleJob,
) -> None:
    got = nf_single.run()
    assert got.returncode == 0, got.stderr
    wrapper = nf_single.kept / "run/bin/apptainer"
    # Run as a task's container starts: `apptainer exec <image> …`, found on PATH.
    task = subprocess.run(
        [str(wrapper), "exec", "img", "bash"],
        env=nf_single.env(APPTAINER_BINDPATH="/refs"),
        capture_output=True,
        text=True,
        timeout=60,
    )
    assert task.returncode == 0, task.stderr
    # An OOM score adjustment above the engine's (the kernel weighs it with each process's memory
    # use); and its /tmp on the instance store, beside any other bind.
    assert (nf_single.job.out / "oom_score_adj").read_text() == "500\n"
    assert (nf_single.job.out / "apptainer-argv").read_text().splitlines() == [
        "exec",
        "img",
        "bash",
        f"APPTAINER_BINDPATH=/refs,{nf_single.scratch}/tmp:/tmp,{nf_single.scratch}/tmp",
        "APPTAINERENV_OMP_NUM_THREADS=<unset>",
    ]
    # A Snakemake job's thread count reaches its container, which --cleanenv would not let it.
    task = subprocess.run(
        [str(wrapper), "exec", "img", "bash"],
        env=nf_single.env(OMP_NUM_THREADS="3"),
        capture_output=True,
        text=True,
        timeout=60,
    )
    assert task.returncode == 0, task.stderr
    argv = (nf_single.job.out / "apptainer-argv").read_text().splitlines()
    assert argv[-1] == "APPTAINERENV_OMP_NUM_THREADS=3"


def test_tasks_mode_is_unchanged_by_the_single_job_variables(sm_job: Job) -> None:
    # BIFFIX_EXECUTION absent or "tasks": the run is today's, whatever else is set.
    for execution in ("", "tasks"):
        got = sm_job.run(BIFFIX_EXECUTION=execution, BIFFIX_SINGLE_JOB_CPUS="8")
        assert got.returncode == 0, got.stderr
        assert _after(sm_job.argv, "--executor") == "aws-batch"
        assert "--sdm" not in sm_job.argv
        # Its metadata and a single-job run's serve each other (lib/snakemake.sh).
        at = sm_job.argv.index("--rerun-triggers")
        assert sm_job.argv[at + 1 : at + 5] == ["mtime", "params", "input", "code"]
