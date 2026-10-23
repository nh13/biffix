//! Reference bundles over HTTP: any viewer lists them (the launch form's
//! dropdown); admins add, edit and delete them (Settings → References).

use std::collections::BTreeSet;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::{Extension, Json};
use futures::StreamExt;

pub(crate) use biffix_api::{
    ReferenceBundle, ReferenceBundleListQuery, ReferenceBundleRequest, SavedReferenceBundle,
};

use crate::api::ApiError;
use crate::auth::actor::{authorize_msg, AuthedActor};
use crate::auth::rbac::Capability;
use crate::runs::submit::{folder_probe, index_base_probe};
use crate::store::references::{matching, validate_request};
use crate::AppState;

/// How many existence checks a write runs at once.
const CHECK_CONCURRENCY: usize = 8;

/// `GET /references` — reference bundles, by id; `kind` and `keys` narrow them to the ones a form
/// can use.
#[utoipa::path(
    get,
    path = "/references",
    params(ReferenceBundleListQuery),
    responses((status = 200, body = Vec<ReferenceBundle>), (status = 403))
)]
pub(crate) async fn list_references(
    State(state): State<AppState>,
    Extension(actor): Extension<AuthedActor>,
    Query(q): Query<ReferenceBundleListQuery>,
) -> Result<Json<Vec<ReferenceBundle>>, ApiError> {
    authorize_msg(&state, &actor, Capability::ViewRuns)?;
    let bundles = state.references.list_bundles().await?;
    let keys: BTreeSet<String> = q
        .keys
        .as_deref()
        .unwrap_or("")
        .split(',')
        .map(str::trim)
        .filter(|k| !k.is_empty())
        .map(str::to_string)
        .collect();
    let kind = q.kind.as_deref().filter(|k| !k.is_empty());
    Ok(Json(matching(&bundles, kind, &keys).into_iter().cloned().collect()))
}

/// Validate a write, then check each URI exists: an object, a folder (ending in `/` or not), or an
/// index named by its base path (files `<uri>.<ext>`), by the rules submit checks a bundle-filled
/// input by (`crate::runs::submit::check_inputs`). A missing one is a warning, not an error: the
/// files may still be on their way.
async fn check(state: &AppState, req: &ReferenceBundleRequest) -> Result<Vec<String>, ApiError> {
    // Bundles are tenant-wide: any project's sources may hold their files.
    let sources = state.projects.current().await.all_sources();
    validate_request(req, &sources).map_err(ApiError::BadRequest)?;
    // Owned pairs: futures that borrow the request are not `Send` for every lifetime, which the
    // handler's future must be.
    let warnings = futures::stream::iter(req.uris.clone().into_iter().map(|(key, uri)| {
        let cloud = state.cloud.clone();
        async move {
            let found = async {
                if uri.ends_with('/') {
                    return cloud.prefix_exists(&uri).await;
                }
                if cloud.head_object(&uri).await?.is_some()
                    || cloud.prefix_exists(&folder_probe(&uri)).await?
                {
                    return Ok(true);
                }
                // Neither an object nor a folder: an index given by its base path.
                cloud.prefix_exists(&index_base_probe(&uri)).await
            }
            .await;
            match found {
                Ok(true) => None,
                Ok(false) => Some(format!("{key}: nothing found at {uri}")),
                Err(e) => Some(format!("{key}: could not check {uri}: {e}")),
            }
        }
    }))
    .buffered(CHECK_CONCURRENCY)
    .filter_map(|w| async move { w })
    .collect::<Vec<_>>()
    .await;
    Ok(warnings)
}

fn bundle_of(req: ReferenceBundleRequest, now: i64, created_by: Option<String>) -> ReferenceBundle {
    ReferenceBundle {
        id: req.id,
        kind: req.kind,
        display_name: req.display_name.trim().to_string(),
        uris: req.uris,
        notes: req.notes.filter(|n| !n.trim().is_empty()),
        igv_genome: req.igv_genome.map(|g| g.trim().to_string()).filter(|g| !g.is_empty()),
        created_at_micros: now,
        updated_at_micros: now,
        created_by,
    }
}

