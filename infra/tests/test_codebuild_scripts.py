"""The CodeBuild scripts (`codebuild/lib/common.sh`, `codebuild/image/import.sh`) and the buildspec
that reads their outputs, run for real in bash: the probe over synthetic file listings, the output
file's writer and reader, the archive's upload order, the import project's import, derive and index
steps and its `main` against a stand-in registry (`fakes/crane.py`) that enforces the workflow
and builds repositories' write-once tags, and the runtime build's shared-object architecture
check."""

from __future__ import annotations

import json
import os
import struct
import subprocess
import sys
from pathlib import Path

import pytest

from biffix_infra.constructs.script_project import inline_buildspec
from biffix_infra.image_restore import keep_tags
from biffix_infra.paths import CODEBUILD_DIR
from biffix_infra.stacks.build_stack import IMAGE_IMPORT_OUTPUTS
from biffix_infra.stacks.data_stack import BUILDS_MUTABLE_TAGS, WORKFLOW_IMMUTABLE_TAGS
from tests.fakes import crane as fake_crane
from tests.fakes.crane import Obj, State

COMMON = CODEBUILD_DIR / "lib/common.sh"
IMPORT = CODEBUILD_DIR / "image/import.sh"
RUNTIME_BUILD = CODEBUILD_DIR / "runtime/build.sh"
FAKE_CRANE = Path(__file__).parent / "fakes/crane.py"
REPO = "reg.example/wf"
# The builds repository: only biffix's build and import projects push to it.
BUILDS = "reg.example/builds"


def test_the_fake_registry_s_write_once_tags_are_the_repositories_own() -> None:
    assert fake_crane.IMMUTABLE == tuple(t.removesuffix("*") for t in WORKFLOW_IMMUTABLE_TAGS)
    assert fake_crane.BUILDS_MUTABLE == tuple(t.removesuffix("*") for t in BUILDS_MUTABLE_TAGS)


def test_a_build_tag_is_write_once_in_the_builds_repository_only(tmp_path: Path) -> None:
    reg = Registry(tmp_path / "registry.json")
    one, two = reg.put(_img("amd64", ["sha256:a"])), reg.put(_img("amd64", ["sha256:b"]))
    for repo, moves in ((REPO, True), (BUILDS, False)):
        reg.crane("tag", f"{repo}@{one}", "build-0123")
        moved = subprocess.run(
            [sys.executable, str(FAKE_CRANE), "tag", f"{repo}@{two}", "build-0123"],
            env={**os.environ, "FAKE_REGISTRY": str(reg.path), "FAKE_BUILDS_REPO": BUILDS},
            capture_output=True,
            text=True,
        )
        assert (moved.returncode == 0) == moves, (repo, moved.stderr)
        # Scratch tags stay mutable everywhere.
        reg.crane("tag", f"{repo}@{one}", "scratch-x")
        reg.crane("tag", f"{repo}@{two}", "scratch-x")


# The digests the `registry` fixture seeds, by name.
RegistryIds = dict[str, str]
# The `registry` fixture: the registry, and the digests it is seeded with by name.
Seeded = tuple["Registry", RegistryIds]


def _bash(
    script: str, env: dict[str, str] | None = None, input: str | None = None
) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["bash", "-c", script],
        env={"PATH": os.environ["PATH"], "FAKE_BUILDS_REPO": BUILDS, **(env or {})},
        capture_output=True,
        text=True,
        timeout=60,
        input=input,
    )


def _common(
    body: str, env: dict[str, str] | None = None, input: str | None = None
) -> subprocess.CompletedProcess[str]:
    return _bash(f'set -euo pipefail\n. "{COMMON}"\n{body}', env, input)


# ── probe ──


def _probe(listing: list[str], *args: str) -> str:
    got = _common(f"probe {' '.join(args)}", input="\n".join(listing) + "\n")
    assert got.returncode == 0, got.stderr
    return got.stdout.strip()


def test_probe_reads_bash_libc_and_the_newest_versions() -> None:
    site = "usr/lib/python3.12/site-packages"
    caps = _probe(
        [
            "./bin/bash",
            "lib/x86_64-linux-gnu/libc.so.6",
            f"{site}/snakemake-9.3.0.dist-info/METADATA",
            f"{site}/snakemake-9.10.0.dist-info/METADATA",
            f"{site}/snakemake_storage_plugin_s3-0.3.6.dist-info/",
            f"{site}/snakemake_executor_plugin_aws_batch-0.2.0.dist-info/",
        ]
    )
    assert caps == ("bash=1,libc=glibc,snakemake=9.10.0,storage_s3=0.3.6,logger=,executor=0.2.0")


@pytest.mark.parametrize(
    ("versions", "newest"),
    [
        (["9.27.0", "9.27.0.dev1"], "9.27.0"),
        (["9.27.0", "9.27.0rc1"], "9.27.0"),
        (["9.27.0rc2", "9.27.0rc10", "9.27.0a5"], "9.27.0rc10"),
        (["9.27.0b1", "9.27.0.dev3"], "9.27.0b1"),
        (["9.27.0a5", "9.27.0rc2", "9.27.0b9"], "9.27.0rc2"),
        (["9.27.0", "9.27.0.post1"], "9.27.0.post1"),
        (["9.27", "9.27.1", "9.3.0"], "9.27.1"),
    ],
)
def test_probe_orders_pre_and_post_releases_as_pep_440_does(
    versions: list[str], newest: str
) -> None:
    site = "usr/lib/python3/site-packages"
    listing = [f"{site}/snakemake-{v}.dist-info/" for v in versions]
    assert f",snakemake={newest}," in _probe(listing)


def test_probe_tells_musl_and_no_libc() -> None:
    assert _probe(["lib/ld-musl-x86_64.so.1", "bin/busybox"]).startswith("bash=0,libc=musl,")
    assert _probe(["bin/busybox"]).startswith("bash=0,libc=none,")


def test_probe_honors_the_prefix() -> None:
    # The newer Snakemake is outside the prefix: only a probe that applies it reports the older.
    listing = [
        "opt/biffix/sm/lib/python3.12/site-packages/snakemake-9.27.0.dist-info/",
        "usr/lib/python3/site-packages/snakemake-9.30.0.dist-info/",
    ]
    assert ",snakemake=9.27.0," in _probe(listing, "opt/biffix/sm/")
    assert ",snakemake=9.30.0," in _probe(listing)


