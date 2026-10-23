"""ProjectStack — one per project.

A project is the strong boundary within a tenant: its own data bucket + KMS key + run base role
(scoped to its bucket, key and data sources), its own Batch task log group, and its own
coordinator role (in the api stack, granted through `grant_coordinator_access`). Adding a project =
one more ProjectStack.

The control plane serves every project: the api stack writes each project's context to SSM for the
backend (`Coordinator`) and grants the backend and sweep Lambdas each project's
`grant_backend_*` (and the image builds its `workflow-sources/`). Those grants are the control
plane's only; nothing here widens a coordinator or run role beyond its own project.
"""

from __future__ import annotations

from typing import Any

from aws_cdk import Duration, RemovalPolicy, Stack
from aws_cdk import aws_iam as iam
from aws_cdk import aws_kms as kms
from aws_cdk import aws_logs as logs
from aws_cdk import aws_s3 as s3
from constructs import Construct

from biffix_infra.config import (
    ABORT_INCOMPLETE_UPLOADS_DAYS,
    ABORT_INCOMPLETE_UPLOADS_RULE_ID,
    NONCURRENT_VERSIONS_RULE_ID,
    RUNS_PREFIX,
    SNAKEMAKE_CACHE_PREFIX,
    UPLOADS_PREFIX,
    WORKFLOW_SOURCES_PREFIX,
    DataSource,
    project_bucket_cors,
)
from biffix_infra.config.cors import CorsRule
from biffix_infra.constructs.encrypted_bucket import EncryptedBucket, customer_key
from biffix_infra.wiring import LOG_RETENTION


