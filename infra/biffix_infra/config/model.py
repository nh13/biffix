"""DeployConfig: the whole deploy config, typed, with each key's default.

The fields are flat (the stacks read `config.nat_mode`, not `config.network.nat_mode`) but grouped
here by the section that parses them; the section modules hold each section's nested types.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from pathlib import Path
from typing import TypedDict

from biffix_infra.config.compute import (
    DEFAULT_INSTANCE_FAMILIES,
    DEFAULT_ROOT_VOLUME_GB,
    GP3_BASELINE_IOPS,
    GP3_BASELINE_THROUGHPUT,
    HostAwsCli,
    QueueClass,
)
from biffix_infra.config.data import ImageArchive
from biffix_infra.config.identity import GoogleOidc
from biffix_infra.config.naming import DeployNames
from biffix_infra.config.projects import Project
from biffix_infra.config.registration import ImageBuildTrust
from biffix_infra.version import release_tag


class QueueVolume(TypedDict):
    """A queue's gp3 root volume: its size (GB), IOPS and throughput (MiB/s)."""

    gb: int
    iops: int
    throughput: int


class QueuePricing(TypedDict):
    """What the cost estimator needs of a queue (`DeployConfig.queue_pricing`), as the reconcile
    Lambda reads it from `BIFFIX_QUEUE_PRICING`."""

    capacity: str
    volume: QueueVolume


