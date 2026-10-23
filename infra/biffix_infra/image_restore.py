"""Restore an archived workflow image into ECR with its original digest (the archive runbook).

Every image a workflow version pins is archived byte for byte by its import build
(codebuild/image/import.sh: an import, a derive, or a built image's index) as an OCI layout in the
tenant's image archive bucket:

    images/<digest>/index.json, images/<digest>/oci-layout   the image's entry point
    blobs/sha256/<hex>                                         manifests, configs and layers

Large blobs move to Glacier Instant Retrieval (readable at once) and then Deep Archive (readable
only after a restore request, which takes hours). To bring an image back — its repository tag was
lost, the image was deleted, or the account is being rebuilt — run:

    uv run python -m biffix_infra.image_restore --tenant <tenant> --digest sha256:… \\
        --repo <account>.dkr.ecr.<region>.amazonaws.com/<repository>

with the repository the recorded image names: the workflow repository for an imported image (or
its derive), the builds repository for a built one (or its derive). The archive is the tenant's
data stack's; after a teardown and an account rebuild, that is a new, empty bucket, so add
`--bucket <the archive teardown kept>` (teardown prints its name). The layout is downloaded into
the temp directory ($TMPDIR), which needs about the image's compressed size free.

The first run requests a restore of any Deep Archive blob and exits 2 ("waiting"); run it again once
they are readable. A tiered manifest (only with a low `image_archive.min_tiered_object_bytes`) hides
what it lists until it is restored, so such an image takes one more round per level. When every
blob is readable it downloads the layout, pushes it with `crane push` (so the pushed image has
exactly the archived digest, which is checked), and keep-tags it as an import does:
`biffix-keep-<digest12>`, and `-<n>` on each platform image an index lists. Needs ops credentials
for the tenant account and `crane` on PATH.

The plan is pure; `main` gathers the archive's objects and applies it with the aws CLI and crane.
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
import tempfile
from collections import deque
from collections.abc import Callable
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

from biffix_infra.config import ARCHIVE_BLOBS_PREFIX

# An OCI index or manifest, or an `aws s3api` answer, as JSON: their shapes are the OCI spec's and
# the API's, and are read unchecked (a missing key is the archive's or AWS's error).
Json = dict[str, Any]

# Storage classes whose objects cannot be read until they are restored.
COLD = {"DEEP_ARCHIVE", "GLACIER"}
KEEP_TAG_PREFIX = "biffix-keep-"
RESTORE_DAYS = 7
# Manifests, configs and indexes are small; a blob larger than this is a layer and is never read.
MAX_METADATA_BYTES = 4 << 20


def blobs_of(index: Json, read_blob: Callable[[str], bytes]) -> list[str]:
    """Every blob digest an OCI index needs, in discovery order: its manifests (an index lists
    per-platform manifests), their configs and layers. `read_blob` returns a blob's bytes, or b""
    for one it cannot read yet (a layer, or a cold blob not yet restored), whose own blobs are then
    not discovered on this run."""
    seen: list[str] = []
    known: set[str] = set()
    todo = deque(m["digest"] for m in index.get("manifests", []))
    while todo:
        digest = todo.popleft()
        if digest in known:
            continue
        known.add(digest)
        seen.append(digest)
        try:
            doc = json.loads(read_blob(digest))
        except (ValueError, UnicodeDecodeError):
            continue  # a layer
        if not isinstance(doc, dict) or "schemaVersion" not in doc:
            continue  # a config
        todo += [m["digest"] for m in doc.get("manifests", []) if "digest" in m]
        todo += [m["digest"] for m in doc.get("layers", []) if "digest" in m]
        if isinstance(doc.get("config"), dict):
            todo.append(doc["config"]["digest"])
    return seen


@dataclass(frozen=True)
class BlobState:
    """What S3 reports for an archived blob."""

    storage_class: str  # STANDARD when absent from the response
    restore: (
        str | None
    )  # the `Restore` header: None, 'ongoing-request="true"', or 'ongoing-request="false", …'

    @classmethod
    def of(cls, head: Json) -> BlobState:
        return cls(head.get("StorageClass", "STANDARD"), head.get("Restore"))

    @property
    def readable(self) -> bool:
        """Whether the blob can be read now. A restored object keeps its cold storage class and
        only gains a finished `Restore`, so the class alone does not say."""
        return self.storage_class not in COLD or (
            self.restore is not None and 'ongoing-request="false"' in self.restore
        )


@dataclass
class RestorePlan:
    """What a run does: request restores, wait for them, or download everything."""

    request: list[str] = field(default_factory=list)
    waiting: list[str] = field(default_factory=list)

    @property
    def ready(self) -> bool:
        return not self.request and not self.waiting


def plan_restore(blobs: dict[str, BlobState]) -> RestorePlan:
    """Which blobs need a restore requested, and which are still being restored."""
    plan = RestorePlan()
    for digest, state in blobs.items():
        if state.storage_class not in COLD:
            continue
        if state.restore is None:
            plan.request.append(digest)
        elif 'ongoing-request="true"' in state.restore:
            plan.waiting.append(digest)
    return plan


def blob_key(digest: str) -> str:
    return f"{ARCHIVE_BLOBS_PREFIX}sha256/{digest.split(':', 1)[1]}"


def keep_tags(digest: str, manifest: Json) -> list[tuple[str, str]]:
    """The `(digest, tag)` keep-tags an import gives the image `digest` whose manifest is
    `manifest`: `biffix-keep-<digest12>` on it and `-<n>` on each platform image an index lists,
    numbered exactly as codebuild/lib/common.sh `platforms`/`keep_tag` do (attestations, os
    `unknown` or none, are skipped), so a later import of the same image finds the same tags."""
    tag = f"{KEEP_TAG_PREFIX}{digest.split(':', 1)[1][:12]}"
    tags = [(digest, tag)]
    n = 0
    for m in manifest.get("manifests") or []:
        if (m.get("platform") or {}).get("os") in (None, "unknown") or m["digest"] == digest:
            continue
        n += 1
        tags.append((m["digest"], f"{tag}-{n}"))
    return tags


class AwsError(SystemExit):
    """An aws CLI call failed; the message is the call and AWS's own error."""


