"""The data stack: encrypted, retained buckets and the ECR repositories' tag rules."""

import json

import aws_cdk as cdk
from aws_cdk.assertions import Template

from biffix_infra.app_graph import Stacks
from biffix_infra.config import ImageArchive
from biffix_infra.stacks.data_stack import (
    BUILDS_MUTABLE_TAGS,
    WORKFLOW_IMMUTABLE_TAGS,
    DataStack,
)
from tests.synth.synth_helpers import (
    _actions,
    _resources,
)


def test_data_buckets_retain_and_encrypt() -> None:
    app = cdk.App()
    data = DataStack(app, "data")
    t = Template.from_stack(data)
    repos = t.find_resources("AWS::ECR::Repository")
    # The coordinator, workflow and builds repositories.
    assert len(repos) == 3, repos
    for r in repos.values():
        assert "LifecyclePolicy" in r["Properties"], r["Properties"].keys()
    # The shared reference bucket and the image archive (events.jsonl lives in the per-project
    # bucket, not here); both are kept when the stack goes.
    buckets = t.find_resources("AWS::S3::Bucket")
    assert len(buckets) == 2, buckets.keys()
    assert all(b["DeletionPolicy"] == "Retain" for b in buckets.values())
    # Each bucket is encrypted with its OWN customer-managed key (not SSE-S3, not the other's key),
    # and expires its own superseded versions.
    keys = t.find_resources("AWS::KMS::Key")
    own_key = {"ReferenceBucket": "ReferenceKey", "ImageArchiveBucket": "ImageArchiveKey"}
    for name, key_name in own_key.items():
        (lid,) = [b for b in buckets if b.startswith(name)]
        props = buckets[lid]["Properties"]
        (sse,) = props["BucketEncryption"]["ServerSideEncryptionConfiguration"]
        default = sse["ServerSideEncryptionByDefault"]
        assert default["SSEAlgorithm"] == "aws:kms", (lid, sse)
        (key_id,) = [k for k in keys if k.startswith(key_name)]
        assert default["KMSMasterKeyID"] == {"Fn::GetAtt": [key_id, "Arn"]}, (lid, sse)
        rules = props["LifecycleConfiguration"]["Rules"]
        assert any("NoncurrentVersionExpiration" in r for r in rules), (lid, rules)


def test_only_the_image_archive_carries_the_archive_role_tag() -> None:
    # teardown.sh keeps the archive by this tag once its stack is gone: its generated name is
    # truncated and cannot be told apart reliably.
    t = Template.from_stack(DataStack(cdk.App(), "data"))
    roles = {
        lid: [kv["Value"] for kv in b["Properties"].get("Tags", []) if kv["Key"] == "biffix:role"]
        for lid, b in t.find_resources("AWS::S3::Bucket").items()
    }
    assert {lid.rstrip("0123456789ABCDEF"): v for lid, v in roles.items()} == {
        "ImageArchiveBucket": ["image-archive"],
        "ReferenceBucket": [],
    }, roles


def test_data_stack_byok_imports_key_no_cmk_created() -> None:
    app = cdk.App()
    arn = "arn:aws:kms:us-east-1:111122223333:key/abcd-1234"
    data = DataStack(
        app,
        "data",
        reference_key_arn=arn,
        image_archive=ImageArchive(key_arn=arn.replace("abcd", "efgh")),
    )
    t = Template.from_stack(data)
    t.resource_count_is("AWS::KMS::Key", 0)


def test_data_stack_creates_cmk_without_byok() -> None:
    app = cdk.App()
    data = DataStack(app, "data")
    t = Template.from_stack(data)
    # The reference bucket's and the image archive's.
    t.resource_count_is("AWS::KMS::Key", 2)


