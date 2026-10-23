"""Image retention for biffix's ECR repos, run by deploy.sh after a deploy.

ECR lifecycle rules select an image when ANY of its tags matches, so a count rule on build tags
would also delete a build that is deployed or that a registered version pins (those carry extra
tags). The repos' lifecycle therefore only expires untagged images, and this step does the rest:

- protect: tag each deployed image, and each image a deployed multi-arch index lists, with
  `deployed-<tenant>-<digest prefix>` (so none of them is ever untagged), and drop such tags from
  images no longer deployed;
- prune: keep the deployed build and the newest `keep` builds' tags, and delete the tags of older
  builds.

Removing a tag with BatchDeleteImage deletes the image AT ONCE when it was the image's last tag
(it does not wait for the untagged lifecycle rule), so a pruned build with no other tag (a
`biffix-keep-*` from an import, a `deployed-*`) is gone immediately: rolling back
to it means pushing it again. For the same reason the plan never removes the last tag of an image
that an image it keeps lists (a multi-arch index's platform images): that tag stays until nothing
kept lists the image any more.

The plan is pure; `main` gathers the repo's images and applies it with the aws CLI.
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
from dataclasses import dataclass, field
from datetime import UTC, datetime
from typing import Any

ARCH_SUFFIX = re.compile(r"-(amd64|arm64)$")
COORDINATOR_BUILD = r"^[0-9a-f]{40}(-amd64|-arm64)?$"


# The manifest media types that list other images (a multi-arch index).
INDEX_MEDIA_TYPES = {
    "application/vnd.oci.image.index.v1+json",
    "application/vnd.docker.distribution.manifest.list.v2+json",
}
_EPOCH = datetime.min.replace(tzinfo=UTC)


@dataclass(frozen=True)
class Image:
    digest: str
    tags: tuple[str, ...]
    pushed_at: datetime  # timezone-aware
    media_type: str | None = None


def pushed_at(value: object) -> datetime:
    """`imagePushedAt` as an aware datetime. The AWS CLI v2 renders it as ISO 8601 in the LOCAL
    timezone (v1 as epoch seconds), so the strings are not ordered across a DST change or a
    timezone: they must be compared as instants."""
    if isinstance(value, (int, float)):
        return datetime.fromtimestamp(value, tz=UTC)
    dt = datetime.fromisoformat(str(value))
    return dt if dt.tzinfo is not None else dt.replace(tzinfo=UTC)


@dataclass
class Plan:
    """Tags to add (`(digest, tag)`) and tags to delete."""

    add: list[tuple[str, str]] = field(default_factory=list)
    delete: list[str] = field(default_factory=list)


def build_of(tag: str) -> str:
    """A build's name: its tag without an architecture suffix."""
    return ARCH_SUFFIX.sub("", tag)


def protect_tag(prefix: str, digest: str) -> str:
    return f"{prefix}{digest.split(':', 1)[-1][:12]}"


def _listed_by(digests: set[str], children: dict[str, list[str]]) -> set[str]:
    """Every digest the images in `digests` list, transitively."""
    listed: set[str] = set()
    todo = [c for d in digests for c in children.get(d, [])]
    while todo:
        d = todo.pop()
        if d not in listed:
            listed.add(d)
            todo += children.get(d, [])
    return listed


def plan_retention(
    images: list[Image],
    *,
    deployed_tag: str,
    children: dict[str, list[str]],
    protect_prefix: str,
    keep: int,
    build_pattern: str = COORDINATOR_BUILD,
) -> Plan:
    """What to tag and untag in one repo. `children` maps each index in the repo (by digest) to the
    digests it lists; a single-platform image has no entry."""
    plan = Plan()
    build_re = re.compile(build_pattern)
    by_tag = {t: img for img in images for t in img.tags}

    # Protect: the deployed build (all its arch variants) and everything those images list.
    deployed_build = build_of(deployed_tag)
    deployed = {
        img.digest
        for tag, img in by_tag.items()
        if tag == deployed_tag or (build_re.match(tag) and build_of(tag) == deployed_build)
    }
    protected = deployed | _listed_by(deployed, children)
    for digest in sorted(protected):
        tag = protect_tag(protect_prefix, digest)
        if by_tag.get(tag) is None or by_tag[tag].digest != digest:
            plan.add.append((digest, tag))
    # Drop protection from images no longer deployed.
    for tag, img in sorted(by_tag.items()):
        if tag.startswith(protect_prefix) and img.digest not in protected:
            plan.delete.append(tag)

    # Prune: builds newest first (by their newest image); keep the deployed one and `keep` more.
    newest: dict[str, datetime] = {}
    for tag, img in by_tag.items():
        if build_re.match(tag):
            b = build_of(tag)
            newest[b] = max(newest.get(b, _EPOCH), img.pushed_at)
    ordered = sorted(newest, key=lambda b: newest[b], reverse=True)
    kept = {deployed_build, *[b for b in ordered if b != deployed_build][:keep]}
    for tag in sorted(by_tag):
        if build_re.match(tag) and build_of(tag) not in kept:
            plan.delete.append(tag)

    # Never remove the last tag of an image something kept lists: that would delete it at once
    # and leave the kept index pointing at nothing. Keep one of its tags (its protection tag when
    # it has one) and look again, since the image it keeps may list others in turn.
    deleting = set(plan.delete)
    while True:
        surviving = protected | {
            img.digest for img in images if any(t not in deleting for t in img.tags)
        }
        listed = _listed_by(surviving, children)
        stranded = [
            img
            for img in images
            if img.digest in listed and img.tags and all(t in deleting for t in img.tags)
        ]
        if not stranded:
            break
        for img in stranded:
            tags = sorted(img.tags)
            deleting.discard(next((t for t in tags if t.startswith(protect_prefix)), tags[0]))
    plan.delete = [t for t in plan.delete if t in deleting]
    return plan


