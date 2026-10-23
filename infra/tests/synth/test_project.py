"""The project stack: a created or imported bucket and its key, and the run role."""

import aws_cdk as cdk
from aws_cdk.assertions import Template

from biffix_infra.stacks.identity_stack import IdentityStack
from biffix_infra.stacks.project_stack import ProjectStack
from tests.synth.synth_helpers import (
    _actions,
    _resources,
    _statements,
)


def test_project_stack_imports_existing_bucket_sse_s3_no_cmk() -> None:
    app = cdk.App()
    ident = IdentityStack(app, "ident")
    proj = ProjectStack(
        app,
        "proj",
        project_id="imported",
        permissions_boundary=ident.permissions_boundary,
        bucket_name="acme-imported-data",
    )
    t = Template.from_stack(proj)
    t.resource_count_is("AWS::S3::Bucket", 0)  # imported, not created
    t.resource_count_is("AWS::KMS::Key", 0)  # SSE-S3, no CMK
    (role,) = t.find_resources("AWS::IAM::Role").values()  # the run base role still exists
    # Batch delivers it to the run's tasks; nothing assumes it with STS, so only ECS is trusted.
    (trust,) = role["Properties"]["AssumeRolePolicyDocument"]["Statement"]
    assert trust["Principal"] == {"Service": "ecs-tasks.amazonaws.com"}, trust


def test_project_stack_imports_existing_bucket_with_kms() -> None:
    app = cdk.App()
    ident = IdentityStack(app, "ident")
    proj = ProjectStack(
        app,
        "proj",
        project_id="imported",
        permissions_boundary=ident.permissions_boundary,
        bucket_name="acme-imported-data",
        key_arn="arn:aws:kms:us-east-1:111122223333:key/imported-1234",
    )
    t = Template.from_stack(proj)
    t.resource_count_is("AWS::S3::Bucket", 0)
    t.resource_count_is("AWS::KMS::Key", 0)
    _assert_run_role_encrypts_with(t, "arn:aws:kms:us-east-1:111122223333:key/imported-1234")


def test_project_stack_byok_imports_key_no_cmk() -> None:
    app = cdk.App()
    ident = IdentityStack(app, "ident")
    arn = "arn:aws:kms:us-east-1:111122223333:key/proj-9999"
    proj = ProjectStack(
        app,
        "proj",
        project_id="demo",
        permissions_boundary=ident.permissions_boundary,
        key_arn=arn,
    )
    t = Template.from_stack(proj)
    t.resource_count_is("AWS::KMS::Key", 0)
    _assert_run_role_encrypts_with(t, arn)


def _assert_run_role_encrypts_with(t: Template, key_arn: str) -> None:
    """The project's run role (its only role) may encrypt and decrypt with `key_arn`, and with no
    other key: without the grant every run on the KMS bucket fails with AccessDenied."""
    kms_statements = [
        st for st in _statements(t) if any(a.startswith("kms:") for a in _actions(st))
    ]
    assert kms_statements, "the run role cannot use its project's key"
    for st in kms_statements:
        assert _resources(st) == [key_arn], st
    granted = {a for st in kms_statements for a in _actions(st)}
    assert {"kms:Decrypt", "kms:Encrypt", "kms:GenerateDataKey*"} <= granted, granted
