//! Who is calling: the authentication middleware, which attaches a request's
//! [`AuthedActor`] (checked with [`crate::auth::actor::authorize`]), and the project a request
//! names.

use axum::extract::State;
use axum::http::{header, Request};
use axum::middleware::Next;
use axum::response::Response;

use crate::api::ApiError;
use crate::auth::actor::AuthedActor;
use crate::auth::{bearer_token, AuthError, Principal};
use crate::AppState;

/// The project a request names (`project`, a version's project or a `--project`): the default when
/// it names none; a 400 when the deploy has several and none is `project` (a single-project deploy
/// uses its one whatever is named, as [`crate::config::projects::Projects::for_version`] does).
pub(crate) async fn request_project(
    state: &AppState,
    project: Option<&str>,
) -> Result<crate::config::projects::ProjectContext, ApiError> {
    let projects = state.projects.current_for(project).await;
    match crate::api::non_blank(project) {
        None => Ok(projects.default_project().clone()),
        Some(p) => {
            projects.for_version(p).cloned().map_err(|e| ApiError::BadRequest(e.to_string()))
        }
    }
}

/// Authenticate the request and attach an [`AuthedActor`]; handlers enforce their own capability.
/// With no auth context (the explicit local-dev opt-out, or a test) every request is admitted as
/// `Disabled`. A JWKS outage is a 503; any other token failure is a 401.
pub(super) async fn require_auth(
    State(state): State<AppState>,
    mut req: Request<axum::body::Body>,
    next: Next,
) -> Result<Response, ApiError> {
    let Some(auth) = state.auth.clone() else {
        req.extensions_mut().insert(AuthedActor::Disabled);
        return Ok(next.run(req).await);
    };
    let header = req.headers().get(header::AUTHORIZATION).and_then(|value| value.to_str().ok());
    let token = bearer_token(header).map_err(|_| ApiError::Unauthorized(String::new()))?;
    let claims = auth.verify(token).await.map_err(|error| match error {
        // The JWKS could not be fetched: our outage, not a bad token. A 401 would log out a user
        // whose token is fine; a 503 says "retry".
        AuthError::Jwks(detail) => {
            tracing::warn!(%detail, "auth: Cognito JWKS unavailable");
            ApiError::Unavailable(String::new())
        }
        _ => ApiError::Unauthorized(String::new()),
    })?;
    req.extensions_mut().insert(AuthedActor::User(Principal::from(claims)));
    Ok(next.run(req).await)
}
