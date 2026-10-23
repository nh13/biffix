"""A role GitHub Actions workflows assume with the account's GitHub OIDC provider."""

from __future__ import annotations

from aws_cdk import Stack
from aws_cdk import aws_iam as iam
from constructs import Construct

_TOKEN = "token.actions.githubusercontent.com"


class GithubOidcRole(iam.Role):
    """A role (under `permissions_boundary`) a GitHub Actions workflow assumes with its OIDC token.
    The trust requires the `aud` claim AWS's STS audience (identical for every GitHub workflow, so
    never the only condition), each `string_equals` claim exactly, and each `string_like` claim
    (`sub` patterns) by pattern. The OIDC provider is an account-level prerequisite
    (infra/README.md), referenced, not created."""

    def __init__(
        self,
        scope: Construct,
        cid: str,
        *,
        permissions_boundary: iam.IManagedPolicy,
        string_equals: dict[str, str] | None = None,
        string_like: dict[str, list[str]] | None = None,
    ) -> None:
        provider = Stack.of(scope).format_arn(
            service="iam", region="", resource="oidc-provider", resource_name=_TOKEN
        )
        conditions: dict[str, dict[str, object]] = {
            "StringEquals": {
                f"{_TOKEN}:aud": "sts.amazonaws.com",
                **{f"{_TOKEN}:{claim}": v for claim, v in (string_equals or {}).items()},
            }
        }
        if string_like:
            conditions["StringLike"] = {f"{_TOKEN}:{claim}": v for claim, v in string_like.items()}
        super().__init__(
            scope,
            cid,
            assumed_by=iam.WebIdentityPrincipal(provider, conditions=conditions),
            permissions_boundary=permissions_boundary,
        )
