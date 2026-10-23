//! `GET /projects`: the deploy's projects, where a workflow version can be registered.

use axum::extract::State;
use axum::{Extension, Json};

pub(crate) use biffix_api::Project;

use crate::api::ApiError;
use crate::auth::actor::AuthedActor;
use crate::auth::rbac::Capability;
use crate::AppState;

/// `GET /projects` — the deploy's projects: where a workflow version can be registered (each has
/// its own bucket, roles and data sources), the default first among equals.
#[utoipa::path(
    get, path = "/projects",
    responses((status = 200, body = Vec<Project>), (status = 403))
)]
pub(crate) async fn list_projects(
    State(state): State<AppState>,
    Extension(actor): Extension<AuthedActor>,
) -> Result<Json<Vec<Project>>, ApiError> {
    crate::auth::actor::authorize_msg(&state, &actor, Capability::ViewRuns)?;
    let projects = state.projects.current().await;
    let default = &projects.default_project().id;
    Ok(Json(
        projects
            .iter()
            .map(|p| Project {
                id: p.id.clone(),
                bucket: p.bucket.clone(),
                uploads: p.uploads,
                viewer_cors: p.viewer_cors,
                default: &p.id == default,
            })
            .collect(),
    ))
}

// ── routes ───────────────────────────────────────────────────────────────────────────────────────

/// The deploy's projects: the routes behind the Cognito middleware.
pub(super) fn routes() -> axum::Router<AppState> {
    axum::Router::new().route("/projects", axum::routing::get(list_projects))
}

/// The deploy's projects: the OpenAPI operations, tagged `projects` in [`super::ApiDoc`].
#[derive(utoipa::OpenApi)]
#[openapi(paths(list_projects), components(schemas(Project)))]
pub(super) struct Doc;
