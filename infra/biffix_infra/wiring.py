"""Wiring the stacks share: a Lambda's Aurora connection and secret reads, and log groups.

Kept out of any one stack module so a stack never imports another stack's module for a helper.
"""

from __future__ import annotations

from aws_cdk import RemovalPolicy, Stack
from aws_cdk import aws_ec2 as ec2
from aws_cdk import aws_iam as iam
from aws_cdk import aws_lambda as lambda_
from aws_cdk import aws_logs as logs
from aws_cdk import aws_rds as rds
from aws_cdk import aws_secretsmanager as secretsmanager
from constructs import Construct

# How long the Lambdas' and builds' CloudWatch logs are kept. Logs are encrypted at rest by
# CloudWatch Logs itself (a HIPAA-eligible service); they carry opaque run/sample ids and no data,
# so they use the service key rather than a CMK whose key policy every writer would need.
LOG_RETENTION = logs.RetentionDays.ONE_YEAR


def wire_db(fn: lambda_.Function, db: rds.DatabaseCluster, secret: secretsmanager.ISecret) -> None:
    """Connect `fn` to Aurora: `secret`'s ARN as `BIFFIX_DB_SECRET_ARN` (the Lambda derives
    DATABASE_URL from it at startup), read on that secret, and a tcp/5432 security-group rule.

    `secret` is the cluster's credentials secret itself (`PersistenceStack.db_secret`), never
    `db.secret`: that is the cluster's SecretTargetAttachment, whose Ref (and so the export the
    consumer imports) changes when the cluster is replaced, which CloudFormation refuses.

    The SG rule is authored from the CONSUMER side so it lands in the consumer's stack, referencing
    the imported Aurora SG: `db.connections.allow_default_port_from(fn)` would add it to the Aurora
    SG in the persistence stack instead, making persistence depend on the consumer (a cycle with the
    secret grant). The port is EXPLICIT (Aurora Postgres = 5432): the cluster's default-port token
    lives in the persistence stack and cannot be inlined into a cross-stack rule's FromPort/ToPort,
    which CloudFormation rejects for tcp.
    """
    fn.add_environment("BIFFIX_DB_SECRET_ARN", secret.secret_arn)
    grant_secret_read(fn, secret)
    fn.connections.allow_to(db, ec2.Port.tcp(5432), "Aurora Postgres")


def grant_secret_read(fn: lambda_.Function, secret: secretsmanager.ISecret) -> None:
    """Let `fn` read `secret`, and decrypt it with its customer-managed key through Secrets
    Manager. Both go on `fn`'s own role: `secret.grant_read` would also name the role in the
    key's policy, in the key's (persistence) stack, making it depend on the consumer's (a cycle).
    The key policy trusts the account's IAM, so the role's policy alone admits it."""
    fn.add_to_role_policy(
        iam.PolicyStatement(
            actions=["secretsmanager:GetSecretValue", "secretsmanager:DescribeSecret"],
            resources=[secret.secret_arn],
        )
    )
    if secret.encryption_key is not None:
        fn.add_to_role_policy(
            iam.PolicyStatement(
                actions=["kms:Decrypt"],
                resources=[secret.encryption_key.key_arn],
                conditions={
                    "StringEquals": {
                        "kms:ViaService": f"secretsmanager.{Stack.of(fn).region}.amazonaws.com"
                    }
                },
            )
        )


def log_group(scope: Construct, cid: str) -> logs.LogGroup:
    """A Lambda's or a build's log group, with a retention (the group Lambda creates by itself
    never expires). They are diagnostics, not records, so the group goes with the stack."""
    return logs.LogGroup(scope, cid, retention=LOG_RETENTION, removal_policy=RemovalPolicy.DESTROY)


def any_ecr_repository_arn(stack: Stack) -> str:
    """Any ECR repository, in any region or account of `stack`'s partition (a pull's repository
    policy still decides)."""
    return stack.format_arn(
        service="ecr", region="*", account="*", resource="repository", resource_name="*"
    )