@pytest.mark.parametrize(
    "name",
    [
        "snakemake_logger_plugin_biffix-1$(touch${IFS}PWNED).dist-info",
        "snakemake-1.0;touch${IFS}PWNED;x.dist-info",
        "snakemake-1.0 x.dist-info",
        "snakemake-1`id`.dist-info",
    ],
)
def test_probe_never_reports_a_version_outside_pep_440(name: str) -> None:
    # A file name in the image is not trusted into the outputs.
    caps = _probe([f"usr/lib/python3/site-packages/{name}/METADATA"])
    # Not sanitized into a version (`snakemake-1.0 x` must not become `1.0x`): no version at all.
    assert caps == "bash=0,libc=none,snakemake=,storage_s3=,logger=,executor=", caps


# ── the outputs file: written by emit, read (never sourced) by the buildspec ──


def test_emit_writes_plain_values_and_refuses_anything_else(tmp_path: Path) -> None:
    out = tmp_path / "out.env"
    env = {"BIFFIX_OUTPUTS": str(out)}
    ok = _common(
        "emit IMAGE reg.example:5000/wf@sha256:abc\n"
        "emit CAPABILITIES bash=1,libc=glibc,snakemake=9.3.0,logger=,executor=1.0+local!1_x\n",
        env,
    )
    assert ok.returncode == 0, ok.stderr
    assert out.read_text().splitlines() == [
        "IMAGE=reg.example:5000/wf@sha256:abc",
        "CAPABILITIES=bash=1,libc=glibc,snakemake=9.3.0,logger=,executor=1.0+local!1_x",
    ]
    for value in ["a$(id)", "a;b", "a b", "a\nFORGED=1", "a'b", "a`b`"]:
        bad = _common('emit ARCHS "$V"', {**env, "V": value})
        assert bad.returncode != 0, value
        assert "unexpected character" in bad.stderr
    bad_key = _common("emit 'bad key' x", env)
    assert bad_key.returncode != 0 and "invalid output name" in bad_key.stderr
    assert out.read_text().count("\n") == 2


def _export_command() -> str:
    phases = inline_buildspec("true", IMAGE_IMPORT_OUTPUTS)["phases"]
    assert isinstance(phases, dict), phases
    commands = phases["build"]["commands"]
    assert isinstance(commands, list) and all(isinstance(c, str) for c in commands), commands
    assert not any(". /tmp/biffix-build.env" in c for c in commands), commands
    return str(commands[2])


def _read_outputs(tmp_path: Path, text: str) -> subprocess.CompletedProcess[str]:
    # The buildspec command reads /tmp/biffix-build.env; run it against a test file instead.
    env_file = tmp_path / "build.env"
    env_file.write_text(text)
    command = _export_command().replace("/tmp/biffix-build.env", str(env_file))
    return _bash(
        f"cd {tmp_path}\n{command}\nstatus=$?\n"
        'printf "%s\\n" "CAPABILITIES=${CAPABILITIES-unset}" "ARCHS=${ARCHS-unset}" '
        '"PATH_OK=$([ "$PATH" = "$ORIG_PATH" ] && echo 1)"\nexit $status',
        {"ORIG_PATH": os.environ["PATH"]},
    )


def test_the_buildspec_exports_output_values_literally(tmp_path: Path) -> None:
    got = _read_outputs(
        tmp_path,
        "CAPABILITIES=bash=1,logger=1$(touch${IFS}PWNED)\nARCHS=amd64;touch PWNED2\n\n",
    )
    assert got.returncode == 0, got.stderr
    assert "CAPABILITIES=bash=1,logger=1$(touch${IFS}PWNED)" in got.stdout
    assert "ARCHS=amd64;touch PWNED2" in got.stdout
    assert not (tmp_path / "PWNED").exists() and not (tmp_path / "PWNED2").exists()


@pytest.mark.parametrize("line", ["PATH=/nowhere", "BASH_ENV=/tmp/x", "NOEQUALS", "=x"])
def test_the_buildspec_refuses_an_undeclared_or_malformed_output(tmp_path: Path, line: str) -> None:
    got = _read_outputs(tmp_path, f"ARCHS=amd64\n{line}\n")
    assert got.returncode != 0, got.stdout
    assert "PATH_OK=1" in got.stdout


# ── archive ──

_FAKE_AWS = """#!/usr/bin/env bash
# `aws s3 ls|cp` over a directory ($FAKE_S3 holds s3://<bucket>/<key> as <bucket>/<key>).
printf '%s\\n' "$*" >> "$FAKE_AWS_LOG"
[ "$1" = s3 ] || exit 2
shift
case "$1" in
  ls)
    path="$FAKE_S3/${2#s3://}"
    if [[ "$2" == */ ]]; then
      [ -d "$path" ] || exit 1
      for f in "$path"*; do echo "2026-01-01 00:00:00 1 $(basename "$f")"; done
    else
      [ -e "$path" ] || exit 1
      echo "2026-01-01 00:00:00 1 $(basename "$path")"
    fi ;;
  cp)
    [ "$2" = --quiet ] && shift
    dest="$FAKE_S3/${3#s3://}"
    mkdir -p "$(dirname "$dest")" && cp "$2" "$dest" ;;
  *) exit 2 ;;
esac
"""

_FAKE_PULL = """#!/usr/bin/env bash
# `crane pull --format=oci <image> <layout>`: a two-blob layout.
[ "$1" = pull ] || exit 2
layout=$4
mkdir -p "$layout/blobs/sha256"
printf a > "$layout/blobs/sha256/aaaa"
printf bb > "$layout/blobs/sha256/bbbb"
echo '{"imageLayoutVersion":"1.0.0"}' > "$layout/oci-layout"
echo '{"manifests":[]}' > "$layout/index.json"
"""


def test_archive_lists_the_blobs_once_uploads_the_new_ones_and_marks_last(tmp_path: Path) -> None:
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    for name, text in {"aws": _FAKE_AWS, "fake-pull": _FAKE_PULL}.items():
        (bin_dir / name).write_text(text)
        (bin_dir / name).chmod(0o755)
    s3 = tmp_path / "s3"
    (s3 / "arch/blobs/sha256").mkdir(parents=True)
    (s3 / "arch/blobs/sha256/aaaa").write_text("a")
    log = tmp_path / "aws.log"
    work = tmp_path / "work"
    work.mkdir()
    env = {
        "PATH": f"{bin_dir}:{os.environ['PATH']}",
        "FAKE_S3": str(s3),
        "FAKE_AWS_LOG": str(log),
    }
    got = _common(
        f'CRANE=("{bin_dir}/fake-pull")\narchive "{REPO}@sha256:1234" s3://arch/ "{work}"', env
    )
    assert got.returncode == 0, got.stderr
    assert got.stdout.strip() == "2"  # only bbbb (2 bytes) was new
    calls = log.read_text().splitlines()
    assert sum(c.startswith("s3 ls s3://arch/blobs/") for c in calls) == 1, calls
    copies = [c.split()[-1] for c in calls if c.startswith("s3 cp")]
    assert copies == [
        "s3://arch/blobs/sha256/bbbb",
        "s3://arch/images/sha256:1234/oci-layout",
        "s3://arch/images/sha256:1234/index.json",
    ]
    # Archived now: a second archive does nothing.
    again = _common(
        f'CRANE=("{bin_dir}/fake-pull")\narchive "{REPO}@sha256:1234" s3://arch/ "{work}"', env
    )
    assert again.stdout.strip() == "0"


