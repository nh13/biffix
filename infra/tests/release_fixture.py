"""A published release, laid out as .github/workflows/release.yml publishes it, for the tests
of `biffix_infra.release` and `deploy.sh --release` (served from a file:// directory)."""

from __future__ import annotations

import hashlib
import io
import json
import tarfile
import zipfile
from pathlib import Path

from biffix_infra import release
from biffix_infra.release import LAMBDA_BINS, Manifest
from biffix_infra.version import release_tag

TAG = release_tag()
IMAGE = "ghcr.io/example/biffix-coordinator"
DIGEST = "sha256:" + "ab" * 32
COMMIT = "c0ffee" * 6 + "c0ff"


def ui_tarball(members: dict[str, bytes]) -> bytes:
    buf = io.BytesIO()
    with tarfile.open(fileobj=buf, mode="w:gz") as tar:
        for name, data in members.items():
            info = tarfile.TarInfo(name)
            info.size = len(data)
            tar.addfile(info, io.BytesIO(data))
    return buf.getvalue()


def zipped(members: dict[str, bytes]) -> bytes:
    buf = io.BytesIO()
    with zipfile.ZipFile(buf, "w") as z:
        for name, data in members.items():
            z.writestr(name, data)
    return buf.getvalue()


def manifest_of(**overrides: object) -> Manifest:
    manifest: Manifest = {
        "schema": 1,
        "version": TAG,
        "commit": COMMIT,
        "lambda_architecture": "x86_64",
        "coordinator_image": {
            "image": IMAGE,
            "digest": DIGEST,
            "platforms": ["linux/amd64", "linux/arm64"],
        },
    }
    manifest.update(overrides)
    return manifest


def publish(
    base: Path,
    tag: str = TAG,
    *,
    manifest: Manifest | None = None,
    ui: bytes | None = None,
    lambdas: dict[str, bytes] | None = None,
    sums_extra: str = "",
    omit_from_sums: tuple[str, ...] = (),
) -> Path:
    """Lay out release `tag` under `base` as the release workflow publishes it."""
    out = base / tag
    out.mkdir(parents=True)
    assets: dict[str, bytes] = {
        release.MANIFEST: json.dumps(manifest or manifest_of()).encode(),
        release.UI_ASSET: ui
        if ui is not None
        else ui_tarball({"./index.html": b"<!doctype html>", "./_app/app.js": b"x"}),
    }
    for bin_name in LAMBDA_BINS:
        assets[release.lambda_asset(bin_name)] = (lambdas or {}).get(
            bin_name, zipped({"bootstrap": bin_name.encode()})
        )
    lines = []
    for name, data in assets.items():
        (out / name).write_bytes(data)
        if name not in omit_from_sums:
            lines.append(f"{hashlib.sha256(data).hexdigest()}  {name}\n")
    (out / release.SUMS).write_text("".join(lines) + sums_extra)
    return out
