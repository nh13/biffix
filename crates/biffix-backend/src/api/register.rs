//! Registration endpoints: freeze an immutable `WorkflowVersion` and list the registered ones.
//! Developer-gated.
//!
//! Registration is static-declarative: the caller (CI/CLI) supplies the resolved `git_sha` +
//! `source_content_hash` + normalized FormSchema; the backend pins the image to an ECR digest,
//! validates queue-classes against the deploy menu, and rejects a duplicate version. The pipeline
//! itself is [`crate::register::pipeline`]; these handlers authorize and shape its responses.

use std::collections::{HashMap, HashSet};

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::{Extension, Json};
pub(crate) use biffix_api::{
    EngineSupport, RegisterRequest, ReregisterPlan, ReregisterRequest, SourceUploadRequest,
    SourceUploadTarget, VersionRelease, VersionRuntime, WorkflowEstimate, WorkflowEstimateQuery,
    WorkflowImage, WorkflowListQuery, WorkflowListing, WorkflowListingUpdateRequest,
    WorkflowRelease, WorkflowReleaseChange, WorkflowReleaseUpdateRequest, WorkflowVersionDetail,
    WorkflowVersionSummary,
};

use crate::api::error::not_found_as;
use crate::api::ApiError;
use crate::auth::actor::{authorize, authorize_msg, AuthedActor};
use crate::auth::rbac::Capability;
use crate::images::view::{image_view, reason_of};
use crate::register::catalog::{catalog_of, form_title};
use crate::register::pipeline::{self, reregister_request};
use crate::store::registry::RegisteredVersion;
use crate::AppState;

/// A version in full, as `GET /workflows/{id}` returns it, before the handler adds what it reads
/// from elsewhere (retention, the runtime's and images' readiness).
fn detail_of(rec: &RegisteredVersion) -> WorkflowVersionDetail {
    WorkflowVersionDetail {
        id: rec.version.id.clone().into(),
        name: rec.name.clone(),
        project: rec.project.clone(),
        source: serde_json::to_value(&rec.source).unwrap_or(serde_json::Value::Null),
        git_sha: rec.version.git_sha.clone(),
        source_content_hash: rec.version.source_content_hash.clone(),
        image_digest: rec.version.image_digest.clone(),
        image_repository: rec.version.image_repository.clone(),
        image_registry_id: rec.version.image_registry_id.clone(),
        image_region: rec.version.image_region.clone(),
        engine: rec.version.engine.as_str().to_string(),
        engine_version: rec.version.engine_version.clone(),
        entrypoint: rec.version.entrypoint.clone(),
        target: rec.version.target.clone(),
        form_schema: rec.version.form_schema.clone(),
        queue_classes: rec.version.queue_classes.clone(),
        about_markdown: rec.version.about_markdown.clone(),
        created_by: rec.created_by.clone(),
        created_by_label: rec.created_by_label.clone(),
        created_at_micros: rec.created_at_micros,
        retention_days: None,
        build: rec.version.build,
        registration_inputs: rec.registration_inputs.clone(),
        runtime: None,
        images: Vec::new(),
        state: None,
        not_ready: Vec::new(),
        // Filled from the version's release by `GET /workflows/{id}`.
        release: VersionRelease::default(),
        can_release: false,
    }
}

/// A version's engine runtime (its coordinator variant) as the API shows it: `ready` once a
/// coordinator can launch, with its image and job definition; the latest build otherwise.
fn runtime_view(report: crate::images::variants::RuntimeReport) -> VersionRuntime {
    let build = report.status.build.as_ref();
    VersionRuntime {
        key: report.runtime_key,
        recipe: report.recipe,
        state: report.status.phase.as_str().to_string(),
        ready: report.launch.is_some(),
        build_arn: build.map(|b| b.build_arn.clone()),
        reason: build.and_then(|b| {
            reason_of(b, report.status.phase == crate::images::build_follow::Phase::Failed)
        }),
        logs_url: build.and_then(|b| b.logs_url.clone()),
        builds: build.map_or(0, |b| b.builds),
        coordinator_image: report.launch.as_ref().map(|v| v.image.clone()),
        // The default project's clone of the variant (the one recorded); a run of another project
        // launches on that project's own clone of the same image (`variants::launch_variant`).
        job_definition: report.launch.map(|v| v.job_definition_arn),
    }
}

