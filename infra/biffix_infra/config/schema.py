"""Every key the deploy config may hold, in one tree.

`SCHEMA` is the one list of keys: the loader rejects any key it does not name (`check`), each
section module parses the keys of its own section, and `tests/test_example_config.py` fails unless
`infra/biffix-deploy.example.yaml` documents every key here. A new key therefore goes in three
places: `SCHEMA`, its section's `parse`, and the example.
"""

from __future__ import annotations

from collections.abc import Iterator
from dataclasses import dataclass, field


@dataclass(frozen=True)
class Block:
    """The keys one mapping in the deploy config may hold.

    `nested` gives the `Block` of a key whose value is itself a mapping (or, with `many`, a list of
    mappings) that is checked key by key; a key whose value is a free-form mapping
    (`instance_families`) is not in it. `item` names one entry of a `many` block in error messages,
    and `label` is the key whose value tells the entries apart (`queue-class 'coordinator'`).
    """

    keys: tuple[str, ...]
    nested: dict[str, Block] = field(default_factory=dict)
    many: bool = False
    item: str = ""
    label: str = ""


OBJECT_LOCK = Block(keys=("mode", "days"))
IMAGE_ARCHIVE = Block(
    keys=(
        "instant_retrieval_after_days",
        "deep_archive_after_days",
        "min_tiered_object_bytes",
        "object_lock",
        "key_arn",
    ),
    nested={"object_lock": OBJECT_LOCK},
)
RETENTION = Block(keys=("intermediates_days", "snakemake_cache_days", "noncurrent_versions_days"))
HOST_AWS_CLI = Block(keys=("version", "sha256_x86_64", "sha256_aarch64"))
QUEUE_CLASS = Block(
    keys=(
        "name",
        "capacity",
        "instances",
        "coordinator",
        "max_vcpus",
        "ebs_gb",
        "ebs_throughput",
        "ebs_iops",
        "imds_hop_limit",
    ),
    many=True,
    item="queue-class",
    label="name",
)
DATA_SOURCE = Block(
    keys=(
        "id",
        "name",
        "bucket",
        "prefix",
        "description",
        "kind",
        "key_arn",
        "viewer_cors_applied",
    ),
    many=True,
    item="data source",
    label="id",
)
PROJECT = Block(
    keys=(
        "id",
        "key_arn",
        "bucket_name",
        "data_sources",
        "uploads_cors_applied",
        "viewer_cors_applied",
    ),
    nested={"data_sources": DATA_SOURCE},
    many=True,
    item="project",
    label="id",
)
AUTHORIZATION = Block(keys=("authenticated_default_role",))
GOOGLE_OIDC = Block(keys=("client_id",))
IMAGE_BUILD_TRUST = Block(keys=("subject", "ref"))
REGISTRATION = Block(
    keys=("github_subjects", "biffix_image_build"),
    nested={"biffix_image_build": IMAGE_BUILD_TRUST},
)

# The whole file, by section (the module that parses each is named in its comment).
SCHEMA = Block(
    keys=(
        # tenancy
        "tenant",
        "region",
        "account",
        # network
        "nat_mode",
        "worker_egress",
        # compute
        "queue_classes",
        "instance_families",
        "coordinator_image_tag",
        "host_aws_cli",
        # projects
        "projects",
        # data
        "reference_key_arn",
        "retention",
        "image_archive",
        # identity
        "authorization",
        "cognito_mfa",
        "cognito_domain_prefix",
        "google_oidc",
        # registration
        "registration",
        # persistence
        "aurora_backup_retention_days",
        "aurora_key_arn",
        # alarms
        "alert_email",
        # edge
        "app_domain",
        "app_certificate_arn",
    ),
    nested={
        "queue_classes": QUEUE_CLASS,
        "host_aws_cli": HOST_AWS_CLI,
        "projects": PROJECT,
        "retention": RETENTION,
        "image_archive": IMAGE_ARCHIVE,
        "authorization": AUTHORIZATION,
        "google_oidc": GOOGLE_OIDC,
        "registration": REGISTRATION,
    },
)


def check(value: object, block: Block = SCHEMA, where: str = "") -> None:
    """Refuse a key `block` does not name, here or in any nested block. An unknown (or misspelled)
    key is an error rather than silently ignored, so the config can never say something the deploy
    does not do (e.g. `retention: {intermediate_days: 7}` quietly keeping the 30-day default). A
    nested block that is null or absent is not checked: its section applies its defaults. Any
    other value must have the block's shape (`object_lock: false` is not a mapping)."""
    if block.many:
        if not isinstance(value, list):
            raise ValueError(f"{where}: expected a list, got {type(value).__name__}")
        # An entry of a list nested in another entry names that entry too
        # (`project 'p', data source 'a'`).
        parent = f"{where.rpartition('.')[0]}, " if "." in where else ""
        for entry in value:
            label = entry.get(block.label) if isinstance(entry, dict) else entry
            _check_mapping(entry, block, f"{parent}{block.item} {label!r}")
    else:
        _check_mapping(value, block, where)


def _check_mapping(value: object, block: Block, where: str) -> None:
    if not isinstance(value, dict):
        raise ValueError(f"{where}: expected a mapping, got {type(value).__name__}")
    unknown = sorted(str(k) for k in value if k not in block.keys)
    if unknown:
        prefix = f"{where}: " if where else ""
        raise ValueError(
            f"{prefix}unknown key(s) {', '.join(unknown)}; expected any of {sorted(block.keys)}"
        )
    for key, child in block.nested.items():
        if value.get(key) is not None:
            check(value[key], child, f"{where}.{key}" if where else key)


def key_paths(block: Block = SCHEMA, prefix: str = "") -> Iterator[str]:
    """Every key in `block` and the blocks nested in it, as a dotted path
    (`projects.data_sources.id`; a list's entries are not indexed)."""
    for key in block.keys:
        path = f"{prefix}{key}"
        yield path
        if key in block.nested:
            yield from key_paths(block.nested[key], f"{path}.")
