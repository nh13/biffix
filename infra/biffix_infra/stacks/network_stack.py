"""NetworkStack — the long-lived VPC.

The backend, Batch workers, and the coordinator run in private subnets with outbound egress via
NAT. The NAT provider is a cost profile (config.nat_mode): a managed NAT gateway (recommended —
HA, no maintenance) or a single t4g.nano NAT instance (low-cost, first-version default). The free
S3 *gateway* endpoint keeps bulk S3 traffic off NAT; interface endpoints (ECR/Logs/STS) are an
optional privacy/data-charge optimization, omitted here to keep the low-cost floor small — that
traffic rides NAT instead.

With `worker_egress: ecr_s3_only` the Batch workers get their own isolated subnets (no route to the
internet) and the interface endpoints they need: ECR (pull the tenant's images), ECS (the container
agent), CloudWatch Logs (task logs); S3 rides a gateway endpoint of their own whose policy reaches
only this account's buckets, the named external ones (data sources, imported project buckets) and
ECR's layer bucket, so a task cannot write to a bucket elsewhere (`IsolatedWorkerEgress`). The
coordinator and the Lambdas keep the NAT (and the unrestricted S3 endpoint).
"""

from __future__ import annotations

from typing import Any

from aws_cdk import Stack
from aws_cdk import aws_ec2 as ec2
from constructs import Construct

from biffix_infra.constructs.nat_instance import allow_vpc_forwarding, nat_instance_provider
from biffix_infra.constructs.worker_egress import WORKER_SUBNETS, IsolatedWorkerEgress


class NetworkStack(Stack):
    def __init__(
        self,
        scope: Construct,
        cid: str,
        *,
        nat_mode: str = "instance",
        worker_egress: str = "nat",
        worker_read_buckets: list[str] | None = None,
        worker_write_buckets: list[str] | None = None,
        **kwargs: Any,
    ) -> None:
        super().__init__(scope, cid, **kwargs)
        isolated = worker_egress == "ecr_s3_only"

        # Cost profile for private-subnet egress. "instance" is the low-cost first-version default
        # (~$4/mo t4g.nano, single point of failure); "gateway" is the recommended HA option.
        instance_nat_provider = (
            nat_instance_provider(self, key_pair_name=f"{cid}-nat")
            if nat_mode == "instance"
            else None
        )
        self.vpc = ec2.Vpc(
            self,
            "Vpc",
            max_azs=2,
            nat_gateways=1,  # one shared NAT (gateway or instance) for the low-cost profile
            nat_gateway_provider=instance_nat_provider or ec2.NatProvider.gateway(),
            subnet_configuration=[
                ec2.SubnetConfiguration(
                    name="public", subnet_type=ec2.SubnetType.PUBLIC, cidr_mask=24
                ),
                ec2.SubnetConfiguration(
                    name="private",
                    subnet_type=ec2.SubnetType.PRIVATE_WITH_EGRESS,
                    cidr_mask=20,
                ),
                # Appended last, so the subnets above keep their CIDRs when it is turned on.
                *(
                    [
                        ec2.SubnetConfiguration(
                            name=WORKER_SUBNETS,
                            subnet_type=ec2.SubnetType.PRIVATE_ISOLATED,
                            cidr_mask=20,
                        )
                    ]
                    if isolated
                    else []
                ),
            ],
        )
        if instance_nat_provider is not None:
            allow_vpc_forwarding(instance_nat_provider, self.vpc)

        # S3 gateway endpoint (free) — keeps bulk S3 traffic off NAT (real data-charge saving).
        # With isolated workers it serves only the other subnets; the workers get their own.
        s3_endpoint = self.vpc.add_gateway_endpoint(
            "S3Endpoint",
            service=ec2.GatewayVpcEndpointAwsService.S3,
            subnets=(
                [
                    ec2.SubnetSelection(subnet_type=ec2.SubnetType.PUBLIC),
                    ec2.SubnetSelection(subnet_type=ec2.SubnetType.PRIVATE_WITH_EGRESS),
                ]
                if isolated
                else None
            ),
        )

        # Where the Batch workers run: the private subnets, or the isolated ones.
        self.worker_subnets: ec2.SubnetSelection | None = None
        if isolated:
            self.worker_subnets = IsolatedWorkerEgress(
                self,
                "WorkerEgress",
                vpc=self.vpc,
                shared_s3_endpoint=s3_endpoint,
                read_buckets=worker_read_buckets or [],
                write_buckets=worker_write_buckets or [],
            ).subnets
        # NOTE: interface endpoints (ECR/ECR-Docker/Logs/STS) are intentionally omitted — at ~$58/mo
        # for 4 endpoints across 2 AZs they cost more than the NAT data charges that traffic incurs
        # at this scale. That traffic rides NAT (`worker_egress: ecr_s3_only` adds the workers'
        # own).