/// A version as the list shows it (and registration returns it), before the handler adds its run
/// count and state.
fn view_of(rec: &RegisteredVersion) -> WorkflowVersionSummary {
    let summary = biffix_workflow::about::summary(
        rec.version.form_schema.get("description").and_then(|d| d.as_str()),
        rec.version.about_markdown.as_deref(),
    );
    WorkflowVersionSummary {
        id: rec.version.id.clone().into(),
        name: rec.name.clone(),
        project: rec.project.clone(),
        image_digest: rec.version.image_digest.clone(),
        image_repository: rec.version.image_repository.clone(),
        image_registry_id: rec.version.image_registry_id.clone(),
        image_region: rec.version.image_region.clone(),
        engine: rec.version.engine.as_str().to_string(),
        engine_version: rec.version.engine_version.clone(),
        git_sha: rec.version.git_sha.clone(),
        queue_classes: rec.version.queue_classes.clone(),
        created_by: rec.created_by.clone(),
        created_by_label: rec.created_by_label.clone(),
        created_at_micros: rec.created_at_micros,
        // Filled by the list handler from the runs-projection aggregate; 0 for a lone `view_of`.
        run_count: 0,
        catalog: catalog_of(&rec.version.form_schema, summary.as_deref()),
        title: form_title(&rec.version.form_schema),
        summary,
        state: None,
        // Filled by the list handler, like `run_count`.
        last_run_at: None,
        // Filled from the workflow's listing by the list and registration handlers.
        hidden: false,
        // Filled by the list handler; a version just registered is never released.
        release: VersionRelease::default(),
        // Filled by the list handler, per (project, name).
        typical: None,
    }
}

/// Presign a PUT for staging a workflow-source tarball: an S3-staged source, which is how a
/// workflow in a private repository is registered. Developer-gated. The client PUTs the tarball to
/// the returned URL, then calls `POST /workflows` with `source = {kind:s3, uri}`, which checks
/// again that the URI stays inside the staging area of the version's project bucket. `project`
/// picks the bucket; one this deploy does not configure is a 400.
#[utoipa::path(
    post, path = "/workflows/source-upload",
    request_body = SourceUploadRequest,
    responses((status = 200, body = SourceUploadTarget), (status = 400), (status = 403), (status = 502))
)]
pub(crate) async fn source_upload(
    State(state): State<AppState>,
    Extension(actor): Extension<AuthedActor>,
    Json(req): Json<SourceUploadRequest>,
) -> Result<Json<SourceUploadTarget>, ApiError> {
    authorize(&state, &actor, Capability::RegisterWorkflow)?;
    // Staged into the bucket of the project the version is registered in.
    let project = crate::api::actor::request_project(&state, Some(&req.project)).await?;
    if project.bucket.is_empty() {
        return Err(ApiError::BadRequest(
            "s3-staged sources are not configured on this deploy (no project bucket)".into(),
        ));
    }
    // The version id becomes an S3 key segment, so reject anything that could escape the intended
    // key or produce a surprising one. A real id is `{git_sha}:{content_hash}`: letters, digits and
    // `:` (`-`, `_` and `.` allowed too), never a lone `.`, a `..`, a slash, whitespace or a control
    // character.
    let version_id = req.version_id.trim();
    let plain =
        version_id.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, ':' | '-' | '_' | '.'));
    if version_id.is_empty() || !plain || version_id == "." || version_id.contains("..") {
        return Err(ApiError::BadRequest(format!("invalid version_id: {:?}", req.version_id)));
    }
    let key = match req.context_tree_hash.as_deref() {
        None => crate::aws::objects::source_object_key(version_id),
        Some(tree_hash) if biffix_workflow::digest::is_sha256_hex(tree_hash) => {
            crate::aws::objects::context_object_key(version_id, tree_hash)
        }
        Some(other) => {
            return Err(ApiError::BadRequest(format!("invalid context_tree_hash: {other:?}")))
        }
    };
    // A version runs whatever its staged source key holds (the coordinator fetches and executes
    // it), so a source is staged once and never replaced: its PUT is write-once, and a 412 says
    // this source is already there. A build context may be staged again: its build checks the
    // tarball's sha256 (and is keyed by it), so replacing it cannot change what a version runs.
    let write_once = req.context_tree_hash.is_none();
    let presigned = state
        .cloud
        .presign_source_put(&project.bucket, &key, write_once)
        .await
        .map_err(|e| ApiError::BadGateway(format!("presign failed: {e}")))?;
    Ok(Json(SourceUploadTarget {
        url: presigned.url,
        headers: presigned.headers,
        uri: presigned.uri,
    }))
}

