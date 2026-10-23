//! Cognito access-token authentication.
//!
//! Production accepts only Cognito-issued RS256 access tokens. Keys are selected by JWT `kid`
//! from the pool's JWKS and refreshed on an unknown key (at most once per cooldown) so signing-key
//! rotation does not require a backend redeploy. A static HMAC verifier exists for deterministic
//! tests only: it is compiled in only with the `test-support` feature, so a production build cannot
//! construct one.
//!
//! Its submodules are the rest of who-may-do-what: [`actor`] (the authenticated caller and the
//! one authorization check), [`rbac`] (roles from IdP groups), [`capability`] (the per-run event
//! token), [`identity`] (a principal's human label) and [`oauth_client`] (the public client config
//! `GET /auth/config` serves).

pub(crate) mod actor;
pub mod capability;
pub(crate) mod identity;
pub mod oauth_client;
pub mod rbac;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use jsonwebtoken::jwk::{AlgorithmParameters, Jwk, KeyAlgorithm, PublicKeyUse};
use jsonwebtoken::{decode, decode_header, Algorithm, DecodingKey, Validation};
use serde::Deserialize;
use tokio::sync::{Mutex, RwLock};

/// Minimum interval between JWKS refreshes triggered by an unknown `kid`. Without it, a token
/// bearing a `kid` absent from the cache forces a JWKS fetch on *every* request, so a client
/// spraying random kids could amplify each request into an outbound fetch. Cognito rotates signing
/// keys rarely (and publishes the new key ahead of using it), so bounding refreshes to once per
/// minute picks up a real rotation promptly while capping attacker-induced fetches. The window
/// starts when a refresh *starts*, so a failed refresh consumes it too.
const JWKS_REFRESH_COOLDOWN: Duration = Duration::from_secs(60);

/// Bounds on one JWKS fetch. It runs at cold start and inline on a request during an unknown-`kid`
/// refresh, so a stalled connection must fail fast rather than hold the request until the Lambda
/// times out.
const JWKS_CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const JWKS_REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

/// The explicit opt-out that lets an HTTP entry point run with no authentication (local dev only).
pub(crate) const AUTH_DISABLED_VAR: &str = "BIFFIX_AUTH_DISABLED";

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum AuthError {
    #[error("invalid or expired token")]
    InvalidToken,
    #[error("missing bearer token")]
    MissingToken,
    #[error("token has no key id")]
    MissingKeyId,
    #[error("token key id is not present in the Cognito JWKS")]
    UnknownKeyId,
    /// The JWKS could not be fetched or held no usable key: our (or Cognito's) outage, not a bad
    /// token, so the HTTP layer answers 503 rather than 401.
    #[error("failed to load Cognito JWKS: {0}")]
    Jwks(String),
}

/// A JWKS fetch or decode failure, as [`AuthError::Jwks`].
fn jwks_error(error: impl std::fmt::Display) -> AuthError {
    AuthError::Jwks(error.to_string())
}

/// The Cognito claims used by biffix. Access tokens identify their app client with `client_id`
/// rather than the ID-token-only `aud` claim.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Claims {
    pub sub: String,
    #[serde(default)]
    pub(crate) email: Option<String>,
    #[serde(rename = "cognito:groups", default)]
    pub groups: Vec<String>,
    /// The Cognito username (present on access tokens); the key for AdminGetUser identity
    /// resolution.
    #[serde(default)]
    pub(crate) username: Option<String>,
    #[serde(default)]
    pub(crate) client_id: Option<String>,
    #[serde(default)]
    pub(crate) token_use: Option<String>,
    pub(crate) exp: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Principal {
    pub(crate) subject: String,
    pub(crate) email: Option<String>,
    pub(crate) username: Option<String>,
    pub(crate) groups: Vec<String>,
}

impl From<Claims> for Principal {
    fn from(claims: Claims) -> Self {
        Self {
            subject: claims.sub,
            email: claims.email,
            username: claims.username,
            groups: claims.groups,
        }
    }
}

/// How an HTTP entry point authenticates requests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AuthMode {
    /// Verify Cognito access tokens issued by this user pool.
    Cognito { issuer: String },
    /// No authentication: every request is admitted as `local` with every capability. Reachable
    /// only through the explicit `BIFFIX_AUTH_DISABLED=true` opt-out, never from a missing issuer.
    Disabled,
}

