"""A biffix Rust Lambda: the `bootstrap` binary cargo-lambda builds, in the private subnets."""

from __future__ import annotations

from typing import Any

from aws_cdk import Duration
from aws_cdk import aws_ec2 as ec2
from aws_cdk import aws_lambda as lambda_
from constructs import Construct

from biffix_infra.wiring import log_group


class RustFunction(lambda_.Function):
    """The Lambda running `code` (a `rust_lambda_code` artifact; the stacks name the binaries
    they deploy) on the provided AL2023 runtime, in `vpc`'s private subnets (it reaches Aurora
    there, and the internet through the NAT), logging to a log group of its own with biffix's
    retention. The log group is `scope`'s child `logs_id` (by default `<cid>Logs`). Every other
    `lambda_.Function` argument passes through."""

    def __init__(
        self,
        scope: Construct,
        cid: str,
        *,
        code: lambda_.Code,
        vpc: ec2.IVpc,
        timeout: Duration,
        logs_id: str | None = None,
        **kwargs: Any,
    ) -> None:
        super().__init__(
            scope,
            cid,
            runtime=lambda_.Runtime.PROVIDED_AL2023,
            handler="bootstrap",
            code=code,
            vpc=vpc,
            vpc_subnets=ec2.SubnetSelection(subnet_type=ec2.SubnetType.PRIVATE_WITH_EGRESS),
            timeout=timeout,
            log_group=log_group(scope, logs_id or f"{cid}Logs"),
            **kwargs,
        )