/// Register (freeze) an immutable workflow version.
#[utoipa::path(
    post, path = "/workflows",
    request_body = RegisterRequest,
    responses(
        (status = 201, body = WorkflowVersionSummary, description = "Registered, and it can run already"),
        (status = 202, body = WorkflowVersionSummary, description = "Registered; its runtime or images are still being built"),
        (status = 400), (status = 403), (status = 409), (status = 502), (status = 503)
    )
)]
pub(crate) async fn register_workflow(
    State(state): State<AppState>,
    Extension(actor): Extension<AuthedActor>,
    Json(req): Json<RegisterRequest>,
) -> Result<(StatusCode, Json<WorkflowVersionSummary>), ApiError> {
    let who = authorize(&state, &actor, Capability::RegisterWorkflow)?;

    // Capture a human label for the registrant, snapshotted onto the immutable version anchor.
    let created_by_label = crate::auth::actor::actor_label(&state, &actor).await;

    let prepared = pipeline::prepare(&state, &req).await?;
    let (status, view) = registered(&state, who, created_by_label, &req, prepared).await?;
    Ok((status, Json(view)))
}

/// Commit a prepared registration and say so: 201 when the version can run already (everything
/// was built before), else 202 (it is registered, and `GET /workflows/{id}` follows its runtime
/// and images).
async fn registered(
    state: &AppState,
    who: String,
    created_by_label: Option<String>,
    req: &RegisterRequest,
    prepared: pipeline::Prepared,
) -> Result<(StatusCode, WorkflowVersionSummary), ApiError> {
    let (rec, readiness) = pipeline::commit(state, who, created_by_label, req, prepared).await?;
    let mut view = view_of(&rec);
    view.state = readiness.map(|r| r.to_string());
    // A new version of a hidden workflow is hidden too (the flag is per (project, name)). The
    // version is registered already, so a failed read says shown rather than failing the response.
    view.hidden = match state.registry.workflow_listing(&rec.project, &rec.name).await {
        Ok(listing) => listing.is_some_and(|l| l.hidden),
        Err(e) => {
            tracing::warn!(error = %e, "registered: workflow listing read failed");
            false
        }
    };
    let status = match readiness {
        Some(crate::images::ReadinessState::Ready) => StatusCode::CREATED,
        _ => StatusCode::ACCEPTED,
    };
    Ok((status, view))
}

/// Register a version again from what it recorded: on this deploy's engine runtime (or the exact
/// `engine_version` given), with its images pinned again, in its project (or the `project` given).
/// Developer-gated, like registration. The same inputs as an existing build of the source are a 409
/// naming it; `dry_run` reports what would be registered and writes nothing.
#[utoipa::path(
    post, path = "/workflows/{id}/reregister",
    params(("id" = String, Path, description = "Frozen version id")),
    request_body = ReregisterRequest,
    responses(
        (status = 200, body = ReregisterPlan, description = "`dry_run`: what would be registered"),
        (status = 201, body = WorkflowVersionSummary),
        (status = 202, body = WorkflowVersionSummary),
        (status = 400), (status = 403), (status = 404), (status = 409), (status = 502), (status = 503)
    )
)]
pub(crate) async fn reregister_workflow(
    State(state): State<AppState>,
    Extension(actor): Extension<AuthedActor>,
    Path(id): Path<String>,
    Json(body): Json<ReregisterRequest>,
) -> Result<axum::response::Response, ApiError> {
    use axum::response::IntoResponse;
    let who = authorize(&state, &actor, Capability::RegisterWorkflow)?;
    let rec = state.registry.get_version(&id).await?;
    let engine_version = crate::api::non_blank(body.engine_version.as_deref());
    let mut req = reregister_request(&rec, engine_version)?;
    // An explicit project moves the new version there (one the deploy configures: `prepare`).
    if let Some(project) = crate::api::non_blank(body.project.as_deref()) {
        req.project = project.to_string();
    }
    let prepared = pipeline::prepare(&state, &req).await?;
    if body.dry_run {
        return Ok(Json(pipeline::plan_of(&state, &rec, &prepared).await?).into_response());
    }
    let created_by_label = crate::auth::actor::actor_label(&state, &actor).await;
    let (status, view) = registered(&state, who, created_by_label, &req, prepared).await?;
    Ok((status, Json(view)).into_response())
}

