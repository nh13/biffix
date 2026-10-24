//! `ApiClient` behaviour a long or failure-prone command depends on, driven against tiny local
//! stubs: paging on the server's own page size, retrying only what is safe to repeat, surfacing
//! the server's reason from discovery, and refreshing the access token mid-command through the
//! token choke point.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use axum::extract::{Query, State};
use axum::routing::{get, post};
use axum::Json;
use serde_json::{json, Value};

use biffix_cli::api::ApiClient;
use biffix_cli::auth::store::{CredentialStore, Credentials, FileCredentialStore};
use biffix_cli::auth::{OAuthEndpoints, TokenSource};
use biffix_cli::error::CliError;
use biffix_cli::secret::Secret;

use crate::common;
use common::{minimal_run, serve, static_client};

// ── Paging on the server's own page size ─────────────────────────────────────────────────────

/// `/runs` clamped to 50 rows a page (below the 200 the client asks for): three full pages, then a
/// short last one.
async fn clamped_runs(Query(q): Query<HashMap<String, String>>) -> Json<Value> {
    let page: usize = q.get("after").map_or(0, |c| c.parse().unwrap());
    let rows = if page < 3 { 50 } else { 10 };
    let runs: Vec<Value> = (0..rows).map(|i| minimal_run(page * 50 + i)).collect();
    Json(json!({
        "runs": runs, "total": 160, "limit": 50,
        "last_cursor": (page + 1).to_string(),
    }))
}

#[tokio::test]
async fn list_runs_pages_on_the_limit_the_server_echoes() {
    let base = serve(axum::Router::new().route("/runs", get(clamped_runs))).await;
    // A client comparing against its own requested 200 would stop after the first 50.
    let runs = static_client(&base).list_runs(None).await.unwrap();
    assert_eq!(runs.len(), 160);
}

// ── Retrying only what is safe to repeat ────────────────────────────────────────────────────

#[derive(Default)]
struct Counter(AtomicU64);

/// Always 503 with a reason, counting calls.
async fn always_503(State(c): State<Arc<Counter>>) -> (axum::http::StatusCode, &'static str) {
    c.0.fetch_add(1, Ordering::SeqCst);
    (axum::http::StatusCode::SERVICE_UNAVAILABLE, "the registry is busy")
}

/// 502 once, then an empty list.
async fn flaky_list(State(c): State<Arc<Counter>>) -> (axum::http::StatusCode, Json<Value>) {
    if c.0.fetch_add(1, Ordering::SeqCst) == 0 {
        return (axum::http::StatusCode::BAD_GATEWAY, Json(json!(null)));
    }
    (axum::http::StatusCode::OK, Json(json!([])))
}

fn register_request() -> biffix_api::RegisterRequest {
    biffix_api::RegisterRequest {
        name: "wf".into(),
        project: "p".into(),
        source: json!({"kind": "git", "repo": "r", "git_ref": "main"}),
        git_sha: "abc".into(),
        source_content_hash: "hash".into(),
        image: None,
        engine: Some("snakemake".into()),
        engine_version: "9.27.0".into(),
        entrypoint: "Snakefile".into(),
        target: None,
        form_schema: json!({"fields": [], "warnings": []}),
        queue_classes: None,
        about_markdown: None,
        registration_inputs: Default::default(),
        images: Default::default(),
        nextflow_plugins: Vec::new(),
    }
}

/// The message of `err`, which must be `Unavailable`.
#[track_caller]
fn unavailable(err: &CliError) -> &str {
    let CliError::Unavailable(m) = err else {
        panic!("expected Unavailable, got {err:?}");
    };
    m
}

#[tokio::test]
async fn a_create_is_not_repeated_after_a_503_but_a_read_is() {
    let creates = Arc::new(Counter::default());
    let reads = Arc::new(Counter::default());
    let base = serve(
        axum::Router::new()
            .route("/workflows", post(always_503).with_state(creates.clone()))
            .route("/engines", get(flaky_list).with_state(reads.clone())),
    )
    .await;
    let client = static_client(&base);

    // POST /workflows: the 503 may follow partial work, so it is sent exactly once, and the error
    // says it may have been applied, how to check, and the server's reason.
    let err = client.register_workflow(&register_request()).await.unwrap_err();
    assert_eq!(creates.0.load(Ordering::SeqCst), 1, "a create was retried");
    let m = unavailable(&err);
    assert!(m.contains("may have been applied"), "{m}");
    assert!(m.contains("biffix workflows list"), "{m}");
    assert!(m.contains("the registry is busy"), "{m}");

    // A GET rides out a 502.
    assert!(client.list_engines().await.unwrap().is_empty());
    assert_eq!(reads.0.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn a_create_that_never_connected_is_reported_plainly() {
    // A closed port: no attempt reaches a server (so retrying was safe), and the final error is a
    // plain "cannot reach", not "may have been applied".
    let err = static_client(&format!("http://{}", common::CLOSED_LOOPBACK))
        .register_workflow(&register_request())
        .await
        .unwrap_err();
    let m = unavailable(&err);
    assert!(m.contains("connection refused"), "{m}");
    assert!(!m.contains("may have been applied"), "{m}");
}

/// A create the server answered but whose reply could not be read may have been applied: the error
/// says so, and where to check, rather than reading as a plain outage.
#[tokio::test]
async fn a_create_whose_reply_breaks_off_may_have_been_applied() {
    let listener = common::bind().await;
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        while let Ok((mut socket, _)) = listener.accept().await {
            let mut buf = vec![0u8; 64 * 1024];
            let _ = socket.read(&mut buf).await;
            let _ = socket
                .write_all(b"HTTP/1.1 201 Created\r\nContent-Length: 100\r\n\r\n{\"id\":")
                .await;
        }
    });
    let err = static_client(&format!("http://{addr}"))
        .register_workflow(&register_request())
        .await
        .unwrap_err();
    assert!(
        matches!(&err, CliError::Unavailable(m)
            if m.contains("may have been applied") && m.contains("biffix workflows list")),
        "{err:?}"
    );
}