@dataclass(frozen=True)
class DeployConfig(DeployNames):
    # ── tenancy ──
    tenant: str
    region: str
    # ── projects ──
    projects: list[Project]
    # ── compute ──
    queue_classes: list[QueueClass]
    # Instance-family → instance-class map (family key → class string, e.g. "c" → "c6i"; the class
    # is passed to Batch as-is and Batch picks the size within it). Defaults to
    # `DEFAULT_INSTANCE_FAMILIES`; a deploy's `instance_families` block overrides/extends it so
    # generations track config, not code.
    instance_families: dict[str, str] = field(
        default_factory=lambda: dict(DEFAULT_INSTANCE_FAMILIES)
    )
    # The base coordinator image's tag in the tenant ECR. ECR tag immutability is deliberate (a run
    # must always resolve the same bytes), which justifies immutable *tags* — not putting the tag
    # in *code*. Shipping a rebuilt image is pushing a NEW tag + bumping this in the deploy config,
    # never a source edit. Runs use coordinator variants (this base + an engine runtime layer) that
    # the backend builds from it. The default is this checkout's release tag (`v<version>`), the
    # tag `deploy.sh --release` imports the release's image under; a deploy that builds its own
    # image names the tag it pushed instead (a commit SHA). Runs cannot launch until the tag names
    # a pushed image.
    coordinator_image_tag: str = field(default_factory=release_tag)
    # The AWS CLI every Batch host installs at boot (see HostAwsCli).
    host_aws_cli: HostAwsCli = field(default_factory=HostAwsCli)
    # ── network ──
    # Outbound-egress cost profile for private subnets:
    #   "gateway"  — managed NAT gateway (recommended: HA, no maintenance; ~$32/mo).
    #   "instance" — a single t4g.nano NAT instance (the low-cost default; ~$4/mo, but a single
    #                point of failure you patch yourself).
    nat_mode: str = "instance"
    # Where Batch workers (every compute environment but the coordinator's) may connect:
    #   "nat"          — anywhere, through the NAT (default).
    #   "ecr_s3_only"  — only the tenant's ECR, ECS, CloudWatch Logs and S3 (same region): workers
    #                    run in isolated subnets reached through VPC endpoints, with no route to the
    #                    internet, so a task can pull no image the tenant does not hold and fetch
    #                    nothing from the internet. Their S3 endpoint only reaches buckets in this
    #                    account, the configured data sources' and imported project buckets, and
    #                    ECR's own layer bucket, so a task cannot copy data to a bucket elsewhere.
    #                    NOT closed: whatever the task's own credentials allow within those buckets
    #                    and CloudWatch Logs. ~$90/mo in interface endpoints; switching it replaces
    #                    the worker compute environments (do it while they are idle).
    worker_egress: str = "nat"
    # ── data ──
    # Optional deployer-provided KMS key ARN for the shared reference bucket (BYOK). None => biffix
    # creates a customer-managed key.
    reference_key_arn: str | None = None
    # Days after a run ends that its intermediate files (a Nextflow run's own `biffix/runs/<run>/
    # work/`) are deleted by the reconcile tick; results are never deleted.
    # `retention: {intermediates_days: null}` keeps them forever (a run can still ask for deletion
    # on success).
    intermediates_retention_days: int | None = 30
    # Days a Snakemake between-workflow cache entry is kept (the reconcile tick deletes older ones
    # from every project bucket); `retention: {snakemake_cache_days: null}` keeps entries forever.
    snakemake_cache_days: int | None = 90
    # Days a created project bucket keeps an object's superseded (overwritten or deleted) versions
    # before S3 expires them. The buckets are versioned, so a delete (the intermediates and cache
    # sweeps above, a user overwriting a result) only hides an object until then: this is the
    # window in which it can still be recovered, and after it the storage is freed. Bucket-wide (an
    # S3 lifecycle filter cannot select `runs/*/work/`). `retention: {noncurrent_versions_days:
    # null}` keeps every version forever, so the sweeps free no space. An imported bucket's owner
    # applies the equivalent rule (the deploy prints it).
    noncurrent_versions_days: int | None = 30
    image_archive: ImageArchive = field(default_factory=ImageArchive)
    # ── identity ──
    # Policy for identities admitted by the tenant Cognito pool that no group maps to a role.
    # Google does not emit cognito:groups; the deployment must therefore state a safe default
    # rather than deny every federated user accidentally.
    authenticated_default_role: str = "viewer"
    # Cognito multi-factor sign-in for the pool's own (non-federated) users: "off", "optional"
    # (each user may enrol an authenticator app) or "required". TOTP only (no SMS). Federated
    # (Google) sign-ins are governed by the IdP's own MFA.
    cognito_mfa: str = "optional"
    # Optional Cognito hosted-UI domain prefix (e.g. "biffix-acme" =>
    # biffix-acme.auth.<region>.amazoncognito.com). It hosts the OAuth endpoints the browser and
    # the CLI sign in through (and Google federation's redirect). None => no hosted-UI domain, and
    # the backend serves no OAuth client (`GET /auth/config` answers 503).
    #
    # STABLE IDENTIFIER — do not churn it. When google_oidc is set, this prefix defines the exact
    # Cognito callback (`https://<prefix>.auth.<region>.amazoncognito.com/oauth2/idpresponse`) that
    # the Google OAuth client must whitelist. Changing the prefix (or account/region) silently
    # breaks federated sign-in until that Google-side redirect URI is re-registered by hand. The
    # identity stack emits the current value as the `GoogleAuthorizedRedirectUri` output per deploy.
    cognito_domain_prefix: str | None = None
    # Optional Google Workspace federation. None => Cognito-only (no upstream IdP wired).
    google_oidc: GoogleOidc | None = None
    # ── registration ──
    # GitHub Actions OIDC subjects allowed to assume the registration role. Empty ⇒ no GitHub-OIDC
    # registration path is provisioned at all (fail-safe by absence). Each deployer sets their OWN
    # repo(s), e.g. ["repo:acme-bio/pipelines:ref:refs/heads/main"].
    github_subjects: list[str] = field(default_factory=list)
    # Trust for biffix's own image-build workflow. None => no such role is provisioned (fail-safe
    # by absence), and biffix images must then be built and pushed by an operator.
    image_build_trust: ImageBuildTrust | None = None
    # ── persistence ──
    # Aurora automated-backup / point-in-time-recovery retention window, in days. The cluster
    # holds the entire event-sourced system of record, so backup depth is an explicit choice, not
    # whatever RDS defaults to. deletion_protection is always on (see PersistenceStack); a
    # deliberate teardown must disable it first, the same as the stack's termination protection.
    aurora_backup_retention_days: int = 14
    # Optional deployer-provided KMS key ARN for the event store: the Aurora cluster's storage,
    # its generated credential and the capability signing key (BYOK). None => biffix creates a
    # customer-managed key. Changing it replaces the cluster.
    aurora_key_arn: str | None = None
    # ── alarms ──
    # Optional address subscribed (by email, confirmed once from the AWS message) to the operational
    # alarms topic. More recipients can be added from the UI's Settings page. None => the topic has
    # no config-managed subscriber.
    alert_email: str | None = None
    # ── edge ──
    # Stable browser URL used by Cognito callbacks (e.g. biffix.example.org). The CloudFront alias
    # is activated only once app_certificate_arn is known (deploy.sh requests and waits for one when
    # it is not set); until then the distribution uses its default *.cloudfront.net domain.
    app_domain: str | None = None
    app_certificate_arn: str | None = None
    # ── tenancy ──
    # The 12-digit AWS account this tenant lives in. deploy.sh and teardown.sh refuse to touch any
    # other account (and `--account`, when also given, must agree). None => the scripts require
    # `--account` instead.
    account: str | None = None

    @classmethod
    def load(cls, path: str | Path) -> DeployConfig:
        """Read and validate the deploy config at `path` (see `biffix_infra.config.load`)."""
        from biffix_infra.config.load import load

        return load(path)

    def queue_pricing(self) -> dict[str, QueuePricing]:
        """Per physical queue, what the cost estimator needs: its configured capacity (the
        fallback spot/on-demand label for a host whose own lifecycle was not recorded) and its root
        volume (a queue without an override gets a 30 GB gp3 root, see ComputeStack)."""
        return {
            self.queue_name(qc): {
                "capacity": qc.capacity.lower(),
                "volume": {
                    "gb": qc.ebs_gb or DEFAULT_ROOT_VOLUME_GB,
                    "iops": qc.ebs_iops or GP3_BASELINE_IOPS,
                    "throughput": qc.ebs_throughput or GP3_BASELINE_THROUGHPUT,
                },
            }
            for qc in self.queue_classes
        }

    @property
    def coordinator_queue_class(self) -> QueueClass:
        """The single on-demand, no-spot coordinator queue-class."""
        coordinators = [qc for qc in self.queue_classes if qc.coordinator]
        if not coordinators:
            raise ValueError("no coordinator queue-class defined in the deploy config")
        if len(coordinators) > 1:
            names = ", ".join(qc.name for qc in coordinators)
            raise ValueError(f"exactly one coordinator queue-class expected, found: {names}")
        return coordinators[0]

    @property
    def data_source_buckets(self) -> list[str]:
        """Every data source's bucket (read-only to runs), across projects."""
        return sorted({s.bucket for p in self.projects for s in p.data_sources})

    @property
    def imported_project_buckets(self) -> list[str]:
        """The existing buckets projects reference in place (possibly in another account)."""
        return sorted({p.bucket_name for p in self.projects if p.bucket_name})