# ── import.sh derive / index, against a registry with write-once tags ──


class Registry:
    def __init__(self, path: Path) -> None:
        self.path = path

    @property
    def state(self) -> State:
        return fake_crane.load(str(self.path)) if self.path.exists() else {}

    def crane(self, *args: str) -> str:
        got = subprocess.run(
            [sys.executable, str(FAKE_CRANE), *args],
            env={**os.environ, "FAKE_REGISTRY": str(self.path), "FAKE_BUILDS_REPO": BUILDS},
            capture_output=True,
            text=True,
            check=True,
        )
        return got.stdout.strip()

    def put(self, obj: Obj, tag: str | None = None, repo: str = REPO) -> str:
        """Store `obj` as the fake does (and tag it in `repo`, as a seed: not a logged write)."""
        state = fake_crane.load(str(self.path))
        digest = fake_crane.put(state, obj)
        if tag:
            state["tags"][f"{repo}:{tag}"] = digest
        fake_crane.save(state, str(self.path))
        return digest

    def files(self, layer: str, names: list[str]) -> None:
        """The files `crane export` lists for the layer `layer`."""
        state = fake_crane.load(str(self.path))
        state.setdefault("layer_files", {})[layer] = names
        fake_crane.save(state, str(self.path))


RUNTIME_FILES = [
    "opt/biffix/sm/lib/python3.12/site-packages/snakemake-9.27.0.dist-info/",
    "opt/biffix/shim/python",
    "opt/biffix/shim-exec",
]


def _img(arch: str, layers: list[str], entrypoint: list[str] | None = None) -> Obj:
    return {
        "kind": "image",
        "arch": arch,
        "layers": layers,
        "config": {"Entrypoint": entrypoint or ["/entry"], "Cmd": ["run"]},
    }


@pytest.fixture
def registry(tmp_path: Path) -> Seeded:
    reg = Registry(tmp_path / "registry.json")
    amd = reg.put(_img("amd64", ["sha256:base-amd"]))
    arm = reg.put(_img("arm64", ["sha256:base-arm"]))
    base = reg.put(
        {
            "kind": "index",
            "manifests": [
                {"digest": amd, "platform": {"os": "linux", "architecture": "amd64"}},
                {"digest": arm, "platform": {"os": "linux", "architecture": "arm64"}},
            ],
        }
    )
    layer_amd = reg.put(_img("amd64", ["sha256:rt-amd"]))
    layer_arm = reg.put(_img("arm64", ["sha256:rt-arm"]))
    # What each layer holds, for `crane export`: the base's own (newer) Snakemake is outside the
    # runtime's prefix.
    for arch, libc in (("amd", "x86_64"), ("arm", "aarch64")):
        reg.files(
            f"sha256:base-{arch}",
            [
                "bin/bash",
                f"lib/{libc}-linux-gnu/libc.so.6",
                "usr/lib/python3/site-packages/snakemake-9.30.0.dist-info/",
            ],
        )
        reg.files(f"sha256:rt-{arch}", RUNTIME_FILES)
    return reg, {
        "base": base,
        "amd": amd,
        "arm": arm,
        "layer_amd": layer_amd,
        "layer_arm": layer_arm,
    }


def _import_step(
    tmp_path: Path, reg: Registry, fn: str, repo: str = REPO, **env: str
) -> subprocess.CompletedProcess[str]:
    work = tmp_path / "work"
    work.mkdir(exist_ok=True)
    script = (
        f'set -euo pipefail\n. "{IMPORT}"\n'
        f'CRANE=("{sys.executable}" "{FAKE_CRANE}")\n'
        f'REPO={repo}\nWORK="{work}"\n{fn}\necho "DIGEST=$DIGEST"'
    )
    return _bash(
        script,
        {
            "FAKE_REGISTRY": str(reg.path),
            "BIFFIX_OUTPUTS": str(tmp_path / "out.env"),
            "BIFFIX_BUILD_SUBJECT": "image:test",
            "BIFFIX_SKIP_ECR_LOGIN": "1",
            **env,
        },
    )


def _derive_env(ids: dict[str, str]) -> dict[str, str]:
    return dict(
        BIFFIX_IMAGE_BASE=f"{REPO}@{ids['base']}",
        BIFFIX_RUNTIME_LAYER_AMD64=f"{REPO}@{ids['layer_amd']}",
        BIFFIX_RUNTIME_LAYER_ARM64=f"{REPO}@{ids['layer_arm']}",
        BIFFIX_DERIVED_KEY="0123456789abcdef",
        BIFFIX_SNAKEMAKE_VERSION="9.27.0",
    )


def _derive(
    tmp_path: Path, reg: Registry, ids: RegistryIds, **extra: str
) -> subprocess.CompletedProcess[str]:
    return _import_step(tmp_path, reg, "derive_image", **{**_derive_env(ids), **extra})


def _digest_of(got: subprocess.CompletedProcess[str]) -> str:
    assert got.returncode == 0, got.stderr
    return got.stdout.strip().rsplit("DIGEST=", 1)[1]


def test_derive_writes_each_immutable_tag_once_and_reuses_what_checks_out(
    tmp_path: Path, registry: Seeded
) -> None:
    reg, ids = registry
    digest = _digest_of(_derive(tmp_path, reg, ids))
    state = reg.state
    assert state["tags"][f"{REPO}:derived-0123456789ab"] == digest
    immutable = [w for w in state["writes"] if w.split(":", 1)[1].startswith("derived-")]
    assert sorted(immutable) == sorted(
        f"{REPO}:derived-0123456789ab{s}" for s in ("", "-amd64", "-arm64")
    )
    # Each architecture is its base image with the runtime layer on top, ending with shim-exec.
    for arch, layer in (("amd64", "sha256:rt-amd"), ("arm64", "sha256:rt-arm")):
        child = json.loads(reg.crane("manifest", f"{REPO}:derived-0123456789ab-{arch}"))
        assert [layer_["digest"] for layer_ in child["layers"]] == [
            f"sha256:base-{arch[:3]}",
            layer,
        ]
        config = json.loads(reg.crane("config", f"{REPO}:derived-0123456789ab-{arch}"))
        assert config["config"]["Entrypoint"] == ["/entry", "/opt/biffix/shim-exec"]
        assert config["config"]["Cmd"] == ["run"]
    # A retry reuses it (and writes no immutable tag again: the fake would refuse).
    again = _derive(tmp_path, reg, ids)
    assert _digest_of(again) == digest
    assert "reusing it" in again.stderr