def test_every_bucket_refuses_plaintext_and_old_tls_and_every_key_rotates(
    default_stacks: Stacks,
) -> None:
    """HIPAA-ready transport and key hygiene: every bucket biffix creates denies any
    request not over TLS, or over TLS older than 1.2, on the bucket and its objects; every CMK it
    creates rotates yearly."""
    stacks = [
        default_stacks["data"],
        default_stacks["edge"],
        *default_stacks["projects"],
    ]
    seen = 0
    for stack in stacks:
        t = Template.from_stack(stack)
        policies = {
            p["Properties"]["Bucket"]["Ref"]: p["Properties"]["PolicyDocument"]["Statement"]
            for p in t.find_resources("AWS::S3::BucketPolicy").values()
        }
        for lid in t.find_resources("AWS::S3::Bucket"):
            seen += 1
            denies = [st for st in policies[lid] if st["Effect"] == "Deny"]
            conditions = {json.dumps(st["Condition"], sort_keys=True): st for st in denies}
            for condition in (
                {"Bool": {"aws:SecureTransport": "false"}},
                {"NumericLessThan": {"s3:TlsVersion": 1.2}},
            ):
                st = conditions.get(json.dumps(condition, sort_keys=True))
                assert st is not None, (stack.stack_name, lid, condition, denies)
                assert _actions(st) == {"s3:*"}, st
                assert st["Principal"] == {"AWS": "*"}, st
                assert sorted(_resources(st)) == [f"{{{lid}.Arn}}", f"{{{lid}.Arn}}/*"], st
    # The data stack's two buckets, the UI bucket and each project's bucket.
    assert seen == 3 + len(default_stacks["projects"]), seen
    keys = [
        k
        for stack in (default_stacks["data"], *default_stacks["projects"])
        for k in Template.from_stack(stack).find_resources("AWS::KMS::Key").values()
    ]
    assert len(keys) == 2 + len(default_stacks["projects"]), keys
    assert all(k["Properties"]["EnableKeyRotation"] is True for k in keys), keys


def test_workflow_repo_tags_biffix_trusts_are_immutable() -> None:
    t = Template.from_stack(DataStack(cdk.App(), "data"))
    repos = t.find_resources("AWS::ECR::Repository")
    (workflow,) = [r["Properties"] for lid, r in repos.items() if lid.startswith("WorkflowRepo")]
    # Every other tag stays mutable for workflow CI; these can never be moved once written.
    assert workflow["ImageTagMutability"] == "MUTABLE_WITH_EXCLUSION"
    filters = workflow["ImageTagMutabilityExclusionFilters"]
    assert {f["ImageTagMutabilityExclusionFilterValue"] for f in filters} == {
        "biffix-keep-*",
        "import-*",
        "derived-*",
        "restore-*",
    }
    assert {f["ImageTagMutabilityExclusionFilterType"] for f in filters} == {"WILDCARD"}
    assert len(WORKFLOW_IMMUTABLE_TAGS) <= 5  # ECR's limit
    (coordinator,) = [r["Properties"] for lid, r in repos.items() if lid.startswith("Coord")]
    assert coordinator["ImageTagMutability"] == "IMMUTABLE"


def test_the_builds_repo_is_write_once_but_scratch_and_kept_like_the_workflow_repo() -> None:
    t = Template.from_stack(DataStack(cdk.App(), "data"))
    repos = t.find_resources("AWS::ECR::Repository")
    (builds,) = [r["Properties"] for lid, r in repos.items() if lid.startswith("BuildsRepo")]
    (workflow,) = [r["Properties"] for lid, r in repos.items() if lid.startswith("WorkflowRepo")]
    # Every tag is written once, except the scratch tags an image made in steps is assembled under.
    assert builds["ImageTagMutability"] == "IMMUTABLE_WITH_EXCLUSION"
    assert [
        (f["ImageTagMutabilityExclusionFilterType"], f["ImageTagMutabilityExclusionFilterValue"])
        for f in builds["ImageTagMutabilityExclusionFilters"]
    ] == [("WILDCARD", "scratch-*")]
    assert BUILDS_MUTABLE_TAGS == ("scratch-*",)
    # Kept, scanned and encrypted exactly as the workflow repository: its lifecycle only expires
    # untagged images (keep-tags hold what a version pins), and both are retained.
    for key in ("LifecyclePolicy", "ImageScanningConfiguration", "EncryptionConfiguration"):
        assert builds.get(key) == workflow.get(key), key
    assert builds["LifecyclePolicy"] is not None
    kept = {
        r.get("DeletionPolicy")
        for lid, r in repos.items()
        if lid.startswith(("BuildsRepo", "WorkflowRepo"))
    }
    assert kept == {"Retain"}
