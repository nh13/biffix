//! `auth/config` — the public, non-secret OAuth client configuration for the frontend and CLI.

use serde::{Deserialize, Serialize};

/// `GET /auth/config` — the public OAuth client configuration (no secrets). The CLI reads this to
/// discover the native client id + registered loopback redirect URIs (no client id is baked in).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct OAuthClientConfig {
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub client_id: String,
    pub redirect_uri: String,
    /// Cognito's hosted-UI logout endpoint. Ending the IdP session is what makes sign-out real; a
    /// client that only clears its own tokens is silently signed back in on the next request.
    pub logout_endpoint: String,
    /// Where Cognito returns the browser after logout (a registered logout URL).
    pub logout_uri: String,
    pub scopes: Vec<String>,
    /// The native CLI app client id, when a CLI client is deployed. Public and
    /// non-secret; the `biffix` CLI discovers it here so no client id is baked into the binary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cli_client_id: Option<String>,
    /// The registered loopback redirect URIs for the CLI client. Cognito matches a
    /// redirect URI exactly, so the CLI must bind one of these registered ports.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cli_redirect_uris: Vec<String>,
    /// The Cognito user pool id, derived from the OIDC issuer. Non-secret (it appears in every
    /// token's issuer), and lets the UI link a run's submitter to the Cognito console. `None` when
    /// the issuer isn't a Cognito issuer (e.g. auth disabled / a local non-Cognito setup).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_pool_id: Option<String>,
    /// The AWS region the deploy runs in (e.g. `us-west-2`), for deep links into the AWS console
    /// (S3, Batch, CloudWatch). Non-secret. `None` when the backend cannot tell (no `AWS_REGION`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
}
