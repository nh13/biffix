"""DataStack — stateful storage. RETAIN + termination protection.

Per-project data buckets are created by ProjectStack; this stack owns the shared read-only
reference bucket, the reference KMS key, and the ECR repos (coordinator,
workflow, builds). A run's durable ``events.jsonl`` is uploaded next to that run's outputs in its
PER-PROJECT bucket (see ``docker/coordinator/entrypoint.sh``), not to a shared bucket here.
"""

from __future__ import annotations

from typing import Any

from aws_cdk import Duration, Stack, Tags
from aws_cdk import aws_ecr as ecr
from aws_cdk import aws_s3 as s3
from constructs import Construct

from biffix_infra.config import ARCHIVE_BLOBS_PREFIX, ImageArchive
from biffix_infra.constructs.encrypted_bucket import EncryptedBucket, customer_key

# Workflow-repository tags biffix writes once and then trusts by name: a version's keep-tags (which
# keep the untagged-expiry lifecycle off what it pins), and the import/derive/restore tags a step
# reuses when they already exist. They are IMMUTABLE, so a push by anyone else holding push on the
# repository (the registration role trusts third-party workflow repos) cannot move a keep-tag off a
# pinned image. Immutability does not stop such a push from writing a tag FIRST, so a step reuses
# a tag here only after checking what it names against what it would produce (an import: the
# source's digest; a derive: the base plus the runtime layer). Every other tag stays mutable
# (workflow CI pushes its own tags, e.g. `latest`). ECR allows at most five.
WORKFLOW_IMMUTABLE_TAGS = ("biffix-keep-*", "import-*", "derived-*", "restore-*")
# The builds repository holds the images biffix builds from a workflow's recipe (and what is
# derived from them). Only biffix's build projects and its import project (which joins a build's
# architectures) can push to it, so a build may reuse a `build-*` tag it finds there: nobody else
# can have written it (a build is not reproducible, so the tag cannot be checked). Every tag is
# immutable except the scratch tags an image made in steps is assembled under.
BUILDS_MUTABLE_TAGS = ("scratch-*",)
# The tag (and its value) marking the image archive bucket, which teardown.sh keeps by default.
ROLE_TAG = "biffix:role"
IMAGE_ARCHIVE_ROLE = "image-archive"


class DataStack(Stack):
    def __init__(
        self,
        scope: Construct,
        cid: str,
        *,
        reference_key_arn: str | None = None,
        image_archive: ImageArchive | None = None,
        **kwargs: Any,
    ) -> None:
        super().__init__(scope, cid, **kwargs)

        self.reference_key = customer_key(self, "ReferenceKey", reference_key_arn)
        # Shared, read-only reference area. Versioned for provenance; the
        # current object is retained, and only the churn of superseded noncurrent versions is aged
        # out (plus abandoned multipart uploads) so storage doesn't grow unbounded.
        self.reference_bucket = EncryptedBucket(
            self,
            "ReferenceBucket",
            encryption_key=self.reference_key,
            lifecycle_rules=[
                s3.LifecycleRule(
                    noncurrent_version_expiration=Duration.days(90),
                    abort_incomplete_multipart_upload_after=Duration.days(7),
                )
            ],
        )

        # ECR: the biffix coordinator image, workflow images (pushed by workflow CI, imported by
        # biffix) and the workflow images biffix builds (the builds repository). A lifecycle rule
        # selects an image when ANY of its tags matches, so a count rule on build tags would also
        # delete a deployed build or an image a registered version pins. Every repo therefore only
        # expires untagged images: deploy.sh's retention step (biffix_infra.image_retention) tags
        # the deployed coordinator and removes its old build tags, and the import project
        # keep-tags every workflow image a version pins (imported, derived or built).
        self.coordinator_repo = self._repository("CoordinatorRepo", ecr.TagMutability.IMMUTABLE)
        self.workflow_repo = self._repository(
            "WorkflowRepo", ecr.TagMutability.MUTABLE_WITH_EXCLUSION, WORKFLOW_IMMUTABLE_TAGS
        )
        self.builds_repo = self._repository(
            "BuildsRepo", ecr.TagMutability.IMMUTABLE_WITH_EXCLUSION, BUILDS_MUTABLE_TAGS
        )
        self.image_archive_bucket = self._image_archive(image_archive or ImageArchive())

    def _repository(
        self, cid: str, mutability: ecr.TagMutability, exclusions: tuple[str, ...] = ()
    ) -> ecr.Repository:
        """An image repository (scanned on push) whose tags have `mutability`, except those
        matching `exclusions`, and whose untagged images expire after 14 days."""
        return ecr.Repository(
            self,
            cid,
            image_scan_on_push=True,
            image_tag_mutability=mutability,
            image_tag_mutability_exclusion_filters=[
                ecr.ImageTagMutabilityExclusionFilter.wildcard(tag) for tag in exclusions
            ]
            or None,
            lifecycle_rules=[
                ecr.LifecycleRule(
                    description="expire untagged after 14 days",
                    tag_status=ecr.TagStatus.UNTAGGED,
                    max_image_age=Duration.days(14),
                    rule_priority=1,
                ),
            ],
        )

    def _image_archive(self, config: ImageArchive) -> s3.Bucket:
        """The bucket every pinned workflow image is archived to, byte for byte (an OCI layout), so
        a lost image can be restored with the same digest. Versioned and never emptied by biffix;
        large blobs move to Glacier Instant Retrieval and then Deep Archive (`image_archive:`)."""
        self.image_archive_key = customer_key(self, "ImageArchiveKey", config.key_arn)
        transitions = [
            s3.Transition(storage_class=storage_class, transition_after=Duration.days(days))
            for storage_class, days in (
                (s3.StorageClass.GLACIER_INSTANT_RETRIEVAL, config.instant_retrieval_after_days),
                (s3.StorageClass.DEEP_ARCHIVE, config.deep_archive_after_days),
            )
            if days is not None
        ]
        rules = [
            s3.LifecycleRule(
                id="abort-incomplete-uploads",
                abort_incomplete_multipart_upload_after=Duration.days(7),
            ),
            # A blob is written once and never replaced, so a noncurrent version only exists if
            # something overwrote or deleted it: keep that for a while, then let it go.
            s3.LifecycleRule(id="noncurrent", noncurrent_version_expiration=Duration.days(30)),
        ]
        if transitions:
            rules.append(
                s3.LifecycleRule(
                    id="tier-blobs",
                    prefix=ARCHIVE_BLOBS_PREFIX,
                    object_size_greater_than=config.min_tiered_object_bytes,
                    transitions=transitions,
                )
            )
        lock = config.object_lock
        bucket = EncryptedBucket(
            self,
            "ImageArchiveBucket",
            encryption_key=self.image_archive_key,
            bucket_key_enabled=True,
            lifecycle_rules=rules,
            object_lock_enabled=lock is not None or None,
            object_lock_default_retention=(
                (
                    s3.ObjectLockRetention.compliance(Duration.days(lock.days))
                    if lock.mode == "COMPLIANCE"
                    else s3.ObjectLockRetention.governance(Duration.days(lock.days))
                )
                if lock
                else None
            ),
        )
        # teardown.sh keeps the archive by default, and once this stack is gone it can tell the
        # archive from the tenant's other retained buckets only by a tag: the generated name is
        # truncated for a longer tenant.
        Tags.of(bucket).add(ROLE_TAG, IMAGE_ARCHIVE_ROLE)
        return bucket
