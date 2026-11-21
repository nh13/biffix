"""Shared fixtures: a real Snakemake run through the executor, with a stand-in for Apptainer."""

from __future__ import annotations

import json
import os
import subprocess
import sys
from pathlib import Path

import pytest

from snakemake_executor_plugin_biffix.images import image_file_name

TOOL = "reg.io/t@sha256:1"
OTHER = "reg.io/o@sha256:3"
DEFAULT = "reg.io/w@sha256:2"
IMAGE_MAP = {"tool:1": TOOL, "other:2": OTHER, DEFAULT: DEFAULT}

# Stands in for Apptainer: records its arguments (a file per call: jobs start side by side), then
# runs the command with the environment `--cleanenv` leaves it (the caller's is dropped, but for
# what the command needs to run here; APPTAINERENV_<NAME> sets <NAME>).
FAKE_APPTAINER = r"""#!/usr/bin/env bash
printf '%s\n' "$*" > "$FAKE_APPTAINER_ARGV/$$"
shift  # run
clean=0
while [ $# -gt 0 ]; do
  case "$1" in
    --home|--bind|--pwd) shift 2 ;;
    --cleanenv) clean=1; shift ;;
    -*) shift ;;
    *) break ;;
  esac
done
image=$1
shift
env=(BIFFIX_TEST_IMAGE="$(basename "$image")")
if [ "$clean" = 1 ]; then
  env+=(PATH="$PATH" HOME="$HOME")
  while IFS='=' read -r -d '' name value; do
    case "$name" in APPTAINERENV_*) env+=("${name#APPTAINERENV_}=$value") ;; esac
  done < <(env -0)
  exec env -i "${env[@]}" "$@"
fi
exec env "${env[@]}" "$@"
"""


class Run:
    """A workflow in a directory of its own, run by the real Snakemake with `--executor biffix`."""

    def __init__(self, root: Path):
        self.root = root
        self.src = root / "src"
        self.src.mkdir()
        self.images = root / "images"
        self.images.mkdir()
        for image in (TOOL, OTHER, DEFAULT):
            (self.images / image_file_name(image)).write_text("image file")
        self.map_file = root / "images.json"
        self.map_file.write_text(json.dumps(IMAGE_MAP))
        self.bin = root / "bin"
        self.bin.mkdir()
        apptainer = self.bin / "apptainer"
        apptainer.write_text(FAKE_APPTAINER)
        apptainer.chmod(0o755)
        self.argv = root / "apptainer-argv"
        self.argv.mkdir()

    def command(
        self,
        snakefile: str,
        *args: str,
        cores: int = 2,
        default_image: str | None = DEFAULT,
        image_map: bool = True,
    ) -> list[str]:
        """Write `snakefile` and return Snakemake's command line for it."""
        (self.src / "Snakefile").write_text(snakefile)
        argv = [sys.executable, "-m", "snakemake", "--executor", "biffix", "--jobs", str(cores)]
        argv += ["--cores", str(cores), "--biffix-images-dir", str(self.images)]
        if image_map:
            argv += ["--biffix-image-map", str(self.map_file)]
        if default_image:
            argv += ["--biffix-default-image", default_image]
        return [*argv, *args]

    def env(self, **extra: str) -> dict[str, str]:
        return {
            "PATH": f"{self.bin}:{Path(sys.executable).parent}:{os.environ['PATH']}",
            "HOME": str(self.root),
            "FAKE_APPTAINER_ARGV": str(self.argv),
            **extra,
        }

    def run(
        self,
        snakefile: str,
        *args: str,
        env: dict[str, str] | None = None,
        cores: int = 2,
        default_image: str | None = DEFAULT,
        image_map: bool = True,
    ) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            self.command(
                snakefile, *args, cores=cores, default_image=default_image, image_map=image_map
            ),
            cwd=self.src,
            env=self.env(**(env or {})),
            capture_output=True,
            text=True,
            timeout=300,
        )

    def calls(self) -> list[str]:
        """The arguments of each call of the stand-in Apptainer, one line per call."""
        return sorted(path.read_text().rstrip("\n") for path in self.argv.iterdir())

    def output(self, name: str) -> str:
        return (self.src / name).read_text().strip()


@pytest.fixture
def run(tmp_path: Path) -> Run:
    return Run(tmp_path)


def file_of(image: str) -> str:
    """The image file's name, which the stand-in Apptainer hands a job as BIFFIX_TEST_IMAGE."""
    return image_file_name(image)
