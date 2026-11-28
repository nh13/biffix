"""The coordinator image's layout: the Dockerfile copies the entrypoint's libraries and helpers by
name, so a file added to lib/ (or one the entrypoint starts to use) and not to the Dockerfile would
pass every other test here and fail only in a real run."""

from __future__ import annotations

import ast
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


def _imports(helper: Path) -> set[str]:
    """The top-level module of every import in `helper`, wherever it is (a function imports a
    sibling lazily as often as the module does at the top)."""
    names: set[str] = set()
    for node in ast.walk(ast.parse(helper.read_text())):
        if isinstance(node, ast.Import):
            names.update(alias.name.split(".")[0] for alias in node.names)
        elif isinstance(node, ast.ImportFrom) and node.module and node.level == 0:
            names.add(node.module.split(".")[0])
    return names


def _referenced() -> set[str]:
    """Every `$LIB_DIR/<file>` the entrypoint and its libraries source or run, and every lib/
    module a helper imports (a helper has lib/ first on its path)."""
    scripts = [COORDINATOR / "entrypoint.sh", *LIB.glob("*.sh")]
    run = {name for s in scripts for name in re.findall(r"\$LIB_DIR/([\w.-]+)", s.read_text())}
    imported = {
        f"{name}.py"
        for helper in LIB.glob("*.py")
        for name in _imports(helper)
        if (LIB / f"{name}.py").is_file()
    }
    return run | imported


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


def _apptainer_install(dockerfile: str) -> str:
    """The Dockerfile's Apptainer RUN instruction, continuation lines joined."""
    joined = dockerfile.replace("\\\n", " ")
    runs = [line for line in joined.splitlines() if line.startswith("RUN ") and "apptainer" in line]
    (run,) = runs
    return run


def test_apptainer_is_the_pinned_non_setuid_package() -> None:
    # Single-job runs' tasks run through it: never the setuid-root build, never unpinned. Both
    # architectures get the same release, each package verified by its own checksum.
    dockerfile = (COORDINATOR / "Dockerfile").read_text()
    pins = dict(re.findall(r"^ARG (APPTAINER_[A-Z0-9_]+)=(\S+)$", dockerfile, re.MULTILINE))
    assert pins.keys() == {
        "APPTAINER_VERSION",
        "APPTAINER_AMD64_SHA256",
        "APPTAINER_ARM64_SHA256",
    }
    for arch in ("AMD64", "ARM64"):
        assert re.fullmatch(r"[0-9a-f]{64}", pins[f"APPTAINER_{arch}_SHA256"]), arch
    assert pins["APPTAINER_AMD64_SHA256"] != pins["APPTAINER_ARM64_SHA256"]
    run = _apptainer_install(dockerfile)
    # The package and its checksum follow the build's architecture; any other fails the build.
    assert run.startswith('RUN arch="$(dpkg --print-architecture)"')
    assert 'case "${arch}" in' in run
    assert re.search(r'amd64\) sha256="\$\{APPTAINER_AMD64_SHA256\}" ;;', run)
    assert re.search(r'arm64\) sha256="\$\{APPTAINER_ARM64_SHA256\}" ;;', run)
    assert re.search(r"\*\) echo [^;]*unsupported architecture[^;]*>&2; exit 1 ;;", run)
    # The package file spells a prerelease with dots where the release tag has a hyphen
    # (v1.6.0-rc.2 holds apptainer_1.6.0.rc.2-...); a final release spells both alike.
    assert 'package_version="$(printf \'%s\' "${APPTAINER_VERSION}" | tr - .)"' in run
    assert (
        "releases/download/v${APPTAINER_VERSION}/"
        "apptainer_${package_version}-trixie%2B_${arch}.deb" in run
    )
    # Verified before it is installed, and the installed version is the pinned one, which the
    # binary reports in the tag's spelling (`apptainer version 1.6.0-rc.2`).
    assert run.index("| sha256sum -c -") < run.index("apt-get install")
    assert 'apptainer --version)" = "apptainer version ${APPTAINER_VERSION}"' in run
    assert "apptainer-suid" not in dockerfile.replace("(not apptainer-suid)", "")
    assert "| sha256sum -c -" in dockerfile
    assert "sed -i 's/^allow setuid = yes$/allow setuid = no/' /etc/apptainer/apptainer.conf" in (
        dockerfile
    )
    assert "grep -qx 'allow setuid = no' /etc/apptainer/apptainer.conf" in dockerfile
    # Built for the base's Debian release (a base bump to a new release needs the matching .deb).
    assert "Debian (trixie)" in dockerfile


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
