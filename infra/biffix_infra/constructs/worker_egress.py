"""`worker_egress: ecr_s3_only`: the Batch workers' isolated subnets reach only ECR, ECS, CloudWatch
Logs and a closed set of S3 buckets, through VPC endpoints."""

from __future__ import annotations

from aws_cdk import Stack
from aws_cdk import aws_ec2 as ec2
from aws_cdk import aws_iam as iam
from constructs import Construct

# The subnet group isolated workers run in.
WORKER_SUBNETS = "workers"
# What an isolated worker must reach besides S3: its images (ECR API + registry), the ECS container
# agent (registration, task state, metrics), and CloudWatch Logs (task logs).
WORKER_ENDPOINTS = {
    "EcrApiEndpoint": ec2.InterfaceVpcEndpointAwsService.ECR,
    "EcrDockerEndpoint": ec2.InterfaceVpcEndpointAwsService.ECR_DOCKER,
    "EcsEndpoint": ec2.InterfaceVpcEndpointAwsService.ECS,
    "EcsAgentEndpoint": ec2.InterfaceVpcEndpointAwsService.ECS_AGENT,
    "EcsTelemetryEndpoint": ec2.InterfaceVpcEndpointAwsService.ECS_TELEMETRY,
    "LogsEndpoint": ec2.InterfaceVpcEndpointAwsService.CLOUDWATCH_LOGS,
}


class IsolatedWorkerEgress(Construct):
    """The endpoints `vpc`'s isolated worker subnets (`subnets`, the `WORKER_SUBNETS` group) need:
    the `WORKER_ENDPOINTS` interface endpoints behind one security group, and an S3 gateway
    endpoint of their own whose policy reaches only this account's buckets, `write_buckets`
    (imported project buckets), `read_buckets` (data sources, read-only) and ECR's layer bucket.
    `shared_s3_endpoint` is the S3 gateway endpoint of the VPC's other subnets.

    The endpoints are the VPC's children (as `vpc.add_*_endpoint` makes them)."""

    def __init__(
        self,
        scope: Construct,
        cid: str,
        *,
        vpc: ec2.Vpc,
        shared_s3_endpoint: ec2.GatewayVpcEndpoint,
        read_buckets: list[str],
        write_buckets: list[str],
    ) -> None:
        super().__init__(scope, cid)
        self.subnets = ec2.SubnetSelection(subnet_group_name=WORKER_SUBNETS)
        endpoint_sg = ec2.SecurityGroup(
            self,
            "EndpointSg",
            vpc=vpc,
            description="HTTPS from the VPC to its interface endpoints",
            allow_all_outbound=False,
        )
        endpoint_sg.add_ingress_rule(
            ec2.Peer.ipv4(vpc.vpc_cidr_block), ec2.Port.tcp(443), "HTTPS from the VPC"
        )
        for name, service in WORKER_ENDPOINTS.items():
            vpc.add_interface_endpoint(
                name,
                service=service,
                subnets=self.subnets,
                security_groups=[endpoint_sg],
                private_dns_enabled=True,
            )
        self.s3_endpoint = self._s3_endpoint(vpc, shared_s3_endpoint, read_buckets, write_buckets)

    def _s3_endpoint(
        self,
        vpc: ec2.Vpc,
        shared: ec2.GatewayVpcEndpoint,
        read_buckets: list[str],
        write_buckets: list[str],
    ) -> ec2.GatewayVpcEndpoint:
        """The isolated workers' own S3 gateway endpoint. Its policy is what makes `ecr_s3_only`
        closed for S3: the default full-access policy would let a task PutObject to any bucket in
        the region, an attacker's included, with credentials it brings. It allows any S3 action on
        a bucket this account owns (the project, reference, archive buckets), the same on imported
        project buckets (`write_buckets`, possibly another account's), read on data-source buckets
        (`read_buckets`), and the image-layer reads ECR pulls make from its own bucket. IAM still
        decides what a task's role may do within those."""
        stack = Stack.of(self)
        endpoint = vpc.add_gateway_endpoint(
            "WorkerS3Endpoint",
            service=ec2.GatewayVpcEndpointAwsService.S3,
            subnets=[self.subnets],
        )
        # A route table holds one route per prefix list: when turning isolation on, the shared
        # endpoint must leave the worker route tables before this one takes them.
        endpoint.node.add_dependency(shared)
        endpoint.add_to_policy(
            iam.PolicyStatement(
                sid="ThisAccountsBuckets",
                principals=[iam.AnyPrincipal()],
                actions=["s3:*"],
                resources=["*"],
                conditions={"StringEquals": {"aws:ResourceAccount": stack.account}},
            )
        )
        endpoint.add_to_policy(
            iam.PolicyStatement(
                sid="EcrImageLayers",
                principals=[iam.AnyPrincipal()],
                actions=["s3:GetObject"],
                resources=[
                    f"arn:{stack.partition}:s3:::prod-{stack.region}-starport-layer-bucket/*"
                ],
            )
        )
        for sid, actions, names in (
            ("ImportedProjectBuckets", ["s3:*"], write_buckets),
            (
                "DataSourceBuckets",
                ["s3:GetObject", "s3:ListBucket", "s3:GetBucketLocation"],
                [b for b in read_buckets if b not in write_buckets],
            ),
        ):
            buckets = sorted(set(names))
            if buckets:
                endpoint.add_to_policy(
                    iam.PolicyStatement(
                        sid=sid,
                        principals=[iam.AnyPrincipal()],
                        actions=actions,
                        resources=[
                            arn
                            for b in buckets
                            for arn in (
                                f"arn:{stack.partition}:s3:::{b}",
                                f"arn:{stack.partition}:s3:::{b}/*",
                            )
                        ],
                    )
                )
        return endpoint
