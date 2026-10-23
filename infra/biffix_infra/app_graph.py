"""The biffix stack graph: every stack a deploy synthesizes, wired together.

Stack graph (all edges inferred by CDK from resource tokens — no manual `add_dependency`):

    data ··> identity ──> project-*      (identity uses data's repositories for its GitHub roles)
    network, data ──> compute
    data, project-* ──> build
    network ──> persistence
    network, persistence, project-* ──> ingest
    network, data, identity, compute, build, project-*, persistence, ingest ──> api ──> edge

`api` is the hub: it owns every runtime role (coordinator + backend) at the DAG sink, so resource
stacks expose `grant_*` methods it calls — grants attach to the role directly, with no
string-constructed ARNs, no per-project inline policy, and no hand-authored dependency graph.

app.py and the synth tests both build the graph here, so the tests assert on the wiring a deploy
really synthesizes; only the stack names, the environment, termination protection and the UI build
differ between them.
"""

from __future__ import annotations

from collections.abc import Callable
from pathlib import Path
from typing import TypedDict

import aws_cdk as cdk
from aws_cdk import aws_ecs as ecs

from biffix_infra.config import DeployConfig, validate_projects
from biffix_infra.stacks.api_stack import ApiStack
from biffix_infra.stacks.build_stack import BuildStack
from biffix_infra.stacks.compute_stack import ComputeStack
from biffix_infra.stacks.data_stack import DataStack
from biffix_infra.stacks.edge_stack import EdgeStack
from biffix_infra.stacks.identity_stack import IdentityStack
from biffix_infra.stacks.ingest_stack import IngestStack
from biffix_infra.stacks.network_stack import NetworkStack
from biffix_infra.stacks.persistence_stack import PersistenceStack
from biffix_infra.stacks.project_stack import ProjectStack


class Stacks(TypedDict):
    """The deploy's stacks by short name (`build_stacks`), and the app they are in."""

    app: cdk.App
    network: NetworkStack
    data: DataStack
    identity: IdentityStack
    compute: ComputeStack
    build: BuildStack
    projects: list[ProjectStack]
    persistence: PersistenceStack
    ingest: IngestStack
    api: ApiStack
    edge: EdgeStack


