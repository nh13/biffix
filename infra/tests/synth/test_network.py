"""The network stack: NAT, the S3 gateway endpoint and locked-down worker egress."""

import dataclasses
import json
from pathlib import Path

import aws_cdk as cdk
from aws_cdk.assertions import Template

from biffix_infra.app_graph import Stacks
from biffix_infra.config import (
    DataSource,
    Project,
)
from biffix_infra.stacks.network_stack import NetworkStack
from tests.synth.synth_helpers import (
    CONFIG,
    _build,
    _grants,
    _logical_id,
    _render,
    _resources,
)


def test_network_has_s3_gateway_endpoint() -> None:
    app = cdk.App()
    net = NetworkStack(app, "net")
    t = Template.from_stack(net)
    t.resource_count_is("AWS::EC2::VPC", 1)
    t.has_resource_properties("AWS::EC2::VPCEndpoint", {"VpcEndpointType": "Gateway"})
    t.resource_count_is("AWS::EC2::VPCEndpoint", 1)


def test_workers_reach_only_ecr_s3_ecs_and_logs_when_egress_is_locked_down(tmp_path: Path) -> None:
    for d in ("off", "on"):
        (tmp_path / d).mkdir()
    # Off (the default): no isolated subnets, no interface endpoints.
    off = Template.from_stack(_build(tmp_path / "off")["network"])
    off.resource_count_is("AWS::EC2::VPCEndpoint", 1)
    assert not [
        s
        for s in off.find_resources("AWS::EC2::Subnet").values()
        if any(t.get("Value") == "Isolated" for t in s["Properties"].get("Tags", []))
    ]

    stacks = _build(
        tmp_path / "on", config=dataclasses.replace(CONFIG, worker_egress="ecr_s3_only")
    )
    net = Template.from_stack(stacks["network"])
    endpoints = net.find_resources("AWS::EC2::VPCEndpoint").values()
    # Exactly these six services, compared by their whole name (a substring `ecs` would also match
    # `ecs-agent`).
    interface = sorted(
        _render(e["Properties"]["ServiceName"])
        for e in endpoints
        if e["Properties"].get("VpcEndpointType") == "Interface"
    )
    assert interface == sorted(
        f"com.amazonaws.{{Ref:AWS::Region}}.{service}"
        for service in ("ecr.api", "ecr.dkr", "ecs", "ecs-agent", "ecs-telemetry", "logs")
    ), interface
    isolated = {
        lid
        for lid, s in net.find_resources("AWS::EC2::Subnet").items()
        if lid.startswith("Vpcworkers")
    }
    assert len(isolated) == 2
    # No isolated subnet routes to the internet (only the S3 gateway endpoint's prefix list): no
    # route, wherever it was added from, is in a route table associated with one of them.
    tables = {
        a["Properties"]["RouteTableId"]["Ref"]
        for a in net.find_resources("AWS::EC2::SubnetRouteTableAssociation").values()
        if a["Properties"]["SubnetId"].get("Ref") in isolated
    }
    assert len(tables) == 2, tables
    routes = net.find_resources("AWS::EC2::Route").values()
    assert not [r for r in routes if r["Properties"]["RouteTableId"].get("Ref") in tables]

    compute = stacks["compute"]
    ces = Template.from_stack(compute).find_resources("AWS::Batch::ComputeEnvironment")
    assert len(ces) == len(CONFIG.queue_classes), ces.keys()
    for qc in CONFIG.queue_classes:
        ce = compute.queue_classes[qc.name].compute_environment
        lid = _logical_id(compute, ce)
        subnets = json.dumps(ces[lid]["Properties"]["ComputeResources"]["Subnets"])
        if qc.coordinator:
            assert "workers" not in subnets, (lid, subnets)
        else:
            assert "workers" in subnets, (lid, subnets)