def _aws(*args: str) -> str:
    try:
        return subprocess.run(["aws", *args], check=True, capture_output=True, text=True).stdout
    except subprocess.CalledProcessError as e:
        raise AwsError(f"aws {' '.join(args[:2])}: {(e.stderr or '').strip()}") from e


_NAME_THE_BUCKET = (
    "pass --bucket <the image archive bucket>: an archive a teardown kept outlives its data stack "
    "(teardown prints its name; it is tagged biffix:role=image-archive)"
)


def archive_bucket(tenant: str) -> str:
    """The tenant's image archive bucket: the live data stack's ImageArchiveBucket."""
    try:
        out = _aws(
            "cloudformation",
            "list-stack-resources",
            "--stack-name",
            f"biffix-{tenant}-data",
            "--query",
            "StackResourceSummaries[?ResourceType=='AWS::S3::Bucket' && "
            "starts_with(LogicalResourceId, 'ImageArchiveBucket')].PhysicalResourceId",
            "--output",
            "text",
        ).split()
    except AwsError as e:
        raise SystemExit(f"{e}\n{_NAME_THE_BUCKET}") from e
    if len(out) != 1:
        raise SystemExit(
            f"expected one image archive bucket for tenant {tenant}, found {out}; "
            f"{_NAME_THE_BUCKET}"
        )
    return out[0]


def _existing_digest(ref: str) -> str | None:
    """The digest `ref` names, or None when it names nothing."""
    out = subprocess.run(["crane", "digest", ref], capture_output=True, text=True)
    return out.stdout.strip() if out.returncode == 0 else None


