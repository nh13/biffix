//! OAuth token exchange and refresh against the deployment's Cognito token endpoint.
//! A public client: the authorization-code exchange sends the `code_verifier` and no
//! secret (RFC 8252); refresh sends the refresh token and the client id.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde::Deserialize;

use crate::error::{CliError, CliResult};
use crate::secret::Secret;

use super::store::Credentials;

/// The Cognito token-endpoint response (fields the CLI uses).
#[derive(Debug, Deserialize)]
pub(crate) struct TokenResponse {
    pub(crate) access_token: String,
    #[serde(default)]
    pub(crate) refresh_token: Option<String>,
    #[serde(default)]
    pub(crate) id_token: Option<String>,
    #[serde(default)]
    pub(crate) expires_in: u64,
}

/// Claims we read (unverified) from our own freshly-issued tokens for display/identity. The token
/// came from the token endpoint over TLS, so this is trusted data, not an authorization decision.
#[derive(Debug, Default, Deserialize)]
struct DisplayClaims {
    #[serde(default)]
    sub: Option<String>,
    #[serde(default)]
    email: Option<String>,
}

/// Decode a JWT payload without verifying the signature. Only ever applied to a token we just
/// received from our own token endpoint; never used to make an authorization decision.
fn decode_claims(jwt: &str) -> DisplayClaims {
    let Some(payload) = jwt.split('.').nth(1) else {
        return DisplayClaims::default();
    };
    let Ok(bytes) = URL_SAFE_NO_PAD.decode(payload) else {
        return DisplayClaims::default();
    };
    serde_json::from_slice(&bytes).unwrap_or_default()
}

impl TokenResponse {
    /// Convert a token response into stored [`Credentials`] for the API at `api_base_url`,
    /// computing `expires_at` from `now`. `previous_refresh` carries a refresh token forward when a
    /// refresh response omits one (a rotated one in the response always wins).
    pub(crate) fn into_credentials(
        self,
        now: SystemTime,
        previous_refresh: Option<Secret<String>>,
        api_base_url: &str,
    ) -> Credentials {
        let claims = decode_claims(&self.access_token);
        let id_claims = self.id_token.as_deref().map(decode_claims).unwrap_or_default();
        let now_unix = now.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
        Credentials {
            expires_at_unix: now_unix.saturating_add(self.expires_in),
            subject: claims.sub.or(id_claims.sub).unwrap_or_default(),
            email: id_claims.email.or(claims.email),
            api_base_url: super::store::normalize_base(api_base_url),
            refresh_token: self.refresh_token.map(Secret::new).or(previous_refresh),
            access_token: Secret::new(self.access_token),
        }
    }
}

/// POST the token endpoint with url-encoded form params and parse the response. A rejection
/// (`400`/`401`) is `AuthRequired` ending in `next`, what to do about it.
async fn post_token(
    client: &reqwest::Client,
    token_endpoint: &str,
    form: &[(&str, &str)],
    next: &str,
) -> CliResult<TokenResponse> {
    let resp = client
        .post(token_endpoint)
        .form(form)
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| classify_transport(&e))?;
    let status = resp.status();
    let body = resp.text().await.map_err(|e| classify_transport(&e))?;
    // The IdP's error body (e.g. `{"error":"invalid_grant"}`) is the actionable detail; surface it.
    // It carries no secret — the form params (code/verifier/refresh token) are not echoed back.
    // Built only for an error: a success body holds the tokens.
    let suffix =
        || if body.trim().is_empty() { String::new() } else { format!(": {}", body.trim()) };
    if status.is_success() {
        serde_json::from_str(&body).map_err(|e| {
            CliError::Unclassified(format!("token endpoint returned unparseable response: {e}"))
        })
    } else if status.as_u16() == 400 || status.as_u16() == 401 {
        // invalid_grant etc. — the code/refresh token is bad; the user must re-authenticate.
        Err(CliError::AuthRequired(format!(
            "the authorization server rejected the request ({status}){}; {next}",
            suffix()
        )))
    } else {
        Err(CliError::Unavailable(format!("token endpoint error {status}{}", suffix())))
    }
}

/// Exchange an authorization `code` for tokens (authorization-code + PKCE).
pub(crate) async fn exchange_code(
    client: &reqwest::Client,
    token_endpoint: &str,
    client_id: &str,
    code: &str,
    code_verifier: &str,
    redirect_uri: &str,
) -> CliResult<TokenResponse> {
    post_token(
        client,
        token_endpoint,
        &[
            ("grant_type", "authorization_code"),
            ("client_id", client_id),
            ("code", code),
            ("code_verifier", code_verifier),
            ("redirect_uri", redirect_uri),
        ],
        // The user is in `biffix login` already: the code expired or was not for this login.
        "the sign-in code was rejected or has expired; start `biffix login` again",
    )
    .await
}

