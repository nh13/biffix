"""`biffix_infra.release`: fetching a published release (download, SHA256SUMS, layout), importing
its coordinator image, the CDK app reading a fetched release, and the release workflow building what
the fetch expects. Releases are served from file:// directories and crane is a stand-in, so nothing
here touches the network or a registry."""

from __future__ import annotations

import json
import re
from pathlib import Path

import pytest
from aws_cdk import aws_lambda as lambda_

from biffix_infra import lambda_artifacts, paths, release
from biffix_infra.paths import INFRA_DIR, REPO_ROOT
from biffix_infra.release import (
    LAMBDA_BINS,
    CoordinatorImage,
    Manifest,
    ReleaseError,
    fetch,
    import_image,
    parse_sums,
)
from tests.helpers import executable
from tests.release_fixture import (
    COMMIT,
    DIGEST,
    IMAGE,
    TAG,
    manifest_of,
    publish,
    ui_tarball,
    zipped,
)


@pytest.fixture
def base(tmp_path: Path) -> Path:
    return tmp_path / "releases"


def _fetch(base: Path, dest: Path, tag: str = TAG) -> Manifest:
    return fetch(tag, dest, base_url=base.as_uri(), expected_tag=TAG)


# ── fetch ─────────────────────────────────────────────────────────────────────────


def test_fetch_lays_the_release_out_for_the_cdk_app(base: Path, tmp_path: Path) -> None:
    publish(base)
    dest = tmp_path / "rel"
    manifest = _fetch(base, dest)
    assert manifest["commit"] == COMMIT
    assert sorted(p.name for p in (dest / "lambda").iterdir()) == sorted(
        f"{b}.zip" for b in LAMBDA_BINS
    )
    assert (dest / "ui/index.html").read_text() == "<!doctype html>"
    assert (dest / "ui/_app/app.js").is_file()
    assert release.load_verified(dest) == manifest
    # Only the layout stays: no downloads, no partial directory.
    assert sorted(p.name for p in dest.iterdir()) == ["lambda", "release.json", "ui"]
    assert not (tmp_path / "rel.partial").exists()


def test_a_fetched_manifest_without_a_version_says_so(tmp_path: Path) -> None:
    (tmp_path / "release.json").write_text(json.dumps({"schema": 1}))
    with pytest.raises(ReleaseError, match="has no version"):
        release.load_verified(tmp_path)


def test_fetch_replaces_an_earlier_fetch(base: Path, tmp_path: Path) -> None:
    publish(base)
    dest = tmp_path / "rel"
    dest.mkdir()
    (dest / "stale").write_text("x")
    _fetch(base, dest)
    assert not (dest / "stale").exists()


def test_fetch_refuses_a_release_that_is_not_this_checkout_s(base: Path, tmp_path: Path) -> None:
    publish(base, "v9.9.9")
    with pytest.raises(ReleaseError, match="git checkout v9.9.9"):
        _fetch(base, tmp_path / "rel", "v9.9.9")


@pytest.mark.parametrize("tag", ["0.1.0", "v1", "v1.2", "latest", "v1.2.3/../x"])
def test_fetch_refuses_what_is_not_a_release_tag(base: Path, tmp_path: Path, tag: str) -> None:
    with pytest.raises(ReleaseError, match="not a release tag"):
        _fetch(base, tmp_path / "rel", tag)


def test_a_corrupt_asset_fails_and_keeps_the_previous_fetch(base: Path, tmp_path: Path) -> None:
    out = publish(base)
    dest = tmp_path / "rel"
    _fetch(base, dest)
    before = (dest / release.MANIFEST).read_text()
    (out / release.lambda_asset("biffix-ingest-lambda")).write_bytes(b"truncated")
    with pytest.raises(ReleaseError, match="biffix-ingest-lambda-x86_64.zip: sha256 is"):
        _fetch(base, dest)
    assert (dest / release.MANIFEST).read_text() == before
    assert not (tmp_path / "rel.partial").exists()