def tag_once(repo: str, digest: str, tag: str) -> None:
    """Tag `repo@digest` unless the tag already names it (the keep-tags are immutable); a tag
    naming another image is an error. codebuild/lib/common.sh `tag_once`."""
    existing = _existing_digest(f"{repo}:{tag}")
    if existing is None:
        subprocess.run(["crane", "tag", f"{repo}@{digest}", tag], check=True)
    elif existing != digest:
        raise SystemExit(f"{repo}:{tag} already names {existing}, not {digest}")


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--tenant", required=True)
    parser.add_argument("--digest", required=True, help="sha256:… of the archived image")
    parser.add_argument("--repo", required=True, help="<registry>/<repository> to push into")
    parser.add_argument("--tier", default="Standard", choices=["Standard", "Bulk"])
    parser.add_argument(
        "--bucket",
        help="the image archive bucket to read (default: the tenant's data stack's); name the "
        "one a teardown kept to restore after an account rebuild",
    )
    args = parser.parse_args(argv)
    bucket = args.bucket or archive_bucket(args.tenant)
    prefix = f"s3://{bucket}"

    with tempfile.TemporaryDirectory() as tmp:
        layout = Path(tmp) / "layout"
        (layout / "blobs" / "sha256").mkdir(parents=True)
        for name in ("index.json", "oci-layout"):
            _aws("s3", "cp", "--quiet", f"{prefix}/images/{args.digest}/{name}", str(layout / name))
        index = json.loads((layout / "index.json").read_text())
        heads: dict[str, Json] = {}

        def head(digest: str) -> Json:
            if digest not in heads:
                # JSON whatever the operator's CLI default output is (text, table, yaml).
                heads[digest] = json.loads(
                    _aws(
                        "s3api",
                        "head-object",
                        "--bucket",
                        bucket,
                        "--key",
                        blob_key(digest),
                        "--output",
                        "json",
                    )
                )
            return heads[digest]

        def read_blob(digest: str) -> bytes:
            path = layout / blob_key(digest)
            if not path.exists():
                # A large blob is a layer and is not read here; a cold one is read once restored
                # (a manifest is tiered only with a low `min_tiered_object_bytes`).
                h = head(digest)
                if not BlobState.of(h).readable or h.get("ContentLength", 0) > MAX_METADATA_BYTES:
                    return b""
                _aws("s3", "cp", "--quiet", f"{prefix}/{blob_key(digest)}", str(path))
            return path.read_bytes()

        blobs = blobs_of(index, read_blob)
        plan = plan_restore({digest: BlobState.of(head(digest)) for digest in blobs})
        for digest in plan.request:
            _aws(
                "s3api",
                "restore-object",
                "--bucket",
                bucket,
                "--key",
                blob_key(digest),
                "--restore-request",
                json.dumps({"Days": RESTORE_DAYS, "GlacierJobParameters": {"Tier": args.tier}}),
            )
        if not plan.ready:
            print(
                f"waiting: {len(plan.request)} blob(s) restore requested, {len(plan.waiting)} "
                f"being restored (Deep Archive takes hours); run again later",
                file=sys.stderr,
            )
            return 2

        for digest in blobs:
            path = layout / blob_key(digest)
            if not path.exists() or path.stat().st_size == 0:
                _aws("s3", "cp", "--quiet", f"{prefix}/{blob_key(digest)}", str(path))
        tag = f"restore-{args.digest.split(':', 1)[1][:12]}"
        # `restore-*` tags are immutable in the workflow and builds repositories: a rerun after a
        # push reuses it.
        if _existing_digest(f"{args.repo}:{tag}") is None:
            subprocess.run(["crane", "push", str(layout), f"{args.repo}:{tag}"], check=True)
        pushed = _existing_digest(f"{args.repo}:{tag}")
        if pushed != args.digest:
            print(f"restored digest {pushed} differs from {args.digest}", file=sys.stderr)
            return 1
        manifest = json.loads((layout / blob_key(args.digest)).read_text())
        tags = keep_tags(args.digest, manifest)
        for digest, keep in tags:
            tag_once(args.repo, digest, keep)
        print(f"restored {args.repo}@{pushed} (tags {tag}, {', '.join(t for _, t in tags)})")
        return 0


if __name__ == "__main__":
    sys.exit(main())
