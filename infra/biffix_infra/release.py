"""Deploy from a published biffix release instead of building from source (`deploy.sh --release`).

A release (.github/workflows/release.yml, run by pushing a `v*` tag) publishes these GitHub
release assets, every one built from the tagged commit:

  release.json              the manifest: version, commit, the Lambdas' architecture, and the
                            coordinator image (its public registry name and the digest of its
                            linux/amd64 + linux/arm64 index)
  <bin>-x86_64.zip          each Rust Lambda's `bootstrap`, zipped by `cargo lambda build`
  biffix-ui.tar.gz          the UI's static build (index.html and _app/ at the top level)
  SHA256SUMS                `sha256sum` over every other asset

`fetch` downloads them, checks each against SHA256SUMS, and lays them out in one directory: the
Lambda zips under `lambda/<bin>.zip`, the UI under `ui/`, and the manifest at `release.json`,
written last, so its presence means the rest was verified. Exported as BIFFIX_RELEASE_DIR, that
directory replaces the from-source artifacts in the CDK app (`paths.release_dir`).

`import-image` copies the coordinator image, by its digest, into the tenant's coordinator ECR
repository under the release tag. The image must live there, not only in the public registry: the
backend resolves the deployed base's digest in that repository and the runtime builds append their
layers to it there, and image retention protects it there. The copy is `crane copy`, which keeps
the index byte for byte, so the tenant's tag names exactly the digest the release recorded (checked
after the copy; a tag that already names another digest is refused, as the repository's tags are
immutable).

SHA256SUMS comes from the same place as the assets, so it catches a corrupt or truncated download,
not a forged release; the release's build provenance (`gh attestation verify`) is what ties the
assets to the workflow run that built them (infra/README.md, "Releases").
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import tarfile
import urllib.request
import zipfile
from dataclasses import dataclass
from pathlib import Path

from biffix_infra import paths, version

# The Rust Lambdas a release carries (lambda_artifacts._LAMBDA_SOURCE_CRATES names the same four;
# a test keeps them in step), and the architecture they are built for: the Lambdas set no
# `architecture`, so they run on Lambda's default, x86_64, which is also what `cargo lambda build`
# targets by default (deploy.sh).
LAMBDA_BINS = (
    "biffix-backend-lambda",
    "biffix-backend-sweep",
    "biffix-ingest-lambda",
    "biffix-reconcile-lambda",
)
LAMBDA_ARCHITECTURE = "x86_64"
MANIFEST = "release.json"
# release.json, as read: its keys checked by `read_manifest`, the rest (e.g. `commit`) as written.
Manifest = dict[str, object]
SUMS = "SHA256SUMS"
UI_ASSET = "biffix-ui.tar.gz"
# The schema of release.json this code reads; a release with another one is refused.
MANIFEST_SCHEMA = 1

TAG = re.compile(r"^v[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.]+)?$")
DIGEST = re.compile(r"^sha256:[0-9a-f]{64}$")
# A registry repository name: <host>/<path>, lowercase (no tag, no digest).
IMAGE = re.compile(r"^[a-z0-9.-]+(:[0-9]+)?/[a-z0-9._/-]+$")
_SUM_LINE = re.compile(r"^([0-9a-f]{64}) [ *]([A-Za-z0-9._-]+)$")


class ReleaseError(Exception):
    """A release that cannot be deployed (missing, corrupt, or not this checkout's)."""


def lambda_asset(bin_name: str) -> str:
    return f"{bin_name}-{LAMBDA_ARCHITECTURE}.zip"


def required_assets() -> list[str]:
    return [MANIFEST, UI_ASSET, *(lambda_asset(b) for b in LAMBDA_BINS)]


def default_base_url() -> str:
    """Where releases are downloaded from: `$BIFFIX_RELEASE_BASE_URL` (a fork's, a mirror's, or a
    file:// directory), else the workspace repository's GitHub release downloads."""
    return os.environ.get("BIFFIX_RELEASE_BASE_URL") or (
        f"{version.repository_url()}/releases/download"
    )


def check_tag(tag: str) -> str:
    if not TAG.match(tag):
        raise ReleaseError(f"not a release tag: {tag!r} (expected vMAJOR.MINOR.PATCH)")
    return tag


def parse_sums(text: str) -> dict[str, str]:
    """`sha256sum` output as {asset: sha256}. A malformed line, or an asset named twice, is an
    error rather than skipped: a check that silently drops a line checks nothing."""
    sums: dict[str, str] = {}
    for n, line in enumerate(text.splitlines(), 1):
        if not line.strip():
            continue
        m = _SUM_LINE.match(line)
        if not m:
            raise ReleaseError(f"{SUMS} line {n} is not `<sha256>  <asset>`: {line!r}")
        digest, name = m.groups()
        if name in sums:
            raise ReleaseError(f"{SUMS} names {name} twice")
        sums[name] = digest
    return sums


def sha256(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def download(url: str, dest: Path) -> None:
    """`url` (https:// or file://) to `dest`."""
    try:
        with urllib.request.urlopen(url, timeout=120) as response, dest.open("wb") as out:
            shutil.copyfileobj(response, out)
    except OSError as e:  # URLError and HTTPError are OSErrors
        raise ReleaseError(f"could not download {url}: {e}") from e


@dataclass(frozen=True)
class CoordinatorImage:
    image: str  # <registry>/<repository>, e.g. ghcr.io/<owner>/biffix-coordinator
    digest: str  # sha256:… of the multi-arch index

    @property
    def reference(self) -> str:
        return f"{self.image}@{self.digest}"


def read_manifest(path: Path, tag: str) -> Manifest:
    """release.json, checked: this schema, this tag, the Lambdas' architecture, and a pinned
    coordinator image."""
    try:
        manifest: object = json.loads(path.read_text())
    except (OSError, ValueError) as e:
        raise ReleaseError(f"{MANIFEST} is not readable JSON: {e}") from e
    if not isinstance(manifest, dict) or manifest.get("schema") != MANIFEST_SCHEMA:
        raise ReleaseError(f"{MANIFEST} is not schema {MANIFEST_SCHEMA}")
    if manifest.get("version") != tag:
        raise ReleaseError(f"{MANIFEST} is for {manifest.get('version')!r}, not {tag}")
    if manifest.get("lambda_architecture") != LAMBDA_ARCHITECTURE:
        raise ReleaseError(
            f"the release's Lambdas are {manifest.get('lambda_architecture')!r}; this checkout "
            f"deploys {LAMBDA_ARCHITECTURE} Lambdas"
        )
    coordinator_image(manifest)
    return manifest


def coordinator_image(manifest: Manifest) -> CoordinatorImage:
    block = manifest.get("coordinator_image")
    if not isinstance(block, dict):
        raise ReleaseError(f"{MANIFEST} names no coordinator image")
    image, digest = block.get("image"), block.get("digest")
    if not isinstance(image, str) or not IMAGE.match(image):
        raise ReleaseError(f"{MANIFEST}: coordinator image {image!r} is not <registry>/<repo>")
    if not isinstance(digest, str) or not DIGEST.match(digest):
        raise ReleaseError(f"{MANIFEST}: coordinator digest {digest!r} is not sha256:<64 hex>")
    return CoordinatorImage(image=image, digest=digest)


def _unpack_ui(archive: Path, dest: Path) -> None:
    try:
        with tarfile.open(archive, "r:gz") as tar:
            # The data filter refuses absolute paths, `..`, links out of `dest` and device files.
            tar.extractall(dest, filter="data")
    except tarfile.TarError as e:
        raise ReleaseError(f"{UI_ASSET} cannot be unpacked: {e}") from e
    if not (dest / "index.html").is_file() or not (dest / "_app").is_dir():
        raise ReleaseError(f"{UI_ASSET} is not a UI build (needs index.html and _app/)")


def _check_lambda_zip(path: Path) -> None:
    try:
        with zipfile.ZipFile(path) as z:
            names = z.namelist()
    except zipfile.BadZipFile as e:
        raise ReleaseError(f"{path.name} is not a zip: {e}") from e
    if "bootstrap" not in names:
        raise ReleaseError(f"{path.name} holds no `bootstrap` (it has {names})")


def fetch(tag: str, dest: Path, *, base_url: str, expected_tag: str) -> Manifest:
    """Download release `tag` from `base_url`, verify it, and lay it out in `dest` (replacing
    what is there). Returns the manifest. `expected_tag` is this checkout's release tag: the infra
    code deploying the artifacts must be the release's own."""
    check_tag(tag)
    if tag != expected_tag:
        raise ReleaseError(
            f"this checkout is biffix {expected_tag}, not {tag}: check out the release "
            f"(`git checkout {tag}`) so the infra deploying it is the release's own"
        )
    # Laid out beside `dest` and swapped in only once complete, so a failed fetch leaves what was
    # there (and never a half-verified directory).
    work = dest.with_name(dest.name + ".partial")
    shutil.rmtree(work, ignore_errors=True)
    try:
        manifest = _fetch_into(work, tag, base_url.rstrip("/"))
    except BaseException:
        shutil.rmtree(work, ignore_errors=True)
        raise
    shutil.rmtree(dest, ignore_errors=True)
    work.rename(dest)
    return manifest


def _fetch_into(work: Path, tag: str, base: str) -> Manifest:
    downloads = work / "downloads"
    downloads.mkdir(parents=True)
    download(f"{base}/{tag}/{SUMS}", downloads / SUMS)
    sums = parse_sums((downloads / SUMS).read_text())
    missing = [name for name in required_assets() if name not in sums]
    if missing:
        raise ReleaseError(f"{SUMS} of {tag} lists no {', '.join(missing)}")
    for name in required_assets():
        path = downloads / name
        download(f"{base}/{tag}/{name}", path)
        actual = sha256(path)
        if actual != sums[name]:
            raise ReleaseError(f"{name}: sha256 is {actual}, {SUMS} says {sums[name]}")

    manifest = read_manifest(downloads / MANIFEST, tag)
    (work / "lambda").mkdir()
    for bin_name in LAMBDA_BINS:
        zipped = downloads / lambda_asset(bin_name)
        _check_lambda_zip(zipped)
        shutil.copy2(zipped, work / "lambda" / f"{bin_name}.zip")
    _unpack_ui(downloads / UI_ASSET, work / "ui")
    shutil.rmtree(downloads)
    # Last: its presence is what marks the directory verified.
    (work / MANIFEST).write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n")
    return manifest


def load_verified(release_dir: Path) -> Manifest:
    """The manifest of a directory `fetch` laid out; an error if it is not one."""
    path = release_dir / MANIFEST
    if not path.is_file():
        raise ReleaseError(
            f"{release_dir} is not a fetched release (no {MANIFEST}): run "
            f"`python -m biffix_infra.release fetch`, or deploy with `deploy.sh --release`"
        )
    try:
        tag = json.loads(path.read_text()).get("version")
    except (OSError, ValueError, AttributeError) as e:
        raise ReleaseError(f"{path} is not a release manifest: {e}") from e
    if not isinstance(tag, str) or not tag:
        raise ReleaseError(f"{path} has no version: fetch the release again")
    return read_manifest(path, tag)


def _head_commit() -> str | None:
    """The checkout's commit, or None outside a git checkout (e.g. a source tarball)."""
    try:
        out = subprocess.run(
            ["git", "-C", str(paths.REPO_ROOT), "rev-parse", "HEAD"],
            capture_output=True,
            text=True,
            check=True,
        )
    except (OSError, subprocess.CalledProcessError):
        return None
    return out.stdout.strip() or None


def _crane(crane: str, *args: str, check: bool = True) -> subprocess.CompletedProcess[str]:
    return subprocess.run([crane, *args], capture_output=True, text=True, check=check)


def import_image(image: CoordinatorImage, target: str, *, crane: str = "crane") -> str:
    """Copy `image` to `target` (`<registry>/<repo>:<tag>`) unless the tag already names it, and
    return the digest the target names, which is the release's."""
    if ":" not in target.rsplit("/", 1)[-1]:
        raise ReleaseError(f"import target {target!r} has no tag")
    existing = _crane(crane, "digest", target, check=False)
    if existing.returncode != 0:
        copied = _crane(crane, "copy", image.reference, target, check=False)
        if copied.returncode != 0:
            raise ReleaseError(f"crane copy {image.reference} {target} failed: {copied.stderr}")
        existing = _crane(crane, "digest", target, check=False)
        if existing.returncode != 0:
            raise ReleaseError(f"crane digest {target} failed after the copy: {existing.stderr}")
    digest = existing.stdout.strip()
    if digest != image.digest:
        raise ReleaseError(
            f"{target} names {digest}, not the release's {image.digest} (tags in the coordinator "
            f"repository are immutable; it was pushed by something else)"
        )
    return digest


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        prog="python -m biffix_infra.release", description=__doc__.splitlines()[0]
    )
    sub = parser.add_subparsers(dest="command", required=True)
    f = sub.add_parser("fetch", help="download and verify a release into a directory")
    f.add_argument("--tag", required=True, help="the release tag, e.g. v0.1.0")
    f.add_argument("--dest", required=True, type=Path, help="the directory to lay it out in")
    f.add_argument(
        "--base-url", default=None, help="default: $BIFFIX_RELEASE_BASE_URL, else GitHub"
    )
    i = sub.add_parser("import-image", help="copy the release's coordinator image into ECR")
    i.add_argument("--release-dir", required=True, type=Path, help="a directory `fetch` laid out")
    i.add_argument("--target", required=True, help="<registry>/<repository>:<tag>")
    i.add_argument("--crane", default="crane", help="the crane binary (default: crane on PATH)")
    d = sub.add_parser("describe", help="print a fetched release's coordinator image reference")
    d.add_argument("--release-dir", required=True, type=Path)
    args = parser.parse_args(argv)

    try:
        if args.command == "fetch":
            base = args.base_url or default_base_url()
            manifest = fetch(args.tag, args.dest, base_url=base, expected_tag=version.release_tag())
            image = coordinator_image(manifest)
            print(f"verified {args.tag} ({manifest.get('commit')}) from {base}", file=sys.stderr)
            print(f"coordinator image: {image.reference}", file=sys.stderr)
            head = _head_commit()
            if head and manifest.get("commit") and head != manifest["commit"]:
                print(
                    f"release: warning: this checkout is at {head}, not {args.tag}'s commit "
                    f"{manifest['commit']}: the infra code deploying it is not the release's own "
                    f"(`git checkout {args.tag}` to deploy exactly the release)",
                    file=sys.stderr,
                )
        elif args.command == "import-image":
            image = coordinator_image(load_verified(args.release_dir))
            print(import_image(image, args.target, crane=args.crane))
        else:
            print(coordinator_image(load_verified(args.release_dir)).reference)
    except ReleaseError as e:
        print(f"release: error: {e}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
