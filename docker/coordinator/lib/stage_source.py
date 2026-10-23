"""Stage an S3 workflow source into the coordinator's source directory.

Usage: stage_source.py tarball <s3 uri> <dest dir> <sha256>
       stage_source.py prefix <s3 uri> <dest dir>

boto3 rather than awscli: one less dependency in the image, and the S3 storage plugin already
requires boto3. Every path is contained within the destination: a tarball member or an S3 key
that would land outside it fails the staging. A tarball whose sha256 is not the recorded one
exits 78 (EX_CONFIG); any other failure exits non-zero with its reason on stderr.
"""

import hashlib
import os
import pathlib
import posixpath
import sys
import tarfile
import tempfile


def stage_tarball(uri: str, dest: pathlib.Path, want: str) -> None:
    """A single immutable tarball (private-repo support): download it, check it is the tarball
    registration recorded (its sha256: the object's key is only a name, and what is under it could
    have been replaced since), then extract it with a containment filter so no member can escape
    the staging dir (the tarball path-traversal footgun)."""
    import boto3

    dest = dest.resolve()
    if not uri.startswith("s3://"):
        sys.exit(f"not an s3 uri: {uri}")
    bucket, _, key = uri[5:].partition("/")
    if not key:
        sys.exit(f"s3 tarball uri has no key: {uri}")
    dest.mkdir(parents=True, exist_ok=True)
    fd, tarpath = tempfile.mkstemp(suffix=".tar.gz")
    os.close(fd)
    try:  # the download goes on every exit, a refused one included
        boto3.client("s3").download_file(bucket, key, tarpath)
        with open(tarpath, "rb") as fh:
            got = hashlib.file_digest(fh, "sha256").hexdigest()
        if got != want:
            print(
                f"biffix-coordinator: the workflow source {uri} has sha256 {got}, "
                f"but its version was registered with {want}; refusing to run it",
                file=sys.stderr,
            )
            sys.exit(78)
        print(
            f"biffix-coordinator: the workflow source matches its registered sha256 {want}",
            file=sys.stderr,
        )
        with tarfile.open(tarpath, "r:gz") as tf:
            # filter="data" (Python 3.12) rejects absolute paths, ".." traversal, and device/
            # special/link members. Belt-and-suspenders: independently confirm each resolved
            # member stays within dest.
            for member in tf.getmembers():
                target = (dest / member.name).resolve()
                if target != dest and dest not in target.parents:
                    sys.exit(f"tar member escapes staging dir: {member.name}")
            tf.extractall(dest, filter="data")
    finally:
        os.unlink(tarpath)
    print("staged workflow source from tarball", file=sys.stderr)


def stage_prefix(uri: str, dest: pathlib.Path) -> None:
    """Every object under the prefix, preserving its path relative to the prefix."""
    import boto3

    dest = dest.resolve()
    if not uri.startswith("s3://"):
        sys.exit(f"not an s3 uri: {uri}")
    bucket, _, prefix = uri[5:].partition("/")
    # Match at a PATH boundary, not a raw string prefix. A source uri names a directory whose
    # contents stage preserving their paths relative to it; without the trailing-slash anchor a
    # sibling key that merely shares the string prefix (runs/abc/src2/... for prefix runs/abc/src)
    # would be pulled in and mis-pathed. Empty prefix => whole bucket; a key equal to the prefix =>
    # a bare single object.
    boundary = prefix + "/" if prefix and not prefix.endswith("/") else prefix
    client = boto3.client("s3")
    count = 0
    for page in client.get_paginator("list_objects_v2").paginate(Bucket=bucket, Prefix=prefix):
        for obj in page.get("Contents", []):
            key = obj["Key"]
            if key.endswith("/"):
                continue
            if boundary and key.startswith(boundary):
                rel = key[len(boundary) :]
            elif prefix and key == prefix:
                rel = posixpath.basename(key)  # a bare single-object uri
            elif not prefix:
                rel = key  # whole-bucket source
            else:
                continue  # shares the string prefix but not the path boundary
            rel = rel.lstrip("/")
            if not rel:
                continue
            # Contain within dest: a key with ".." segments must not escape the staging dir.
            target = (dest / rel).resolve()
            if target != dest and dest not in target.parents:
                sys.exit(f"s3 key escapes staging dir: {key}")
            target.parent.mkdir(parents=True, exist_ok=True)
            client.download_file(bucket, key, str(target))
            count += 1
    if count == 0:
        sys.exit(f"no objects found under {uri}")
    print(f"staged {count} file(s)", file=sys.stderr)


if __name__ == "__main__":
    match sys.argv[1:]:
        case ["tarball", uri, dest, sha256]:
            stage_tarball(uri, pathlib.Path(dest), sha256)
        case ["prefix", uri, dest]:
            stage_prefix(uri, pathlib.Path(dest))
        case _:
            sys.exit(__doc__)
