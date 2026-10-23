"""The persistence stack: Aurora and the capability secret."""

import dataclasses
import json
from pathlib import Path

from aws_cdk import Stack
from aws_cdk import aws_lambda as lambda_
from aws_cdk.assertions import Match, Template

from biffix_infra.app_graph import Stacks
from tests.synth.synth_helpers import (
    CONFIG,
    _actions,
    _build,
    _logical_id,
    _render,
    _resources,
    _statements,
    _statements_for,
)


def test_persistence_owns_aurora_and_capability_secret(tmp_path: Path) -> None:
    # A NON-default aurora_backup_retention_days proves the retention tracks config rather than a
    # hardcoded 14-day literal (a default-valued assertion could not tell the two apart).
    stacks = _build(tmp_path, config=dataclasses.replace(CONFIG, aurora_backup_retention_days=30))
    t = Template.from_stack(stacks["persistence"])
    t.resource_count_is("AWS::RDS::DBCluster", 1)
    t.has_resource("AWS::RDS::DBCluster", {"DeletionPolicy": "Retain"})
    # Backup depth for the system of record is an explicit choice (here overridden to 30
    # days), and deletion protection is a second guard independent of the RETAIN removal policy.
    t.has_resource_properties(
        "AWS::RDS::DBCluster",
        # Encrypted at rest.
        Match.object_like(
            {"BackupRetentionPeriod": 30, "DeletionProtection": True, "StorageEncrypted": True}
        ),
    )
    # The credentials secret Aurora uses + the capability signing key.
    t.has_resource_properties(
        "AWS::SecretsManager::Secret",
        Match.object_like(
            {"Description": Match.string_like_regexp(".*capability token signing key.*")}
        ),
    )


def _kms_key_ids(t: Template) -> dict[str, object]:
    """The KmsKeyId of the cluster and of each secret, by logical id."""
    return {
        lid: r["Properties"].get("KmsKeyId")
        for kind in ("AWS::RDS::DBCluster", "AWS::SecretsManager::Secret")
        for lid, r in t.find_resources(kind).items()
    }


def test_the_event_store_and_its_secrets_use_a_customer_managed_key(default_stacks: Stacks) -> None:
    # The event log is the store most likely to hold regulated data: a key the deployer owns, not
    # the AWS-managed aws/rds and aws/secretsmanager keys.
    t = Template.from_stack(default_stacks["persistence"])
    (key,) = t.find_resources("AWS::KMS::Key", {"DeletionPolicy": "Retain"})
    assert t.find_resources("AWS::KMS::Key")[key]["Properties"]["EnableKeyRotation"] is True
    ids = _kms_key_ids(t)
    assert len(ids) == 3, ids  # the cluster, its credentials secret, the capability key
    assert all(v == {"Fn::GetAtt": [key, "Arn"]} for v in ids.values()), ids
    # Each secret's reader decrypts it through Secrets Manager only, by its own role's policy.
    for name in ("ingest", "api"):
        decrypts = [
            st
            for st in _statements(Template.from_stack(default_stacks[name]))
            if "kms:Decrypt" in _actions(st) and any("EventStoreKey" in r for r in _resources(st))
        ]
        assert decrypts, name
        assert all(
            _render(st["Condition"]["StringEquals"]["kms:ViaService"])
            == "secretsmanager.{Ref:AWS::Region}.amazonaws.com"
            for st in decrypts
        ), decrypts


def test_the_event_store_key_can_be_the_deployer_s_own(tmp_path: Path) -> None:
    arn = "arn:aws:kms:us-east-1:123456789012:key/byok"
    stacks = _build(tmp_path, config=dataclasses.replace(CONFIG, aurora_key_arn=arn))
    t = Template.from_stack(stacks["persistence"])
    t.resource_count_is("AWS::KMS::Key", 0)
    assert set(_kms_key_ids(t).values()) == {arn}


def test_consumers_import_the_db_secret_not_the_cluster_s_attachment(
    default_stacks: Stacks,
) -> None:
    # The cluster's SecretTargetAttachment changes its Ref whenever the cluster is replaced, and
    # CloudFormation refuses to change an export another stack imports, so a consumer importing it
    # would block every cluster replacement. They import the credentials secret's own ARN.
    persistence = default_stacks["persistence"]
    t = Template.from_stack(persistence).to_json()
    secret = _logical_id(persistence, persistence.db_secret)
    assert t["Resources"][secret]["Type"] == "AWS::SecretsManager::Secret"
    exports = {o["Export"]["Name"]: o["Value"] for o in t["Outputs"].values()}
    (secret_export,) = [n for n, v in exports.items() if v == {"Ref": secret}]
    attachments = {
        lid
        for lid, r in t["Resources"].items()
        if r["Type"] == "AWS::SecretsManager::SecretTargetAttachment"
    }
    assert attachments
    attachment_exports = {
        n for n, v in exports.items() if any(lid in _render(v) for lid in attachments)
    }
    imported = f"{{Import:{secret_export}}}"
    ingest, api = default_stacks["ingest"], default_stacks["api"]
    consumers: list[tuple[Stack, list[lambda_.Function]]] = [
        (ingest, [ingest.ingest_lambda, ingest.reconcile_lambda]),
        (api, [api.backend_lambda, api.sweep_lambda]),
    ]
    for stack, fns in consumers:
        consumer = Template.from_stack(stack)
        functions = consumer.find_resources("AWS::Lambda::Function")
        for fn in fns:
            env = functions[_logical_id(stack, fn)]["Properties"]["Environment"]["Variables"]
            assert _render(env["BIFFIX_DB_SECRET_ARN"]) == imported, fn.node.path
            reads = {
                r
                for st in _statements_for(consumer, stack, fn.role)
                if "secretsmanager:GetSecretValue" in _actions(st)
                for r in _resources(st)
            }
            assert imported in reads, fn.node.path
        rendered = json.dumps(consumer.to_json())
        assert not any(e in rendered for e in attachment_exports), stack.stack_name
