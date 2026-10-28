//! `GET /auth/config`: the public, non-secret OAuth client configuration for the static frontend
//! and the CLI. Composition reads it from the deploy once ([`crate::auth::oauth_client`]); this
//! serves it.

use axum::extract::State;
use axum::Json;

pub(crate) use biffix_api::OAuthClientConfig;

use crate::api::ApiError;
use crate::auth::oauth_client::OAuthClient;
use crate::AppState;

/// The route's path, which [`super::needs_database`] exempts: it reads no database.
pub(super) const PATH: &str = "/auth/config";

#[utoipa::path(
    get,
    path = "/auth/config",
    responses(
        (status = 200, body = OAuthClientConfig),
        (status = 503, description = "the deploy configures no OAuth client; the body names the unset variables"),
    )
)]
pub(crate) async fn get(
    State(state): State<AppState>,
) -> Result<Json<OAuthClientConfig>, ApiError> {
    match &state.deploy.oauth_client {
        OAuthClient::Configured(config) => Ok(Json(config.as_ref().clone())),
        OAuthClient::Unconfigured(why) => Err(ApiError::Unavailable(why.clone())),
    }
}

// ── routes ───────────────────────────────────────────────────────────────────────────────────────

/// The OAuth bootstrap: the routes open before a caller has a token.
pub(super) fn open_routes() -> axum::Router<AppState> {
    axum::Router::new().route("/auth/config", axum::routing::get(get))
}

/// The OAuth bootstrap: the OpenAPI operations, tagged `auth_config` in [`super::ApiDoc`].
#[derive(utoipa::OpenApi)]
#[openapi(paths(get), components(schemas(OAuthClientConfig)))]
pub(super) struct Doc;
