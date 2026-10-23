"""The permissions boundary on the roles a workflow's own code or a third party's CI acts as: each
project's run base role and coordinator role, and the GitHub-OIDC registration and image-build
roles. The Lambda, CodeBuild and Batch instance roles, which run only biffix's code, carry none."""

from __future__ import annotations

from aws_cdk import Stack
from aws_cdk import aws_iam as iam
from constructs import Construct

# The IAM privilege-escalation surface no bounded role may touch. biffix never mutates IAM at
# runtime: roles and boundaries are all provisioned in CDK.
_IAM_ESCALATION = [
    "iam:CreateUser",
    "iam:CreateRole",
    "iam:CreatePolicy",
    "iam:CreatePolicyVersion",
    "iam:SetDefaultPolicyVersion",
    "iam:CreateAccessKey",
    "iam:CreateLoginProfile",
    "iam:UpdateLoginProfile",
    "iam:AttachUserPolicy",
    "iam:AttachRolePolicy",
    "iam:AttachGroupPolicy",
    "iam:PutUserPolicy",
    "iam:PutRolePolicy",
    "iam:PutGroupPolicy",
    "iam:UpdateAssumeRolePolicy",
    "iam:CreateInstanceProfile",
    "iam:AddRoleToInstanceProfile",
    "iam:PutRolePermissionsBoundary",
    "iam:DeleteRolePermissionsBoundary",
    "iam:PutUserPermissionsBoundary",
    "iam:DeleteUserPermissionsBoundary",
]
# The services the bounded roles use (verified against the SDK call sites: Batch for the
# coordinator; S3 + KMS for the run and coordinator roles; ECR for the GitHub-OIDC roles' pushes and
# Batch-on-EC2 workers; the rest for what the engines and tasks call).
_SERVICES = [
    "sts:AssumeRole",
    "sts:TagSession",
    "batch:*",
    "s3:*",
    "kms:Decrypt",
    "kms:Encrypt",
    "kms:GenerateDataKey",
    "kms:DescribeKey",
    "logs:*",
    "sqs:*",
    "secretsmanager:GetSecretValue",
    "ecr:*",
    "ecs:*",
    "ec2:Describe*",
    "ec2:CreateTags",
]


class PermissionsBoundary(iam.ManagedPolicy):
    """A ceiling on what any biffix-managed role can ever acquire. The ALLOW is scoped to the
    concrete services biffix roles use. A boundary is a ceiling not a grant, so it stays
    per-service (action "*") rather than per-action — tightening to per-action risks silently
    capping a legitimate call."""

    def __init__(self, scope: Construct, cid: str) -> None:
        super().__init__(
            scope,
            cid,
            statements=[
                # Deny the full IAM privilege-escalation surface from any bounded role. Closes
                # vectors a short denylist misses (Put/Attach RolePolicy, CreateRole, ...), incl.
                # removing one's own boundary.
                iam.PolicyStatement(
                    effect=iam.Effect.DENY, actions=_IAM_ESCALATION, resources=["*"]
                ),
                # The coordinator passes a job role to the child jobs it registers with Batch
                # (the Snakemake AWS Batch executor sets jobRoleArn). PassRole is a ceiling
                # here, scoped to biffix-managed roles so a bounded role can never pass an
                # arbitrary (e.g. admin) role to a service.
                iam.PolicyStatement(
                    effect=iam.Effect.ALLOW,
                    actions=["iam:PassRole"],
                    resources=[
                        Stack.of(scope).format_arn(
                            service="iam", region="", resource="role", resource_name="biffix-*"
                        )
                    ],
                ),
                iam.PolicyStatement(effect=iam.Effect.ALLOW, actions=_SERVICES, resources=["*"]),
            ],
        )