impl AuthMode {
    /// Decide the mode from the values of `BIFFIX_OIDC_ISSUER` and `BIFFIX_AUTH_DISABLED`.
    /// **Fail-closed:** a missing issuer is a deploy error, not "auth off"; turning auth off takes
    /// `BIFFIX_AUTH_DISABLED=true` and no issuer. Setting both is ambiguous and is refused too.
    pub(crate) fn from_env_values(
        issuer: Option<&str>,
        disabled: Option<&str>,
    ) -> Result<Self, AuthModeError> {
        let issuer = issuer.map(str::trim).filter(|value| !value.is_empty());
        let disabled =
            biffix_core::env::parse_bool(AUTH_DISABLED_VAR, disabled)?.unwrap_or_default();
        match (issuer, disabled) {
            (Some(_), true) => Err(AuthModeError::Ambiguous),
            (Some(issuer), false) => Ok(Self::Cognito { issuer: issuer.to_string() }),
            (None, true) => Ok(Self::Disabled),
            (None, false) => Err(AuthModeError::Unauthenticated),
        }
    }
}

/// Why an HTTP entry point refuses its authentication settings.
#[derive(Debug, thiserror::Error)]
pub enum AuthModeError {
    /// `BIFFIX_AUTH_DISABLED` is not a boolean.
    #[error(transparent)]
    NotBoolean(#[from] biffix_core::env::EnvError),
    /// Both an issuer and the opt-out.
    #[error("both BIFFIX_OIDC_ISSUER and {AUTH_DISABLED_VAR}=true are set; set exactly one")]
    Ambiguous,
    /// Neither an issuer nor the opt-out.
    #[error(
        "BIFFIX_OIDC_ISSUER is unset: refusing to start without authentication \
         (set {AUTH_DISABLED_VAR}=true only for local development)"
    )]
    Unauthenticated,
}

/// The accepted app clients: the web client(s) (`BIFFIX_OIDC_AUDIENCE`, comma-separated for
/// forward compatibility) plus the optional native CLI client (`BIFFIX_CLI_CLIENT_ID`).
/// Whitespace is trimmed, empty entries dropped, and duplicates removed with order preserved.
pub(crate) fn accepted_audiences(web: &str, cli: Option<&str>) -> Vec<String> {
    let mut audiences: Vec<String> = Vec::new();
    for client in web.split(',').chain(cli) {
        let client = client.trim();
        if !client.is_empty() && !audiences.iter().any(|a| a == client) {
            audiences.push(client.to_string());
        }
    }
    audiences
}

/// Verify a token against one explicit key: signature, `exp`, and the *presence and value* of
/// `iss` and `aud`. Test-only (the `test-support` feature); production uses
/// [`JwtVerifier::cognito`].
#[cfg(feature = "test-support")]
pub fn verify_claims(
    token: &str,
    key: &DecodingKey,
    issuer: &str,
    audience: &str,
    algorithm: Algorithm,
) -> Result<Claims, AuthError> {
    let mut validation = Validation::new(algorithm);
    // jsonwebtoken checks `iss`/`aud` only when present unless they are required.
    validation.set_required_spec_claims(&["exp", "iss", "aud"]);
    validation.set_issuer(&[issuer]);
    validation.set_audience(&[audience]);
    decode::<Claims>(token, key, &validation)
        .map(|data| data.claims)
        .map_err(|_| AuthError::InvalidToken)
}

/// Runtime JWT verifier. Production construction ([`JwtVerifier::cognito`]) is the only one a
/// production build has; `Static` exists only with the `test-support` feature.
pub enum JwtVerifier {
    #[cfg(feature = "test-support")]
    Static {
        key: DecodingKey,
        algorithm: Algorithm,
    },
    Cognito(CognitoJwtVerifier),
}

impl JwtVerifier {
    /// A verifier over one fixed key (tests only).
    #[cfg(feature = "test-support")]
    pub fn static_key(key: DecodingKey, algorithm: Algorithm) -> Self {
        Self::Static { key, algorithm }
    }

    /// Load the Cognito pool's JWKS for `issuer`. The verifier then accepts only tokens whose
    /// `iss` is this same issuer, so the signing keys and the issuer check cannot disagree.
    pub(crate) async fn cognito(issuer: &str) -> Result<Self, AuthError> {
        Ok(Self::Cognito(CognitoJwtVerifier::load(issuer, JWKS_REFRESH_COOLDOWN).await?))
    }

