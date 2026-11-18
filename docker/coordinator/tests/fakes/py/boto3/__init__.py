"""A stand-in for the slice of boto3's S3 client the coordinator entrypoint uses, over a directory:
s3://<bucket>/<key> is $FAKE_S3/<bucket>/<key>; and for its ECR login token. For the entrypoint
tests only. The parameter names
are boto3's (Bucket, Key, …)."""

from __future__ import annotations

import base64
import io
import os
import shutil
import time
from collections.abc import Iterator
from pathlib import Path

from botocore.exceptions import ClientError


def _path(bucket: str, key: str) -> Path:
    return Path(os.environ["FAKE_S3"]) / bucket / key


class _Paginator:
    def paginate(self, Bucket: str, Prefix: str = "") -> Iterator[dict[str, list[dict[str, str]]]]:
        root = Path(os.environ["FAKE_S3"]) / Bucket
        keys = (
            sorted(str(p.relative_to(root)) for p in root.rglob("*") if p.is_file())
            if root.exists()
            else []
        )
        matched = [{"Key": k} for k in keys if k.startswith(Prefix)]
        # As S3 does: a page with no keys has no Contents at all.
        yield {"Contents": matched} if matched else {}


class _S3:
    def download_file(self, bucket: str, key: str, dest: str) -> None:
        src = _path(bucket, key)
        if not src.is_file():
            raise ClientError({"Error": {"Code": "404"}}, "HeadObject")
        shutil.copyfile(src, dest)

    def upload_file(self, src: str, bucket: str, key: str) -> None:
        dest = _path(bucket, key)
        dest.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(src, dest)

    def put_object(self, Bucket: str, Key: str, Body: bytes) -> None:
        # FAKE_S3_DENY_PUT: refuse every write; FAKE_S3_SLOW_PUT: take that many seconds first.
        if os.environ.get("FAKE_S3_DENY_PUT"):
            raise ClientError({"Error": {"Code": "AccessDenied"}}, "PutObject")
        time.sleep(float(os.environ.get("FAKE_S3_SLOW_PUT") or 0))
        dest = _path(Bucket, Key)
        dest.parent.mkdir(parents=True, exist_ok=True)
        dest.write_bytes(Body)

    def delete_object(self, Bucket: str, Key: str) -> None:
        # As S3 does, a missing key is no error; FAKE_S3_DENY_PUT refuses this write too.
        if os.environ.get("FAKE_S3_DENY_PUT"):
            raise ClientError({"Error": {"Code": "AccessDenied"}}, "DeleteObject")
        _path(Bucket, Key).unlink(missing_ok=True)

    def get_object(self, Bucket: str, Key: str) -> dict[str, io.BytesIO]:
        # FAKE_S3_DENY_GET: refuse every read, as S3 does without the grant (or the KMS key's).
        if os.environ.get("FAKE_S3_DENY_GET"):
            raise ClientError({"Error": {"Code": "AccessDenied"}}, "GetObject")
        src = _path(Bucket, Key)
        if not src.is_file():
            raise ClientError({"Error": {"Code": "NoSuchKey"}}, "GetObject")
        return {"Body": io.BytesIO(src.read_bytes())}

    def get_paginator(self, name: str) -> _Paginator:
        assert name == "list_objects_v2", name
        return _Paginator()


class _ECR:
    """ECR's login token for FAKE_ECR_REGISTRY, of the password FAKE_ECR_PASSWORD; FAKE_ECR_DENY
    refuses it, as ECR does without the grant."""

    def get_authorization_token(self) -> dict[str, list[dict[str, str]]]:
        if os.environ.get("FAKE_ECR_DENY"):
            raise ClientError({"Error": {"Code": "AccessDeniedException"}}, "GetAuthorizationToken")
        token = base64.b64encode(f"AWS:{os.environ['FAKE_ECR_PASSWORD']}".encode()).decode()
        endpoint = f"https://{os.environ['FAKE_ECR_REGISTRY']}"
        return {"authorizationData": [{"authorizationToken": token, "proxyEndpoint": endpoint}]}


def client(service: str, **_kwargs: object) -> _S3 | _ECR:
    assert service in ("s3", "ecr"), service
    return _S3() if service == "s3" else _ECR()