#[tokio::test]
async fn an_unreachable_upload_names_its_host_not_the_api() {
    // A presigned upload goes to the object store, not the API: a refused one says so.
    let upload = biffix_api::SourceUploadTarget {
        url: format!("http://{}/obj?X-Amz-Signature=s", common::CLOSED_LOOPBACK),
        headers: Default::default(),
        uri: "s3://bucket/obj".into(),
    };
    let err = static_client("https://api.example/api")
        .put_presigned(&upload, b"body".to_vec())
        .await
        .unwrap_err();
    let m = unavailable(&err);
    assert!(m.contains("cannot reach the upload host 127.0.0.1"), "{m}");
    assert!(!m.contains("biffix API"), "{m}");
}

#[tokio::test]
async fn discovery_gives_up_after_max_retries_and_surfaces_the_servers_reason() {
    let calls = Arc::new(Counter::default());
    let base = serve(
        axum::Router::new().route(
            "/auth/config",
            get(|State(c): State<Arc<Counter>>| async move {
                c.0.fetch_add(1, Ordering::SeqCst);
                (
                    axum::http::StatusCode::SERVICE_UNAVAILABLE,
                    "the CLI OAuth client is not configured",
                )
            })
            .with_state(calls.clone()),
        ),
    )
    .await;
    let err = ApiClient::discover(&reqwest::Client::new(), &base, false).await.unwrap_err();
    assert!(matches!(err, CliError::Unavailable(_)), "{err:?}");
    assert!(err.to_string().contains("the CLI OAuth client is not configured"), "{err:?}");
    // One attempt plus MAX_RETRIES (4) retries, then it gives up.
    assert_eq!(calls.0.load(Ordering::SeqCst), 5);
}

// ── The access token is refreshed during a long command ─────────────────────────────────────

/// A fake API + token endpoint. The API accepts only the token the token endpoint issued last
/// (`valid`); a test revokes it to model an access token expiring mid-command, or sets
/// `reject_all` to model a revoked session.
#[derive(Default)]
struct AuthStub {
    valid: Mutex<String>,
    reject_all: AtomicBool,
    issued: AtomicU64,
    rejected: AtomicU64,
    /// Every form posted to the token endpoint, in order.
    token_forms: Mutex<Vec<HashMap<String, String>>>,
}

async fn authed_list(
    State(s): State<Arc<AuthStub>>,
    headers: axum::http::HeaderMap,
) -> (axum::http::StatusCode, Json<Value>) {
    let bearer = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or_default()
        .to_string();
    if s.reject_all.load(Ordering::SeqCst) || bearer != *s.valid.lock().unwrap() {
        s.rejected.fetch_add(1, Ordering::SeqCst);
        return (axum::http::StatusCode::UNAUTHORIZED, Json(json!(null)));
    }
    (axum::http::StatusCode::OK, Json(json!([])))
}

async fn issue_token(
    State(s): State<Arc<AuthStub>>,
    axum::Form(form): axum::Form<HashMap<String, String>>,
) -> Json<Value> {
    // Recorded, not asserted here: a panic in a handler surfaces as a transport error.
    s.token_forms.lock().unwrap().push(form);
    let n = s.issued.fetch_add(1, Ordering::SeqCst) + 1;
    let token = format!("tok-{n}");
    *s.valid.lock().unwrap() = token.clone();
    Json(json!({
        "access_token": token,
        "refresh_token": format!("refresh-{n}"),
        "expires_in": 3600,
        "token_type": "Bearer",
    }))
}

struct AuthHarness {
    stub: Arc<AuthStub>,
    base: String,
    store: Arc<FileCredentialStore>,
    _dir: tempfile::TempDir,
}

