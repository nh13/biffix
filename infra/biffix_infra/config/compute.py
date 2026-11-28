"""The compute section: the queue-class menu, the instance families, the coordinator image and the
AWS CLI every Batch host installs (ComputeStack, the api stack's coordinator template)."""

from __future__ import annotations

import re
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
    # A single-job queue-class: each job on it is a whole run (the engine and every task, in nested
    # apptainer) given the whole host. Its compute environment is its own (never shared with
    # tasks-mode work), on-demand, on exact instance types, and its hosts boot with the relaxed
    # seccomp profile and the instance-store `/scratch` (ComputeStack). Not a worker queue-class:
    # no coordinator submits to it; the backend does, for a single-job run.
    single_job: bool = False
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
    check_queue_class_name(name)
    where = f"queue-class {name!r}:"
    qc = QueueClass(
        **{
            **raw,
            "coordinator": flag(f"{where} coordinator", raw.get("coordinator")),
            "single_job": flag(f"{where} single_job", raw.get("single_job")),
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
    if qc.single_job:
        check_single_job(qc)
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


def check_queue_class_name(name: str) -> None:
    """Refuse a queue-class name of digits only. The maps keyed by queue-class name that the stacks
    render to JSON (the coordinator's BIFFIX_QUEUE_CLASS_MAP, whose first entry is the default
    worker queue-class) go through CDK's JavaScript, which puts integer-like keys first: such a name
    would silently become the default."""
    if re.fullmatch(r"[0-9]+", name):
        raise ValueError(
            f"queue-class name {name!r} is digits only; give it a letter (e.g. 'q{name}'): the "
            "JSON the stacks render would put it first and make it the default"
        )


def check_single_job(qc: QueueClass) -> None:
    """A single-job queue-class's constraints. On-demand: a reclaimed host would lose the whole
    run, which keeps its work on the host's own disks. Exact instance types only: a job asks for
    the whole host, so the backend must know each type's size, and a family would let Batch place
    it on a larger host beside another run. One size (`c6id.8xlarge` with `m6id.8xlarge`, never
    with `c6id.16xlarge`), for the same reason; for a metal size, one family too, as a metal type
    is the whole machine, whose size differs by family. One architecture, x86_64 or Graviton
    (arm64), judged by the type's name here; the backend checks EC2's own answer. Not the
    first-generation Graviton `a1`, which the ECS-optimized AL2023 AMI these hosts run does not
    support. A suffix is not always one vCPU count (`hpc6id.32xlarge` has 64, `x2idn.32xlarge`
    128); the backend refuses types of different vCPU counts from EC2's description, which this
    config check cannot see. Never also the coordinator queue-class: that one runs tasks-mode
    coordinators, which must not land on these hosts."""
    where = f"single-job queue-class {qc.name!r}"
    if qc.coordinator:
        raise ValueError(f"{where} cannot also be the coordinator queue-class")
    if qc.capacity != "ON_DEMAND":
        raise ValueError(
            f"{where} has capacity {qc.capacity!r}, expected ON_DEMAND: a reclaimed host would "
            "lose the whole run"
        )
    if not qc.instances:
        raise ValueError(f"{where} names no instances; give exact types like 'c6id.8xlarge'")
    families = [entry for entry in qc.instances if "." not in entry]
    if families:
        raise ValueError(
            f"{where} names instance families {families}; give exact types like 'c6id.8xlarge' "
            "(a job asks for the whole host, so its size must be known)"
        )
    sizes = sorted({entry.split(".", 1)[1] for entry in qc.instances})
    if len(sizes) > 1:
        raise ValueError(
            f"{where} names several sizes {sizes} in {qc.instances}; give one size (e.g. "
            "'c6id.8xlarge' and 'm6id.8xlarge') and a single-job queue-class per size: a job "
            "sized for the smaller host fits twice on a larger one, so two runs could share "
            "that host's CPUs, memory and /scratch"
        )
    metal_families = sorted({entry.split(".", 1)[0] for entry in qc.instances})
    if sizes[0].startswith("metal") and len(metal_families) > 1:
        raise ValueError(
            f"{where} names {sizes[0]} types of several families {metal_families}; a metal size "
            "is the whole machine, which differs by family (z1d.metal has 48 vCPUs, r6id.metal "
            "128), so give one family: a job sized for the smaller host fits twice on a larger one"
        )
    graviton = [entry for entry in qc.instances if is_graviton(entry)]
    x86_64 = [entry for entry in qc.instances if not is_graviton(entry)]
    if graviton and x86_64:
        raise ValueError(
            f"{where} mixes Graviton (arm64) types {graviton} with x86_64 types {x86_64}; give "
            "each single-job queue-class types of one architecture (and a queue-class per "
            "architecture): Batch may place a run on any of its types, and a run's task images "
            "are checked against one architecture"
        )
    a1 = [entry for entry in qc.instances if entry.split(".", 1)[0] == "a1"]
    if a1:
        raise ValueError(
            f"{where} names first-generation Graviton types {a1}; single-job hosts run the "
            "ECS-optimized AL2023 AMI, which does not support A1 instances (nor do they have the "
            "instance store single-job runs work on): use a later Graviton type, such as c7gd"
        )
    accelerated = [entry for entry in qc.instances if is_accelerated(entry)]
    if accelerated:
        raise ValueError(
            f"{where} names accelerated (GPU, FPGA or ML chip) types {accelerated}; single-job "
            "hosts run the ECS-optimized AL2023 AMI, which has no accelerator driver, and a run "
            "asks for no accelerator, so it would pay for one it cannot use"
        )


def is_graviton(instance_type: str) -> bool:
    """Whether exact type `instance_type` is a Graviton (arm64) type: its class is a family and a
    generation followed by `g` (`c7gd.8xlarge`, `m8g.4xlarge`, `r6gd.large`), which no x86 class
    is (`g5`, `g6e` are GPU classes: no generation digit before their `g`), or the first-generation
    Graviton `a1`."""
    family = instance_type.split(".", 1)[0]
    return family == "a1" or re.fullmatch(r"[a-z]+\d+g[a-z0-9-]*", family) is not None


def is_accelerated(instance_type: str) -> bool:
    """Whether exact type `instance_type` is an accelerated type: a GPU (`g5`, `g6e`, `gr6`,
    `gr6f`, `p4d`), FPGA (`f1`), Inferentia (`inf2`), Trainium (`trn1`), Gaudi (`dl1`) or video
    transcoding (`vt1`) class, each a family prefix and then the generation digit. No x86 general,
    compute, memory or storage class starts with one of these prefixes and a digit."""
    family = instance_type.split(".", 1)[0]
    return re.fullmatch(r"(g|gr|p|f|inf|trn|dl|vt)\d[a-z0-9-]*", family) is not None


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
