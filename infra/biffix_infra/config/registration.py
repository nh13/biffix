"""The registration section: who may register workflows from GitHub Actions, and who may build
biffix's own images (IdentityStack's GitHub-OIDC roles)."""

from __future__ import annotations

import re
from dataclasses import dataclass

from biffix_infra.config.fields import Fields, Raw, text, text_list

# A GitHub OIDC `sub` naming one repository, `repo:<owner>/<repository>:<context>` (either name
# may carry GitHub's immutable `@<id>`). These values become trust conditions, so the owner and
# repository may hold no wildcard: a subject may widen only the context within its own repository
# (`repo:acme/pipelines:*`, any branch or environment of that one repository).
_SUBJECT = re.compile(r"repo:[^/*?:]+/[^/*?:]+:.+")


@dataclass(frozen=True)
class ImageBuildTrust:
    """Trust for biffix's OWN coordinator/worker image build (a first-party concern).

    Deliberately separate from `registration.github_subjects`, which trusts third-party WORKFLOW
    repositories. Those repos dispatch image builds from PR branches on purpose, so their role
    cannot be ref-pinned; this one can be, and is: `ref` is enforced as an IAM trust condition, so
    AWS refuses a token minted from any other branch regardless of what GitHub allows. That matters
    because GitHub's required-reviewer protection is unavailable on private repos outside the
    supported plans, leaving the environment gate advisory.
    """

    subject: str  # e.g. "repo:example/biffix:environment:image-build"
    ref: str = "refs/heads/main"


def _subject(where: str, value: object) -> str:
    subject = text(where, value)
    if not _SUBJECT.fullmatch(subject):
        raise ValueError(
            f"{where} {subject!r}: expected repo:<owner>/<repository>:<context>, with no wildcard "
            "in the owner or repository"
        )
    return subject


def parse_image_build_trust(block: Raw | None) -> ImageBuildTrust | None:
    """`registration.biffix_image_build: {subject, ref}`; None (no such role) when unset. `ref`
    pins the trust to one branch, so it may be left out (the default branch) but never null."""
    if not block:
        return None
    where = "registration.biffix_image_build"
    if not block.get("subject"):
        raise ValueError(f"{where} requires a `subject`")
    trust = ImageBuildTrust(subject=_subject(f"{where}.subject", block["subject"]))
    if "ref" in block:
        ref = text(f"{where}.ref", block["ref"])
        if not ref.startswith("refs/") or any(c in ref for c in "*?"):
            raise ValueError(f"{where}.ref {ref!r}: expected one branch, e.g. refs/heads/main")
        trust = ImageBuildTrust(subject=trust.subject, ref=ref)
    return trust


def parse(raw: Raw) -> Fields:
    """The registration keys, as DeployConfig fields."""
    # `or {}`: a present-but-null `registration:` block means its defaults.
    registration = raw.get("registration") or {}
    where = "registration.github_subjects"
    subjects = text_list(where, registration.get("github_subjects"))
    return {
        "github_subjects": [_subject(f"{where} entry", s) for s in subjects],
        "image_build_trust": parse_image_build_trust(registration.get("biffix_image_build")),
    }
