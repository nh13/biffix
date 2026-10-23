"""The logical ids a deploy must not change: a resource whose logical id changes is replaced, and
these hold the tenant's data (RETAIN: replacing one orphans the old and starts an empty new one) or
are looked up by their logical id (deploy.sh, teardown.sh, image_restore.py). Moving one into a
construct changes its id unless it stays its scope's `Default` child; this test catches that."""

import pytest
from aws_cdk.assertions import Template

from biffix_infra.app_graph import Stacks
from tests.synth.synth_helpers import _single_stacks

# (stack, resource type, logical id, whether CloudFormation retains it).
STABLE = [
    # deploy.sh reads ReferenceBucket* and CoordinatorRepo*, teardown.sh and image_restore.py
    # ImageArchiveBucket*.
    ("data", "AWS::KMS::Key", "ReferenceKeyFC159260", True),
    ("data", "AWS::S3::Bucket", "ReferenceBucket7BB82C12", True),
    ("data", "AWS::KMS::Key", "ImageArchiveKeyDFE11203", True),
    ("data", "AWS::S3::Bucket", "ImageArchiveBucket13FCE476", True),
    ("data", "AWS::ECR::Repository", "CoordinatorRepoB4C9D7CE", True),
    ("data", "AWS::ECR::Repository", "WorkflowRepo0035F18F", True),
    ("data", "AWS::ECR::Repository", "BuildsRepo0F370AFF", True),
    ("project", "AWS::KMS::Key", "ProjectKeyFDD03260", True),
    ("project", "AWS::S3::Bucket", "ProjectBucket01EBB8FB", True),
    ("identity", "AWS::Cognito::UserPool", "UserPool6BA7E5F2", True),
    ("persistence", "AWS::KMS::Key", "EventStoreKey037088EE", True),
    ("persistence", "AWS::RDS::DBCluster", "EventStore165FCE28", True),
    ("persistence", "AWS::RDS::DBInstance", "EventStorewriterAF9E43B7", True),
    ("persistence", "AWS::RDS::DBSubnetGroup", "EventStoreSubnets8957A887", True),
    ("persistence", "AWS::SecretsManager::Secret", "EventStoreSecretA632A5EE", True),
    ("persistence", "AWS::SecretsManager::Secret", "CapabilitySigningKey32A2424E", True),
    # A replaced distribution could not take over the app domain's alias in place.
    ("edge", "AWS::CloudFront::Distribution", "Edge7038544F", False),
]


@pytest.mark.parametrize(("stack", "rtype", "logical_id", "retained"), STABLE)
def test_stateful_resources_keep_their_logical_ids(
    default_stacks: Stacks, stack: str, rtype: str, logical_id: str, retained: bool
) -> None:
    target = (
        default_stacks["projects"][0]
        if stack == "project"
        else _single_stacks(default_stacks)[stack]
    )
    resources = Template.from_stack(target).find_resources(rtype)
    assert logical_id in resources, sorted(resources)
    if retained:
        assert resources[logical_id]["DeletionPolicy"] == "Retain"


def test_every_retained_resource_is_pinned(default_stacks: Stacks) -> None:
    # A new RETAIN resource joins STABLE, so its logical id is guarded from the start.
    pinned = {(s, lid) for s, _, lid, retained in STABLE if retained}
    stacks = _single_stacks(default_stacks)
    stacks["project"] = default_stacks["projects"][0]
    retained = {
        (name, lid)
        for name, st in stacks.items()
        for lid, r in Template.from_stack(st).to_json()["Resources"].items()
        if r.get("DeletionPolicy") == "Retain"
    }
    assert retained == pinned
