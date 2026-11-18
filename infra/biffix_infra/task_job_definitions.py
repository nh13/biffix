"""Task job definition retention, run by deploy.sh after a deploy.

At launch the backend registers one task job definition per (project, tenant image, engine), named
with the deploy's task prefix (`DeployNames.task_job_definition_prefix`), and reuses it across
runs; nothing deregisters it, so they would accumulate (an account holds a bounded number of
ACTIVE job definitions). Once a definition's image is gone from the tenant's ECR (image retention,
a lifecycle rule, a deleted repository) no task can run on it, so this step deregisters it. A later
launch naming that image again registers a new one.

Only definitions under this tenant's task prefix, and only when the image is in this tenant's
registry and ECR says it does not exist (`ImageNotFoundException`, or
`RepositoryNotFoundException` for its repository). A definition naming no image, an image outside
the registry, or an image ECR could not answer for (any other error) is left alone and counted.
Deregistering is idempotent (an INACTIVE definition is not listed again), and `--dry-run` only
prints what it would do.

The plan is pure (it asks a lookup whether each image exists); `main` lists the definitions, looks
the images up and deregisters with the aws CLI.
"""

from __future__ import annotations

import argparse
import enum
import json
import subprocess
import sys
from collections.abc import Callable
from dataclasses import dataclass, field
from typing import Any

# The ECR errors that mean an image does not exist: it, or its whole repository, is gone.
_GONE_ERRORS = ("ImageNotFoundException", "RepositoryNotFoundException")


@dataclass(frozen=True)
class TaskDefinition:
    arn: str
    name: str
    image: str | None = None


@dataclass(frozen=True)
class ImageRef:
    """An image in the tenant's registry: its repository, and its digest or else its tag."""

    repository: str
    digest: str | None = None
    tag: str | None = None

    def image_id(self) -> str:
        """The image as `describe-images --image-ids` takes it."""
        return f"imageDigest={self.digest}" if self.digest else f"imageTag={self.tag}"


class Presence(enum.Enum):
    PRESENT = "present"
    GONE = "gone"
    UNKNOWN = "unknown"


def parse_image(image: str, registry: str) -> ImageRef | None:
    """`image` (`<registry>/<repo>@sha256:…` or `<registry>/<repo>:<tag>`) as an ImageRef, or None
    when it is not in `registry` (`<account>.dkr.ecr.<region>.<suffix>`) or has neither a digest
    nor a tag."""
    prefix = f"{registry}/"
    if not image.startswith(prefix):
        return None
    rest = image[len(prefix) :]
    if "@" in rest:
        # A digest wins over any tag beside it (`<repo>:<tag>@sha256:…`), which is no part of the
        # repository's name.
        named, digest = rest.split("@", 1)
        repository = named.rsplit(":", 1)[0] if ":" in named.rsplit("/", 1)[-1] else named
        return ImageRef(repository, digest=digest) if repository and digest else None
    repository, sep, tag = rest.rpartition(":")
    return ImageRef(repository, tag=tag) if sep and repository and tag else None


@dataclass
class Plan:
    """The definitions to deregister, and how many under the prefix are kept and why."""

    deregister: list[TaskDefinition] = field(default_factory=list)
    present: int = 0
    # No image, or one outside the tenant's registry.
    foreign: int = 0
    # ECR did not say whether the image exists.
    unknown: int = 0


def plan_retention(
    definitions: list[TaskDefinition],
    *,
    prefix: str,
    registry: str,
    lookup: Callable[[ImageRef], Presence],
) -> Plan:
    """Which of the ACTIVE `definitions` named with `prefix` run an image of `registry` that
    `lookup` says is gone. Each image is looked up once."""
    plan = Plan()
    seen: dict[ImageRef, Presence] = {}
    for d in definitions:
        if not d.name.startswith(prefix):
            continue
        ref = parse_image(d.image, registry) if d.image else None
        if ref is None:
            plan.foreign += 1
            continue
        if ref not in seen:
            seen[ref] = lookup(ref)
        match seen[ref]:
            case Presence.GONE:
                plan.deregister.append(d)
            case Presence.PRESENT:
                plan.present += 1
            case Presence.UNKNOWN:
                plan.unknown += 1
    return plan


# ── AWS side (aws CLI; deploy.sh already requires it) ─────────────────────────────────────────


def _aws(*args: str) -> dict[str, Any]:
    """An aws command's JSON answer (its shape is the command's, read unchecked)."""
    out = subprocess.run(
        ["aws", *args, "--output", "json"], check=True, capture_output=True, text=True
    )
    answer: dict[str, Any] = json.loads(out.stdout or "{}")
    return answer


def list_active(prefix: str) -> list[TaskDefinition]:
    """Every ACTIVE job definition named with `prefix` (the aws CLI follows every page)."""
    answer = _aws("batch", "describe-job-definitions", "--status", "ACTIVE")
    return [
        TaskDefinition(
            arn=d["jobDefinitionArn"],
            name=d["jobDefinitionName"],
            image=(d.get("containerProperties") or {}).get("image"),
        )
        for d in answer.get("jobDefinitions", [])
        if d["jobDefinitionName"].startswith(prefix)
    ]


def image_presence(ref: ImageRef) -> Presence:
    """Whether ECR holds `ref`: GONE only when it says the image or its repository does not
    exist; UNKNOWN for any other error (a throttle, a denied call)."""
    try:
        _aws(
            "ecr",
            "describe-images",
            "--repository-name",
            ref.repository,
            "--image-ids",
            ref.image_id(),
        )
    except subprocess.CalledProcessError as e:
        if any(error in (e.stderr or "") for error in _GONE_ERRORS):
            return Presence.GONE
        print(f"  could not look up {ref.repository} {ref.image_id()}: {e.stderr}".rstrip())
        return Presence.UNKNOWN
    return Presence.PRESENT


def apply(plan: Plan, dry_run: bool) -> list[str]:
    """Deregister `plan`'s definitions (or print them, with `dry_run`); return the ones Batch
    refused, with why."""
    refused: list[str] = []
    for d in plan.deregister:
        print(f"  deregister {d.arn} ({d.image})")
        if dry_run:
            continue
        try:
            _aws("batch", "deregister-job-definition", "--job-definition", d.arn)
        except subprocess.CalledProcessError as e:
            refused.append(f"{d.arn} ({(e.stderr or '').strip()})")
    return refused


def main(argv: list[str] | None = None) -> int:
    p = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    p.add_argument("--prefix", required=True, help="e.g. biffix-<tenant>_task-")
    p.add_argument(
        "--registry", required=True, help="e.g. <account>.dkr.ecr.<region>.amazonaws.com"
    )
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
    plan = plan_retention(
        list_active(a.prefix), prefix=a.prefix, registry=a.registry, lookup=image_presence
    )
    print(
        f"task job definitions: {len(plan.deregister)} to deregister (image gone); kept: "
        f"{plan.present} with their image, {plan.foreign} with no image of the tenant's registry, "
        f"{plan.unknown} whose image could not be looked up"
    )
    refused = apply(plan, a.dry_run)
    for line in refused:
        print(f"Batch did not deregister {line}", file=sys.stderr)
    return 1 if refused else 0


if __name__ == "__main__":
    sys.exit(main())