def test_an_asset_the_sums_do_not_list_fails(base: Path, tmp_path: Path) -> None:
    publish(base, omit_from_sums=(release.UI_ASSET,))
    with pytest.raises(ReleaseError, match="lists no biffix-ui.tar.gz"):
        _fetch(base, tmp_path / "rel")


def test_a_missing_release_fails(base: Path, tmp_path: Path) -> None:
    base.mkdir()
    with pytest.raises(ReleaseError, match="could not download"):
        _fetch(base, tmp_path / "rel")


@pytest.mark.parametrize(
    ("manifest", "error"),
    [
        (manifest_of(schema=2), "not schema 1"),
        (manifest_of(version="v0.0.1"), "is for 'v0.0.1'"),
        (manifest_of(lambda_architecture="arm64"), "Lambdas are 'arm64'"),
        (manifest_of(coordinator_image=None), "names no coordinator image"),
        (
            manifest_of(coordinator_image={"image": IMAGE, "digest": "sha256:abc"}),
            "is not sha256",
        ),
        (
            manifest_of(coordinator_image={"image": f"{IMAGE}:latest", "digest": DIGEST}),
            "is not <registry>/<repo>",
        ),
    ],
)
def test_a_manifest_this_checkout_cannot_deploy_fails(
    base: Path, tmp_path: Path, manifest: Manifest, error: str
) -> None:
    publish(base, manifest=manifest)
    with pytest.raises(ReleaseError, match=re.escape(error)):
        _fetch(base, tmp_path / "rel")


@pytest.mark.parametrize(
    ("members", "error"),
    [
        ({"./index.html": b"x"}, "not a UI build"),
        # Climbs out of `<dest>.partial/ui` to where the assertion below looks.
        ({"../../escape.html": b"x"}, "cannot be unpacked"),
    ],
)
def test_a_ui_tarball_that_is_not_a_build_fails(
    base: Path, tmp_path: Path, members: dict[str, bytes], error: str
) -> None:
    publish(base, ui=ui_tarball(members))
    with pytest.raises(ReleaseError, match=error):
        _fetch(base, tmp_path / "rel")
    assert not (tmp_path / "escape.html").exists()


@pytest.mark.parametrize(
    ("data", "error"), [(b"not a zip", "is not a zip"), (zipped({"main": b"x"}), "no `bootstrap`")]
)
def test_a_lambda_zip_without_a_bootstrap_fails(
    base: Path, tmp_path: Path, data: bytes, error: str
) -> None:
    publish(base, lambdas={"biffix-backend-sweep": data})
    with pytest.raises(ReleaseError, match=error):
        _fetch(base, tmp_path / "rel")


def test_parse_sums_reads_sha256sum_output() -> None:
    a, b = "a" * 64, "b" * 64
    assert parse_sums(f"{a}  one.zip\n{b} *two.tar.gz\n\n") == {"one.zip": a, "two.tar.gz": b}


@pytest.mark.parametrize(
    ("text", "error"),
    [
        ("abc  x.zip\n", "line 1"),
        (f"{'a' * 64}  ../x.zip\n", "line 1"),
        (f"{'a' * 64}  x.zip\n{'b' * 64}  x.zip\n", "names x.zip twice"),
    ],
)
def test_parse_sums_refuses_what_it_cannot_check(text: str, error: str) -> None:
    with pytest.raises(ReleaseError, match=error):
        parse_sums(text)