/// Serve the stub and seed a credential (`tok-0`, refresh `refresh-0`) expiring at `expires_at`.
async fn auth_harness(expires_at: u64) -> AuthHarness {
    let stub = Arc::new(AuthStub::default());
    *stub.valid.lock().unwrap() = "tok-0".into();
    let base = serve(
        axum::Router::new()
            .route("/workflows", get(authed_list))
            .route("/oauth2/token", post(issue_token))
            .with_state(stub.clone()),
    )
    .await;
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(FileCredentialStore::new(dir.path().join("credentials.json")));
    store
        .save(
            "default",
            &Credentials {
                access_token: Secret::new("tok-0".into()),
                refresh_token: Some(Secret::new("refresh-0".into())),
                expires_at_unix: expires_at,
                subject: "user-1".into(),
                email: None,
                api_base_url: base.clone(),
            },
        )
        .unwrap();
    AuthHarness { stub, base, store, _dir: dir }
}

fn refreshing_client(h: &AuthHarness) -> ApiClient {
    let http = reqwest::Client::new();
    let source = TokenSource::load(
        h.store.clone(),
        "default",
        &h.base,
        Some(OAuthEndpoints {
            token_endpoint: format!("{}/oauth2/token", h.base),
            client_id: "cli-1".into(),
        }),
        http.clone(),
    )
    .unwrap();
    ApiClient::with_token_source(http, &h.base, source, false)
}

fn unix_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs()
}

#[tokio::test]
async fn a_token_that_expires_mid_command_is_refreshed_and_the_request_replayed() {
    let h = auth_harness(unix_now() + 3600).await;
    let client = refreshing_client(&h);

    // A long poll (as `runs follow` does): the first calls use the cached token.
    for _ in 0..3 {
        client.list_workflows().await.expect("poll with the cached token");
    }
    assert_eq!(h.stub.issued.load(Ordering::SeqCst), 0);

    // The access token expires mid-poll: the next call gets a 401, refreshes once, and replays —
    // the caller sees success, not AuthRequired.
    *h.stub.valid.lock().unwrap() = "expired".into();
    client.list_workflows().await.expect("refresh and replay after a 401");
    assert_eq!(h.stub.rejected.load(Ordering::SeqCst), 1);
    assert_eq!(h.stub.issued.load(Ordering::SeqCst), 1);

    // The poll carries on with the new token, and the refresh was persisted (rotated refresh
    // token included) for the next command.
    for _ in 0..3 {
        client.list_workflows().await.unwrap();
    }
    assert_eq!(h.stub.issued.load(Ordering::SeqCst), 1);
    let saved = h.store.load("default").unwrap().unwrap();
    assert_eq!(saved.access_token.expose(), "tok-1");
    assert_eq!(saved.refresh_token.unwrap().expose(), "refresh-1");
    assert_eq!(saved.api_base_url, h.base);
}

#[tokio::test]
async fn a_token_inside_the_refresh_margin_is_refreshed_before_it_is_sent() {
    let h = auth_harness(unix_now() + 5).await;
    let client = refreshing_client(&h);
    client.list_workflows().await.unwrap();
    // Refreshed up front: the API never saw the nearly-expired token.
    assert_eq!(h.stub.issued.load(Ordering::SeqCst), 1);
    assert_eq!(h.stub.rejected.load(Ordering::SeqCst), 0);
    // A public-client refresh: the CLI client id, the stored refresh token, and no secret.
    let forms = h.stub.token_forms.lock().unwrap().clone();
    let expected: HashMap<String, String> =
        [("grant_type", "refresh_token"), ("client_id", "cli-1"), ("refresh_token", "refresh-0")]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
    assert_eq!(forms, vec![expected]);
}

#[tokio::test]
async fn a_revoked_session_is_auth_required_after_a_single_refresh() {
    let h = auth_harness(unix_now() + 3600).await;
    h.stub.reject_all.store(true, Ordering::SeqCst);
    let err = refreshing_client(&h).list_workflows().await.unwrap_err();
    assert!(matches!(err, CliError::AuthRequired(_)), "{err:?}");
    // One refresh and one replay, never a loop.
    assert_eq!(h.stub.issued.load(Ordering::SeqCst), 1);
    assert_eq!(h.stub.rejected.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn a_token_another_process_refreshed_is_adopted_not_refreshed_again() {
    let h = auth_harness(unix_now() + 3600).await;
    let client = refreshing_client(&h);
    // Another `biffix` process refreshes the profile meanwhile: the API now takes only its token.
    h.store
        .save(
            "default",
            &Credentials {
                access_token: Secret::new("tok-other".into()),
                refresh_token: Some(Secret::new("refresh-other".into())),
                expires_at_unix: unix_now() + 3600,
                subject: "user-1".into(),
                email: None,
                api_base_url: h.base.clone(),
            },
        )
        .unwrap();
    *h.stub.valid.lock().unwrap() = "tok-other".into();
    client.list_workflows().await.expect("the stored token, replayed");
    // The cached token's 401 sent this command to the store, not the token endpoint.
    assert_eq!(h.stub.rejected.load(Ordering::SeqCst), 1);
    assert_eq!(h.stub.issued.load(Ordering::SeqCst), 0);
    let saved = h.store.load("default").unwrap().unwrap();
    assert_eq!(saved.refresh_token.unwrap().expose(), "refresh-other");
}
