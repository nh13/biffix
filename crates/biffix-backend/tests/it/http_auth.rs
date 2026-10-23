//! API authentication: a protected route needs a valid JWT for an accepted audience, the open
//! routes need none, and the docs are served only when the deployment exposes them.
//!
//! Which roles each protected route admits (and that every one of them refuses a caller with no
//! token or no mapped group) is `capability_gates.rs`, over a table checked against the router.

use std::sync::Arc;

use axum::http::StatusCode;

use biffix_backend::auth::rbac::Role;
use biffix_backend::testing::{self, MemStore, TestState};
use biffix_backend::{api, AppState};

const SECRET: &[u8] = b"itsasecret";
const ISS: &str = "https://cognito-idp.us-east-1.amazonaws.com/pool";
const AUD: &str = "biffix-web";

const CLI_AUD: &str = "biffix-cli";

fn state_with_audiences(audiences: Vec<String>) -> AppState {
    let store = MemStore::new();
    store.insert(testing::run_detail("r1", "running"));
    let auth = testing::static_auth(SECRET, ISS, audiences, &[("biffix-viewers", Role::Viewer)]);
    TestState::new().with_store(Arc::new(store)).with_auth(auth).build()
}

/// A viewer token minted for the given audience (`aud`), the static path's stand-in for
/// `client_id`.
fn token_for_aud(aud: &str) -> String {
    testing::user_token(SECRET, ISS, aud, "u", &["biffix-viewers"])
}

fn state_with_auth() -> AppState {
    state_with_audiences(vec![AUD.into()])
}

fn token(groups: &[&str]) -> String {
    testing::user_token(SECRET, ISS, AUD, "u", groups)
}

/// `GET uri` as `bearer`: the status only.
async fn get(app: axum::Router, uri: &str, bearer: Option<&str>) -> StatusCode {
    testing::call_as(app, "GET", uri, None, bearer).await.0
}

#[tokio::test]
async fn protected_route_without_token_is_401() {
    let app = api::router(state_with_auth());
    assert_eq!(get(app, "/runs", None).await, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn protected_route_with_viewer_token_is_200() {
    let app = api::router(state_with_auth());
    let t = token(&["biffix-viewers"]);
    assert_eq!(get(app, "/runs", Some(&t)).await, StatusCode::OK);
}

#[tokio::test]
async fn token_without_mapped_group_is_403() {
    let app = api::router(state_with_auth());
    let t = token(&["some-other-group"]);
    assert_eq!(get(app, "/runs", Some(&t)).await, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn healthz_is_open_even_with_auth_on() {
    let app = api::router(state_with_auth());
    assert_eq!(get(app, "/healthz", None).await, StatusCode::OK);
}

// ── Multi-audience acceptance (the web and CLI app clients) ──────────────────────

#[tokio::test]
async fn cli_audience_accepted_when_in_the_set() {
    // Both the web and CLI clients are accepted; a CLI-minted token gets in.
    let app = api::router(state_with_audiences(vec![AUD.into(), CLI_AUD.into()]));
    assert_eq!(get(app, "/runs", Some(&token_for_aud(CLI_AUD))).await, StatusCode::OK);
}

#[tokio::test]
async fn audience_not_in_the_set_is_401() {
    // A token for a client that is not accepted is rejected at authentication.
    let app = api::router(state_with_audiences(vec![AUD.into()]));
    assert_eq!(get(app, "/runs", Some(&token_for_aud(CLI_AUD))).await, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn empty_audience_set_fails_closed() {
    // Fail-closed: with no accepted client ids, even a well-formed token is rejected.
    let app = api::router(state_with_audiences(vec![]));
    assert_eq!(get(app, "/runs", Some(&token_for_aud(AUD))).await, StatusCode::UNAUTHORIZED);
}