def _index(*children: tuple[str, str]) -> Obj:
    return {
        "kind": "index",
        "manifests": [
            {"digest": d, "platform": {"os": "linux", "architecture": arch}} for d, arch in children
        ],
    }


def test_derive_refuses_a_derived_tag_that_is_not_the_base_plus_the_runtime(
    tmp_path: Path, registry: Seeded
) -> None:
    reg, ids = registry
    # One image per planned architecture (so the count matches), but not the runtime's layer: only
    # the layer comparison can refuse it.
    evil = [
        (reg.put(_img(arch, [f"sha256:base-{arch[:3]}", "sha256:evil"])), arch)
        for arch in ("amd64", "arm64")
    ]
    bogus = reg.put(_index(*evil), tag="derived-0123456789ab")
    got = _derive(tmp_path, reg, ids)
    assert got.returncode != 0
    assert f"names {bogus}, which is not the base with the runtime layer on top" in got.stderr


@pytest.mark.parametrize(
    "entrypoint",
    [["/evil", "/opt/biffix/shim-exec"], ["/entry"], ["/entry", "/opt/biffix/shim-exec", "x"]],
)
def test_derive_refuses_a_derived_tag_whose_entrypoint_is_not_the_rewrite(
    tmp_path: Path, registry: Seeded, entrypoint: list[str]
) -> None:
    # The right layers, but not the base's entrypoint ending with shim-exec: only the config
    # comparison can refuse it.
    reg, ids = registry
    planted = [
        (
            reg.put(_img(arch, [f"sha256:base-{arch[:3]}", f"sha256:rt-{arch[:3]}"], entrypoint)),
            arch,
        )
        for arch in ("amd64", "arm64")
    ]
    bogus = reg.put(_index(*planted), tag="derived-0123456789ab")
    got = _derive(tmp_path, reg, ids)
    assert got.returncode != 0
    assert f"names {bogus}, which is not the base with the runtime layer on top" in got.stderr


def test_index_joins_once_and_refuses_a_tag_naming_other_images(
    tmp_path: Path, registry: Seeded
) -> None:
    reg, ids = registry
    manifests = f"{BUILDS}@{ids['amd']},{BUILDS}@{ids['arm']}"
    step = dict(BIFFIX_INDEX_MANIFESTS=manifests, BIFFIX_CONTENT_KEY="fedcba9876543210")
    digest = _digest_of(_import_step(tmp_path, reg, "index_image", repo=BUILDS, **step))
    index = json.loads(reg.crane("manifest", f"{BUILDS}:build-fedcba987654"))
    assert sorted(m["digest"] for m in index["manifests"]) == sorted([ids["amd"], ids["arm"]])
    again = _import_step(tmp_path, reg, "index_image", repo=BUILDS, **step)
    assert _digest_of(again) == digest
    # A build tag over as many other images is not reused: only the membership check can refuse
    # it.
    other = dict(step, BIFFIX_CONTENT_KEY="999999999999ffff")
    reg.put(
        _index((ids["layer_amd"], "amd64"), (ids["layer_arm"], "arm64")),
        tag="build-999999999999",
        repo=BUILDS,
    )
    got = _import_step(tmp_path, reg, "index_image", repo=BUILDS, **other)
    assert got.returncode != 0 and "does not join exactly" in got.stderr
    # One image is tagged as it is.
    single = dict(
        BIFFIX_INDEX_MANIFESTS=f"{BUILDS}@{ids['amd']}", BIFFIX_CONTENT_KEY="aaaa0000aaaa0000"
    )
    tagged = _import_step(tmp_path, reg, "index_image", repo=BUILDS, **single)
    assert _digest_of(tagged) == ids["amd"]
    # A manifest outside the repository is refused: another registry's, or the workflow
    # repository's (which anyone holding its push could have written).
    for outside in (f"other.example/x@{ids['amd']}", f"{REPO}@{ids['amd']}"):
        foreign = dict(BIFFIX_INDEX_MANIFESTS=outside, BIFFIX_CONTENT_KEY="b" * 16)
        refused = _import_step(tmp_path, reg, "index_image", repo=BUILDS, **foreign)
        assert refused.returncode != 0 and "not an image in" in refused.stderr, outside


def test_keep_tag_is_write_once(tmp_path: Path, registry: Seeded) -> None:
    reg, ids = registry
    platforms = tmp_path / "platforms.txt"
    platforms.write_text(f"{ids['amd']} linux/amd64\n{ids['arm']} linux/arm64\n")
    body = f'CRANE=("{sys.executable}" "{FAKE_CRANE}")\nkeep_tag {REPO} {ids["base"]} "{platforms}"'
    env = {"FAKE_REGISTRY": str(reg.path)}
    first = _common(body, env)
    assert first.returncode == 0, first.stderr
    tag = first.stdout.strip()
    assert tag == f"biffix-keep-{ids['base'][7:19]}"
    assert _common(body, env).returncode == 0  # the tags already name these digests
    assert reg.state["tags"][f"{REPO}:{tag}-2"] == ids["arm"]
    # A keep tag naming something else is an error, never moved.
    moved = _common(
        f'CRANE=("{sys.executable}" "{FAKE_CRANE}")\ntag_once {REPO} {ids["amd"]} {tag}', env
    )
    assert moved.returncode != 0 and "already names" in moved.stderr


def test_platforms_lists_each_platform_image_and_skips_attestations(registry: Seeded) -> None:
    reg, ids = registry
    attested = reg.put(
        {
            "kind": "index",
            "manifests": [
                {"digest": ids["amd"], "platform": {"os": "linux", "architecture": "amd64"}},
                {"digest": "sha256:att", "platform": {"os": "unknown", "architecture": "unknown"}},
                {
                    "digest": ids["arm"],
                    "platform": {"os": "linux", "architecture": "arm64", "variant": "v8"},
                },
            ],
        }
    )
    body = f'CRANE=("{sys.executable}" "{FAKE_CRANE}")\n'
    env = {"FAKE_REGISTRY": str(reg.path)}
    got = _common(body + f"platforms {REPO}@{attested}", env)
    assert got.returncode == 0, got.stderr
    assert got.stdout.splitlines() == [f"{ids['amd']} linux/amd64", f"{ids['arm']} linux/arm64/v8"]
    single = _common(body + f"platforms {REPO}@{ids['amd']}", env)
    assert single.stdout.splitlines() == [f"{ids['amd']} "]


