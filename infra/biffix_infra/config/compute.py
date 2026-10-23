"""The compute section: the queue-class menu, the instance families, the coordinator image and the
AWS CLI every Batch host installs (ComputeStack, the api stack's coordinator template)."""

from __future__ import annotations

from dataclasses import dataclass, field

from biffix_infra.config.fields import Fields, Raw, count, flag, is_count, text, text_list
from biffix_infra.config.s3_layout import HOST_PREFIX

# Default instance-family → instance-class mapping (family key → the instance CLASS passed to AWS
# Batch, e.g. "c6i"; Batch then selects a size within that family per each job's resource requests).
# Generations drift, so this belongs in config with the queue-class menu; a deploy overrides or
# extends it via the `instance_families` block of its deploy config.
DEFAULT_INSTANCE_FAMILIES: dict[str, str] = {
    "c": "c6i",
    "m": "m6i",
    "r": "r6i",
    "g": "g5",
    "arm64": "c7g",
    "highmem": "x2iedn",
}

# A host's root volume when its queue-class sets no `ebs_gb`: gp3 at its baseline iops/throughput.
DEFAULT_ROOT_VOLUME_GB = 30
GP3_BASELINE_IOPS = 3000
GP3_BASELINE_THROUGHPUT = 125
# gp3's limits: iops up to 16000 and 500 per GiB, throughput up to 1000 MiB/s.
GP3_MAX_IOPS = 16000
GP3_IOPS_PER_GIB = 500
GP3_MAX_THROUGHPUT = 1000

CAPACITIES = ("SPOT", "ON_DEMAND")

# Where hosts install the AWS CLI. Nextflow requires the path to end in `/bin/aws` and bind-mounts
# its grandparent (`/opt/biffix/aws-cli`) read-only into each task; the coordinator
# (docker/coordinator/lib/nextflow.sh) sets `aws.batch.cliPath` to this same path.
HOST_AWS_CLI_PATH = "/opt/biffix/aws-cli/bin/aws"


@dataclass(frozen=True)
class QueueClass:
    """One entry in the queue-class menu — the source of truth registration validates against."""

    name: str
    capacity: str = "SPOT"  # SPOT | ON_DEMAND
    # Either family keys ("c", "m", "r", "arm64", "g", "highmem") — sizes chosen by Batch — or
    # EXACT instance types ("c8g.4xlarge", "m7g.4xlarge"). Benchmarks pin exact types because the
    # size IS the measurement; general workloads use families. Detected by the presence of a ".".
    instances: list[str] = field(default_factory=list)
    coordinator: bool = False
    # The most vCPUs the queue-class's compute environment may run at once (its spend ceiling).
    max_vcpus: int = 256
    # Launch-template overrides. Every compute environment gets a launch template (the host boot
    # script, an encrypted root volume and IMDSv2-only metadata); these override its root volume
    # (e.g. 1 TB of scratch for a large sort; None => 30 GB gp3) and raise the IMDS hop limit so
    # in-container tooling can read instance metadata (None => 1: the host only, no container).
    ebs_gb: int | None = None
    ebs_throughput: int | None = None  # MiB/s (gp3: 125 baseline .. 1000)
    ebs_iops: int | None = None  # gp3: 3000 baseline .. 16000
    imds_hop_limit: int | None = None  # 2 lets a container (one hop past the host) reach IMDSv2


@dataclass(frozen=True)
class HostAwsCli:
    """The AWS CLI v2 every Batch host installs at boot (the launch template's user data).

    Nextflow stages each task's S3 I/O with an `aws` binary it bind-mounts from the host
    (`aws.batch.cliPath`), so task images need no AWS CLI of their own. The bundle is pinned by
    version and per-arch sha256 here; `deploy.sh` stages the zips into the reference bucket under
    `host/aws-cli/<version>/<arch>.zip` and hosts verify the checksum before installing.
    """

    version: str = "2.37.4"
    sha256_x86_64: str = "0c59444563f4df735eeb5481f6165f95dae546c33761760d8be9855d5cfe2d12"
    sha256_aarch64: str = "869aa72b9bdb931a9158d0cea9f4f0dc3f7aecf24ea1552da549b87907d28ab2"

    def key(self, arch: str) -> str:
        """The reference-bucket key of the zip for `arch` (`x86_64` | `aarch64`)."""
        return f"{HOST_PREFIX}aws-cli/{self.version}/{arch}.zip"


