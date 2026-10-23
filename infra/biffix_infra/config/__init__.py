"""The deploy config (`infra/biffix-deploy.example.yaml` documents every key).

Infra config is *data, not code*: the whole CDK app is parameterized by this file, so a new
deployment is a config edit + ``cdk deploy``, not a code change.

    schema       every key the file may hold (the loader refuses any other)
    load         file -> DeployConfig: key check, then each section's parse
    model        DeployConfig, the typed whole, with each key's default
    tenancy, network, compute, projects, data, identity, registration, persistence, alarms, edge
                 one module per section: its nested types, parsing and validation
    naming       the names and URLs derived from the config (stacks, queues, build projects)
    s3_layout    where biffix keeps things in its buckets, and their lifecycle rules
    cors         the browser's CORS rules, and what to print for buckets biffix does not own

The names below are re-exported so callers import them from `biffix_infra.config`.
"""

from biffix_infra.config.compute import (
    DEFAULT_INSTANCE_FAMILIES,
    DEFAULT_ROOT_VOLUME_GB,
    GP3_BASELINE_IOPS,
    GP3_BASELINE_THROUGHPUT,
    HOST_AWS_CLI_PATH,
    HostAwsCli,
    QueueClass,
)
from biffix_infra.config.cors import (
    CORS_MAX_AGE_SECONDS,
    bucket_cors_instructions,
    project_bucket_cors,
    upload_bucket_settings,
    upload_cors_rule,
    viewer_bucket_cors,
    viewer_cors_rule,
)
from biffix_infra.config.data import ImageArchive, ObjectLock, parse_image_archive
from biffix_infra.config.identity import GoogleOidc
from biffix_infra.config.model import DeployConfig
from biffix_infra.config.naming import IMAGE_BUILD_ARCHS
from biffix_infra.config.projects import DataSource, Project, validate_projects
from biffix_infra.config.registration import ImageBuildTrust
from biffix_infra.config.s3_layout import (
    ABORT_INCOMPLETE_UPLOADS_DAYS,
    ABORT_INCOMPLETE_UPLOADS_RULE_ID,
    ARCHIVE_BLOBS_PREFIX,
    HOST_PREFIX,
    NONCURRENT_VERSIONS_RULE_ID,
    RUNS_PREFIX,
    SNAKEMAKE_CACHE_PREFIX,
    UPLOADS_PREFIX,
    WORKFLOW_SOURCES_PREFIX,
    noncurrent_versions_lifecycle_rule,
)

__all__ = [
    "ABORT_INCOMPLETE_UPLOADS_DAYS",
    "ARCHIVE_BLOBS_PREFIX",
    "CORS_MAX_AGE_SECONDS",
    "DEFAULT_INSTANCE_FAMILIES",
    "DEFAULT_ROOT_VOLUME_GB",
    "GP3_BASELINE_IOPS",
    "GP3_BASELINE_THROUGHPUT",
    "HOST_AWS_CLI_PATH",
    "HOST_PREFIX",
    "IMAGE_BUILD_ARCHS",
    "ABORT_INCOMPLETE_UPLOADS_RULE_ID",
    "NONCURRENT_VERSIONS_RULE_ID",
    "RUNS_PREFIX",
    "SNAKEMAKE_CACHE_PREFIX",
    "UPLOADS_PREFIX",
    "WORKFLOW_SOURCES_PREFIX",
    "DataSource",
    "DeployConfig",
    "GoogleOidc",
    "HostAwsCli",
    "ImageArchive",
    "ImageBuildTrust",
    "ObjectLock",
    "Project",
    "QueueClass",
    "bucket_cors_instructions",
    "noncurrent_versions_lifecycle_rule",
    "parse_image_archive",
    "project_bucket_cors",
    "upload_bucket_settings",
    "upload_cors_rule",
    "validate_projects",
    "viewer_bucket_cors",
    "viewer_cors_rule",
]