def test_restore_keep_tags_are_the_ones_the_import_writes(tmp_path: Path, registry: Seeded) -> None:
    """biffix_infra.image_restore.keep_tags numbers an index's platform images as common.sh
    `platforms` + `keep_tag` do (an attestation and a platform-less entry are skipped), so a restore
    and a later import of the same image agree on its tags."""
    reg, ids = registry
    index = _index((ids["amd"], "amd64"))
    index["manifests"] += [
        {"digest": "sha256:att", "platform": {"os": "unknown", "architecture": "unknown"}},
        {"digest": ids["arm"], "platform": {"os": "linux", "architecture": "arm64"}},
        {"digest": ids["layer_amd"]},
    ]
    digest = reg.put(index)
    body = f'CRANE=("{sys.executable}" "{FAKE_CRANE}")\n'
    env = {"FAKE_REGISTRY": str(reg.path)}
    platforms = tmp_path / "platforms.txt"
    listed = _common(body + f'platforms {REPO}@{digest} >"{platforms}"', env)
    assert listed.returncode == 0, listed.stderr
    got = _common(body + f'keep_tag {REPO} {digest} "{platforms}"', env)
    assert got.returncode == 0, got.stderr
    written = {
        (d, key.split(":", 1)[1])
        for key, d in reg.state["tags"].items()
        if key.split(":", 1)[1].startswith("biffix-keep-")
    }
    manifest = json.loads(reg.crane("manifest", f"{REPO}@{digest}"))
    assert set(keep_tags(digest, manifest)) == written
    assert len(written) == 3


# ── import.sh main, end to end (crane stubbed by the fake, archive stubbed) ──


def _main(
    tmp_path: Path, reg: Registry, **env: str
) -> tuple[subprocess.CompletedProcess[str], list[list[str]]]:
    outputs = tmp_path / "main.env"
    script = (
        f'set -euo pipefail\n. "{IMPORT}"\n'
        f'install_crane() {{ CRANE=("{sys.executable}" "{FAKE_CRANE}"); }}\n'
        "archive() { echo 7; }\nmain"
    )
    got = _bash(
        script,
        {
            "TMPDIR": str(tmp_path),
            "FAKE_REGISTRY": str(reg.path),
            "BIFFIX_OUTPUTS": str(outputs),
            "BIFFIX_BUILD_SUBJECT": "image:test",
            "BIFFIX_IMAGE_REPO": REPO,
            "BIFFIX_BUILDS_REPO": BUILDS,
            "BIFFIX_ARCHIVE_URI": "s3://arch/",
            "BIFFIX_SKIP_ECR_LOGIN": "1",
            **env,
        },
    )
    lines = outputs.read_text().splitlines() if outputs.exists() else []
    return got, [line.split("=", 1) for line in lines]


def _source_env(ids: dict[str, str], **extra: str) -> dict[str, str]:
    return {
        "BIFFIX_IMAGE_SOURCE": f"src.example/tool@{ids['base']}",
        "BIFFIX_CONTENT_KEY": "c0ffee0123456789",
        **extra,
    }


def test_import_copies_the_pinned_source_and_emits_its_outputs_in_order(
    tmp_path: Path, registry: Seeded
) -> None:
    reg, ids = registry
    got, outputs = _main(tmp_path, reg, **_source_env(ids))
    assert got.returncode == 0, got.stderr
    keep = f"biffix-keep-{ids['base'][7:19]}"
    assert outputs == [
        ["SOURCE_DIGEST", ids["base"]],
        ["IMAGE", f"{REPO}@{ids['base']}"],
        ["IMAGE_DIGEST", ids["base"]],
        ["ARCHS", "amd64,arm64"],
        # No prefix on an import: the image's own Snakemake.
        ["CAPABILITIES", "bash=1,libc=glibc,snakemake=9.30.0,storage_s3=,logger=,executor="],
        ["KEEP_TAG", keep],
        ["ARCHIVE_BYTES", "7"],
        ["ARCHIVED", "1"],
    ]
    tags = reg.state["tags"]
    assert tags[f"{REPO}:import-c0ffee012345"] == ids["base"]
    assert tags[f"{REPO}:{keep}-1"] == ids["amd"] and tags[f"{REPO}:{keep}-2"] == ids["arm"]


def test_import_needs_the_builds_repository(tmp_path: Path, registry: Seeded) -> None:
    reg, ids = registry
    got, outputs = _main(tmp_path, reg, BIFFIX_BUILDS_REPO="", **_source_env(ids))
    assert got.returncode != 0 and "BIFFIX_BUILDS_REPO" in got.stderr
    assert outputs == [] and not reg.state.get("writes")


def test_index_main_writes_the_builds_repository(tmp_path: Path, registry: Seeded) -> None:
    reg, ids = registry
    step = dict(
        BIFFIX_IMAGE_STEP="index",
        BIFFIX_INDEX_MANIFESTS=f"{BUILDS}@{ids['amd']},{BUILDS}@{ids['arm']}",
        BIFFIX_CONTENT_KEY="fedcba9876543210",
    )
    got, outputs = _main(tmp_path, reg, **step)
    assert got.returncode == 0, got.stderr
    values = dict(outputs)
    assert values["IMAGE"].startswith(f"{BUILDS}@sha256:"), values
    written = reg.state["writes"]
    assert written and all(w.startswith(f"{BUILDS}:") for w in written), written
    assert f"{BUILDS}:build-fedcba987654" in written
    assert f"{BUILDS}:{values['KEEP_TAG']}" in written
    # The architectures a build pushed into the workflow repository are not joined.
    stale = dict(step, BIFFIX_INDEX_MANIFESTS=f"{REPO}@{ids['amd']},{REPO}@{ids['arm']}")
    refused, _ = _main(tmp_path, reg, **{**stale, "BIFFIX_CONTENT_KEY": "1" * 16})
    assert refused.returncode != 0 and f"not an image in {BUILDS}" in refused.stderr