def parse_queue_class(raw: Raw) -> QueueClass:
    """One `queue_classes` entry (its keys are QueueClass's fields), validated."""
    name = text("queue-class name", raw.get("name"))
    where = f"queue-class {name!r}:"
    qc = QueueClass(
        **{
            **raw,
            "coordinator": flag(f"{where} coordinator", raw.get("coordinator")),
            "instances": text_list(f"{where} instances", raw.get("instances")),
        }
    )
    if not is_count(qc.max_vcpus, 1):
        raise ValueError(
            f"queue-class {qc.name!r} has max_vcpus {qc.max_vcpus!r}, expected a whole number >= 1"
        )
    if qc.capacity not in CAPACITIES:
        raise ValueError(
            f"queue-class {qc.name!r} has capacity {qc.capacity!r}, "
            f"expected one of {sorted(CAPACITIES)}"
        )
    # The launch template's gp3 root volume (`ebs_gb`, else DEFAULT_ROOT_VOLUME_GB) takes these as
    # they are, so a value gp3 refuses would fail only at CloudFormation deploy.
    gb = DEFAULT_ROOT_VOLUME_GB if qc.ebs_gb is None else count(f"{where} ebs_gb", qc.ebs_gb, 1)
    if qc.ebs_iops is not None:
        count(f"{where} ebs_iops", qc.ebs_iops, GP3_BASELINE_IOPS, GP3_MAX_IOPS)
        if qc.ebs_iops > GP3_IOPS_PER_GIB * gb:
            raise ValueError(
                f"{where} ebs_iops {qc.ebs_iops}: gp3 allows at most {GP3_IOPS_PER_GIB} per GiB "
                f"({GP3_IOPS_PER_GIB * gb} on its {gb} GiB root volume)"
            )
    if qc.ebs_throughput is not None:
        count(
            f"{where} ebs_throughput",
            qc.ebs_throughput,
            GP3_BASELINE_THROUGHPUT,
            GP3_MAX_THROUGHPUT,
        )
    if qc.imds_hop_limit is not None:
        count(f"{where} imds_hop_limit", qc.imds_hop_limit, 1, 64)
    return qc


def parse_host_aws_cli(block: Raw | None) -> HostAwsCli:
    """`host_aws_cli: {version, sha256_x86_64, sha256_aarch64}`; the default when unset. A version
    override must bring both checksums: a checksum left at its default would not match."""
    if not block:
        return HostAwsCli()
    required = ("version", "sha256_x86_64", "sha256_aarch64")
    missing = [k for k in required if not block.get(k)]
    if missing:
        raise ValueError(f"host_aws_cli requires {', '.join(missing)}")
    return HostAwsCli(**{key: text(f"host_aws_cli.{key}", block[key]) for key in required})


def parse_instance_families(block: object) -> dict[str, str]:
    """The default instance families, overridden or extended by `instance_families` (family key
    → instance class, both strings)."""
    if block is None:
        return dict(DEFAULT_INSTANCE_FAMILIES)
    if not isinstance(block, dict):
        raise ValueError(f"instance_families {block!r}: expected a mapping of family to class")
    families = {
        text("instance_families key", key): text(f"instance_families.{key}", value)
        for key, value in block.items()
    }
    return {**DEFAULT_INSTANCE_FAMILIES, **families}


def parse(raw: Raw) -> Fields:
    """The compute keys, as DeployConfig fields (`queue_classes` is required)."""
    return {
        "queue_classes": [parse_queue_class(qc) for qc in raw["queue_classes"]],
        "instance_families": parse_instance_families(raw.get("instance_families")),
        "host_aws_cli": parse_host_aws_cli(raw.get("host_aws_cli")),
        # null is the default (the release tag), so the example can show the key unset. A tag is
        # never read from a number: an unquoted `1.10` is the float 1.1.
        **(
            {"coordinator_image_tag": text("coordinator_image_tag", raw["coordinator_image_tag"])}
            if raw.get("coordinator_image_tag") is not None
            else {}
        ),
    }