/// `POST /references` — add a bundle (admin). 409 when the id is taken.
#[utoipa::path(
    post,
    path = "/references",
    request_body = ReferenceBundleRequest,
    responses((status = 201, body = SavedReferenceBundle), (status = 400), (status = 403), (status = 409))
)]
pub(crate) async fn create_reference(
    State(state): State<AppState>,
    Extension(actor): Extension<AuthedActor>,
    Json(req): Json<ReferenceBundleRequest>,
) -> Result<(StatusCode, Json<SavedReferenceBundle>), ApiError> {
    let who = authorize_msg(&state, &actor, Capability::ManageReferenceBundles)?;
    let warnings = check(&state, &req).await?;
    let bundle = bundle_of(req, biffix_core::clock::epoch_micros(), Some(who));
    state.references.create_bundle(&bundle).await?;
    Ok((StatusCode::CREATED, Json(SavedReferenceBundle { bundle, warnings })))
}

/// `PUT /references/{id}` — replace a bundle's contents (admin). Runs already launched keep the
/// URIs they used (their anchors record them). The bundle keeps who created it and when.
#[utoipa::path(
    put,
    path = "/references/{id}",
    params(("id" = String, Path, description = "Bundle id")),
    request_body = ReferenceBundleRequest,
    responses((status = 200, body = SavedReferenceBundle), (status = 400), (status = 403), (status = 404))
)]
pub(crate) async fn update_reference(
    State(state): State<AppState>,
    Extension(actor): Extension<AuthedActor>,
    Path(id): Path<String>,
    Json(req): Json<ReferenceBundleRequest>,
) -> Result<Json<SavedReferenceBundle>, ApiError> {
    authorize_msg(&state, &actor, Capability::ManageReferenceBundles)?;
    if req.id != id {
        return Err(ApiError::BadRequest(
            "a bundle's id cannot change; add a new bundle instead".into(),
        ));
    }
    let warnings = check(&state, &req).await?;
    // The store keeps the creation stamp (the one place that rule lives); what it returns is the
    // bundle as stored.
    let bundle = state
        .references
        .update_bundle(&bundle_of(req, biffix_core::clock::epoch_micros(), None))
        .await?;
    Ok(Json(SavedReferenceBundle { bundle, warnings }))
}

/// `DELETE /references/{id}` — remove a bundle (admin). Forms stop offering it; runs launched with
/// it keep their record.
#[utoipa::path(
    delete,
    path = "/references/{id}",
    params(("id" = String, Path, description = "Bundle id")),
    responses((status = 204), (status = 403), (status = 404))
)]
pub(crate) async fn delete_reference(
    State(state): State<AppState>,
    Extension(actor): Extension<AuthedActor>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    authorize_msg(&state, &actor, Capability::ManageReferenceBundles)?;
    state.references.delete_bundle(&id).await?;
    Ok(StatusCode::NO_CONTENT)
}

// ── routes ───────────────────────────────────────────────────────────────────────────────────────

/// Reference bundles: the routes behind the Cognito middleware.
pub(super) fn routes() -> axum::Router<AppState> {
    axum::Router::new()
        .route("/references", axum::routing::get(list_references).post(create_reference))
        .route("/references/{id}", axum::routing::put(update_reference).delete(delete_reference))
}

/// Reference bundles: the OpenAPI operations, tagged `references` in [`super::ApiDoc`].
#[derive(utoipa::OpenApi)]
#[openapi(
    paths(list_references, create_reference, update_reference, delete_reference),
    components(schemas(
        ReferenceBundle,
        ReferenceBundleRequest,
        SavedReferenceBundle,
        biffix_api::ReferenceUse
    ))
)]
pub(super) struct Doc;