def test_a_derive_is_pushed_next_to_its_base(tmp_path: Path, registry: Seeded) -> None:
    reg, ids = registry
    # An imported base: the workflow repository.
    got, outputs = _main(tmp_path, reg, BIFFIX_IMAGE_STEP="derive", **_derive_env(ids))
    assert got.returncode == 0, got.stderr
    assert dict(outputs)["IMAGE"].startswith(f"{REPO}@"), outputs
    assert f"{REPO}:derived-0123456789ab" in reg.state["tags"]
    # A built base: the builds repository, and nothing is written to the workflow repository.
    before = list(reg.state["writes"])
    built = {**_derive_env(ids), "BIFFIX_IMAGE_BASE": f"{BUILDS}@{ids['base']}"}
    got, outputs = _main(tmp_path, reg, BIFFIX_IMAGE_STEP="derive", **built)
    assert got.returncode == 0, got.stderr
    assert dict(outputs)["IMAGE"].startswith(f"{BUILDS}@"), outputs
    new = reg.state["writes"][len(before) :]
    assert new and all(w.startswith(f"{BUILDS}:") for w in new), new
    # A base in neither repository is refused before anything is written.
    elsewhere = {**_derive_env(ids), "BIFFIX_IMAGE_BASE": f"other.example/x@{ids['base']}"}
    got, outputs = _main(tmp_path, reg, BIFFIX_IMAGE_STEP="derive", **elsewhere)
    assert got.returncode != 0 and "is in neither" in got.stderr
    assert outputs == [] and len(reg.state["writes"]) == len(before) + len(new)


def test_import_refuses_an_unpinned_source(tmp_path: Path, registry: Seeded) -> None:
    reg, ids = registry
    got, outputs = _main(
        tmp_path, reg, **_source_env(ids, BIFFIX_IMAGE_SOURCE="src.example/tool:latest")
    )
    assert got.returncode != 0
    assert "BIFFIX_IMAGE_SOURCE is not pinned: src.example/tool:latest" in got.stderr
    assert outputs == [] and reg.state["writes"] == []


def test_import_refuses_a_copy_whose_digest_changed(tmp_path: Path, registry: Seeded) -> None:
    reg, ids = registry
    got, outputs = _main(tmp_path, reg, **_source_env(ids, FAKE_CRANE_COPY_CHANGES_DIGEST="1"))
    assert got.returncode != 0
    assert f"differs from the source's {ids['base']}" in got.stderr
    assert outputs == []
    assert not [w for w in reg.state["writes"] if "biffix-keep-" in w]


def test_derive_main_probes_the_runtime_under_its_prefix(tmp_path: Path, registry: Seeded) -> None:
    reg, ids = registry
    got, outputs = _main(tmp_path, reg, BIFFIX_IMAGE_STEP="derive", **_derive_env(ids))
    assert got.returncode == 0, got.stderr
    derived = reg.state["tags"][f"{REPO}:derived-0123456789ab"]
    assert [k for k, _ in outputs] == [
        "BASE_DIGEST",
        "IMAGE",
        "IMAGE_DIGEST",
        "ARCHS",
        "CAPABILITIES",
        "KEEP_TAG",
        "ARCHIVE_BYTES",
        "ARCHIVED",
    ]
    values = dict(outputs)
    assert values["BASE_DIGEST"] == ids["base"] and values["IMAGE_DIGEST"] == derived
    assert values["ARCHS"] == "amd64,arm64"
    assert values["CAPABILITIES"] == (
        "bash=1,libc=glibc,snakemake=9.27.0,storage_s3=,logger=,executor="
    )


def test_derive_main_refuses_a_runtime_of_another_snakemake(
    tmp_path: Path, registry: Seeded
) -> None:
    reg, ids = registry
    env = {**_derive_env(ids), "BIFFIX_SNAKEMAKE_VERSION": "9.28.0"}
    got, outputs = _main(tmp_path, reg, BIFFIX_IMAGE_STEP="derive", **env)
    assert got.returncode != 0
    assert "derived linux/amd64 lacks the runtime's Snakemake 9.28.0" in got.stderr
    assert "ARCHIVED" not in dict(outputs)


def test_derive_main_refuses_an_image_without_the_python_shim(
    tmp_path: Path, registry: Seeded
) -> None:
    reg, ids = registry
    reg.files("sha256:rt-amd", [f for f in RUNTIME_FILES if f != "opt/biffix/shim/python"])
    got, outputs = _main(tmp_path, reg, BIFFIX_IMAGE_STEP="derive", **_derive_env(ids))
    assert got.returncode != 0
    assert "derived linux/amd64 lacks /opt/biffix/shim/python" in got.stderr
    assert "ARCHIVED" not in dict(outputs)


def test_derive_main_refuses_an_entrypoint_that_does_not_end_with_shim_exec(
    tmp_path: Path, registry: Seeded
) -> None:
    reg, ids = registry
    # A derived tag already pushed, with the right layers but the image's own entrypoint.
    children = [
        (reg.put(_img(arch, [f"sha256:base-{arch[:3]}", f"sha256:rt-{arch[:3]}"])), arch)
        for arch in ("amd64", "arm64")
    ]
    bogus = reg.put(_index(*children), tag="derived-0123456789ab")
    got, outputs = _main(tmp_path, reg, BIFFIX_IMAGE_STEP="derive", **_derive_env(ids))
    # Refused before it is reused (derived_matches compares the config too); the probe's own
    # entrypoint check is the second line of defence.
    assert got.returncode != 0
    assert "reusing it" not in got.stderr
    assert f"names {bogus}, which is not the base with the runtime layer on top" in got.stderr
    assert "ARCHIVED" not in dict(outputs)


# ── runtime/build.sh check_elf_arch ──


def _function(path: Path, name: str) -> str:
    """The text of shell function `name` in `path` (the script runs as a whole, so it cannot be
    sourced for one function)."""
    text = path.read_text()
    start = text.index(f"\n{name}() {{\n") + 1
    return text[start : text.index("\n}\n", start) + 3]


def _elf(machine: int) -> bytes:
    return b"\x7fELF" + bytes(14) + struct.pack("<H", machine) + bytes(8)


def test_check_elf_arch_refuses_a_shared_object_built_for_the_other_architecture(
    tmp_path: Path,
) -> None:
    fn = _function(RUNTIME_BUILD, "check_elf_arch")
    lib = tmp_path / "sm/lib/python3.12/site-packages"
    lib.mkdir(parents=True)
    (lib / "_ext.cpython-312-x86_64-linux-gnu.so").write_bytes(_elf(0x3E))
    (lib / "libfoo.so.1").write_bytes(_elf(0x3E))
    (lib / "not-elf.so").write_text("a linker script")

    def check(arch: str) -> subprocess.CompletedProcess[str]:
        return _common(f'{fn}\ncheck_elf_arch "{tmp_path / "sm"}" {arch}')

    ok = check("x86_64")
    assert ok.returncode == 0, ok.stderr
    bad = check("aarch64")
    assert bad.returncode != 0
    assert "shared objects not built for aarch64" in bad.stderr
    assert "_ext.cpython-312-x86_64-linux-gnu.so" in bad.stderr and "libfoo.so.1" in bad.stderr
    # Every shared object is checked, not only the first.
    (lib / "_arm.so").write_bytes(_elf(0xB7))
    mixed = check("x86_64")
    assert mixed.returncode != 0 and "_arm.so" in mixed.stderr