def test_the_default_source_is_the_workspace_repository(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.delenv("BIFFIX_RELEASE_BASE_URL", raising=False)
    assert release.default_base_url() == "https://github.com/nh13/biffix/releases/download"
    monkeypatch.setenv("BIFFIX_RELEASE_BASE_URL", "file:///mirror")
    assert release.default_base_url() == "file:///mirror"


# ── import-image ──────────────────────────────────────────────────────────────────

# `crane digest <ref>` answers from $CRANE_STATE (no file: no such tag); `crane copy <src> <dst>`
# records the source's digest there (or $CRANE_COPIES_AS, a registry that changed it). Every call is
# logged to $CRANE_LOG.
_CRANE = """#!/bin/bash
printf '%s\\n' "$*" >> "$CRANE_LOG"
case "$1" in
  digest) [ -f "$CRANE_STATE" ] && cat "$CRANE_STATE" ;;
  copy) [ "${CRANE_COPY_RC:-0}" = 0 ] || { echo "denied" >&2; exit 1; }
        printf '%s\\n' "${CRANE_COPIES_AS:-${2##*@}}" > "$CRANE_STATE" ;;
  *) exit 2 ;;
esac
"""

TARGET = "111122223333.dkr.ecr.us-east-1.amazonaws.com/coord:" + TAG


@pytest.fixture
def crane(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> dict[str, Path]:
    executable(tmp_path / "bin/crane", _CRANE)
    files = {"bin": tmp_path / "bin/crane", "state": tmp_path / "state", "log": tmp_path / "log"}
    monkeypatch.setenv("CRANE_STATE", str(files["state"]))
    monkeypatch.setenv("CRANE_LOG", str(files["log"]))
    files["log"].touch()
    return files


def _image() -> CoordinatorImage:
    return CoordinatorImage(image=IMAGE, digest=DIGEST)


def test_import_copies_the_release_digest_under_the_tag(crane: dict[str, Path]) -> None:
    assert import_image(_image(), TARGET, crane=str(crane["bin"])) == DIGEST
    assert crane["log"].read_text().splitlines() == [
        f"digest {TARGET}",
        f"copy {IMAGE}@{DIGEST} {TARGET}",
        f"digest {TARGET}",
    ]


def test_import_reuses_a_tag_that_already_names_the_digest(crane: dict[str, Path]) -> None:
    crane["state"].write_text(DIGEST + "\n")
    assert import_image(_image(), TARGET, crane=str(crane["bin"])) == DIGEST
    assert crane["log"].read_text().splitlines() == [f"digest {TARGET}"]


def test_import_refuses_a_tag_that_names_another_image(crane: dict[str, Path]) -> None:
    crane["state"].write_text("sha256:" + "cd" * 32 + "\n")
    with pytest.raises(ReleaseError, match="immutable"):
        import_image(_image(), TARGET, crane=str(crane["bin"]))


def test_import_refuses_a_copy_that_changed_the_digest(
    crane: dict[str, Path], monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setenv("CRANE_COPIES_AS", "sha256:" + "ef" * 32)
    with pytest.raises(ReleaseError, match="not the release's"):
        import_image(_image(), TARGET, crane=str(crane["bin"]))


def test_import_reports_a_failed_copy(
    crane: dict[str, Path], monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setenv("CRANE_COPY_RC", "1")
    with pytest.raises(ReleaseError, match="crane copy .* failed: denied"):
        import_image(_image(), TARGET, crane=str(crane["bin"]))


def test_import_needs_a_tagged_target(crane: dict[str, Path]) -> None:
    with pytest.raises(ReleaseError, match="has no tag"):
        import_image(_image(), "localhost:5000/coord", crane=str(crane["bin"]))


# ── the command line ──────────────────────────────────────────────────────────────


def test_the_cli_fetches_describes_and_imports(
    base: Path, tmp_path: Path, crane: dict[str, Path], capsys: pytest.CaptureFixture[str]
) -> None:
    publish(base)
    dest = tmp_path / "rel"
    assert (
        release.main(["fetch", "--tag", TAG, "--dest", str(dest), "--base-url", base.as_uri()]) == 0
    )
    assert f"coordinator image: {IMAGE}@{DIGEST}" in capsys.readouterr().err
    assert release.main(["describe", "--release-dir", str(dest)]) == 0
    assert capsys.readouterr().out.strip() == f"{IMAGE}@{DIGEST}"
    args = ["import-image", "--release-dir", str(dest), "--target", TARGET]
    assert release.main([*args, "--crane", str(crane["bin"])]) == 0
    assert capsys.readouterr().out.strip() == DIGEST


def test_the_cli_reports_a_release_error_without_a_traceback(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    assert release.main(["describe", "--release-dir", str(tmp_path)]) == 1
    assert "release: error:" in capsys.readouterr().err


@pytest.mark.parametrize(
    "argv",
    [[], ["fetch", "--dest", "x"], ["fetch", "--tag", TAG], ["import-image", "--target", "t"]],
)
def test_the_cli_requires_its_arguments(argv: list[str]) -> None:
    with pytest.raises(SystemExit) as e:
        release.main(argv)
    assert e.value.code == 2


# ── the CDK app reading a fetched release ────────────────────────────────────────


def test_a_release_dir_supplies_the_lambdas_and_the_ui(
    base: Path, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    publish(base)
    dest = tmp_path / "rel"
    _fetch(base, dest)
    monkeypatch.setenv("BIFFIX_RELEASE_DIR", str(dest))
    # The from-source guards are off for a release: nothing is built from this tree.
    monkeypatch.delenv("BIFFIX_ALLOW_UNBUILT_LAMBDA", raising=False)
    code = lambda_artifacts.rust_lambda_code("biffix-ingest-lambda")
    assert isinstance(code, lambda_.AssetCode)
    assert code.path == str(dest / "lambda/biffix-ingest-lambda.zip")
    assert paths.ui_build_dir() == dest / "ui"


def test_an_unverified_release_dir_is_a_hard_failure(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    (tmp_path / "lambda").mkdir()
    (tmp_path / "lambda/biffix-ingest-lambda.zip").write_bytes(zipped({"bootstrap": b"x"}))
    monkeypatch.setenv("BIFFIX_RELEASE_DIR", str(tmp_path))
    with pytest.raises(ReleaseError, match="not a fetched release"):
        lambda_artifacts.rust_lambda_code("biffix-ingest-lambda")


def test_a_release_missing_a_lambda_is_a_hard_failure(
    base: Path, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    publish(base)
    dest = tmp_path / "rel"
    _fetch(base, dest)
    (dest / "lambda/biffix-backend-sweep.zip").unlink()
    monkeypatch.setenv("BIFFIX_RELEASE_DIR", str(dest))
    with pytest.raises(FileNotFoundError, match="biffix-backend-sweep.zip"):
        lambda_artifacts.rust_lambda_code("biffix-backend-sweep")


def test_without_a_release_the_ui_is_the_tree_s_build(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.delenv("BIFFIX_RELEASE_DIR", raising=False)
    assert paths.ui_build_dir() == paths.UI_BUILD


# ── the release workflow builds what fetch reads ─────────────────────────────────

WORKFLOW = (REPO_ROOT / ".github/workflows/release.yml").read_text()
_BUILD = re.compile(
    r"cargo lambda build --release(?: --output-format zip)? (--features \S+) +--bin (\S+)"
)


def test_the_release_carries_the_lambdas_the_stacks_deploy() -> None:
    assert set(LAMBDA_BINS) == set(lambda_artifacts._LAMBDA_SOURCE_CRATES)


def test_the_workflow_builds_each_lambda_as_deploy_sh_does() -> None:
    """The same binaries with the same features, so a release deploy runs what a source deploy
    would."""
    released = {b: f for f, b in _BUILD.findall(WORKFLOW)}
    from_source = {b: f for f, b in _BUILD.findall((INFRA_DIR / "deploy.sh").read_text())}
    assert released == from_source
    assert sorted(released) == sorted(LAMBDA_BINS)


def test_the_workflow_publishes_the_assets_fetch_reads() -> None:
    assert f'"dist/${{bin}}-{release.LAMBDA_ARCHITECTURE}.zip"' in WORKFLOW
    for name in (release.UI_ASSET, release.MANIFEST, release.SUMS):
        assert f"dist/{name}" in WORKFLOW
    assert f"schema: {release.MANIFEST_SCHEMA}," in WORKFLOW
    assert f'lambda_architecture: "{release.LAMBDA_ARCHITECTURE}"' in WORKFLOW
