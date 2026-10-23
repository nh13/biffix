//! `POST /samples/detect`: group files into samples and, given the
//! workflow's samplesheet, the rows for them. The launch form calls it for its preview and for
//! each change to the naming rule, so the UI and every other caller read file names one way.

use axum::extract::State;
use axum::{Extension, Json};

use biffix_api::{DetectedSamples, SampleDetectRequest};
use biffix_workflow::samples::{detect_for_sheet, tokenize, SampleDetectError};
use biffix_workflow::text::basename;

use crate::api::ApiError;
use crate::auth::actor::{authorize_msg, AuthedActor};
use crate::auth::rbac::Capability;
use crate::AppState;

/// Files one request may name.
const MAX_FILES: usize = 20_000;

/// The route's request-body limit, so [`MAX_FILES`] is the limit a caller meets (with its message)
/// rather than axum's 2 MiB default, which a list of ~16k realistic S3 URIs already exceeds. 6 MiB is
/// also the API Lambda's own request-payload cap, so nothing larger could arrive anyway: 20k files
/// at up to ~300 bytes each fit.
pub(crate) const BODY_LIMIT: usize = 6 * 1024 * 1024;

/// Group files into samples.
#[utoipa::path(
    post, path = "/samples/detect",
    request_body(content = Object, description = "biffix_api::SampleDetectRequest"),
    responses(
        (status = 200, body = Object, description = "biffix_api::DetectedSamples"),
        (status = 400, description = "a bad regular expression, or too many files"),
        (status = 403),
        (status = 413, description = "the request body is over 6 MiB"),
    )
)]
pub(crate) async fn detect_samples(
    State(state): State<AppState>,
    Extension(actor): Extension<AuthedActor>,
    Json(req): Json<SampleDetectRequest>,
) -> Result<Json<DetectedSamples>, ApiError> {
    authorize_msg(&state, &actor, Capability::ViewRuns)?;
    if req.files.len() > MAX_FILES {
        return Err(ApiError::BadRequest(format!("at most {MAX_FILES} files per request")));
    }
    respond(&req).map(Json).map_err(|e| ApiError::BadRequest(e.to_string()))
}

/// Detect `req`'s samples (and its samplesheet's rows), with the first file's name split into the
/// parts a naming rule can pick from.
fn respond(req: &SampleDetectRequest) -> Result<DetectedSamples, SampleDetectError> {
    let (detection, rows) = detect_for_sheet(&req.files, &req.rule, req.samplesheet.as_ref())?;
    let example = req.files.first().map(|f| basename(&f.uri).to_string());
    Ok(DetectedSamples {
        tokens: example.as_deref().map(tokenize).unwrap_or_default(),
        example,
        detection,
        rows,
    })
}

// ── routes ───────────────────────────────────────────────────────────────────────────────────────

/// Sample detection from file names: the routes behind the Cognito middleware.
pub(super) fn routes() -> axum::Router<AppState> {
    axum::Router::new().route(
        "/samples/detect",
        axum::routing::post(detect_samples).layer(axum::extract::DefaultBodyLimit::max(BODY_LIMIT)),
    )
}

/// Sample detection from file names: the OpenAPI operations, tagged `samples` in [`super::ApiDoc`].
#[derive(utoipa::OpenApi)]
#[openapi(paths(detect_samples))]
pub(super) struct Doc;
