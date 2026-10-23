//! Direct `ApiClient` coverage that does not need the full control-plane router: the paginated
//! `runs list` (follows the keyset cursor to exhaustion and sends `filter=`, not `state=`) and the
//! shared 503/transport retry loop. Both drive the real client against a tiny local stub so the
//! actual request shaping and retry predicate are exercised.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use axum::extract::{Query, State};
use axum::routing::get;
use axum::Json;
use serde_json::{json, Value};

use biffix_cli::api::ApiClient;

use crate::common;
use common::{minimal_run, serve, static_client};

/// One recorded `/runs` request's query params (filter / limit / after).
#[derive(Clone, Default, serde::Deserialize)]
#[serde(default)]
struct RunsQuery {
    filter: Option<String>,
    limit: Option<String>,
    after: Option<String>,
}

#[derive(Default)]
struct StubState {
    runs_queries: Mutex<Vec<RunsQuery>>,
    /// Remaining 503s the `/auth/config` route should emit before succeeding.
    config_503s: AtomicU64,
}

/// `/runs`: record the query; return a FULL first page (200 rows + a cursor) so the client keeps
/// paging, then an empty page once `after` is set — exercising the follow-the-cursor loop.
async fn runs(State(state): State<Arc<StubState>>, Query(q): Query<RunsQuery>) -> Json<Value> {
    let paging = q.after.is_some();
    state.runs_queries.lock().unwrap().push(q);
    if paging {
        Json(json!({ "runs": [], "total": 200, "limit": 200 }))
    } else {
        let rows: Vec<Value> = (0..200).map(minimal_run).collect();
        Json(json!({ "runs": rows, "total": 200, "limit": 200, "last_cursor": "cursor-1" }))
    }
}

/// `/auth/config`: emit `config_503s` service-unavailable responses, then a valid config.
async fn auth_config(State(state): State<Arc<StubState>>) -> (axum::http::StatusCode, Json<Value>) {
    if state.config_503s.load(Ordering::SeqCst) > 0 {
        state.config_503s.fetch_sub(1, Ordering::SeqCst);
        return (axum::http::StatusCode::SERVICE_UNAVAILABLE, Json(json!({})));
    }
    (
        axum::http::StatusCode::OK,
        Json(json!({
            "authorization_endpoint": "https://auth.example/oauth2/authorize",
            "token_endpoint": "https://auth.example/oauth2/token",
            "cli_client_id": "cli-1",
            "cli_redirect_uris": ["http://localhost:8976/callback"],
        })),
    )
}

async fn start_stub(state: Arc<StubState>) -> String {
    serve(
        axum::Router::new()
            .route("/runs", get(runs))
            .route("/auth/config", get(auth_config))
            .with_state(state),
    )
    .await
}

#[tokio::test]
async fn list_runs_follows_the_cursor_and_sends_filter_not_state() {
    let state = Arc::new(StubState::default());
    let base = start_stub(state.clone()).await;
    let all = static_client(&base)
        .list_runs(Some(biffix_api::RunFilter::Active))
        .await
        .expect("list_runs");

    // Both pages were concatenated (200 from page 1 + 0 from the terminating empty page).
    assert_eq!(all.len(), 200);

    let queries = state.runs_queries.lock().unwrap();
    assert_eq!(queries.len(), 2, "should have paged exactly twice");
    // First request: filter=active (the wire name is `filter`, NOT `state`), a real page limit, no
    // cursor.
    assert_eq!(queries[0].filter.as_deref(), Some("active"));
    assert_eq!(queries[0].limit.as_deref(), Some("200"));
    assert!(queries[0].after.is_none());
    // Second request: follows the cursor returned by the first page.
    assert_eq!(queries[1].after.as_deref(), Some("cursor-1"));
    assert_eq!(queries[1].filter.as_deref(), Some("active"));
}

#[tokio::test]
async fn discover_retries_503_then_succeeds() {
    let state = Arc::new(StubState::default());
    state.config_503s.store(2, Ordering::SeqCst); // two 503s, then a 200
    let base = start_stub(state.clone()).await;

    let discovered = ApiClient::discover(&reqwest::Client::new(), &base, false)
        .await
        .expect("discover should retry past 503s");
    assert_eq!(discovered.cli_client_id.as_deref(), Some("cli-1"));
    assert_eq!(state.config_503s.load(Ordering::SeqCst), 0, "both 503s were consumed");
}

