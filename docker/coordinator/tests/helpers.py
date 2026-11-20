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
# job's command itself.
_RECORDING_APPTAINER = r"""#!/usr/bin/env bash
case "${1:-}" in --version|version) echo "apptainer version 1.5.5"; exit 0 ;; esac
shift  # exec
while [ $# -gt 0 ]; do
  case "$1" in
    --home|--bind|-B) shift 2 ;;
    -*) shift ;;
    *) break ;;
  esac
done
echo "$1" >> "$FAKE_IMAGES"
export BIFFIX_TEST_IMAGE="$1"
shift
exec "$@"
"""


class RealSnakemake:
    """A workflow run by the real Snakemake (the dev dependency) through a single-job run's wrapper
    Snakefile, with a fake apptainer that records each job's image."""

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
            {"tool:1": "reg.io/t@sha256:1", "docker.io/library/tool:1": "reg.io/t@sha256:1",
             "reg.io/w@sha256:2": "reg.io/w@sha256:2", "$default": "reg.io/w@sha256:2"}
        ))  # fmt: skip
        self.task_images = root / "task-images.json"
        self.recorded = root / "recorded"

    def run(
        self,
        snakefile: str,
        *args: str,
        convert: bool = True,
        cpus: int = 1,
        memory_mib: int = 61440,
    ) -> subprocess.CompletedProcess[str]:
        sj = module("single_job")
        (self.src / "Snakefile").write_text(snakefile)
        if convert:
            for image in ("reg.io/t@sha256:1", "reg.io/w@sha256:2"):
                (self.images / sj.image_file_name(image)).write_text("image file")
        built = bash(
            'run_setup_step() { shift; "$@"; }; ARGS=(); snakemake_single_job_args; '
            'printf "%s\\0" "${ARGS[@]}"',
            "snakemake",
            SNAKEFILE=str(self.src / "Snakefile"),
            WORK=str(self.root),
            SCRATCH=str(self.root),
            SJ_IMAGES=str(self.images),
            SJ_BIN=str(self.bin),
            SJ_CPUS=str(cpus),
            SJ_TASK_MEMORY_MIB=str(memory_mib),
            BIFFIX_IMAGE_MAP_FILE=str(self.map_file),
            BIFFIX_WORKER_IMAGE="reg.io/w@sha256:2",
        )
        assert built.returncode == 0, built.stderr
        return subprocess.run(
            [sys.executable, "-m", "snakemake", *built.stdout.rstrip("\0").split("\0"), *args],
            cwd=self.src,
            env={"PATH": f"{self.bin}:{os.environ['PATH']}", "HOME": str(self.root),
                 "FAKE_IMAGES": str(self.recorded)},
            capture_output=True,
            text=True,
            timeout=300,
        )  # fmt: skip
