"""Splitting an `s3://` uri into its bucket and key, for the coordinator's helpers (imported from
lib/, which a helper run as `python3 <lib>/<helper>.py` has first on its path)."""

from __future__ import annotations


def split(uri: str) -> tuple[str, str]:
    """The bucket and key ("" for none) of an `s3://` uri; a ValueError for one with no bucket or
    another scheme."""
    bucket, _, key = uri.removeprefix("s3://").partition("/")
    if not uri.startswith("s3://") or not bucket:
        raise ValueError(f"not an s3 uri: {uri}")
    return bucket, key


def split_object(uri: str) -> tuple[str, str]:
    """The bucket and key of an `s3://` uri naming an object; a ValueError for one with no key."""
    bucket, _, key = uri.removeprefix("s3://").partition("/")
    if not uri.startswith("s3://") or not bucket or not key:
        raise ValueError(f"not an s3 object uri: {uri}")
    return bucket, key


def split_prefix(uri: str) -> tuple[str, str]:
    """The bucket and key prefix of an `s3://` uri ending in `/`; a ValueError for any other."""
    bucket, _, prefix = uri.removeprefix("s3://").partition("/")
    if not uri.startswith("s3://") or not bucket or not prefix.endswith("/"):
        raise ValueError(f"not an s3 prefix: {uri}")
    return bucket, prefix
