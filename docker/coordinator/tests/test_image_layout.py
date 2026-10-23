"""The coordinator image's layout: the Dockerfile copies the entrypoint's libraries and helpers by
name, so a file added to lib/ (or one the entrypoint starts to use) and not to the Dockerfile would
pass every other test here and fail only in a real run."""

from __future__ import annotations

import re
import sys
from pathlib import Path

COORDINATOR = Path(__file__).resolve().parents[1]
LIB = COORDINATOR / "lib"
IMAGE_LIB = "/usr/local/lib/biffix/"


def _lib_files() -> set[str]:
    return {p.name for p in LIB.iterdir() if p.is_file() and p.suffix in (".sh", ".py")}


def _copied_to_the_image_lib() -> set[str]:
    """What the Dockerfile's COPY instructions put in /usr/local/lib/biffix/."""
    text = (COORDINATOR / "Dockerfile").read_text().replace("\\\n", " ")
    copied = set()
    for line in text.splitlines():
        words = line.split()
        if not words or words[0] != "COPY":
            continue
        *sources, dest = words[1:]
        for src in sources:
            name = Path(src).name
            if dest == IMAGE_LIB or dest == IMAGE_LIB + name:
                assert src == f"docker/coordinator/lib/{name}", src
                copied.add(name)
    return copied


def _referenced() -> set[str]:
    """Every `$LIB_DIR/<file>` the entrypoint and its libraries source or run."""
    scripts = [COORDINATOR / "entrypoint.sh", *LIB.glob("*.sh")]
    return {name for s in scripts for name in re.findall(r"\$LIB_DIR/([\w.-]+)", s.read_text())}


def test_the_image_carries_every_library_and_helper() -> None:
    assert _copied_to_the_image_lib() == _lib_files()


def test_everything_the_entrypoint_uses_is_in_lib() -> None:
    referenced = _referenced()
    assert referenced <= _lib_files(), referenced - _lib_files()
    # And nothing in lib/ is dead weight in the image.
    assert _lib_files() <= referenced, _lib_files() - referenced


def test_the_entrypoint_is_still_the_coordinator_command() -> None:
    # The coordinator job definition's command is `biffix-coordinator` (infra constructs).
    dockerfile = (COORDINATOR / "Dockerfile").read_text()
    assert "COPY docker/coordinator/entrypoint.sh /usr/local/bin/biffix-coordinator" in dockerfile
    assert "RUN chmod +x /usr/local/bin/biffix-coordinator" in dockerfile


def test_no_helper_shadows_a_module_it_may_import() -> None:
    # A helper runs as `python3 <lib>/<helper>.py`, which puts lib/ first on sys.path: a helper
    # named like a standard-library module (queue, events…) would replace it for every helper,
    # boto3's own imports included.
    names = {Path(f).stem for f in _lib_files() if f.endswith(".py")}
    assert not names & set(sys.stdlib_module_names)
    assert not names & {"boto3", "botocore", "snakemake", "urllib3", "s3transfer"}


def test_the_libraries_only_define_functions() -> None:
    # Sourcing a library runs nothing: the entrypoint decides the order of the steps.
    for lib in LIB.glob("*.sh"):
        depth = 0
        for n, line in enumerate(lib.read_text().splitlines(), 1):
            code = line.strip()
            if depth == 0 and code and not code.startswith("#"):
                assert re.match(r"^[a-z0-9_]+\(\) \{$", code), f"{lib.name}:{n}: {line}"
            depth += code.count("{") - code.count("}") if not code.startswith("#") else 0