def test_network_instance_nat_uses_no_managed_gateway() -> None:
    app = cdk.App()
    net = NetworkStack(app, "net", nat_mode="instance")
    t = Template.from_stack(net)
    t.resource_count_is("AWS::EC2::NatGateway", 0)
    assert t.find_resources("AWS::EC2::Instance"), "expected a NAT instance"
    groups = t.find_resources("AWS::EC2::SecurityGroup")
    nat_group = next(
        r
        for r in groups.values()
        if r["Properties"]["GroupDescription"] == "Security Group for NAT instances"
    )
    ingress = nat_group["Properties"]["SecurityGroupIngress"]
    assert all(rule.get("CidrIp") != "0.0.0.0/0" for rule in ingress)
    assert ingress[0]["CidrIp"]["Fn::GetAtt"][1] == "CidrBlock"


def test_network_gateway_nat_uses_managed_gateway() -> None:
    app = cdk.App()
    net = NetworkStack(app, "net", nat_mode="gateway")
    t = Template.from_stack(net)
    t.resource_count_is("AWS::EC2::NatGateway", 1)
    assert not t.find_resources("AWS::EC2::Instance"), "no NAT instance in gateway mode"


def test_isolated_workers_reach_only_this_account_s_and_named_buckets(tmp_path: Path) -> None:
    default = CONFIG.projects[0]
    config = dataclasses.replace(
        CONFIG,
        worker_egress="ecr_s3_only",
        projects=[
            dataclasses.replace(
                default, data_sources=(DataSource(id="core", name="Core", bucket="core-drop"),)
            ),
            Project(id="legacy", bucket_name="legacy-bucket"),
        ],
    )
    net = Template.from_stack(_build(tmp_path, config=config)["network"])
    gateways = {
        lid: e["Properties"]
        for lid, e in net.find_resources("AWS::EC2::VPCEndpoint").items()
        if e["Properties"].get("VpcEndpointType") == "Gateway"
    }
    assert len(gateways) == 2, gateways
    (shared,) = [p for lid, p in gateways.items() if "WorkerS3" not in lid]
    (worker,) = [p for lid, p in gateways.items() if "WorkerS3" in lid]
    # The shared endpoint does not serve the worker subnets; the worker one serves only them
    # (both of them: an endpoint on no route table would leave the workers without S3).
    assert "workers" not in json.dumps(shared["RouteTableIds"])
    assert len(worker["RouteTableIds"]) == 2, worker["RouteTableIds"]
    assert all("workers" in json.dumps(r) for r in worker["RouteTableIds"])
    assert "PolicyDocument" not in shared
    statements = {s["Sid"]: s for s in worker["PolicyDocument"]["Statement"]}
    assert set(statements) == {
        "ThisAccountsBuckets",
        "EcrImageLayers",
        "ImportedProjectBuckets",
        "DataSourceBuckets",
    }
    assert statements["ThisAccountsBuckets"]["Condition"] == {
        "StringEquals": {"aws:ResourceAccount": {"Ref": "AWS::AccountId"}}
    }
    assert "starport-layer-bucket" in json.dumps(statements["EcrImageLayers"]["Resource"])
    assert statements["EcrImageLayers"]["Action"] == "s3:GetObject"
    assert _resources(statements["ImportedProjectBuckets"]) == [
        "arn:aws:s3:::legacy-bucket",
        "arn:aws:s3:::legacy-bucket/*",
    ]
    data = statements["DataSourceBuckets"]
    assert _resources(data) == ["arn:aws:s3:::core-drop", "arn:aws:s3:::core-drop/*"]
    assert not _grants(data, "s3:PutObject"), data


def test_the_default_egress_keeps_one_unrestricted_s3_endpoint(default_stacks: Stacks) -> None:
    net = Template.from_stack(default_stacks["network"])
    (endpoint,) = net.find_resources("AWS::EC2::VPCEndpoint").values()
    assert "PolicyDocument" not in endpoint["Properties"]
