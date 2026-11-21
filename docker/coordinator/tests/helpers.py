"""Helpers the coordinator's test modules share: run a library function in bash or a helper as
the entrypoint does, import a helper as a module, and run a real Snakemake through a single-job
run's wrapper Snakefile."""

from __future__ import annotations

import importlib.util
import json
import os
import subprocess
import sys
from pathlib import Path
from types import ModuleType

LIB = Path(__file__).resolve().parents[1] / "lib"
FAKES = Path(__file__).parent / "fakes" / "py"


def bash(code: str, *libs: str, stdin: str = "", **env: str) -> subprocess.CompletedProcess[str]:
    """`code` in bash under the entrypoint's options, with common.sh and `libs` sourced."""
    sources = "".join(f'. "$LIB_DIR/{lib}.sh"\n' for lib in ("common", *libs))
    return subprocess.run(
        ["bash", "-c", f"set -euo pipefail\n{sources}{code}"],
        input=stdin,
        env={"PATH": os.environ["PATH"], "LIB_DIR": str(LIB), "PYTHONPATH": str(FAKES)} | env,
        capture_output=True,
        text=True,
        timeout=60,
    )


def helper(name: str, *args: str, stdin: str = "", **env: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [sys.executable, str(LIB / f"{name}.py"), *args],
        input=stdin,
        env={"PATH": os.environ["PATH"], "PYTHONPATH": str(FAKES)} | env,
        capture_output=True,
        text=True,
        timeout=60,
    )


def module(name: str) -> ModuleType:
    # As for a helper run as `python3 <lib>/<helper>.py`: lib/ is on the path, for the modules
    # the helpers share (s3_uri).
    if str(LIB) not in sys.path:
        sys.path.insert(0, str(LIB))
    spec = importlib.util.spec_from_file_location(name, LIB / f"{name}.py")
    assert spec is not None and spec.loader is not None, name
    mod = importlib.util.module_from_spec(spec)
    # Registered first, as an import does: a dataclass looks its module up while it is defined.
    sys.modules[name] = mod
    spec.loader.exec_module(mod)
    return mod


# A fake apptainer for a real Snakemake run: it records the image each job runs in, then runs the
# job's command itself, with the environment Apptainer would give it under --cleanenv (the
# caller's is dropped, but for what the command needs to run here; APPTAINERENV_<NAME> sets
# <NAME>).
_RECORDING_APPTAINER = r"""#!/usr/bin/env bash
case "${1:-}" in --version|version) echo "apptainer version 1.5.5"; exit 0 ;; esac
shift  # run
clean=0
while [ $# -gt 0 ]; do
  case "$1" in
    --home|--bind|-B|--pwd) shift 2 ;;
    --cleanenv) clean=1; shift ;;
    -*) shift ;;
    *) break ;;
  esac
done
echo "$1" >> "$FAKE_IMAGES"
image=$1
shift
env=(BIFFIX_TEST_IMAGE="$image")
if [ "$clean" = 1 ]; then
  env+=(PATH="$PATH" HOME="$HOME")
  while IFS='=' read -r -d '' name value; do
    case "$name" in APPTAINERENV_*) env+=("${name#APPTAINERENV_}=$value") ;; esac
  done < <(env -0)
  exec env -i "${env[@]}" "$@"
fi
exec env "${env[@]}" "$@"
"""

# The run's default image, and the images its map names.
WORKER_IMAGE = "reg.io/w@sha256:2"
TOOL_IMAGE = "reg.io/t@sha256:1"
OTHER_IMAGE = "reg.io/o@sha256:3"


class RealSnakemake:
    """A workflow run by the real Snakemake (the dev dependency) as a single-job run runs it: the
    command line lib/snakemake.sh builds (the wrapper Snakefile, the biffix executor), with a fake
    apptainer that records each job's image."""

    def __init__(self, root: Path):
        self.root, self.src, self.images = root, root / "src", root / "images"
        self.src.mkdir()
        (self.src / "mod").mkdir()
        self.images.mkdir()
        self.bin = root / "bin"
        self.bin.mkdir()
        exe = self.bin / "apptainer"
        exe.write_text(_RECORDING_APPTAINER)
        exe.chmod(0o755)
        self.map_file = root / "images.json"
        self.map_file.write_text(json.dumps(
            {"tool:1": TOOL_IMAGE, "docker.io/library/tool:1": TOOL_IMAGE, "other:2": OTHER_IMAGE,
             WORKER_IMAGE: WORKER_IMAGE, "$default": WORKER_IMAGE}
        ))  # fmt: skip
        self.recorded = root / "recorded"
        # The logger plugin's events of a run with `events=True`.
        self.events_file = self.src / ".biffix_events.jsonl"

    def image_file(self, image: str) -> str:
        """The image file a job in the tenant image `image` runs from."""
        return str(self.images / module("single_job").image_file_name(image))

    def task_images(self) -> dict[str, str | None]:
        """Each task's image as the logger plugin reported it (a run with `events=True`), by task
        key."""
        events = [json.loads(line) for line in self.events_file.read_text().splitlines()]
        return {e["task_key"]: e.get("image") for e in events if e["kind"] == "task_created"}

    def run(
        self,
        snakefile: str,
        *args: str,
        convert: bool = True,
        cpus: int = 1,
        memory_mib: int = 61440,
        default_image: str = WORKER_IMAGE,
        events: bool = False,
        env: dict[str, str] | None = None,
    ) -> subprocess.CompletedProcess[str]:
        (self.src / "Snakefile").write_text(snakefile)
        if convert:
            for image in (TOOL_IMAGE, WORKER_IMAGE, OTHER_IMAGE):
                Path(self.image_file(image)).write_text("image file")
        # As run_snakemake builds a single-job run's arguments (snakemake_mode_args).
        built = bash(
            'run_setup_step() { shift; "$@"; }; ARGS=(); ENVVARS=(); snakemake_mode_args; '
            'printf "%s\\0" "${ARGS[@]}"',
            "single_job",
            "snakemake",
            EXECUTION="single_job",
            SNAKEFILE=str(self.src / "Snakefile"),
            WORK=str(self.root),
            SCRATCH=str(self.root),
            SJ_IMAGES=str(self.images),
            SJ_BIN=str(self.bin),
            SJ_CPUS=str(cpus),
            SJ_TASK_MEMORY_MIB=str(memory_mib),
            BIFFIX_IMAGE_MAP_FILE=str(self.map_file),
            BIFFIX_WORKER_IMAGE=default_image,
            **(env or {}),
        )
        assert built.returncode == 0, built.stderr
        logger = ["--logger", "biffix", "--logger-biffix-run-id", "run-1"] if events else []
        return subprocess.run(
            [sys.executable, "-m", "snakemake", *built.stdout.rstrip("\0").split("\0"),
             *logger, *args],
            cwd=self.src,
            env={"PATH": f"{self.bin}:{os.environ['PATH']}", "HOME": str(self.root),
                 "FAKE_IMAGES": str(self.recorded), **(env or {})},
            capture_output=True,
            text=True,
            timeout=300,
        )  # fmt: skip
