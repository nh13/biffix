"""The compute stack: a queue and compute environment per queue-class, and the hosts."""

import dataclasses
import json
from pathlib import Path

import aws_cdk as cdk
import pytest
from aws_cdk import aws_s3 as s3
from aws_cdk.assertions import Match, Template

from biffix_infra.app_graph import Stacks
from biffix_infra.config import (
    HOST_AWS_CLI_PATH,
    DeployConfig,
    QueueClass,
)
from biffix_infra.stacks import compute_stack
from biffix_infra.stacks.compute_stack import ComputeStack, host_install_header
from biffix_infra.stacks.data_stack import DataStack
from biffix_infra.stacks.network_stack import NetworkStack
from tests.deploy_config_file import write_config
from tests.helpers import REPO_ROOT
from tests.synth.synth_helpers import (
    CONFIG,
    _build,
    _grants,
    _logical_id,
    _statements,
)


def _host_bucket(app: cdk.App) -> s3.IBucket:
    """The bucket hosts fetch their boot-time installs from (the reference bucket in app.py)."""
    return DataStack(app, "data").reference_bucket


def test_compute_makes_a_queue_per_class_and_no_job_definition() -> None:
    app = cdk.App()
    net = NetworkStack(app, "net")
    compute = ComputeStack(
        app, "compute", vpc=net.vpc, config=CONFIG, host_bucket=_host_bucket(app)
    )
    t = Template.from_stack(compute)
    t.resource_count_is("AWS::Batch::JobQueue", len(CONFIG.queue_classes))
    # The coordinator job definition lives in the api stack, and worker jobs run on job definitions
    # the engines' executors register: compute owns none.
    t.resource_count_is("AWS::Batch::JobDefinition", 0)


def test_compute_accepts_exact_instance_types_and_launch_template() -> None:
    cfg = DeployConfig(
        tenant="dev",
        region="us-east-1",
        projects=[],
        queue_classes=[
            QueueClass(
                name="sort-xl",
                capacity="ON_DEMAND",
                instances=["c7g.4xlarge"],
                ebs_gb=1000,
                ebs_throughput=1000,
                ebs_iops=16000,
                imds_hop_limit=2,
            ),
            QueueClass(name="coordinator", capacity="ON_DEMAND", coordinator=True),
        ],
    )
    app = cdk.App()
    net = NetworkStack(app, "net")
    compute = ComputeStack(app, "compute", vpc=net.vpc, config=cfg, host_bucket=_host_bucket(app))
    t = Template.from_stack(compute)
    t.has_resource_properties(
        "AWS::EC2::LaunchTemplate",
        Match.object_like(
            {
                "LaunchTemplateData": Match.object_like(
                    {"MetadataOptions": Match.object_like({"HttpPutResponseHopLimit": 2})}
                )
            }
        ),
    )
    t.has_resource_properties(
        "AWS::Batch::ComputeEnvironment",
        Match.object_like(
            {
                "ComputeResources": Match.object_like(
                    {"InstanceTypes": Match.array_with(["c7g.4xlarge"])}
                )
            }
        ),
    )


def test_compute_family_map_comes_from_config() -> None:
    # Instance families live in config, not a hardcoded map. A family entry resolves to the
    # bare instance CLASS (Batch picks the size), never a pinned "<class>.xlarge".
    cfg = dataclasses.replace(CONFIG, instance_families={**CONFIG.instance_families, "c": "c7i"})
    app = cdk.App()
    net = NetworkStack(app, "net")
    compute = ComputeStack(app, "compute", vpc=net.vpc, config=cfg, host_bucket=_host_bucket(app))
    t = Template.from_stack(compute)
    t.has_resource_properties(
        "AWS::Batch::ComputeEnvironment",
        Match.object_like(
            {"ComputeResources": Match.object_like({"InstanceTypes": Match.array_with(["c7i"])})}
        ),
    )
    # And it is the bare family, never a pinned size — pinning would cap the queue at one size.
    envs = t.find_resources("AWS::Batch::ComputeEnvironment")
    all_types = [
        it
        for e in envs.values()
        for it in e["Properties"]["ComputeResources"].get("InstanceTypes", [])
    ]
    assert "c7i.xlarge" not in all_types, all_types


def test_compute_rejects_a_spot_coordinator() -> None:
    bad = DeployConfig(
        tenant="dev",
        region="us-east-1",
        projects=[],
        queue_classes=[QueueClass(name="coordinator", capacity="SPOT", coordinator=True)],
    )
    app = cdk.App()
    net = NetworkStack(app, "net")
    with pytest.raises(ValueError, match="coordinator"):
        ComputeStack(app, "compute", vpc=net.vpc, config=bad, host_bucket=_host_bucket(app))