/// The engines biffix runs and the releases a workflow version may pin.
#[utoipa::path(get, path = "/engines", responses((status = 200, body = [EngineSupport])))]
pub(crate) async fn list_engines(
    State(state): State<AppState>,
    Extension(actor): Extension<AuthedActor>,
) -> Result<Json<Vec<EngineSupport>>, ApiError> {
    authorize(&state, &actor, Capability::ViewRuns)?;
    Ok(Json(biffix_workflow::engines::SUPPORTED.iter().copied().map(EngineSupport::from).collect()))
}

/// List registered workflow versions (newest first), each saying whether it is released.
#[utoipa::path(
    get, path = "/workflows",
    params(WorkflowListQuery),
    responses(
        (status = 200, body = [WorkflowVersionSummary]),
        (status = 400, description = "`released` is not `true` or `false`"),
        (status = 503, description = "the registry, or the versions' release state, could not be read"),
    )
)]
pub(crate) async fn list_workflows(
    State(state): State<AppState>,
    Extension(actor): Extension<AuthedActor>,
    axum::extract::Query(q): axum::extract::Query<WorkflowListQuery>,
) -> Result<Json<Vec<WorkflowVersionSummary>>, ApiError> {
    authorize(&state, &actor, Capability::ViewRuns)?;
    let list = state.registry.list_versions().await.map_err(ApiError::status_only)?;
    // Attach per-version run stats from the runs projection. A stats failure is non-fatal — the
    // registry is the source of truth here, so fall back to zeros rather than 503 the whole list.
    let stats = state.store.run_stats_by_workflow().await.unwrap_or_else(|e| {
        tracing::warn!(error = %e, "list_workflows: run stats read failed");
        Default::default()
    });
    // Likewise the listing: on a failed read nothing is hidden, so the list shows every workflow,
    // rather than no list at all. Keyed project → names, so each version probes it by reference.
    let hidden: HashMap<String, HashSet<String>> = match state.registry.hidden_workflows().await {
        Ok(set) => set.into_iter().fold(HashMap::new(), |mut m, (project, name)| {
            m.entry(project).or_insert_with(HashSet::new).insert(name);
            m
        }),
        Err(e) => {
            tracing::warn!(error = %e, "list_workflows: hidden workflows read failed");
            HashMap::new()
        }
    };
    // The releases. Unlike the stats and the listing there is no safe fallback: a failed read
    // would make every version a developer version, and the UI's released-only default would
    // then hide every pipeline behind a false "nothing is released yet", so it is a 503.
    let ids: Vec<String> = list.iter().map(|rec| rec.version.id.clone()).collect();
    let released = state.registry.released_versions(&ids).await.map_err(|e| {
        tracing::warn!(error = %e, "list_workflows: releases read failed");
        ApiError::status_only(e)
    })?;
    // Each pipeline's typical run, from one grouped read rather than a request per catalog card.
    // Like the stats, a failure only leaves it out.
    let typical =
        state.store.typical_bases(crate::runs::estimate::BASIS_RUNS).await.unwrap_or_else(|e| {
            tracing::warn!(error = %e, "list_workflows: typical runs read failed");
            Default::default()
        });
    let views = list
        .iter()
        .filter(|rec| {
            q.released.is_none_or(|want| released.contains_key(rec.version.id.as_str()) == want)
        })
        .map(|rec| {
            let mut v = view_of(rec);
            let s = stats.get(&rec.version.id).copied().unwrap_or_default();
            v.run_count = s.count;
            v.last_run_at = s
                .last_submitted_micros
                .map(|m| biffix_core::time::Timestamp::from_micros(m).as_epoch_seconds());
            v.hidden = hidden.get(&rec.project).is_some_and(|names| names.contains(&rec.name));
            v.release = VersionRelease::of(released.get(rec.version.id.as_str()));
            v.typical = typical
                .get(&(rec.project.clone(), rec.name.clone()))
                .and_then(|past| crate::runs::estimate::typical(past));
            v
        })
        .collect();
    Ok(Json(views))
}

