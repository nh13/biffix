"""Download one of the run's maps and check its shape: the image map (declared image -> the tenant
copy to run) or the job definition map (tenant image -> the task job definition it runs on).

Usage: image_map.py <s3 uri> <dest file> [<what the map is called in errors, default "image map">]

Exits non-zero (with its reason on stderr) when the map cannot be read or is not a JSON object.
"""

import json
import sys

from s3_uri import split


def fetch(uri: str, dest: str, what: str = "image map") -> None:
    import boto3

    try:
        bucket, key = split(uri)
    except ValueError as e:
        sys.exit(str(e))
    if not key:
        sys.exit(f"not an s3 object uri: {uri}")
    boto3.client("s3").download_file(bucket, key, dest)
    with open(dest) as f:
        if not isinstance(json.load(f), dict):
            sys.exit(f"the {what} is not a JSON object")


if __name__ == "__main__":
    fetch(*sys.argv[1:4])