# ── runtime/build.sh variant reuse ──


def _variant_matches(reg: Registry, ids: dict[str, str], variant: str) -> bool:
    fns = _function(RUNTIME_BUILD, "variant_image_matches") + _function(
        RUNTIME_BUILD, "variant_matches"
    )
    got = _common(
        f'CRANE=("{sys.executable}" "{FAKE_CRANE}")\nREPO={REPO}\n'
        f"BIFFIX_BASE_IMAGE={REPO}@{ids['base']}\nARCHS=(amd64 arm64)\n"
        f"declare -A LAYER_IMAGE=([amd64]={ids['layer_amd']} [arm64]={ids['layer_arm']})\n"
        f"{fns}\nif variant_matches {variant}; then echo match; else echo other; fi",
        {"FAKE_REGISTRY": str(reg.path)},
    )
    assert got.returncode == 0, got.stderr
    return got.stdout.strip() == "match"


def test_a_variant_is_reused_only_when_it_is_the_base_plus_the_runtime_layer(
    registry: Seeded,
) -> None:
    # The variant's tag is predictable and whoever may push to the coordinator repository could
    # have planted it: it is reused only when each architecture checks out, layer for layer.
    reg, ids = registry

    def variant(*images: tuple[str, list[str]]) -> str:
        return reg.put(_index(*((reg.put(_img(arch, layers)), arch) for arch, layers in images)))

    good = [
        ("amd64", ["sha256:base-amd", "sha256:rt-amd"]),
        ("arm64", ["sha256:base-arm", "sha256:rt-arm"]),
    ]
    assert _variant_matches(reg, ids, variant(*good))
    # Another base under the runtime layer.
    other_base = [("amd64", ["sha256:evil", "sha256:rt-amd"]), good[1]]
    assert not _variant_matches(reg, ids, variant(*other_base))
    # A layer slipped in between.
    extra = [("amd64", ["sha256:base-amd", "sha256:evil", "sha256:rt-amd"]), good[1]]
    assert not _variant_matches(reg, ids, variant(*extra))
    # An architecture missing, or one listed twice in its place.
    assert not _variant_matches(reg, ids, variant(good[0]))
    assert not _variant_matches(reg, ids, variant(good[0], good[0]))


# ── runtime/build.sh check_nf_awsbatch_executor ──

# What Nextflow 25.10.4 logs (DEBUG, .nextflow.log) when nf-biffix's executor takes `awsbatch`
# over nf-amazon's, then aborts in register() on the smoke's local workDir.
_NF_PRIORITY = (
    "DEBUG nextflow.executor.ExecutorFactory - Executor BiffixAwsBatchExecutor has priority"
    " - skipping class nextflow.cloud.aws.batch.AwsBatchExecutor\n"
)
_NF_ABORT = (
    "nextflow.exception.AbortOperationException: When using `awsbatch` executor an S3 bucket"
    " must be provided\n"
    "\tat nextflow.cloud.aws.batch.AwsBatchExecutor.register(AwsBatchExecutor.groovy:167)\n"
)
_NF_OURS_FRAME = (
    "\tat io.github.nh13.biffix.BiffixAwsBatchExecutor.register(BiffixAwsBatchExecutor.groovy:53)\n"
)
# The other discovery order: nf-biffix's replaces nf-amazon's.
_NF_REPLACING = (
    "DEBUG nextflow.executor.ExecutorFactory - Replacing executor AwsBatchExecutor with class"
    " io.github.nh13.biffix.BiffixAwsBatchExecutor\n"
)


@pytest.mark.parametrize("workflow_plugins", [False, True], ids=["plugins-block", "defaults"])
@pytest.mark.parametrize(
    ("log", "ok"),
    [
        (_NF_PRIORITY + _NF_ABORT + _NF_OURS_FRAME, True),
        (_NF_REPLACING + _NF_ABORT + _NF_OURS_FRAME, True),
        # Stock nf-amazon's executor (nf-biffix without its executor, or an older nf-biffix).
        (_NF_ABORT, False),
        # The name went to nf-biffix, but the executor that ran was not its.
        (_NF_PRIORITY + _NF_ABORT, False),
        # Nextflow did not even start.
        (None, False),
    ],
    ids=["priority", "replacing", "stock", "not-instantiated", "no-log"],
)
def test_the_nextflow_layer_smoke_requires_nf_biffix_s_awsbatch_executor(
    tmp_path: Path, log: str | None, ok: bool, workflow_plugins: bool
) -> None:
    fn = _function(RUNTIME_BUILD, "check_nf_awsbatch_executor")
    nf = tmp_path / "nf"
    (nf / "bin").mkdir(parents=True)
    canned = tmp_path / "canned.log"
    if log is not None:
        canned.write_text(log)
    # A stand-in launcher (`nextflow -log <file> run …`): records its configs and environment,
    # writes the canned log, and fails as the real run does.
    launcher = nf / "bin/nextflow"
    launcher.write_text(
        "#!/bin/bash\n"
        "cat awsbatch.config > seen.config; env > seen.env\n"
        "[ ! -f nextflow.config ] || cat nextflow.config > seen.pipeline.config\n"
        f'[ -f "{canned}" ] && cp "{canned}" "$2"\n'
        "exit 1\n"
    )
    launcher.chmod(0o755)
    smoke = tmp_path / "smoke"
    extra = " nf-prov@1.4.0 nf-schema@2.7.2" if workflow_plugins else ""
    got = _common(
        f'{fn}\ncheck_nf_awsbatch_executor "{nf}" "{smoke}" nf-amazon@3.4.4 nf-biffix@0.5.0{extra}'
    )
    assert (got.returncode == 0) == ok, got.stderr
    if ok:
        assert "awsbatch is nf-biffix@0.5.0's executor (over nf-amazon@3.4.4)" in got.stderr
    else:
        assert "awsbatch is not nf-biffix@0.5.0's executor in the layer" in got.stderr
    config = (smoke / "seen.config").read_text()
    assert "process.executor = 'awsbatch'" in config
    env = (smoke / "seen.env").read_text().splitlines()
    assert "NXF_OFFLINE=true" in env and f"NXF_PLUGINS_DIR={nf}/plugins" in env
    assert "BIFFIX_PROJECT=smoke" in env
    if workflow_plugins:
        # As the coordinator runs a layer with workflow plugins: biffix's are Nextflow's defaults,
        # merged with the pipeline's own `plugins {}` (its nextflow.config), none in the -c config.
        assert "plugins" not in config
        assert "NXF_PLUGINS_DEFAULT=nf-amazon@3.4.4,nf-biffix@0.5.0" in env
        assert (smoke / "seen.pipeline.config").read_text() == (
            "plugins {\n  id 'nf-prov@1.4.0'\n  id 'nf-schema@2.7.2'\n}\n"
        )
    else:
        # As before workflow plugins: a `plugins {}` block in the -c config, no defaults.
        assert "plugins { id 'nf-amazon@3.4.4'; id 'nf-biffix@0.5.0' }" in config
        assert not any(v.startswith("NXF_PLUGINS_DEFAULT=") for v in env)
        assert not (smoke / "seen.pipeline.config").exists()


