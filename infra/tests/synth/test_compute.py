"""The compute stack: a queue and compute environment per queue-class, and the hosts."""

import base64
import dataclasses
import email
import gzip
import json
import posixpath
import subprocess
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
from biffix_infra.host_boot import (
    DISABLE_PRIVILEGED,
    INSTALL_AWS_CLI,
    SINGLE_JOB_BOOT_SCRIPT,
    host_install_header,
    single_job_boot_script,
)
from biffix_infra.stacks.compute_stack import ComputeStack
from biffix_infra.stacks.data_stack import DataStack
from biffix_infra.stacks.network_stack import NetworkStack
from tests.deploy_config_file import write_config
from tests.helpers import REPO_ROOT
from tests.synth.synth_helpers import (
    CONFIG,
    SINGLE_JOB_CLASSES,
    Json,
    _build,
    _grants,
    _logical_id,
    _render,
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
        # Coordinator and worker hosts alike refuse privileged containers (nothing biffix runs
        # on Batch is privileged), set before the ECS agent starts.
        assert f"echo {DISABLE_PRIVILEGED} >> /etc/ecs/ecs.config" in user_data
        assert DISABLE_PRIVILEGED == "ECS_DISABLE_PRIVILEGED=true"
    # The boot script reads exactly the variables the stack sets (its own test runs the script
    # against `host_install_header`, the one source of these lines).
    script = INSTALL_AWS_CLI.read_text()
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


# EC2's limit on a launch's user data (before base64), which Batch's own merged-in part also counts
# against.
_USER_DATA_MAX_BYTES = 16 * 1024
# The longest a token in the user data (the reference bucket's name) resolves to.
_BUCKET_NAME_MAX = 63


def _resolve(value: object) -> str:
    """A CloudFormation string as deployed: joins joined, `Fn::Base64` encoded, and any other
    deploy-time value (the bucket name) at its longest."""
    if isinstance(value, str):
        return value
    if isinstance(value, dict) and "Fn::Join" in value:
        sep, parts = value["Fn::Join"]
        return str(sep).join(_resolve(p) for p in parts)
    if isinstance(value, dict) and "Fn::Base64" in value:
        return base64.b64encode(_resolve(value["Fn::Base64"]).encode()).decode()
    return "x" * _BUCKET_NAME_MAX


def _bytes(payload: object) -> bytes:
    """A MIME leaf part's decoded payload, which is bytes."""
    assert isinstance(payload, bytes), payload
    return payload


def _user_data(compute: ComputeStack, queue_class: str) -> tuple[str, list[tuple[str, str]]]:
    """`queue_class`'s launch-template user data as a host receives it (each deploy-time value at
    its longest), and its MIME parts as (content type, decoded body), in order."""
    lt = compute.queue_classes[queue_class].launch_template
    templates = Template.from_stack(compute).find_resources("AWS::EC2::LaunchTemplate")
    data: Json = templates[_logical_id(compute, lt)]["Properties"]["LaunchTemplateData"]
    raw = _resolve(data["UserData"]["Fn::Base64"])
    message = email.message_from_string(raw)
    parts = [
        (part.get_content_type(), _bytes(part.get_payload(decode=True)).decode())
        for part in message.walk()
        if not part.is_multipart()
    ]
    return raw, parts


def _compute_resources(compute: ComputeStack, queue_class: str) -> Json:
    envs = Template.from_stack(compute).find_resources("AWS::Batch::ComputeEnvironment")
    ce = compute.queue_classes[queue_class].compute_environment
    resources: Json = envs[_logical_id(compute, ce)]["Properties"]["ComputeResources"]
    return resources


def test_a_single_job_queue_class_has_its_own_on_demand_exact_type_environment(
    single_job_stacks: Stacks,
) -> None:
    compute = single_job_stacks["compute"]
    assert isinstance(compute, ComputeStack)
    assert list(compute.single_job_queues) == [qc.name for qc in SINGLE_JOB_CLASSES]
    assert "cpu-spot" in compute.worker_queues
    assert not set(compute.worker_queues) & {*compute.single_job_queues, "coordinator"}
    queues = Template.from_stack(compute).find_resources("AWS::Batch::JobQueue")
    templates = set()
    for qc in SINGLE_JOB_CLASSES:
        resources = _compute_resources(compute, qc.name)
        assert resources["Type"] == "EC2", resources
        assert resources["InstanceTypes"] == qc.instances
        assert resources["AllocationStrategy"] == "BEST_FIT_PROGRESSIVE"
        assert resources["MaxvCpus"] == qc.max_vcpus
        # Its boothook is written for the ECS-optimized AL2023 AMI.
        assert resources["Ec2Configuration"] == [{"ImageType": "ECS_AL2023"}], resources
        # A kernel fix reaches its hosts on an infrastructure update (infra/README.md, "Taking a
        # kernel fix on single-job hosts"): Batch takes the newest AMI only when this is set, and
        # only under its service-linked role (no ServiceRole of our own).
        assert resources["UpdateToLatestImageVersion"] is True, resources
        envs = Template.from_stack(compute).find_resources("AWS::Batch::ComputeEnvironment")
        ce = compute.queue_classes[qc.name].compute_environment
        assert "ServiceRole" not in envs[_logical_id(compute, ce)]["Properties"]
        templates.add(json.dumps(resources["LaunchTemplate"], sort_keys=True))
        # Its queue runs on its compute environment alone.
        queue = queues[_logical_id(compute, compute.queues[qc.name])]["Properties"]
        (order,) = queue["ComputeEnvironmentOrder"]
        ce = compute.queue_classes[qc.name].compute_environment
        assert order["ComputeEnvironment"] == {
            "Fn::GetAtt": [_logical_id(compute, ce), "ComputeEnvironmentArn"]
        }
    # Each has its own launch template, none of them a tasks-mode host's.
    others = {
        json.dumps(_compute_resources(compute, name)["LaunchTemplate"], sort_keys=True)
        for name in ("coordinator", *compute.worker_queues)
    }
    assert len(templates) == len(SINGLE_JOB_CLASSES)
    assert not templates & others


def test_a_single_job_host_sits_with_the_coordinator_not_the_isolated_workers(
    single_job_stacks: Stacks,
) -> None:
    compute = single_job_stacks["compute"]
    assert isinstance(compute, ComputeStack)

    def subnets(name: str) -> list[str]:
        return [_render(s) for s in _compute_resources(compute, name)["Subnets"]]

    coordinator = subnets("coordinator")
    # The fixture puts workers in the isolated subnets (worker_egress: ecr_s3_only).
    assert subnets("cpu-spot") != coordinator
    for qc in SINGLE_JOB_CLASSES:
        assert subnets(qc.name) == coordinator


def test_only_a_single_job_host_boots_with_the_seccomp_profile_and_scratch(
    single_job_stacks: Stacks,
) -> None:
    compute = single_job_stacks["compute"]
    assert isinstance(compute, ComputeStack)
    for qc in SINGLE_JOB_CLASSES:
        raw, parts = _user_data(compute, qc.name)
        # A boothook (it runs before Docker and the ECS agent start), then the two parts every
        # host runs.
        assert [t for t, _ in parts] == [
            "text/cloud-boothook",
            "text/x-shellscript",
            "text/x-shellscript",
        ]
        (_, boothook), (_, harden), (_, install) = parts
        # The shim unpacks the boot script (its own test runs it) and runs it.
        assert boothook.startswith("#!/bin/bash\n")
        script = SINGLE_JOB_BOOT_SCRIPT
        head, payload = boothook.split(f"if base64 -d <<'EOF' | gunzip > {script}; then\n", 1)
        assert "set -o pipefail\n" in head
        payload, rest = payload.split("\nEOF\n", 1)
        assert gzip.decompress(base64.b64decode(payload)).decode() == single_job_boot_script()
        assert rest.splitlines() == [
            f"  bash {script}",
            "else",
            '  echo "biffix: single-job boot script did not unpack"',
            "fi",
        ]
        assert f"echo {DISABLE_PRIVILEGED} >> /etc/ecs/ecs.config" in harden
        assert "biffix_install_host_aws_cli" in install
        # Room to spare under EC2's limit for Batch's own part.
        assert len(raw.encode()) < _USER_DATA_MAX_BYTES - 2048, len(raw)
    for name in ("coordinator", *compute.worker_queues):
        _, parts = _user_data(compute, name)
        assert [t for t, _ in parts] == ["text/x-shellscript", "text/x-shellscript"], name
        assert not any("seccomp" in body for _, body in parts), name


def test_a_boot_script_that_does_not_unpack_is_never_run(
    single_job_stacks: Stacks, tmp_path: Path
) -> None:
    # A truncated payload (gunzip fails mid-stream) leaves a partial script; the shim must not
    # run it.
    compute = single_job_stacks["compute"]
    assert isinstance(compute, ComputeStack)
    _, ((_, boothook), *_) = _user_data(compute, SINGLE_JOB_CLASSES[0].name)
    script = tmp_path / "single-job-boot.sh"
    body, rest = boothook.split("\nEOF\n", 1)
    lines = body.splitlines()
    # Cut inside the payload (whole base64 lines, so only gunzip fails).
    truncated = "\n".join([*lines[: len(lines) - 3], "EOF", rest])
    shim = truncated.replace(SINGLE_JOB_BOOT_SCRIPT, str(script)).replace(
        f"mkdir -p {posixpath.dirname(SINGLE_JOB_BOOT_SCRIPT)}", f"mkdir -p {tmp_path}"
    )
    assert SINGLE_JOB_BOOT_SCRIPT not in shim
    result = subprocess.run(["bash", "-c", shim], capture_output=True, text=True)
    assert "biffix: single-job boot script did not unpack" in result.stdout, result
    assert "seccomp" not in result.stdout


def test_compute_refuses_a_single_job_queue_class_built_in_code() -> None:
    # The loader refuses these (test_config_keys); a config built in code is checked at synth.
    bad = DeployConfig(
        tenant="dev",
        region="us-east-1",
        projects=[],
        queue_classes=[
            QueueClass(name="coordinator", capacity="ON_DEMAND", coordinator=True),
            QueueClass(name="single", capacity="SPOT", single_job=True, instances=["c6id.8xlarge"]),
        ],
    )
    app = cdk.App()
    net = NetworkStack(app, "net")
    with pytest.raises(ValueError, match="ON_DEMAND"):
        ComputeStack(app, "compute", vpc=net.vpc, config=bad, host_bucket=_host_bucket(app))
