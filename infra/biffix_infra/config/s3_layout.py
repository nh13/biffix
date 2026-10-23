"""Where biffix keeps things in its buckets, and the lifecycle rules it applies to them.

Fixed layout, not config: the backend, the CodeBuild scripts and the stacks all agree on these.
"""

from __future__ import annotations

# Uploads from the browser go under this prefix of a project bucket; incomplete ones are aborted
# (their parts deleted) after ABORT_INCOMPLETE_UPLOADS_DAYS.
UPLOADS_PREFIX = "uploads/"
ABORT_INCOMPLETE_UPLOADS_DAYS = 7
# Where each run's own files live (`biffix/runs/<run_id>/`), including a Nextflow run's `work/`;
# `biffix_core::s3_layout::RUNS_PREFIX` must match.
RUNS_PREFIX = "biffix/runs/"
# Where the CLI stages workflow sources (and image build contexts) in a project's bucket: the
# backend presigns uploads into it and the image builds read their contexts from it (every project).
WORKFLOW_SOURCES_PREFIX = "workflow-sources/"
# Where a project's Snakemake between-workflow output cache lives (a workflow opts in with
# `cache: true`); `biffix_core::s3_layout::SNAKEMAKE_CACHE_PREFIX` must match.
SNAKEMAKE_CACHE_PREFIX = "biffix/cache/snakemake/"
# The image archive's layout (codebuild/image/import.sh): content-addressed blobs
# (`blobs/sha256/<hex>`) and per image its OCI index. Only blobs move to cold storage.
ARCHIVE_BLOBS_PREFIX = "blobs/"
# Reference-bucket prefix holding what Batch hosts install at boot (read by every host's role).
HOST_PREFIX = "host/"

# One S3 lifecycle rule, as `aws s3api` JSON (only ever printed or compared, never read back).
LifecycleRule = dict[str, object]

NONCURRENT_VERSIONS_RULE_ID = "biffix-expire-noncurrent-versions"
ABORT_INCOMPLETE_UPLOADS_RULE_ID = "biffix-abort-incomplete-uploads"


def abort_incomplete_uploads_rule() -> LifecycleRule:
    """The lifecycle rule (as `aws s3api` JSON) deleting the parts of browser uploads abandoned
    under UPLOADS_PREFIX."""
    return {
        "ID": ABORT_INCOMPLETE_UPLOADS_RULE_ID,
        "Status": "Enabled",
        "Filter": {"Prefix": UPLOADS_PREFIX},
        "AbortIncompleteMultipartUpload": {"DaysAfterInitiation": ABORT_INCOMPLETE_UPLOADS_DAYS},
    }


def noncurrent_versions_lifecycle_rule(days: int) -> LifecycleRule:
    """The lifecycle rule (as `aws s3api` JSON) that makes deletes in a versioned project bucket
    free space: superseded versions expire `days` after they stop being current, then the delete
    markers left with no versions behind them are removed. ProjectStack applies it to buckets it
    creates; for an imported, versioned bucket the deploy prints it for the owner to add."""
    return {
        "ID": NONCURRENT_VERSIONS_RULE_ID,
        "Status": "Enabled",
        "Filter": {},
        "NoncurrentVersionExpiration": {"NoncurrentDays": days},
        "Expiration": {"ExpiredObjectDeleteMarker": True},
    }