    /// Verify a token, accepting it if its client identifies any of `audiences` (the web and CLI
    /// app clients share one API). **Fail-closed:** an empty audience set accepts nothing, at both
    /// the static and Cognito paths.
    pub(crate) async fn verify(
        &self,
        token: &str,
        issuer: &str,
        audiences: &[String],
    ) -> Result<Claims, AuthError> {
        if audiences.is_empty() {
            return Err(AuthError::InvalidToken);
        }
        match self {
            #[cfg(feature = "test-support")]
            Self::Static { key, algorithm } => {
                // The static/test path validates the `aud` claim, so try each accepted audience and
                // take the first that verifies; a token for none of them is rejected.
                for audience in audiences {
                    if let Ok(claims) = verify_claims(token, key, issuer, audience, *algorithm) {
                        return Ok(claims);
                    }
                }
                Err(AuthError::InvalidToken)
            }
            Self::Cognito(verifier) => verifier.verify(token, issuer, audiences).await,
        }
    }
}

pub struct CognitoJwtVerifier {
    /// The pool this verifier's keys belong to.
    issuer: String,
    jwks_uri: String,
    /// One client for every fetch (connection reuse), with bounded connect and request timeouts.
    client: reqwest::Client,
    /// The hot path: a known `kid` takes only this read lock. It is write-locked only briefly, to
    /// install a fetched set, never across a fetch.
    keys: RwLock<BTreeMap<String, Arc<DecodingKey>>>,
    /// The last refresh. Held across the fetch, so unknown-`kid` refreshes run one at a time and a
    /// caller queued behind one re-checks the cache instead of fetching again. Known-`kid`
    /// requests never touch it.
    refresh: Mutex<LastRefresh>,
    cooldown: Duration,
}

/// When the last JWKS refresh started (`None`: a refresh is allowed now), and whether it failed.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct LastRefresh {
    started: Option<Instant>,
    failed: bool,
}

impl LastRefresh {
    /// A refresh is allowed now (the cooldown has elapsed).
    #[cfg(test)]
    pub(crate) fn elapsed() -> Self {
        Self::default()
    }
}

impl CognitoJwtVerifier {
    async fn load(issuer: &str, cooldown: Duration) -> Result<Self, AuthError> {
        let issuer = issuer.trim_end_matches('/').to_string();
        let jwks_uri = format!("{issuer}/.well-known/jwks.json");
        let client = reqwest::Client::builder()
            .connect_timeout(JWKS_CONNECT_TIMEOUT)
            .timeout(JWKS_REQUEST_TIMEOUT)
            .build()
            .map_err(jwks_error)?;
        let keys = fetch_keys(&client, &jwks_uri).await?;
        Ok(Self {
            issuer,
            jwks_uri,
            client,
            keys: RwLock::new(keys),
            refresh: Mutex::new(LastRefresh { started: Some(Instant::now()), failed: false }),
            cooldown,
        })
    }

    async fn cached(&self, kid: &str) -> Option<Arc<DecodingKey>> {
        self.keys.read().await.get(kid).cloned()
    }

    /// The key for an unknown `kid` after (at most) one refresh. `Ok(None)` when the kid is still
    /// unknown, or when the cooldown forbids a refresh after one that succeeded; inside the
    /// cooldown of one that failed, the failure (`AuthError::Jwks`): our outage, not a bad token,
    /// so a caller queued behind it or arriving later is not answered 401 (signed out) for a key
    /// that may well be valid.
    async fn refresh_for(&self, kid: &str) -> Result<Option<Arc<DecodingKey>>, AuthError> {
        let mut last = self.refresh.lock().await;
        // Another caller may have installed a set that has this kid while we waited.
        if let Some(key) = self.cached(kid).await {
            return Ok(Some(key));
        }
        if last.started.is_some_and(|started| started.elapsed() < self.cooldown) {
            return if last.failed {
                Err(AuthError::Jwks(
                    "the last JWKS refresh failed; retry after its cooldown".into(),
                ))
            } else {
                Ok(None)
            };
        }
        // Stamp before fetching: a failed (or slow) refresh consumes the cooldown too, so an
        // endpoint outage cannot turn every unknown-kid request into an outbound fetch.
        *last = LastRefresh { started: Some(Instant::now()), failed: true };
        let refreshed = fetch_keys(&self.client, &self.jwks_uri).await?;
        last.failed = false;
        let key = refreshed.get(kid).cloned();
        *self.keys.write().await = refreshed;
        Ok(key)
    }

