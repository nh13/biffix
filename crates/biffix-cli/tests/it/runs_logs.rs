//! The log command as the real binary, with isolated credentials and loopback HTTP.

use axum::{
    http::{HeaderMap, StatusCode},
    routing::{get, post},
    Json, Router,
};
use serde_json::{json, Value};
use std::process::Output;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use crate::common::{biffix, log_in, serve};

async fn invoke(app: Router, format: &str, args: &[&str]) -> Output {
    let base = serve(app).await;
    let home = tempfile::tempdir().unwrap();
    log_in(home.path(), "default", &base);
    let mut cmd = biffix(home.path());
    cmd.args(["--allow-insecure", "--url", &base, "--output", format, "runs", "logs"]);
    cmd.args(args);
    tokio::time::timeout(
        std::time::Duration::from_secs(12),
        tokio::process::Command::from(cmd).kill_on_drop(true).output(),
    )
    .await
    .unwrap()
    .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn coordinator_logs_are_authenticated_paged_and_sanitized() {
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    let app = Router::new().route("/runs/r1/coordinator-logs", get(move |headers: HeaderMap, axum::extract::Query(query): axum::extract::Query<std::collections::HashMap<String,String>>| {
        let n = seen.fetch_add(1, Ordering::SeqCst);
        async move {
            assert_eq!(headers["authorization"], "Bearer tok");
            assert_eq!(query.get("direction").map(String::as_str), Some("forward"));
            assert_eq!(query.get("limit").map(String::as_str), Some("1000"));
            let (events, token) = match n {
                0 => { assert!(!query.contains_key("cursor")); (json!([{"ts_ms":1,"message":"first\u{1b}[2J\n"}]), "f/1") },
                1 => { assert_eq!(query["cursor"], "f/1"); (json!([]), "f/2") },
                2 => { assert_eq!(query["cursor"], "f/2"); (json!([{"ts_ms":1,"message":"same"},{"ts_ms":1,"message":"same"}]), "f/3") },
                3 => { assert_eq!(query["cursor"], "f/3"); (json!([]), "f/3") },
                _ => panic!("reader did not stop at the current end"),
            };
            Json(json!({"log_stream":"s","events":events,"next_forward_token":token,"ended":false}))
        }
    }));
    let output = invoke(app, "human", &["r1"]).await;
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert_eq!(output.stdout, b"first[2J\nsame\nsame\n");
    assert_eq!(calls.load(Ordering::SeqCst), 4);
}

#[tokio::test(flavor = "multi_thread")]
async fn task_keys_stay_in_the_body_and_json_log_values_are_preserved() {
    let message = "a\u{7f}\u{9b}2J\r\n\tvalue";
    let app = Router::new().route(
        "/runs/r1/task-logs/query",
        post(move |headers: HeaderMap, Json(body): Json<Value>| async move {
            assert_eq!(headers["authorization"], "Bearer tok");
            assert_eq!(body["task"], "align:sample=a/b?x=1&é");
            assert_eq!(body["direction"], "forward");
            assert_eq!(body["limit"], 1000);
            assert!(body.get("job").is_none());
            assert!(body.get("execution").is_none());
            Json(json!({"log_stream":"s","events":[{"ts_ms":7,"message":message}],"ended":true}))
        }),
    );
    let output = invoke(app, "json", &["r1", "--task", "align:sample=a/b?x=1&é"]).await;
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let text = String::from_utf8(output.stdout).unwrap();
    assert_eq!(text.lines().count(), 1);
    assert!(text.chars().all(|c| !c.is_control() || c == '\n'));
    assert_eq!(serde_json::from_str::<Value>(&text).unwrap(), json!({"ts_ms":7,"message":message}));
}

#[tokio::test(flavor = "multi_thread")]
async fn empty_logs_and_read_errors_keep_their_exit_semantics() {
    for (status, code) in [
        (StatusCode::OK, 0),
        (StatusCode::NOT_FOUND, 5),
        (StatusCode::FORBIDDEN, 4),
        (StatusCode::BAD_REQUEST, 6),
    ] {
        let app = Router::new().route(
            "/runs/r1/coordinator-logs",
            get(move || async move { (status, Json(json!({"events":[],"ended":true}))) }),
        );
        let out = invoke(app, "human", &["r1"]).await;
        assert_eq!(out.status.code(), Some(code), "{}", String::from_utf8_lossy(&out.stderr));
        assert!(out.stdout.is_empty());
        if code == 5 {
            assert!(String::from_utf8_lossy(&out.stderr).contains("CloudWatch"));
        }
    }
}

#[test]
fn logs_help_and_missing_run_id_are_usage_checked() {
    let home = tempfile::tempdir().unwrap();
    let help = biffix(home.path()).args(["runs", "logs", "--help"]).output().unwrap();
    assert!(help.status.success(), "{}", String::from_utf8_lossy(&help.stderr));
    assert!(String::from_utf8_lossy(&help.stdout).contains("--task"));
    let missing = biffix(home.path()).args(["runs", "logs"]).output().unwrap();
    assert_eq!(missing.status.code(), Some(2));
}

#[tokio::test(flavor = "multi_thread")]
async fn an_empty_finished_follow_is_noted_on_stderr() {
    let app = Router::new().route(
        "/runs/r1/coordinator-logs",
        get(|| async { Json(json!({"events":[], "ended":true})) }),
    );
    let out = invoke(app, "json", &["r1", "--follow"]).await;
    assert!(out.status.success());
    assert!(out.stdout.is_empty());
    assert!(String::from_utf8_lossy(&out.stderr).contains("no log output"));
}

#[tokio::test(flavor = "multi_thread")]
async fn followed_logs_finish_successfully_after_the_source_ends() {
    let calls = Arc::new(AtomicUsize::new(0));
    let counted = calls.clone();
    let app=Router::new().route("/runs/r1/coordinator-logs",get(move |axum::extract::Query(query): axum::extract::Query<std::collections::HashMap<String, String>>| {
        let n=counted.fetch_add(1,Ordering::SeqCst);
        assert_eq!(query.get("cursor").map(String::as_str), if n == 0 { None } else { Some("f1") });
        async move {
            Json(json!({"log_stream":"s","events":if n==0 {json!([{"ts_ms":1,"message":"failed run's log"}])} else {json!([])},"next_forward_token":"f1","ended":true}))
        }
    }));
    let out = invoke(app, "human", &["r1", "--follow"]).await;
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(out.stdout, b"failed run's log\n");
    assert_eq!(calls.load(Ordering::SeqCst), 3);
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn followed_json_is_flushed_live_and_ctrl_c_terminates_an_older_server_follow() {
    use std::os::unix::process::ExitStatusExt;
    use std::process::Stdio;
    use std::time::Duration;
    use tokio::io::{AsyncBufReadExt, BufReader};
    let polled = Arc::new(tokio::sync::Notify::new());
    let notify = polled.clone();
    let calls = Arc::new(AtomicUsize::new(0));
    let app=Router::new().route("/runs/r1/coordinator-logs",get(move |axum::extract::Query(query): axum::extract::Query<std::collections::HashMap<String, String>>| {
        let n=calls.fetch_add(1,Ordering::SeqCst);
        assert_eq!(query.get("cursor").map(String::as_str), if n == 0 { None } else { Some("f1") });
        if n>=2 {notify.notify_one();}
        async move {
            Json(json!({"log_stream":"s","events":if n==0 {json!([{"ts_ms":1,"message":"live"}])} else {json!([])},"next_forward_token":"f1"}))
        }
    }));
    let base = serve(app).await;
    let home = tempfile::tempdir().unwrap();
    log_in(home.path(), "default", &base);
    let mut command = biffix(home.path());
    command.args([
        "--allow-insecure",
        "--url",
        &base,
        "--output",
        "json",
        "runs",
        "logs",
        "r1",
        "--follow",
    ]);
    let mut child = tokio::process::Command::from(command)
        .kill_on_drop(true)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let line = tokio::time::timeout(Duration::from_secs(5), lines.next_line())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(serde_json::from_str::<Value>(&line).unwrap(), json!({"ts_ms":1,"message":"live"}));
    tokio::time::timeout(Duration::from_secs(8), polled.notified()).await.unwrap();
    let pid = child.id().unwrap().to_string();
    let sent =
        tokio::process::Command::new("/bin/kill").args(["-INT", &pid]).status().await.unwrap();
    assert!(sent.success());
    let status = tokio::time::timeout(Duration::from_secs(3), child.wait()).await.unwrap().unwrap();
    assert_eq!(status.signal(), Some(2));
}