def build_stacks(
    app: cdk.App,
    config: DeployConfig,
    *,
    name: Callable[[str], str] = lambda part: part,
    env: cdk.Environment | None = None,
    termination_protection: bool = False,
    ui_asset_path: str | Path | None = None,
) -> Stacks:
    """Add the deploy's stacks to `app` and return them by short name (`network`, …, `projects`
    as a list). `name` maps a short name to the stack's id; `termination_protection` applies to the
    stacks holding retained tenant state (data, identity, projects, persistence); `ui_asset_path`
    is the SvelteKit build (the edge stack's default when `None`)."""
    if not config.projects:
        raise ValueError("at least one project is required for backend runtime wiring")
    # DeployConfig.load validates these too; a config built in code (tests) is checked here.
    validate_projects(config.projects)
    protect = {"termination_protection": termination_protection}
    coordinator_queue_names = [
        config.queue_name(qc) for qc in config.queue_classes if qc.coordinator
    ]
    worker_queue_names = [
        config.queue_name(qc) for qc in config.queue_classes if not qc.coordinator
    ]

    network = NetworkStack(
        app,
        name("network"),
        nat_mode=config.nat_mode,
        worker_egress=config.worker_egress,
        worker_read_buckets=config.data_source_buckets,
        worker_write_buckets=config.imported_project_buckets,
        env=env,
    )
    data = DataStack(
        app,
        name("data"),
        reference_key_arn=config.reference_key_arn,
        image_archive=config.image_archive,
        env=env,
        **protect,
    )
    identity = IdentityStack(
        app,
        name("identity"),
        github_subjects=config.github_subjects,
        public_base_url=config.public_base_url,
        cognito_domain_prefix=config.cognito_domain_prefix,
        google_oidc=config.google_oidc,
        google_secret_name=config.google_secret_name,
        # CI (keyless GitHub OIDC) pushes the coordinator/worker images to these repos; third-party
        # workflow repos (the registration role) only ever the workflow one. Neither is given the
        # builds repository: only the build stack's CodeBuild projects push there.
        workflow_repository=data.workflow_repo,
        coordinator_repository=data.coordinator_repo,
        image_build_trust=config.image_build_trust,
        mfa=config.cognito_mfa,
        env=env,
        **protect,
    )
    compute = ComputeStack(
        app,
        name("compute"),
        vpc=network.vpc,
        config=config,
        host_bucket=data.reference_bucket,
        worker_subnets=network.worker_subnets,
        env=env,
    )
    build = BuildStack(
        app,
        name("build"),
        config=config,
        coordinator_repo=data.coordinator_repo,
        artifact_bucket=data.reference_bucket,
        workflow_repo=data.workflow_repo,
        builds_repo=data.builds_repo,
        image_archive_bucket=data.image_archive_bucket,
        env=env,
    )
    projects = [
        ProjectStack(
            app,
            name(f"project-{project.id}"),
            project_id=project.id,
            permissions_boundary=identity.permissions_boundary,
            key_arn=project.key_arn,
            bucket_name=project.bucket_name,
            data_sources=project.data_sources,
            browser_origin=config.public_base_url,
            uploads_cors_applied=project.uploads_cors_applied,
            viewer_cors_applied=project.viewer_cors_applied,
            noncurrent_versions_days=config.noncurrent_versions_days,
            env=env,
            **protect,
        )
        for project in config.projects
    ]
    # The backend routes each request to its version's (or run's) project, read from one SSM
    # parameter per project (ApiStack). The first declarative project is only the default: the
    # project a request names none for.
    default_project = projects[0]
    # Image builds read their context from where the CLI stages workflow sources: the version's
    # project's bucket, so every project's.
    for project in projects:
        build.grant_context_read(project.bucket, project.key)

    persistence = PersistenceStack(
        app, name("persistence"), vpc=network.vpc, config=config, env=env, **protect
    )
    ingest = IngestStack(
        app,
        name("ingest"),
        vpc=network.vpc,
        db=persistence.db,
        db_secret=persistence.db_secret,
        coordinator_queue_names=coordinator_queue_names,
        worker_queue_names=worker_queue_names,
        api_readyz_url=config.api_readyz_url,
        alert_email=config.alert_email,
        queue_pricing=config.queue_pricing(),
        retention_projects=projects,
        intermediates_retention_days=config.intermediates_retention_days,
        snakemake_cache_days=config.snakemake_cache_days,
        build_project_names=config.build_project_names,
        alarm_prefix=config.alarm_prefix,
        env=env,
    )
    api = ApiStack(
        app,
        name("api"),
        config=config,
        vpc=network.vpc,
        db=persistence.db,
        db_secret=persistence.db_secret,
        capability_secret=persistence.capability_secret,
        user_pool=identity.user_pool,
        user_pool_client=identity.user_pool_client,
        cli_user_pool_client=identity.cli_user_pool_client,
        cli_redirect_uris=identity.cli_redirect_uris,
        permissions_boundary=identity.permissions_boundary,
        compute=compute,
        projects=projects,
        default_project=default_project,
        coordinator_image=ecs.ContainerImage.from_ecr_repository(
            data.coordinator_repo, config.coordinator_image_tag
        ),
        coordinator_image_uri=data.coordinator_repo.repository_uri_for_tag(
            config.coordinator_image_tag
        ),
        worker_image_repo_uri=data.workflow_repo.repository_uri,
        build=build,
        alerts_topic=ingest.alerts_topic,
        env=env,
    )
    # `edge` is a pure sink: importing api.http_api is the only edge CDK needs.
    edge = EdgeStack(
        app,
        name("edge"),
        http_api=api.http_api,
        app_domain=config.app_domain,
        app_certificate_arn=config.app_certificate_arn,
        ui_asset_path=ui_asset_path,
        env=env,
    )

    # Tag the tenant's data-holding resources with the exact tenant, so teardown.sh can find this
    # tenant's retained buckets and Aurora cluster (including ones a destroyed stack left behind) by
    # an exact match rather than a name prefix another tenant's names could share. Scoped to the
    # types teardown deletes: a tag change replaces some other types (e.g. Batch compute
    # environments).
    cdk.Tags.of(app).add(
        "biffix:tenant",
        config.tenant,
        include_resource_types=["AWS::S3::Bucket", "AWS::RDS::DBCluster"],
    )
    return {
        "app": app,
        "network": network,
        "data": data,
        "identity": identity,
        "compute": compute,
        "build": build,
        "projects": projects,
        "persistence": persistence,
        "ingest": ingest,
        "api": api,
        "edge": edge,
    }