/// Hide a workflow from the Lab's pipeline cards, or show it again. Applies to every version of
/// the (project, name); the Expert list still shows a hidden one. Developer-gated, like
/// registering. The name travels in the body since it may contain `/`.
#[utoipa::path(
    put, path = "/workflows/listing",
    request_body = WorkflowListingUpdateRequest,
    responses(
        (status = 200, body = WorkflowListing),
        (status = 403),
        (status = 404, description = "no registered version has that project and name"),
        (status = 503),
    )
)]
pub(crate) async fn put_workflow_listing(
    State(state): State<AppState>,
    Extension(actor): Extension<AuthedActor>,
    Json(req): Json<WorkflowListingUpdateRequest>,
) -> Result<Json<WorkflowListing>, ApiError> {
    let who = authorize_msg(&state, &actor, Capability::RegisterWorkflow)?;
    let entry = state
        .registry
        .put_workflow_listing(&req.project, &req.name, req.hidden, Some(&who))
        .await
        .map_err(not_found_as(|| {
            format!("no workflow `{}` is registered in project `{}`", req.name, req.project)
        }))?;
    Ok(Json(entry))
}

/// Release a version, or (`released: false`) un-release it so it is a developer version again.
/// Developer-gated, like registering. Recorded with who and when, and kept: the response's
/// `history` lists every change. Releasing a released version (or un-releasing a developer one)
/// changes nothing and records nothing.
#[utoipa::path(
    put, path = "/workflows/{id}/release",
    params(("id" = String, Path, description = "Frozen version id")),
    request_body = WorkflowReleaseUpdateRequest,
    responses(
        (status = 200, body = WorkflowRelease),
        (status = 403),
        (status = 404, description = "no version has that id"),
        (status = 503),
    )
)]
pub(crate) async fn put_workflow_release(
    State(state): State<AppState>,
    Extension(actor): Extension<AuthedActor>,
    Path(id): Path<String>,
    Json(req): Json<WorkflowReleaseUpdateRequest>,
) -> Result<Json<WorkflowRelease>, ApiError> {
    let who = authorize_msg(&state, &actor, Capability::RegisterWorkflow)?;
    let label = crate::auth::actor::actor_label(&state, &actor).await;
    let history: Vec<WorkflowReleaseChange> = state
        .registry
        .set_version_released(&id, req.released, Some(&who), label.as_deref())
        .await
        .map_err(not_found_as(|| format!("no workflow version `{id}` is registered")))?;
    Ok(Json(WorkflowRelease::of(&id.as_str().into(), history, true)))
}

/// `GET /workflows/{id}/estimate?samples=N` — what a launch of this workflow with N samples may
/// cost and how long it may take, from the workflow's recent successful runs (any version, same
/// project and name). No estimate until two such runs recorded a cost and a sample count. `typical`
/// is one run of it, whatever its sample count (a running analysis's ETA, a catalog card's "usually
/// ~2 h"): from two successful runs.
#[utoipa::path(
    get, path = "/workflows/{id}/estimate",
    params(
        ("id" = String, Path, description = "Frozen version id"),
        WorkflowEstimateQuery,
    ),
    responses((status = 200, body = WorkflowEstimate), (status = 404))
)]
pub(crate) async fn estimate_workflow(
    State(state): State<AppState>,
    Extension(actor): Extension<AuthedActor>,
    Path(id): Path<String>,
    axum::extract::Query(q): axum::extract::Query<WorkflowEstimateQuery>,
) -> Result<Json<WorkflowEstimate>, ApiError> {
    authorize(&state, &actor, Capability::ViewRuns)?;
    let rec = state.registry.get_version(&id).await.map_err(ApiError::status_only)?;
    // No samples, no estimate: then only the typical run is read.
    let past = if q.samples == 0 {
        Vec::new()
    } else {
        state
            .store
            .estimate_basis(&rec.project, &rec.name, crate::runs::estimate::BASIS_RUNS)
            .await
            .map_err(ApiError::status_only)?
    };
    let typical = state
        .store
        .typical_basis(&rec.project, &rec.name, crate::runs::estimate::BASIS_RUNS)
        .await
        .map_err(ApiError::status_only)?;
    Ok(Json(WorkflowEstimate {
        estimate: crate::runs::estimate::estimate(&past, q.samples),
        typical: crate::runs::estimate::typical(&typical),
    }))
}

