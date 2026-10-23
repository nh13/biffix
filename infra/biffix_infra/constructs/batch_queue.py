"""One queue-class of the Batch fleet: its launch template, compute environment and job queue."""

from __future__ import annotations

from typing import cast

from aws_cdk import aws_batch as batch
from aws_cdk import aws_ec2 as ec2
from aws_cdk import aws_s3 as s3
from constructs import Construct

from biffix_infra.config import DEFAULT_ROOT_VOLUME_GB, HOST_PREFIX, QueueClass


class BatchQueueClass(Construct):
    """Queue-class `qc`'s job queue (`queue`, named `queue_name`; this construct's default child)
    on one EC2 compute environment (`compute_environment`) whose hosts boot from `user_data` and
    may read the `host/` prefix of `host_bucket` (where the boot script downloads from).

    `subnets` places the hosts (`None`: the VPC's private subnets); `instance_families` resolves
    the queue-class's family entries (`resolve_instances`)."""

    def __init__(
        self,
        scope: Construct,
        cid: str,
        *,
        qc: QueueClass,
        queue_name: str,
        vpc: ec2.IVpc,
        subnets: ec2.SubnetSelection | None,
        user_data: ec2.MultipartUserData,
        host_bucket: s3.IBucket,
        instance_families: dict[str, str],
    ) -> None:
        super().__init__(scope, cid)
        spot = qc.capacity == "SPOT"
        if qc.coordinator and spot:
            raise ValueError(
                f"coordinator queue-class {qc.name!r} must be ON_DEMAND, not spot: a reclaimed "
                "coordinator would kill its whole run"
            )
        instance_types, instance_classes = resolve_instances(
            qc.instances, qc.name, instance_families
        )
        has_explicit = bool(instance_types or instance_classes)
        self.launch_template = _launch_template(self, qc, user_data)
        self.compute_environment = batch.ManagedEc2EcsComputeEnvironment(
            self,
            "ComputeEnvironment",
            vpc=vpc,
            vpc_subnets=subnets,
            spot=spot,
            minv_cpus=0,
            maxv_cpus=qc.max_vcpus,
            instance_types=instance_types or None,
            instance_classes=instance_classes or None,
            use_optimal_instance_classes=not has_explicit,
            launch_template=self.launch_template,
            allocation_strategy=(
                batch.AllocationStrategy.SPOT_CAPACITY_OPTIMIZED
                if spot
                else batch.AllocationStrategy.BEST_FIT_PROGRESSIVE
            ),
        )
        # Pin the CE to the template's latest version. Batch resolves an unversioned template
        # once, when the CE is created, so a user-data change would otherwise never reach new
        # hosts; a changed version number makes CloudFormation update the CE's infrastructure.
        cfn_ce = cast(batch.CfnComputeEnvironment, self.compute_environment.node.default_child)
        cfn_ce.add_property_override(
            "ComputeResources.LaunchTemplate.Version",
            self.launch_template.latest_version_number,
        )
        # Every host downloads the CLI from `host/` at boot (and only that prefix, for now).
        # A managed EC2 compute environment always has an instance role (made when not given).
        instance_role = self.compute_environment.instance_role
        assert instance_role is not None
        host_bucket.grant_read(instance_role, f"{HOST_PREFIX}*")
        self.queue = batch.JobQueue(
            self,
            "Default",
            job_queue_name=queue_name,
            compute_environments=[
                batch.OrderedComputeEnvironment(
                    compute_environment=self.compute_environment, order=1
                )
            ],
        )


def resolve_instances(
    entries: list[str], qc_name: str, families: dict[str, str]
) -> tuple[list[ec2.InstanceType], list[ec2.InstanceClass]]:
    """Split a queue-class's `instances` list into exact types and whole instance CLASSES.

    An entry with a "." is an EXACT type (`c8g.4xlarge`, used by benchmarks where the size IS the
    measurement) → an `ec2.InstanceType`. Otherwise it is a family key resolved via the config
    `instance_families` map (family → class string, e.g. "c" → "c6i") → an `ec2.InstanceClass`.
    Passing a class (not a `<class>.<size>`) is what lets AWS Batch pick a size within the family
    per each job's resource requests — the point of a general-workload family entry; a
    `<class>.<size>` would cap every such queue-class at a single size."""
    types: list[ec2.InstanceType] = []
    classes: list[ec2.InstanceClass] = []
    for entry in entries:
        if "." in entry:
            types.append(ec2.InstanceType(entry))
            continue
        cls = families.get(entry)
        if cls is None:
            raise ValueError(
                f"queue-class {qc_name!r}: unknown instance family {entry!r}; "
                f"configured families: {sorted(families)} (or give an exact type like "
                f"'c8g.4xlarge')"
            )
        # The config class string ("c6i", "x2iedn") maps to the ec2.InstanceClass enum member of
        # the same name (C6I, X2IEDN); Batch then selects the size within that class.
        member = cls.upper().replace("-", "_")
        try:
            classes.append(getattr(ec2.InstanceClass, member))
        except AttributeError as e:
            raise ValueError(
                f"queue-class {qc_name!r}: instance family {entry!r} maps to class {cls!r}, not a "
                f"known ec2.InstanceClass ({member}); give an exact type like 'c8g.4xlarge', or "
                f"fix the instance_families map."
            ) from e
    return types, classes


def _launch_template(
    scope: Construct, qc: QueueClass, user_data: ec2.MultipartUserData
) -> ec2.LaunchTemplate:
    """The queue-class's launch template: the host boot script, an encrypted root volume (the
    queue-class's size and speed, else 30 GB gp3) and IMDSv2-only instance metadata.
    Encryption does not rely on the account's EBS encryption-by-default: task scratch holds
    genomic data. IMDSv2 at the default hop limit keeps task containers (bridge networking, one
    hop past the host) from reading the instance role's credentials; `imds_hop_limit: 2`
    deliberately opens metadata to them."""
    root = ec2.BlockDevice(
        device_name="/dev/xvda",
        volume=ec2.BlockDeviceVolume.ebs(
            volume_size=qc.ebs_gb or DEFAULT_ROOT_VOLUME_GB,
            volume_type=ec2.EbsDeviceVolumeType.GP3,
            throughput=qc.ebs_throughput,
            iops=qc.ebs_iops,
            encrypted=True,
            delete_on_termination=True,
        ),
    )
    return ec2.LaunchTemplate(
        scope,
        "LaunchTemplate",
        block_devices=[root],
        user_data=user_data,
        http_tokens=ec2.LaunchTemplateHttpTokens.REQUIRED,
        # None leaves it unset (the EC2 default).
        http_put_response_hop_limit=qc.imds_hop_limit,
    )