class ProjectStack(Stack):
    def __init__(
        self,
        scope: Construct,
        cid: str,
        *,
        project_id: str,
        permissions_boundary: iam.IManagedPolicy,
        key_arn: str | None = None,
        bucket_name: str | None = None,
        data_sources: tuple[DataSource, ...] = (),
        browser_origin: str = "http://localhost:5173",
        uploads_cors_applied: bool = False,
        viewer_cors_applied: bool = False,
        noncurrent_versions_days: int | None = 30,
        **kwargs: Any,
    ) -> None:
        super().__init__(scope, cid, **kwargs)
        self.project_id = project_id
        self.data_sources = data_sources
        # Browsers may upload into the bucket when it has the CORS rule they need: a created bucket
        # gets it below; an imported one only when the deploy config says its owner applied it.
        self.uploads_enabled = bucket_name is None or uploads_cors_applied
        # Likewise the alignment viewer's cross-origin Range reads (`viewer_cors_rule`).
        self.viewer_cors = bucket_name is None or viewer_cors_applied

        # Data bucket. Two modes:
        #  - bucket_name set  -> IMPORT an existing bucket (migration: reference data in place,
        #    never take ownership — product non-goal: no data movement). biffix attaches only IAM
        #    grants; it does NOT set lifecycle/encryption/versioning on a bucket it doesn't own
        #    (CloudFormation would fight the real bucket: settings the stack applies drift from the
        #    ones its owner keeps, and each deploy reverts the other's changes).
        #  - bucket_name None -> CREATE a biffix-managed, KMS-encrypted, versioned, RETAIN bucket.
        # KMS:
        #  - key_arn set  -> import that CMK and grant the run role encrypt/decrypt.
        #  - key_arn None + imported bucket -> assume SSE-S3 (no CMK, e.g. acme-genomics-data); no
        #    KMS grant needed, S3 handles encryption transparently.
        #  - key_arn None + created bucket -> mint a per-project CMK.
        self.key: kms.IKey | None = None
        if key_arn or not bucket_name:
            self.key = customer_key(self, "ProjectKey", key_arn)
        if bucket_name:
            self.bucket = s3.Bucket.from_bucket_name(self, "ProjectBucket", bucket_name)
        else:
            assert self.key is not None  # minted above: a created bucket always has its own CMK
            self.bucket = EncryptedBucket(
                self,
                "ProjectBucket",
                encryption_key=self.key,
                # Browser uploads: the site PUTs parts to presigned URLs and
                # reads each part's ETag; abandoned uploads' parts are deleted after a week. The
                # alignment viewer reads BAMs and their indexes in Range GETs. The same rules the
                # deploy prints for an imported bucket (`project_bucket_cors`).
                cors=[
                    _cdk_cors_rule(rule)
                    for rule in project_bucket_cors(browser_origin)["CORSRules"]
                ],
                lifecycle_rules=[
                    s3.LifecycleRule(
                        id=ABORT_INCOMPLETE_UPLOADS_RULE_ID,
                        prefix=UPLOADS_PREFIX,
                        abort_incomplete_multipart_upload_after=Duration.days(
                            ABORT_INCOMPLETE_UPLOADS_DAYS
                        ),
                    ),
                    # Versioned, so a delete only hides an object: its bytes stay as a noncurrent
                    # version. Expiring those is what makes the intermediates and cache sweeps
                    # (key-level deletes) actually free storage, and bounds how long an overwritten
                    # or deleted result can be recovered (config `noncurrent_versions_days`).
                    # Bucket-wide: a lifecycle filter cannot select `runs/*/work/`.
                    *(
                        [
                            s3.LifecycleRule(
                                id=NONCURRENT_VERSIONS_RULE_ID,
                                noncurrent_version_expiration=Duration.days(
                                    noncurrent_versions_days
                                ),
                                expired_object_delete_marker=True,
                            )
                        ]
                        if noncurrent_versions_days is not None
                        else []
                    ),
                ],
            )

        # The per-project run base role a run's child jobs run as. Batch delivers it to each task
        # (the coordinator registers the child job definitions with it); nothing assumes it with
        # STS, so only ECS tasks are trusted.
        self.run_base_role = iam.Role(
            self,
            "RunBaseRole",
            assumed_by=iam.ServicePrincipal("ecs-tasks.amazonaws.com"),
            permissions_boundary=permissions_boundary,
        )
        self._grant_read_write(self.run_base_role)

        # The log group this project's task jobs write to (the coordinator passes it, from the
        # BIFFIX_TASK_LOG_GROUP its job definition carries, as Nextflow's `aws.batch.logsGroup`
        # and Snakemake's `--aws-batch-log-group`), so one project's coordinator cannot read
        # another project's task logs. Named after the stack (`biffix-<tenant>-project-<id>`), so
        # tenants sharing an account never collide. Hosts write it with their instance role (the
        # ECS agent's awslogs driver; the managed AmazonEC2ContainerServiceforEC2Role allows
        # CreateLogStream/PutLogEvents). Every coordinator logs to the shared `/aws/batch/job`
        # (compute stack).
        self.task_log_group_name = f"/biffix/{self.stack_name}/batch"
        self.task_log_group = logs.LogGroup(
            self,
            "TaskLogGroup",
            log_group_name=self.task_log_group_name,
            retention=LOG_RETENTION,
            removal_policy=RemovalPolicy.DESTROY,
        )

    def grant_data_sources_read(self, grantee: iam.IGrantable) -> None:
        """Grant read on every data source: list the bucket only under the source's prefix, and get
        objects under it (plus decrypt with its CMK, when it has one). Principal-side only — biffix
        never touches a data source's bucket policy."""
        for source in self.data_sources:
            bucket_arn = self.format_arn(
                service="s3", region="", account="", resource=source.bucket
            )
            iam.Grant.add_to_principal(
                grantee=grantee,
                actions=["s3:GetObject", "s3:GetObjectVersion"],
                resource_arns=[f"{bucket_arn}/{source.prefix}*"],
            )
            iam.Grant.add_to_principal(
                grantee=grantee,
                actions=["s3:ListBucket"],
                resource_arns=[bucket_arn],
                conditions=(
                    {"StringLike": {"s3:prefix": [f"{source.prefix}*"]}} if source.prefix else None
                ),
            )
            if source.key_arn:
                iam.Grant.add_to_principal(
                    grantee=grantee, actions=["kms:Decrypt"], resource_arns=[source.key_arn]
                )

    def grant_coordinator_access(self, grantee: iam.IGrantable) -> None:
        """Grant this project's coordinator (head job) read/write on this project's bucket + key
        and read on its data sources. The coordinator keeps the engine's metadata here, stages
        every child job's I/O under the run prefix via the S3 default storage provider, and stats
        (Nextflow: stages) inputs itself.

        Called by the api stack for the project's OWN coordinator role only, so a run can reach
        one project's data. The grant lands in the role's own policy (downstream of
        this stack) with no cycle. For an IMPORTED bucket/key the grant is principal-side only; CDK
        never mutates a bucket policy on a bucket biffix may not own."""
        self._grant_read_write(grantee)

    def _grant_read_write(self, grantee: iam.IGrantable) -> None:
        """Read and write the project's bucket, and read its data sources (a run reads its inputs
        from them too)."""
        self.bucket.grant_read_write(grantee)
        # Only grant KMS when there's a CMK (SSE-S3 imported buckets have none).
        if self.key is not None:
            self.key.grant_encrypt_decrypt(grantee)
        self.grant_data_sources_read(grantee)

    def grant_backend_read(self, grantee: iam.IGrantable) -> None:
        """Grant the backend read-only access to this project's bucket (to HEAD-check submit input
        URIs). For a CREATED bucket `grant_read` also adds `kms:Decrypt` via the bucket's own key
        reference; for an IMPORTED bucket CDK doesn't know the key, so the CMK decrypt is granted
        explicitly here — mirroring `grant_coordinator_access`. Without it, a HEAD on an
        imported+BYOK project bucket gets AccessDenied (the failure the sibling grant already
        avoids)."""
        self.bucket.grant_read(grantee)
        if self.key is not None:
            self.key.grant_decrypt(grantee)

    def grant_backend_run_inputs(self, grantee: iam.IGrantable) -> None:
        """Grant the backend `s3:PutObject` on runs' `inputs/` folders, where it writes the
        samplesheet a submit gave as rows (`biffix/runs/<run>/inputs/<param>.csv`). Nothing else
        under the run prefix, and nothing outside it."""
        self._grant_put(grantee, f"{RUNS_PREFIX}*/inputs/*")

    def grant_backend_run_provenance(self, grantee: iam.IGrantable) -> None:
        """Grant the backend sweep `s3:PutObject` on runs' `provenance/` folders, where it writes
        each ended run's RO-Crate (`biffix/runs/<run>/provenance/ro-crate-metadata.json`). Nothing
        else under the run prefix, and nothing outside it."""
        self._grant_put(grantee, f"{RUNS_PREFIX}*/provenance/*")

    def grant_backend_uploads(self, grantee: iam.IGrantable) -> None:
        """Grant the backend what it needs to run browser uploads under `uploads/`: start and
        complete multipart uploads (`s3:PutObject`), abort them and list their parts. The browser
        PUTs parts with URLs the backend presigns, so these grants bound what the browser can
        write. Nothing outside `uploads/`."""
        iam.Grant.add_to_principal(
            grantee=grantee,
            actions=["s3:PutObject", "s3:AbortMultipartUpload", "s3:ListMultipartUploadParts"],
            resource_arns=[self.bucket.arn_for_objects(f"{UPLOADS_PREFIX}*")],
        )
        if self.key is not None:
            self.key.grant_encrypt_decrypt(grantee)

    def grant_retention_delete(self, grantee: iam.IGrantable) -> None:
        """Grant the retention sweep (the reconcile Lambda) what it needs to delete Nextflow runs'
        intermediate files: list under `biffix/runs/` and delete under a `work/` there. IAM cannot
        name one path segment (`*` also matches `/`), so the grant reaches any `work/` below
        `biffix/runs/`, not only a run's own; the sweep's own key selection (a run's
        `biffix/runs/<run>/work/`) is what keeps results out of reach, and S3 versioning keeps a
        deleted object recoverable for `noncurrent_versions_days`."""
        self._grant_list_delete(grantee, RUNS_PREFIX, f"{RUNS_PREFIX}*/work/*")

    def grant_cache_expiry(self, grantee: iam.IGrantable) -> None:
        """Grant the cache-expiry sweep (the reconcile Lambda) what it needs to delete old Snakemake
        cache entries: list and delete only under `biffix/cache/snakemake/`. The cache holds copies
        a later run can rebuild; runs' results are out of reach."""
        self._grant_list_delete(grantee, SNAKEMAKE_CACHE_PREFIX, f"{SNAKEMAKE_CACHE_PREFIX}*")

    def _grant_list_delete(self, grantee: iam.IGrantable, prefix: str, objects: str) -> None:
        """List the bucket under `prefix`, and delete the objects matching `objects`."""
        iam.Grant.add_to_principal(
            grantee=grantee,
            actions=["s3:ListBucket"],
            resource_arns=[self.bucket.bucket_arn],
            conditions={"StringLike": {"s3:prefix": [f"{prefix}*"]}},
        )
        iam.Grant.add_to_principal(
            grantee=grantee,
            actions=["s3:DeleteObject"],
            resource_arns=[self.bucket.arn_for_objects(objects)],
        )

    def grant_backend_source_staging(self, grantee: iam.IGrantable) -> None:
        """Grant the backend `s3:PutObject` on the workflow-source staging prefix so it can sign
        presigned PUTs for S3-staged source (private-repo support): a presigned URL only authorizes
        what the signer's own role permits. Scoped tightly to `workflow-sources/*` — the backend
        never needs to write anywhere else in the project bucket. For an imported+BYOK bucket the
        CMK encrypt is granted explicitly, mirroring `grant_backend_read`."""
        self._grant_put(grantee, f"{WORKFLOW_SOURCES_PREFIX}*")

    def _grant_put(self, grantee: iam.IGrantable, pattern: str) -> None:
        """`s3:PutObject` on the project bucket's keys matching `pattern`, plus encrypt with its
        CMK (granted explicitly: for an imported bucket CDK does not know the key)."""
        self.bucket.grant_put(grantee, pattern)
        if self.key is not None:
            self.key.grant_encrypt(grantee)


def _cdk_cors_rule(rule: CorsRule) -> s3.CorsRule:
    """An `aws s3api` CORS rule (biffix_infra.config's JSON) as CDK's."""
    return s3.CorsRule(
        allowed_origins=rule["AllowedOrigins"],
        allowed_methods=[s3.HttpMethods(m) for m in rule["AllowedMethods"]],
        allowed_headers=rule["AllowedHeaders"],
        exposed_headers=rule["ExposeHeaders"],
        max_age=rule["MaxAgeSeconds"],
    )
