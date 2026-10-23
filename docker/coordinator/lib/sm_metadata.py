"""Snakemake's run metadata (.snakemake/metadata + incomplete markers), kept at the run's STORE.

Usage: sm_metadata.py sync <s3 uri> <.snakemake dir> <fingerprint file> [final]
       sm_metadata.py restore <s3 uri> <.snakemake dir>

`sync` uploads the two directories as one tar.gz. A periodic sync skips the upload when nothing
under them changed since the last one (the file list, sizes and mtimes, recorded in the
fingerprint file); a `final` one always uploads. `restore` extracts that tarball when the STORE
has one (BIFFIX_RESUME=1 logs that it has none). A failed sync is logged and swallowed. A failed
restore (anything but a missing object) is logged and exits 1, and the caller then skips every
sync, so the STORE's record is never replaced by this run's alone.
"""

import hashlib
import io
import os
import sys
import tarfile

SUBDIRS = ("metadata", "incomplete")


def bucket_key(uri: str) -> tuple[str, str]:
    """The bucket and key of an s3:// uri with both; a ValueError for anything else."""
    bucket, _, key = uri.removeprefix("s3://").partition("/")
    if not uri.startswith("s3://") or not bucket or not key:
        raise ValueError(f"not an s3 object uri: {uri}")
    return bucket, key


def fingerprint(root: str) -> str:
    """A digest of every file's path, size and mtime under the metadata subdirectories."""
    seen = []
    for sub in SUBDIRS:
        for d, _, files in os.walk(os.path.join(root, sub)):
            for f in files:
                p = os.path.join(d, f)
                try:
                    st = os.stat(p)
                except OSError:
                    continue
                seen.append(f"{os.path.relpath(p, root)}\0{st.st_size}\0{st.st_mtime_ns}")
    return hashlib.sha256("\n".join(sorted(seen)).encode()).hexdigest()


def sync(uri: str, root: str, fp_file: str, force: bool) -> None:
    try:
        bucket, key = bucket_key(uri)
        current = fingerprint(root)
        try:
            with open(fp_file) as fh:
                last = fh.read()
        except OSError:
            last = ""
        if current == last and not force:
            return
        buf = io.BytesIO()
        with tarfile.open(fileobj=buf, mode="w:gz") as tar:
            for sub in SUBDIRS:
                path = os.path.join(root, sub)
                if os.path.isdir(path):
                    tar.add(path, arcname=sub)
        import boto3  # only once there is something to upload: an unchanged periodic sync skips it

        boto3.client("s3").put_object(Bucket=bucket, Key=key, Body=buf.getvalue())
        with open(fp_file, "w") as fh:
            fh.write(current)
    except Exception as e:
        print(
            f"biffix-coordinator: snakemake metadata sync failed (swallowed): {e}", file=sys.stderr
        )


def restore(uri: str, root: str, resume: bool) -> None:
    """Extract the STORE's metadata into `root`. Only a missing object means there is nothing to
    restore; any other failure raises, since syncing this run's metadata over the object would
    then replace the STORE's record with this run's jobs alone."""
    import boto3
    from botocore.exceptions import ClientError

    bucket, key = bucket_key(uri)
    try:
        body = boto3.client("s3").get_object(Bucket=bucket, Key=key)["Body"].read()
    except ClientError as e:
        if e.response.get("Error", {}).get("Code") not in ("NoSuchKey", "404", "NotFound"):
            raise
        if resume:
            print(
                f"biffix-coordinator: no snakemake metadata to restore ({e}); "
                "resuming from the existing outputs alone",
                file=sys.stderr,
            )
        return
    os.makedirs(root, exist_ok=True)
    with tarfile.open(fileobj=io.BytesIO(body), mode="r:gz") as tar:
        tar.extractall(root, filter="data")
    print(
        f"biffix-coordinator: restored snakemake metadata from s3://{bucket}/{key}", file=sys.stderr
    )


if __name__ == "__main__":
    match sys.argv[1:]:
        case ["sync", uri, root, fp_file, *final]:
            sync(uri, root, fp_file, force=bool(final))
        case ["restore", uri, root]:
            try:
                restore(uri, root, resume=os.environ.get("BIFFIX_RESUME") == "1")
            except Exception as e:
                print(
                    f"biffix-coordinator: WARNING: snakemake metadata restore failed: {e}; this "
                    f"run's metadata is not synced to {uri}, so the record there is kept",
                    file=sys.stderr,
                )
                sys.exit(1)
        case _:
            sys.exit(__doc__)
