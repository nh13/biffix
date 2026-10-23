"""The data section: the reference bucket's key, how long run files are kept (`retention`), and
the image archive (DataStack, ProjectStack, the reconcile sweeps)."""

from __future__ import annotations

from dataclasses import dataclass

from biffix_infra.config.fields import Fields, Raw, count, is_count, optional_texts, present

OBJECT_LOCK_MODES = ("COMPLIANCE", "GOVERNANCE")


@dataclass(frozen=True)
class ObjectLock:
    """S3 Object Lock for the image archive: every archived object is immutable for `days`."""

    mode: str  # COMPLIANCE | GOVERNANCE
    days: int


@dataclass(frozen=True)
class ImageArchive:
    """The image archive (`image_archive:` in the deploy config): every image a workflow version
    pins is kept byte-for-byte as an OCI layout in its own bucket, so a lost image can be restored
    with an identical digest (infra/README.md). Only `blobs/` objects of at least
    `min_tiered_object_bytes` move to cold storage; manifests and indexes (well under the 1 MiB
    default) then stay in Standard. A lower threshold tiers them too; the restore runbook
    (`image_restore`) still converges, with one more round of restores per level of the image.

    `object_lock` can only be turned on when the bucket is created (the first deploy with the
    archive); `key_arn` brings your own KMS key.
    """

    instant_retrieval_after_days: int | None = 0
    deep_archive_after_days: int | None = 90
    min_tiered_object_bytes: int = 1024 * 1024
    object_lock: ObjectLock | None = None
    key_arn: str | None = None


def parse_image_archive(block: Raw | None) -> ImageArchive:
    """`image_archive: {instant_retrieval_after_days, deep_archive_after_days,
    min_tiered_object_bytes, object_lock: {mode, days}, key_arn}`; defaults when unset. A tier set
    to null is skipped."""
    fields = {
        **present(
            block or {},
            "instant_retrieval_after_days",
            "deep_archive_after_days",
            "min_tiered_object_bytes",
        ),
        **optional_texts(block or {}, "image_archive.", "key_arn"),
    }
    lock = (block or {}).get("object_lock")
    if lock is not None:
        mode = str(lock.get("mode", "")).upper()
        if mode not in OBJECT_LOCK_MODES or not is_count(lock.get("days"), 1):
            raise ValueError(
                "image_archive.object_lock needs mode (COMPLIANCE|GOVERNANCE) + days (>= 1)"
            )
        fields["object_lock"] = ObjectLock(mode=mode, days=lock["days"])
    for tier in ("instant_retrieval_after_days", "deep_archive_after_days"):
        if fields.get(tier) is not None:
            count(f"image_archive.{tier}", fields[tier], 0)
    archive = ImageArchive(**fields)
    size = archive.min_tiered_object_bytes
    if not is_count(size, 0):
        raise ValueError(f"image_archive.min_tiered_object_bytes {size!r}: expected a byte count")
    ir, deep = archive.instant_retrieval_after_days, archive.deep_archive_after_days
    if ir is not None and deep is not None and deep <= ir:
        raise ValueError(
            "image_archive.deep_archive_after_days must be after the instant-retrieval tier"
        )
    return archive


# `retention:` key → (DeployConfig field, the fewest days it may say). Each defaults on its field;
# null keeps forever.
_RETENTION = {
    "intermediates_days": ("intermediates_retention_days", 0),
    "snakemake_cache_days": ("snakemake_cache_days", 1),
    "noncurrent_versions_days": ("noncurrent_versions_days", 1),
}


def parse_retention(block: Raw | None) -> Fields:
    """The `retention:` days the config sets, as DeployConfig fields; null (keep forever) or a
    whole number of days."""
    fields = {}
    for key, days in present(block or {}, *_RETENTION).items():
        name, minimum = _RETENTION[key]
        if days is not None and not is_count(days, minimum):
            expected = "a whole number of days" + (f" (>= {minimum})" if minimum else "")
            raise ValueError(f"retention.{key} {days!r}: expected {expected}")
        fields[name] = days
    return fields


def parse(raw: Raw) -> Fields:
    """The data keys, as DeployConfig fields."""
    return {
        **optional_texts(raw, "", "reference_key_arn"),
        **parse_retention(raw.get("retention")),
        "image_archive": parse_image_archive(raw.get("image_archive")),
    }
