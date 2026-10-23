//! `POST /runs`: launch a run. Operator-gated; the request's checks, fills and launch are
//! [`crate::runs::submit::request::submit_run`]'s.

use axum::extract::State;
use axum::http::StatusCode;
use axum::{Extension, Json};

pub(crate) use biffix_api::{RunSubmitRequest, SubmittedRun};

use crate::api::ApiError;
use crate::auth::actor::{authorize, AuthedActor};
use crate::auth::rbac::Capability;
use crate::AppState;

/// Submit a run.
///
/// 503 when AWS or the store is unavailable (retry), or when the deploy has no capability secret,
/// so no run can launch.
#[utoipa::path(
    post, path = "/runs",
    request_body = RunSubmitRequest,
    responses((status = 202, body = SubmittedRun), (status = 400), (status = 403), (status = 404),
              (status = 409), (status = 503))
)]
pub(crate) async fn submit_run(
    State(state): State<AppState>,
    Extension(actor): Extension<AuthedActor>,
    Json(req): Json<RunSubmitRequest>,
) -> Result<(StatusCode, Json<SubmittedRun>), ApiError> {
    let actor_id = authorize(&state, &actor, Capability::LaunchWorkflow)?;
    let submitted = crate::runs::submit::request::submit_run(&state, &actor, actor_id, req).await?;
    Ok((StatusCode::ACCEPTED, Json(submitted)))
}

// ── routes ───────────────────────────────────────────────────────────────────────────────────────

/// Run submit: the routes behind the Cognito middleware.
pub(super) fn routes() -> axum::Router<AppState> {
    axum::Router::new().route("/runs", axum::routing::post(submit_run))
}

/// Run submit: the OpenAPI operations, tagged `submit` in [`super::ApiDoc`].
#[derive(utoipa::OpenApi)]
#[openapi(paths(submit_run), components(schemas(RunSubmitRequest, SubmittedRun)))]
pub(super) struct Doc;