# ── AWS side (aws CLI; deploy.sh already requires it) ─────────────────────────────────────────


def _aws(*args: str) -> dict[str, Any]:
    """An `aws ecr` command's JSON answer (its shape is the command's, read unchecked)."""
    out = subprocess.run(
        ["aws", "ecr", *args, "--output", "json"], check=True, capture_output=True, text=True
    )
    answer: dict[str, Any] = json.loads(out.stdout or "{}")
    return answer


def list_images(repo: str) -> list[Image]:
    details = _aws("describe-images", "--repository-name", repo)["imageDetails"]
    return [
        Image(
            d["imageDigest"],
            tuple(d.get("imageTags", [])),
            pushed_at(d["imagePushedAt"]),
            d.get("imageManifestMediaType"),
        )
        for d in details
    ]


# The same list as biffix-workflow's `MANIFEST_MEDIA_TYPES`
# (crates/biffix-workflow/src/images/registry.rs); keep them in step.
MEDIA_TYPES = [
    "application/vnd.oci.image.index.v1+json",
    "application/vnd.docker.distribution.manifest.list.v2+json",
    "application/vnd.oci.image.manifest.v1+json",
    "application/vnd.docker.distribution.manifest.v2+json",
]


def get_manifest(repo: str, digest: str) -> tuple[str, str | None]:
    image = _aws(
        "batch-get-image",
        "--repository-name",
        repo,
        "--image-ids",
        f"imageDigest={digest}",
        "--accepted-media-types",
        *MEDIA_TYPES,
    )["images"][0]
    return image["imageManifest"], image.get("imageManifestMediaType")


def children_of(manifest: str) -> list[str]:
    return [m["digest"] for m in json.loads(manifest).get("manifests", []) if "digest" in m]


def apply(repo: str, plan: Plan, dry_run: bool) -> list[str]:
    """Apply `plan` (or print it, with `dry_run`); return the tags ECR refused to remove, with
    why. batch-delete-image reports those only in its answer's `failures` (it exits 0), and a
    tag that is already gone (ImageNotFound) is not one."""
    refused: list[str] = []
    for digest, tag in plan.add:
        print(f"  tag {digest[:19]} {tag}")
        if dry_run:
            continue
        manifest, media_type = get_manifest(repo, digest)
        args = ["put-image", "--repository-name", repo, "--image-manifest", manifest]
        args += ["--image-digest", digest, "--image-tag", tag]
        if media_type:
            args += ["--image-manifest-media-type", media_type]
        try:
            _aws(*args)
        except subprocess.CalledProcessError as e:
            if "AlreadyExists" not in (e.stderr or ""):
                raise
    for tag in plan.delete:
        print(f"  untag {tag}")
        if dry_run:
            continue
        image_id = f"imageTag={tag}"
        out = _aws("batch-delete-image", "--repository-name", repo, "--image-ids", image_id)
        for failure in out.get("failures") or []:
            if failure.get("failureCode") != "ImageNotFound":
                why = f"{failure.get('failureCode')}: {failure.get('failureReason')}"
                refused.append(f"{tag} ({why})")
    return refused


def main(argv: list[str] | None = None) -> int:
    p = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    p.add_argument("--repo", required=True)
    p.add_argument("--deployed-tag", required=True)
    p.add_argument("--protect-prefix", required=True, help="e.g. deployed-<tenant>-")
    p.add_argument("--keep", type=int, default=5)
    p.add_argument("--dry-run", action="store_true")
    a = p.parse_args(argv)
    try:
        return _retain(a)
    except subprocess.CalledProcessError as e:
        # The call and AWS's own error, not a bare "returned non-zero exit status".
        call = " ".join(e.cmd[:3]) if isinstance(e.cmd, list) else e.cmd
        print(f"{call}: {(e.stderr or '').strip()}", file=sys.stderr)
        return 1


def _retain(a: argparse.Namespace) -> int:
    images = list_images(a.repo)
    deployed = [i for i in images if a.deployed_tag in i.tags]
    if not deployed:
        print(f"deployed tag {a.deployed_tag} is not in {a.repo}; not pruning", file=sys.stderr)
        return 1
    # What each index lists (an image whose media type is not reported is read to find out).
    children = {
        i.digest: listed
        for i in images
        if i.media_type in INDEX_MEDIA_TYPES or i.media_type is None
        if (listed := children_of(get_manifest(a.repo, i.digest)[0]))
    }
    plan = plan_retention(
        images,
        deployed_tag=a.deployed_tag,
        children=children,
        protect_prefix=a.protect_prefix,
        keep=a.keep,
    )
    print(f"{a.repo}: {len(plan.add)} to protect, {len(plan.delete)} tags to remove")
    refused = apply(a.repo, plan, a.dry_run)
    for tag in refused:
        print(f"ECR did not remove the tag {tag}", file=sys.stderr)
    return 1 if refused else 0


if __name__ == "__main__":
    sys.exit(main())