    async fn verify(
        &self,
        token: &str,
        issuer: &str,
        audiences: &[String],
    ) -> Result<Claims, AuthError> {
        // The keys are this pool's; a token is only valid if it claims this same pool. (The
        // empty-audience fail-closed check lives in `JwtVerifier::verify`, the only caller.)
        if issuer.trim_end_matches('/') != self.issuer {
            return Err(AuthError::InvalidToken);
        }
        let header = decode_header(token).map_err(|_| AuthError::InvalidToken)?;
        if header.alg != Algorithm::RS256 {
            return Err(AuthError::InvalidToken);
        }
        let kid = header.kid.ok_or(AuthError::MissingKeyId)?;

        let key = match self.cached(&kid).await {
            Some(key) => key,
            None => self.refresh_for(&kid).await?.ok_or(AuthError::UnknownKeyId)?,
        };

        let mut validation = Validation::new(Algorithm::RS256);
        // jsonwebtoken checks `iss` only when present unless it is required.
        validation.set_required_spec_claims(&["exp", "iss"]);
        validation.set_issuer(&[&self.issuer]);
        // Cognito access tokens carry `client_id`, not `aud`; validate it explicitly below.
        validation.validate_aud = false;
        let claims =
            decode::<Claims>(token, &key, &validation).map_err(|_| AuthError::InvalidToken)?.claims;
        // The token's client must be one of the accepted app clients (web or CLI), and it must be
        // an access token (an ID token for the same client is not an API credential).
        let client_accepted =
            claims.client_id.as_deref().is_some_and(|client| audiences.iter().any(|a| a == client));
        if claims.token_use.as_deref() != Some("access") || !client_accepted {
            return Err(AuthError::InvalidToken);
        }
        Ok(claims)
    }
}

/// The JWKS document, with each key left raw so one key this crate cannot parse (an unsupported
/// `kty`, say) is skipped instead of failing the whole set.
#[derive(Deserialize)]
struct RawJwkSet {
    keys: Vec<serde_json::Value>,
}

async fn fetch_keys(
    client: &reqwest::Client,
    uri: &str,
) -> Result<BTreeMap<String, Arc<DecodingKey>>, AuthError> {
    let response =
        client.get(uri).send().await.map_err(jwks_error)?.error_for_status().map_err(jwks_error)?;
    let set: RawJwkSet = response.json().await.map_err(jwks_error)?;
    let mut keys = BTreeMap::new();
    for raw in set.keys {
        let jwk: Jwk = match serde_json::from_value(raw) {
            Ok(jwk) => jwk,
            Err(error) => {
                tracing::warn!(%error, "JWKS: skipping a key that does not parse");
                continue;
            }
        };
        if let Some((kid, key)) = signing_key(&jwk) {
            keys.insert(kid, Arc::new(key));
        }
    }
    if keys.is_empty() {
        return Err(AuthError::Jwks("JWKS contained no keyed RS256 signing keys".to_owned()));
    }
    Ok(keys)
}

/// The `(kid, key)` of an RS256 signing JWK, or `None` (with a warning for anything unexpected)
/// for a key this verifier would never use.
fn signing_key(jwk: &Jwk) -> Option<(String, DecodingKey)> {
    let Some(kid) = jwk.common.key_id.clone() else {
        tracing::warn!("JWKS: skipping a key with no kid");
        return None;
    };
    let is_signing = matches!(jwk.common.public_key_use, None | Some(PublicKeyUse::Signature));
    let is_rs256 = matches!(jwk.common.key_algorithm, None | Some(KeyAlgorithm::RS256))
        && matches!(jwk.algorithm, AlgorithmParameters::RSA(_));
    if !is_signing || !is_rs256 {
        tracing::warn!(kid, "JWKS: skipping a key that is not an RS256 signing key");
        return None;
    }
    match DecodingKey::from_jwk(jwk) {
        Ok(key) => Some((kid, key)),
        Err(error) => {
            tracing::warn!(kid, %error, "JWKS: skipping a key that does not load");
            None
        }
    }
}

/// The token from an `Authorization: Bearer <token>` header. The scheme is case-insensitive
/// (RFC 7235); a missing, other-scheme, or empty token is [`AuthError::MissingToken`].
pub fn bearer_token(header: Option<&str>) -> Result<&str, AuthError> {
    let (scheme, token) = header
        .and_then(|value| value.trim_start().split_once(' '))
        .ok_or(AuthError::MissingToken)?;
    let token = token.trim();
    if !scheme.eq_ignore_ascii_case("bearer") || token.is_empty() {
        return Err(AuthError::MissingToken);
    }
    Ok(token)
}

#[cfg(test)]
mod tests;