/// Fetch one registered version by its frozen id, including its `form_schema`. Viewer-gated, like
/// the other reads. The list view omits `form_schema`, so the CLI needs this to
/// validate submit params against the registered schema.
#[utoipa::path(
    get, path = "/workflows/{id}",
    params(("id" = String, Path, description = "Frozen version id (`{git_sha}:{content_hash}`)")),
    responses(
        (status = 200, body = WorkflowVersionDetail), (status = 403), (status = 404),
        (status = 503, description = "the version, or its release state, could not be read"),
    )
)]
pub(crate) async fn get_workflow(
    State(state): State<AppState>,
    Extension(actor): Extension<AuthedActor>,
    Path(id): Path<String>,
) -> Result<Json<WorkflowVersionDetail>, ApiError> {
    authorize(&state, &actor, Capability::ViewRuns)?;
    let rec = state.registry.get_version(&id).await.map_err(ApiError::status_only)?;
    let mut d = detail_of(&rec);
    d.retention_days = state.deploy.retention_days;
    // A version whose release cannot be read is a 503, not a "developer version".
    let released =
        state.registry.released_versions(std::slice::from_ref(&id)).await.map_err(|e| {
            tracing::warn!(error = %e, "get_workflow: could not read its release");
            ApiError::status_only(e)
        })?;
    d.release = VersionRelease::of(released.get(id.as_str()));
    // Releasing takes the same capability as registering.
    d.can_release = authorize(&state, &actor, Capability::RegisterWorkflow).is_ok();
    match crate::images::readiness(&state, &rec).await {
        Ok(readiness) => {
            d.runtime = Some(runtime_view(readiness.runtime));
            d.images = readiness.images.iter().map(image_view).collect();
            d.state = Some(readiness.state.to_string());
            d.not_ready = readiness.not_ready;
        }
        Err(e) => tracing::warn!(error = %e, "get_workflow: could not report readiness"),
    }
    Ok(Json(d))
}

/// Build a version's coordinator variant again after its automatic builds failed (e.g. once a
/// transient upstream outage has passed). Developer-gated, like registration. A variant that is
/// ready or building is left as it is; the response is its state either way.
#[utoipa::path(
    post, path = "/workflows/{id}/runtime/rebuild",
    params(("id" = String, Path, description = "Frozen version id")),
    responses(
        (status = 202, body = VersionRuntime), (status = 403), (status = 404), (status = 409),
        (status = 502), (status = 503)
    )
)]
pub(crate) async fn rebuild_runtime(
    State(state): State<AppState>,
    Extension(actor): Extension<AuthedActor>,
    Path(id): Path<String>,
) -> Result<(StatusCode, Json<VersionRuntime>), ApiError> {
    authorize(&state, &actor, Capability::RegisterWorkflow)?;
    let rec = state.registry.get_version(&id).await?;
    crate::images::variants::rebuild(&state, &rec.version).await.map_err(|e| match e {
        crate::images::variants::NotRunnable::Error(e) => ApiError::from(e),
        e => ApiError::Conflict(e.to_string()),
    })?;
    let report = crate::images::variants::report(&state, &rec.version).await?;
    Ok((StatusCode::ACCEPTED, Json(runtime_view(report))))
}