/// A token endpoint the code or refresh token would be sent to must be an HTTPS URL (HTTP only
/// from a deployment reached over HTTP, which needed the local-dev opt-in).
#[tokio::test]
async fn discovery_refuses_an_endpoint_that_is_not_https() {
    let base = serve(axum::Router::new().route(
        "/auth/config",
        get(|| async {
            Json(json!({
                "authorization_endpoint": "https://auth.example/oauth2/authorize",
                "token_endpoint": "ftp://auth.example/oauth2/token",
                "cli_client_id": "cli-1",
            }))
        }),
    ))
    .await;
    let err = ApiClient::discover(&reqwest::Client::new(), &base, false).await.unwrap_err();
    assert!(
        matches!(&err, biffix_cli::error::CliError::Unclassified(m)
            if m.contains("token endpoint") && m.contains("not an HTTPS URL")),
        "{err:?}"
    );
}

/// A stub S3 object for `put_presigned`: honors `If-None-Match: *` as S3's conditional write does.
#[derive(Default)]
struct ObjectStub {
    body: Mutex<Option<Vec<u8>>>,
}

async fn put_object(
    State(state): State<Arc<ObjectStub>>,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> axum::http::StatusCode {
    let mut stored = state.body.lock().unwrap();
    let write_once = headers.get("if-none-match").is_some_and(|v| v.as_bytes() == b"*");
    if write_once && stored.is_some() {
        return axum::http::StatusCode::PRECONDITION_FAILED;
    }
    *stored = Some(body.to_vec());
    axum::http::StatusCode::OK
}

#[tokio::test]
async fn a_write_once_put_sends_its_headers_and_an_existing_object_is_already_staged() {
    let state = Arc::new(ObjectStub::default());
    let base = serve(
        axum::Router::new().route("/obj", axum::routing::put(put_object)).with_state(state.clone()),
    )
    .await;
    let client = static_client(&base);
    let upload = |headers: &[(&str, &str)]| biffix_api::SourceUploadTarget {
        url: format!("{base}/obj"),
        headers: headers.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
        uri: "s3://bucket/obj".into(),
    };
    let once = upload(&[("if-none-match", "*")]);
    assert!(client.put_presigned(&once, b"first".to_vec()).await.unwrap());
    // Staged already: not replaced, and not an error.
    assert!(!client.put_presigned(&once, b"second".to_vec()).await.unwrap());
    assert_eq!(state.body.lock().unwrap().as_deref(), Some(&b"first"[..]));
    // Without the condition (a build context), the PUT replaces the object.
    assert!(client.put_presigned(&upload(&[]), b"third".to_vec()).await.unwrap());
    assert_eq!(state.body.lock().unwrap().as_deref(), Some(&b"third"[..]));
}

/// A presigned URL's query is its signature (and any session token): a failed upload's error names
/// the object, never the query.
#[tokio::test]
async fn a_failed_upload_never_reports_the_presigned_query() {
    // A response whose body breaks off: reading it fails, and the error names the URL.
    let listener = common::bind().await;
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        while let Ok((mut socket, _)) = listener.accept().await {
            let mut buf = vec![0u8; 64 * 1024];
            let _ = socket.read(&mut buf).await;
            let _ = socket
                .write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 100\r\n\r\ncut short")
                .await;
        }
    });
    let base = format!("http://{addr}");
    let upload = biffix_api::SourceUploadTarget {
        url: format!("{base}/obj?X-Amz-Credential=AKIA&X-Amz-Signature=deadbeef"),
        headers: Default::default(),
        uri: "s3://bucket/obj".into(),
    };
    let err = static_client(&base).put_presigned(&upload, b"body".to_vec()).await.unwrap_err();
    let message = err.to_string();
    assert!(message.contains(&format!("{base}/obj")), "{message}");
    assert!(!message.contains("X-Amz") && !message.contains("deadbeef"), "{message}");
}