def test_every_host_installs_the_pinned_aws_cli_from_the_reference_bucket(
    default_stacks: Stacks,
) -> None:
    t = Template.from_stack(default_stacks["compute"])
    envs = t.find_resources("AWS::Batch::ComputeEnvironment")
    templates = t.find_resources("AWS::EC2::LaunchTemplate")
    assert len(envs) == len(CONFIG.queue_classes)
    cli = CONFIG.host_aws_cli
    for env in envs.values():
        lt = env["Properties"]["ComputeResources"]["LaunchTemplate"]
        lt_id = lt["LaunchTemplateId"]["Ref"]
        # Pinned to the latest version, so a user-data change reaches new hosts.
        assert lt["Version"] == {"Fn::GetAtt": [lt_id, "LatestVersionNumber"]}
        user_data = json.dumps(templates[lt_id]["Properties"]["LaunchTemplateData"]["UserData"])
        assert "multipart/mixed" in user_data
        # Each value bound to the variable the boot script reads (a swapped per-arch checksum or a
        # renamed variable would fail every host's install).
        for assignment in (
            f"BIFFIX_AWS_CLI_SHA256_X86_64={cli.sha256_x86_64}\\n",
            f"BIFFIX_AWS_CLI_SHA256_AARCH64={cli.sha256_aarch64}\\n",
            f"BIFFIX_AWS_CLI_KEY_PREFIX=host/aws-cli/{cli.version}/\\n",
            "BIFFIX_AWS_CLI_DIR=/opt/biffix/aws-cli\\n",
            f"BIFFIX_REGION={CONFIG.region}\\n",
        ):
            assert assignment in user_data, assignment
        assert "biffix_install_host_aws_cli" in user_data
    # The boot script reads exactly the variables the stack sets (its own test runs the script
    # against `host_install_header`, the one source of these lines).
    script = (Path(compute_stack.__file__).parent.parent / "host/install_aws_cli.sh").read_text()
    header = host_install_header(cli, bucket_name="b", region="r")
    for name in (line.split("=", 1)[0] for line in header):
        assert f"${name}" in script or f"${{{name}" in script, name


def test_every_host_boots_with_an_encrypted_root_and_imdsv2_only(
    default_stacks: Stacks, tmp_path: Path
) -> None:
    t = Template.from_stack(default_stacks["compute"])
    templates = t.find_resources("AWS::EC2::LaunchTemplate")
    assert len(templates) == len(CONFIG.queue_classes)
    for lt in templates.values():
        data = lt["Properties"]["LaunchTemplateData"]
        assert data["MetadataOptions"]["HttpTokens"] == "required", data
        assert "HttpPutResponseHopLimit" not in data["MetadataOptions"], data
        (root,) = data["BlockDeviceMappings"]
        assert root["DeviceName"] == "/dev/xvda"
        assert root["Ebs"]["Encrypted"] is True
        assert (root["Ebs"]["VolumeSize"], root["Ebs"]["VolumeType"]) == (30, "gp3")
    # A queue-class's own root volume and hop limit are still encrypted and IMDSv2-only.
    big = QueueClass(name="big", capacity="ON_DEMAND", ebs_gb=1000, imds_hop_limit=2)
    cfg = dataclasses.replace(CONFIG, queue_classes=[*CONFIG.queue_classes, big])
    stacks = _build(tmp_path, config=cfg)
    compute = stacks["compute"]
    templates = Template.from_stack(compute).find_resources("AWS::EC2::LaunchTemplate")
    big_lt = compute.queue_classes["big"].launch_template
    data = templates[_logical_id(compute, big_lt)]["Properties"]["LaunchTemplateData"]
    assert data["BlockDeviceMappings"][0]["Ebs"]["VolumeSize"] == 1000
    assert data["MetadataOptions"] == {"HttpPutResponseHopLimit": 2, "HttpTokens": "required"}
    assert data["BlockDeviceMappings"][0]["Ebs"]["Encrypted"] is True


def test_a_queue_class_sets_its_compute_environment_s_vcpu_ceiling(tmp_path: Path) -> None:
    # (test_config_keys covers the values the loader refuses.)
    cfg = DeployConfig.load(
        write_config(
            tmp_path,
            queue_classes=[
                {"name": "coordinator", "capacity": "ON_DEMAND", "coordinator": True},
                {"name": "bench", "capacity": "ON_DEMAND", "max_vcpus": 64},
            ],
        )
    )
    assert [qc.max_vcpus for qc in cfg.queue_classes] == [256, 64]
    app = cdk.App()
    net = NetworkStack(app, "net")
    compute = ComputeStack(app, "compute", vpc=net.vpc, config=cfg, host_bucket=_host_bucket(app))
    envs = Template.from_stack(compute).find_resources("AWS::Batch::ComputeEnvironment")
    assert sorted(e["Properties"]["ComputeResources"]["MaxvCpus"] for e in envs.values()) == [
        64,
        256,
    ]


def test_hosts_read_only_the_host_prefix_of_the_reference_bucket(default_stacks: Stacks) -> None:
    t = Template.from_stack(default_stacks["compute"])
    object_resources = [
        json.dumps(resource)
        for statement in _statements(t)
        if _grants(statement, "s3:GetObject")
        for resource in (
            statement["Resource"]
            if isinstance(statement["Resource"], list)
            else [statement["Resource"]]
        )
        if "/" in json.dumps(resource)
    ]
    assert object_resources, "hosts cannot read the host AWS CLI"
    assert all('"/host/*"' in r for r in object_resources), object_resources
    assert not any(_grants(st, "s3:PutObject") for st in _statements(t))


def test_entrypoint_points_nextflow_at_the_host_aws_cli() -> None:
    nextflow = (REPO_ROOT / "docker/coordinator/lib/nextflow.sh").read_text()
    assert f"aws.batch.cliPath = '{HOST_AWS_CLI_PATH}'" in nextflow


def test_batch_job_logs_expire(default_stacks: Stacks) -> None:
    """Coordinators and Snakemake's child jobs log to Batch's default group, which the backend
    reads (Nextflow's log to their project's own group, created with the same retention)."""
    t = Template.from_stack(default_stacks["compute"])
    (retention,) = t.find_resources("Custom::LogRetention").values()
    assert retention["Properties"]["LogGroupName"] == "/aws/batch/job"
    assert retention["Properties"]["RetentionInDays"] == 365
