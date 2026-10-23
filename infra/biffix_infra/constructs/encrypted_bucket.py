"""biffix's retained, customer-key-encrypted S3 buckets (the reference, image archive and project
buckets) and their keys."""

from __future__ import annotations

from typing import Any

from aws_cdk import RemovalPolicy
from aws_cdk import aws_kms as kms
from aws_cdk import aws_s3 as s3
from constructs import Construct


def customer_key(scope: Construct, cid: str, key_arn: str | None) -> kms.IKey:
    """BYOK: the deployer's key when `key_arn` is set, else a biffix-managed CMK (rotated, and
    RETAIN like the data it encrypts). An imported key is owned/rotated/audited by the deployer
    (manage-AWS-through-AWS); biffix never sets a removal policy on it."""
    if key_arn:
        return kms.Key.from_key_arn(scope, cid, key_arn)
    return kms.Key(scope, cid, enable_key_rotation=True, removal_policy=RemovalPolicy.RETAIN)


class EncryptedBucket(s3.Bucket):
    """A bucket encrypted with `encryption_key`, reachable only over TLS 1.2+, versioned, with no
    public access, and RETAIN: it holds data biffix cannot rebuild. Every other `s3.Bucket`
    argument (lifecycle rules, CORS, object lock, ...) passes through."""

    def __init__(
        self, scope: Construct, cid: str, *, encryption_key: kms.IKey, **kwargs: Any
    ) -> None:
        super().__init__(
            scope,
            cid,
            encryption=s3.BucketEncryption.KMS,
            encryption_key=encryption_key,
            enforce_ssl=True,
            minimum_tls_version=1.2,
            versioned=True,
            block_public_access=s3.BlockPublicAccess.BLOCK_ALL,
            removal_policy=RemovalPolicy.RETAIN,
            **kwargs,
        )
