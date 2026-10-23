//! A run's provenance: its Workflow Run RO-Crate (`ro-crate-metadata.json`), built from the log on
//! request (so it always reflects every event received so far). A copy is also written into the
//! run's S3 prefix once it ends (`crate::runs::provenance::write_due`).

use axum::extract::{Path, State};
use axum::response::Response;
use axum::Extension;

use biffix_core::ids::RunId;

use crate::api::{attachment, ApiError};
use crate::auth::actor::AuthedActor;
use crate::runs::access::{load_run, RunLoad};
use crate::runs::provenance::{run_crate, METADATA_FILE};
use crate::AppState;

/// `GET /runs/{run_id}/provenance` — the run's Workflow Run RO-Crate metadata, as a download.
#[utoipa::path(
    get, path = "/runs/{run_id}/provenance",
    params(("run_id" = String, Path, description = "biffix run id")),
    responses(
        (status = 200, description = "The run's ro-crate-metadata.json (WRROC 0.6 Provenance Run Crate)",
         content_type = "application/ld+json", body = Object),
        (status = 403),
        (status = 404)
    )
)]
pub(crate) async fn get_provenance(
    State(state): State<AppState>,
    Extension(actor): Extension<AuthedActor>,
    Path(rid): Path<RunId>,
) -> Result<Response, ApiError> {
    let detail = load_run(&state, &actor, &rid, RunLoad::Full).await?;
    let ro_crate = run_crate(&state, &rid, detail).await.map_err(ApiError::status_only)?;
    let body =
        serde_json::to_vec_pretty(&ro_crate).map_err(|_| ApiError::Internal(String::new()))?;
    Ok(attachment("application/ld+json", &format!("{rid}-{METADATA_FILE}"), body))
}

// ── routes ───────────────────────────────────────────────────────────────────────────────────────

/// A run's provenance crate: the routes behind the Cognito middleware.
pub(super) fn routes() -> axum::Router<AppState> {
    axum::Router::new().route("/runs/{run_id}/provenance", axum::routing::get(get_provenance))
}

/// A run's provenance crate: the OpenAPI operations, tagged `provenance` in [`super::ApiDoc`].
#[derive(utoipa::OpenApi)]
#[openapi(paths(get_provenance))]
pub(super) struct Doc;