/// Import a version's image again after its automatic imports failed. Developer-gated, like
/// registration. An image that is imported or importing is left as it is.
#[utoipa::path(
    post, path = "/workflows/{id}/images/{name}/rebuild",
    params(
        ("id" = String, Path, description = "Frozen version id"),
        ("name" = String, Path, description = "Image name (`default`, …)")
    ),
    responses((status = 202, body = WorkflowImage), (status = 403), (status = 404), (status = 502), (status = 503))
)]
pub(crate) async fn rebuild_image(
    State(state): State<AppState>,
    Extension(actor): Extension<AuthedActor>,
    Path((id, name)): Path<(String, String)>,
) -> Result<(StatusCode, Json<WorkflowImage>), ApiError> {
    authorize(&state, &actor, Capability::RegisterWorkflow)?;
    let rec = state.registry.get_version(&id).await?;
    let status = crate::images::rebuild(&state, &rec, &name)
        .await?
        .ok_or(ApiError::NotFound(format!("version {id} has no image `{name}`")))?;
    Ok((StatusCode::ACCEPTED, Json(image_view(&status))))
}

// ── routes ───────────────────────────────────────────────────────────────────────────────────────

/// Workflow registration, the catalog, and image and runtime rebuilds: the routes behind the
/// Cognito middleware.
pub(super) fn routes() -> axum::Router<AppState> {
    axum::Router::new()
        .route("/workflows", axum::routing::get(list_workflows).post(register_workflow))
        .route("/workflows/source-upload", axum::routing::post(source_upload))
        // Static, so it wins over `{id}` (no version id is `listing`).
        .route("/workflows/listing", axum::routing::put(put_workflow_listing))
        .route("/workflows/{id}", axum::routing::get(get_workflow))
        .route("/workflows/{id}/estimate", axum::routing::get(estimate_workflow))
        .route("/workflows/{id}/release", axum::routing::put(put_workflow_release))
        .route("/workflows/{id}/runtime/rebuild", axum::routing::post(rebuild_runtime))
        .route("/workflows/{id}/images/{name}/rebuild", axum::routing::post(rebuild_image))
        .route("/workflows/{id}/reregister", axum::routing::post(reregister_workflow))
        .route("/engines", axum::routing::get(list_engines))
}

/// Workflow registration, the catalog, and image and runtime rebuilds: the OpenAPI operations,
/// tagged `register` in [`super::ApiDoc`].
#[derive(utoipa::OpenApi)]
#[openapi(
    paths(
        register_workflow,
        source_upload,
        list_workflows,
        put_workflow_listing,
        put_workflow_release,
        get_workflow,
        estimate_workflow,
        rebuild_runtime,
        rebuild_image,
        reregister_workflow,
        list_engines
    ),
    components(schemas(
        biffix_api::ImageArchBuild,
        biffix_api::ImageBuildRequest,
        biffix_api::NextflowPluginRequest,
        RegisterRequest,
        WorkflowVersionSummary,
        biffix_api::CatalogEntry,
        biffix_api::WorkflowListingUpdateRequest,
        biffix_api::WorkflowListing,
        biffix_api::WorkflowReleaseUpdateRequest,
        biffix_api::VersionRelease,
        biffix_api::WorkflowReleaseChange,
        biffix_api::WorkflowRelease,
        EngineSupport,
        biffix_api::RegistrationInputs,
        biffix_api::RegistrationOverrides,
        ReregisterRequest,
        ReregisterPlan,
        biffix_api::ReregisterImagePlan,
        biffix_api::VersionRuntime,
        biffix_api::WorkflowImage,
        biffix_api::ImageRequest,
        WorkflowVersionDetail,
        biffix_api::WorkflowEstimate,
        biffix_api::LaunchEstimate,
        biffix_api::TypicalRun,
        SourceUploadRequest,
        SourceUploadTarget
    ))
)]
pub(super) struct Doc;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_store_outage_in_a_rebuild_is_retryable_and_a_corrupt_recipe_ours() {
        use crate::images::build_follow::BuildError;
        use crate::store::StoreError;
        let status = |e| ApiError::from(e).status();
        assert_eq!(
            status(BuildError::Store(StoreError::Unavailable("db".into()))),
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(status(BuildError::Store(StoreError::NotFound)), StatusCode::NOT_FOUND);
        assert_eq!(status(BuildError::Recipe("bad".into())), StatusCode::INTERNAL_SERVER_ERROR);
    }
}