# ── runtime/build.sh install_nf_workflow_plugins ──


def _install_workflow_plugins(
    tmp_path: Path, plugins: str | None
) -> subprocess.CompletedProcess[str]:
    fns = "".join(
        _function(RUNTIME_BUILD, name)
        for name in ("unzip_to", "install_nf_plugin", "install_nf_workflow_plugins")
    )
    nf = tmp_path / "nf"
    (nf / "plugins").mkdir(parents=True)
    work = tmp_path / "work"
    work.mkdir()
    # A stand-in `fetch` (common.sh's): records its arguments and writes a zip of one file.
    stub = (
        'fetch() { echo "$*" >> "$WORK/fetched"; '
        "python3 -c 'import sys, zipfile; "
        'zipfile.ZipFile(sys.argv[1], "w").writestr("x", "1")\' "$2"; }\n'
    )
    env = {"WORK": str(work)}
    if plugins is not None:
        env["BIFFIX_RT_NF_WORKFLOW_PLUGINS"] = plugins
    return _common(f'{fns}\n{stub}install_nf_workflow_plugins "{nf}"', env)


def test_the_workflow_s_plugins_are_fetched_checked_unpacked_and_marked(tmp_path: Path) -> None:
    a, b = "a" * 64, "b" * 64
    got = _install_workflow_plugins(tmp_path, f"nf-prov@1.4.0@{b} nf-schema@2.7.2@{a}")
    assert got.returncode == 0, got.stderr
    nf, work = tmp_path / "nf", tmp_path / "work"
    assert (work / "fetched").read_text().splitlines() == [
        "https://registry.nextflow.io/api/v1/plugins/nf-prov/1.4.0/download/nf-prov-1.4.0.zip"
        f" {work}/nf-prov-1.4.0.zip {b}",
        "https://registry.nextflow.io/api/v1/plugins/nf-schema/2.7.2/download/nf-schema-2.7.2.zip"
        f" {work}/nf-schema-2.7.2.zip {a}",
    ]
    assert (nf / "plugins/nf-prov-1.4.0/x").read_text() == "1"
    assert (nf / "plugins/nf-schema-2.7.2/x").read_text() == "1"
    # The marker the coordinator chooses its plugin mode by.
    assert (nf / "workflow-plugins").read_text() == "nf-prov@1.4.0\nnf-schema@2.7.2\n"


@pytest.mark.parametrize("plugins", [None, ""], ids=["unset", "empty"])
def test_a_workflow_without_plugins_leaves_no_marker(tmp_path: Path, plugins: str | None) -> None:
    got = _install_workflow_plugins(tmp_path, plugins)
    assert got.returncode == 0, got.stderr
    assert not (tmp_path / "nf/workflow-plugins").exists()
    assert not (tmp_path / "work/fetched").exists()


@pytest.mark.parametrize("entry", ["nf-schema@2.7.2", "nf-schema", "@2.7.2@" + "a" * 64])
def test_a_malformed_workflow_plugin_entry_is_refused(tmp_path: Path, entry: str) -> None:
    got = _install_workflow_plugins(tmp_path, entry)
    assert got.returncode != 0
    assert f"bad BIFFIX_RT_NF_WORKFLOW_PLUGINS entry '{entry}'" in got.stderr
    assert not (tmp_path / "nf/workflow-plugins").exists()


# ── runtime/build.sh check_nf_workflow_plugins ──


@pytest.mark.parametrize("ok", [True, False], ids=["loads", "unloadable"])
def test_the_nextflow_layer_smoke_loads_the_workflow_s_plugins_as_the_coordinator_does(
    tmp_path: Path, ok: bool
) -> None:
    fn = _function(RUNTIME_BUILD, "check_nf_workflow_plugins")
    nf = tmp_path / "nf"
    (nf / "bin").mkdir(parents=True)
    # A stand-in launcher: records its config and environment, and succeeds or fails as told.
    launcher = nf / "bin/nextflow"
    launcher.write_text(
        f"#!/bin/bash\ncat nextflow.config > seen.config; env > seen.env\nexit {0 if ok else 1}\n"
    )
    launcher.chmod(0o755)
    smoke = tmp_path / "smoke"
    got = _common(
        f"BIFFIX_RT_ENGINE_VERSION=25.10.4\n{fn}\n"
        f'check_nf_workflow_plugins "{nf}" "{smoke}" nf-amazon@3.4.4,nf-biffix@0.5.1'
        " nf-prov@1.4.0 nf-schema@2.7.2"
    )
    assert (got.returncode == 0) == ok, got.stderr
    if ok:
        assert "the workflow's plugins load offline (nf-prov@1.4.0 nf-schema@2.7.2)" in got.stderr
    else:
        assert "cannot load the workflow's plugins (nf-prov@1.4.0 nf-schema@2.7.2)" in got.stderr
    # The pipeline's own list in its config; biffix's as Nextflow's defaults, as the coordinator.
    config = (smoke / "seen.config").read_text()
    assert config == "plugins {\n  id 'nf-prov@1.4.0'\n  id 'nf-schema@2.7.2'\n}\n"
    env = (smoke / "seen.env").read_text().splitlines()
    assert "NXF_PLUGINS_DEFAULT=nf-amazon@3.4.4,nf-biffix@0.5.1" in env
    assert "NXF_OFFLINE=true" in env and f"NXF_PLUGINS_DIR={nf}/plugins" in env