/// Refresh an access token using a refresh token (no user interaction).
pub(crate) async fn refresh(
    client: &reqwest::Client,
    token_endpoint: &str,
    client_id: &str,
    refresh_token: &str,
) -> CliResult<TokenResponse> {
    post_token(
        client,
        token_endpoint,
        &[
            ("grant_type", "refresh_token"),
            ("client_id", client_id),
            ("refresh_token", refresh_token),
        ],
        "run `biffix login`",
    )
    .await
}

/// Classify a transport failure talking to the OAuth token endpoint. Delegates to the
/// shared, domain-neutral classifier naming the authorization server as the target.
fn classify_transport(e: &reqwest::Error) -> CliError {
    CliError::unreachable("the authorization server", e)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jwt_with(payload: serde_json::Value) -> String {
        let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"none"}"#);
        let body = URL_SAFE_NO_PAD.encode(payload.to_string());
        format!("{header}.{body}.")
    }

    #[test]
    fn credentials_carry_forward_refresh_and_read_identity() {
        let resp = TokenResponse {
            access_token: jwt_with(serde_json::json!({"sub": "user-9"})),
            refresh_token: None,
            id_token: Some(jwt_with(serde_json::json!({"email": "bio@lab.example.org"}))),
            expires_in: 3600,
        };
        let prev = Some(Secret::new("old-refresh".to_string()));
        let creds = resp.into_credentials(UNIX_EPOCH, prev, "https://x.example/api/");
        assert_eq!(creds.subject, "user-9");
        assert_eq!(creds.email.as_deref(), Some("bio@lab.example.org"));
        assert_eq!(creds.expires_at_unix, 3600);
        assert_eq!(creds.api_base_url, "https://x.example/api");
        // The refresh token is carried forward when the response omits one.
        assert_eq!(creds.refresh_token.unwrap().expose(), "old-refresh");
    }

    #[test]
    fn a_rotated_refresh_token_replaces_the_old_one() {
        let resp = TokenResponse {
            access_token: jwt_with(serde_json::json!({"sub": "user-9"})),
            refresh_token: Some("new-refresh".into()),
            id_token: None,
            expires_in: 60,
        };
        let creds = resp.into_credentials(
            UNIX_EPOCH,
            Some(Secret::new("old-refresh".to_string())),
            "https://x.example/api",
        );
        assert_eq!(creds.refresh_token.unwrap().expose(), "new-refresh");
    }

    /// A rejected grant needs a new sign-in, and the message says how for each grant: a login
    /// in progress starts again, a refresh logs in.
    #[tokio::test]
    async fn a_rejected_grant_is_auth_required_saying_what_to_do() {
        let app = axum::Router::new().route(
            "/oauth2/token",
            axum::routing::post(|| async {
                (axum::http::StatusCode::BAD_REQUEST, r#"{"error":"invalid_grant"}"#)
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/oauth2/token", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let http = reqwest::Client::new();
        let err = exchange_code(&http, &endpoint, "cli-1", "c", "v", "http://localhost/cb")
            .await
            .unwrap_err();
        assert!(
            matches!(&err, CliError::AuthRequired(m) if m.contains("invalid_grant")
                && m.ends_with("start `biffix login` again")),
            "{err:?}"
        );
        let err = refresh(&http, &endpoint, "cli-1", "r").await.unwrap_err();
        assert!(
            matches!(&err, CliError::AuthRequired(m) if m.ends_with("; run `biffix login`")),
            "{err:?}"
        );
    }

    /// The code exchange is a public-client PKCE request (RFC 8252): the CLI client id, the code,
    /// its verifier and the redirect URI it was issued for, and never a client secret.
    #[tokio::test]
    async fn the_code_exchange_sends_the_verifier_and_no_secret() {
        use std::collections::HashMap;
        use std::sync::{Arc, Mutex};

        type Forms = Arc<Mutex<Vec<HashMap<String, String>>>>;
        let forms = Forms::default();
        let app = axum::Router::new()
            .route(
                "/oauth2/token",
                axum::routing::post(
                    |axum::extract::State(forms): axum::extract::State<Forms>,
                     axum::Form(form): axum::Form<HashMap<String, String>>| async move {
                        forms.lock().unwrap().push(form);
                        axum::Json(serde_json::json!({"access_token": "a", "expires_in": 60}))
                    },
                ),
            )
            .with_state(forms.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let resp = exchange_code(
            &reqwest::Client::new(),
            &format!("http://{addr}/oauth2/token"),
            "cli-1",
            "the-code",
            "the-verifier",
            "http://localhost:8976/callback",
        )
        .await
        .unwrap();
        assert_eq!(resp.access_token, "a");
        let expected: HashMap<String, String> = [
            ("grant_type", "authorization_code"),
            ("client_id", "cli-1"),
            ("code", "the-code"),
            ("code_verifier", "the-verifier"),
            ("redirect_uri", "http://localhost:8976/callback"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        assert_eq!(*forms.lock().unwrap(), vec![expected]);
    }
}
