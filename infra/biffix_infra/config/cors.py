"""The CORS rules the browser needs on a bucket, and what the deploy prints for the owners of
buckets biffix does not own (it never changes one)."""

from __future__ import annotations

import json
from typing import TYPE_CHECKING, TypedDict

from biffix_infra.config.s3_layout import LifecycleRule, abort_incomplete_uploads_rule

if TYPE_CHECKING:
    from biffix_infra.config.model import DeployConfig

# How long a browser may cache a CORS preflight answer, in seconds.
CORS_MAX_AGE_SECONDS = 3000


class CorsRule(TypedDict):
    """One S3 CORS rule, as `aws s3api` JSON."""

    AllowedOrigins: list[str]
    AllowedMethods: list[str]
    AllowedHeaders: list[str]
    ExposeHeaders: list[str]
    MaxAgeSeconds: int


class CorsConfiguration(TypedDict):
    """A bucket's whole CORS configuration, as `aws s3api` JSON."""

    CORSRules: list[CorsRule]


class LifecycleConfiguration(TypedDict):
    """A bucket's whole lifecycle configuration, as `aws s3api` JSON."""

    Rules: list[LifecycleRule]


class BucketSettings(TypedDict):
    """A project bucket's settings: its CORS and its lifecycle configuration."""

    cors: CorsConfiguration
    lifecycle: LifecycleConfiguration


def upload_cors_rule(origin: str) -> CorsRule:
    """The CORS rule (as `aws s3api` JSON) browser uploads need: the biffix site PUTs parts to
    presigned URLs and reads each part's ETag."""
    return {
        "AllowedOrigins": [origin],
        "AllowedMethods": ["PUT"],
        "AllowedHeaders": ["*"],
        "ExposeHeaders": ["ETag"],
        "MaxAgeSeconds": CORS_MAX_AGE_SECONDS,
    }


def viewer_cors_rule(origin: str) -> CorsRule:
    """The CORS rule (as `aws s3api` JSON) the alignment viewer needs: the biffix site reads a
    BAM/CRAM, its index and the reference through presigned URLs in Range requests, and reads the
    response's range and size headers."""
    return {
        "AllowedOrigins": [origin],
        "AllowedMethods": ["GET", "HEAD"],
        "AllowedHeaders": ["Range"],
        "ExposeHeaders": ["Content-Range", "Content-Length", "Accept-Ranges", "ETag"],
        "MaxAgeSeconds": CORS_MAX_AGE_SECONDS,
    }


def project_bucket_cors(origin: str) -> CorsConfiguration:
    """A project bucket's whole CORS configuration (as `aws s3api` JSON): uploads and the viewer.
    One configuration, since `put-bucket-cors` replaces a bucket's rules."""
    return {"CORSRules": [upload_cors_rule(origin), viewer_cors_rule(origin)]}


def viewer_bucket_cors(origin: str) -> CorsConfiguration:
    """A data source bucket's CORS configuration (as `aws s3api` JSON): the viewer's reads only."""
    return {"CORSRules": [viewer_cors_rule(origin)]}


def upload_bucket_settings(origin: str) -> BucketSettings:
    """The bucket settings a project bucket needs, as `aws s3api` JSON: its CORS rules (browser
    uploads and the alignment viewer, `project_bucket_cors`), and a lifecycle rule deleting
    abandoned upload parts. ProjectStack applies the same rules to buckets it creates; for an
    imported bucket the deploy prints these for its owner to apply (then sets
    `uploads_cors_applied` and `viewer_cors_applied`)."""
    return {
        "cors": project_bucket_cors(origin),
        "lifecycle": {"Rules": [abort_incomplete_uploads_rule()]},
    }


def bucket_cors_instructions(config: DeployConfig) -> list[str]:
    """What the deploy prints for the owners of buckets biffix does not own (it never changes
    one): for each imported project bucket missing the upload or viewer CORS rule, ONE
    `put-bucket-cors` with both rules (it replaces the bucket's rules, so it must carry both), and
    the abandoned-uploads lifecycle rule when uploads are off; for each other bucket a data source
    reads from without the viewer rule, a GET/HEAD-only `put-bucket-cors`. Each bucket once.

    The viewer rule is a bucket's, not a source's: a bucket any of its sources flags
    (`viewer_cors_applied`) has it, as the backend reads the flags (`bucket_allows_viewer`), so
    nothing is printed for it."""
    origin = config.public_base_url
    out: list[str] = []
    handled: set[str] = set()
    for project in config.projects:
        bucket = project.bucket_name
        if not bucket or bucket in handled:
            continue
        handled.add(bucket)
        if project.uploads_cors_applied and project.viewer_cors_applied:
            continue
        settings = upload_bucket_settings(origin)
        off = [
            what
            for what, applied in (
                ("browser uploads", project.uploads_cors_applied),
                ("the alignment viewer", project.viewer_cors_applied),
            )
            if not applied
        ]
        lines = [
            f"[biffix] project `{project.id}`: {' and '.join(off)} "
            f"{'is' if len(off) == 1 else 'are'} off for the imported bucket {bucket}. "
            f"To turn {'it' if len(off) == 1 else 'them'} on, its owner applies",
            f"  aws s3api put-bucket-cors --bucket {bucket} "
            f"--cors-configuration '{json.dumps(settings['cors'])}'",
        ]
        if not project.uploads_cors_applied:
            lines.append(
                f"  aws s3api put-bucket-lifecycle-configuration --bucket {bucket} "
                f"--lifecycle-configuration '{json.dumps(settings['lifecycle'])}'"
            )
        lines.append(
            "then sets `uploads_cors_applied: true` and `viewer_cors_applied: true` on the project "
            "in the deploy config. (put-bucket-cors REPLACES the bucket's CORS rules; this one "
            "includes the upload rule; merge any other rules the bucket has first."
            + (
                " put-bucket-lifecycle-configuration REPLACES existing rules: merge them first.)"
                if not project.uploads_cors_applied
                else ")"
            )
        )
        out.append("\n".join(lines))
    # A bucket a data source reads from (other than a project bucket, whose rules are above) that
    # no source records the rule on.
    flagged = {
        source.bucket
        for project in config.projects
        for source in project.data_sources
        if source.viewer_cors_applied
    }
    sources: dict[str, list[str]] = {}
    for project in config.projects:
        for source in project.data_sources:
            if source.bucket in flagged or source.bucket in handled:
                continue
            sources.setdefault(source.bucket, []).append(f"{project.id}/{source.id}")
    for bucket, ids in sources.items():
        out.append(
            f"[biffix] data source {', '.join(f'`{i}`' for i in ids)}: the alignment viewer cannot "
            f"read reference files from {bucket} until its owner applies\n"
            f"  aws s3api put-bucket-cors --bucket {bucket} "
            f"--cors-configuration '{json.dumps(viewer_bucket_cors(origin))}'\n"
            f"then sets `viewer_cors_applied: true` on the data source in the deploy config. "
            f"(put-bucket-cors REPLACES the bucket's CORS rules: merge any other rules the bucket "
            f"has first.)"
        )
    return out
