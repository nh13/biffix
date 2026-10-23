"""Download the run's image map (declared image -> the tenant copy to run) and check its shape.

Usage: image_map.py <s3 uri> <dest file>

Exits non-zero (with its reason on stderr) when the map cannot be read or is not a JSON object.
"""

import json
import sys


def fetch(uri: str, dest: str) -> None:
    import boto3

    if not uri.startswith("s3://"):
        sys.exit(f"not an s3 uri: {uri}")
    bucket, _, key = uri[5:].partition("/")
    boto3.client("s3").download_file(bucket, key, dest)
    with open(dest) as f:
        if not isinstance(json.load(f), dict):
            sys.exit("the image map is not a JSON object")


if __name__ == "__main__":
    fetch(sys.argv[1], sys.argv[2])
