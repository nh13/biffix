//! Task and coordinator logs, served through biffix (not the AWS console), and every Batch attempt of a task.

use super::*;

#[tokio::test]
async fn task_logs_returns_events_for_started_task() {
    let (status, v) = post_json(
        api::router(state_with_tasks()),
        "/runs/r1/task-logs/query",
        serde_json::json!({"task": "started"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    // The stream is derived server-side from the task, not the client — the fake echoes it back.
    assert_eq!(v["log_stream"], "stream-A");
    assert_eq!(v["events"][0]["message"], "log line for stream-A");
    assert!(v["next_backward_token"].is_string());
}

#[tokio::test]
async fn task_logs_empty_when_no_stream_yet() {
    let (status, v) = post_json(
        api::router(state_with_tasks()),
        "/runs/r1/task-logs/query",
        serde_json::json!({"task": "waiting"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    // A queued task has no stream yet: an empty page, not a 404.
    assert!(v["log_stream"].is_null());
    assert_eq!(v["events"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn task_logs_404_for_unknown_task() {
    let (status, _) = post_json(
        api::router(state_with_tasks()),
        "/runs/r1/task-logs/query",
        serde_json::json!({"task": "nope"}),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn task_logs_404_when_stream_expired() {
    // The stream exists on the task row but CloudWatch reports it gone (rotation/retention).
    let (status, _) = post_json(
        api::router(state_with_tasks()),
        "/runs/r1/task-logs/query",
        serde_json::json!({"task": "gone"}),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn task_logs_404_for_unknown_run() {
    let (status, _) = post_json(
        api::router(state_with_tasks()),
        "/runs/nope/task-logs/query",
        serde_json::json!({"task": "started"}),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// `state_with_tasks`, where the `started` task (display job `job-A`) also has an earlier
/// reclaimed attempt `job-A0`, and the `waiting` task has its own attempt `job-B`.
fn state_with_attempts() -> AppState {
    let store = MemStore::new();
    let mut detail = testing::run_detail("r1", "running");
    detail.tasks = vec![
        task_view("started", "running", Some("stream-A"), Some("job-A")),
        task_view("waiting", "queued", None, Some("job-B")),
    ];
    store.insert(detail);
    let mut reclaimed = attempt_view("job-A0", 1, "failed", "stream-A0");
    reclaimed.status_reason = Some("Host EC2 (instance i-1) terminated.".into());
    store.insert_attempts(
        "r1",
        "started",
        vec![reclaimed, attempt_view("job-A", 2, "running", "stream-A")],
    );
    store.insert_attempts("r1", "waiting", vec![attempt_view("job-B", 1, "queued", "stream-B")]);
    TestState::new().with_store(Arc::new(store)).build()
}

#[tokio::test]
async fn attempts_lists_every_attempt_and_marks_the_display_one() {
    let (status, v) = post_json(
        api::router(state_with_attempts()),
        "/runs/r1/attempts/query",
        serde_json::json!({"task": "started"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["task_key"], "started");
    let attempts = v["attempts"].as_array().unwrap();
    assert_eq!(attempts.len(), 2);
    assert_eq!(attempts[0]["batch_job_id"], "job-A0");
    assert_eq!(attempts[0]["status_reason"], "Host EC2 (instance i-1) terminated.");
    assert!(attempts[0].get("display").is_none(), "not the display attempt");
    assert_eq!(attempts[1]["batch_job_id"], "job-A");
    assert_eq!(attempts[1]["display"], true);
}

#[tokio::test]
async fn attempts_empty_for_a_task_without_batch_attempts() {
    // The base state has the task but no recorded attempts (as for a local rule or a cache hit).
    let (status, v) = post_json(
        api::router(state_with_tasks()),
        "/runs/r1/attempts/query",
        serde_json::json!({"task": "started"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["attempts"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn attempts_404_for_unknown_task_or_run() {
    let app = || api::router(state_with_attempts());
    let (status, _) =
        post_json(app(), "/runs/r1/attempts/query", serde_json::json!({"task": "nope"})).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) =
        post_json(app(), "/runs/nope/attempts/query", serde_json::json!({"task": "started"})).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn task_logs_with_job_serves_that_attempts_stream() {
    let (status, v) = post_json(
        api::router(state_with_attempts()),
        "/runs/r1/task-logs/query",
        serde_json::json!({"task": "started", "job": "job-A0"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["log_stream"], "stream-A0");
    assert_eq!(v["events"][0]["message"], "log line for stream-A0");
}

#[tokio::test]
async fn task_logs_with_job_of_another_task_is_404() {
    // job-B belongs to `waiting`; naming it under `started` must not serve its stream.
    let (status, _) = post_json(
        api::router(state_with_attempts()),
        "/runs/r1/task-logs/query",
        serde_json::json!({"task": "started", "job": "job-B"}),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// Run `r1` of `engine` with tasks `new` (its stream in the project's task log group) and `old`
/// (its stream only in the shared group: a run from before the group existed), on a deploy whose
/// default project has a task log group.
fn state_with_project_log_group(engine: &str) -> (AppState, Arc<FakeCloud>) {
    let store = MemStore::new();
    let mut detail = testing::run_detail("r1", "failed");
    detail.run.engine = Some(engine.into());
    detail.coordinator_log_stream = Some("coord-stream".into());
    detail.tasks = vec![
        task_view("new", "running", Some("nf-stream"), Some("job-N")),
        task_view("old", "running", Some("shared-stream"), Some("job-O")),
    ];
    store.insert(detail);
    let cloud = Arc::new(FakeCloud::new());
    cloud
        .project_logs
        .lock()
        .unwrap()
        .insert("nf-stream".into(), vec!["from the project group".into()]);
    let state = TestState::from(state_with_tasks())
        .with_store(Arc::new(store))
        .with_cloud(cloud.clone())
        .with_projects(testing::one_project(biffix_backend::config::projects::ProjectContext {
            task_log_group: "/biffix/project-demo/batch".into(),
            ..testing::project("demo", "")
        }))
        .build();
    (state, cloud)
}

#[tokio::test]
async fn a_task_s_log_is_read_from_its_project_s_group_else_the_shared_one() {
    for engine in ["nextflow", "snakemake"] {
        a_task_s_log_is_read_from_its_project_s_group_else_the_shared_one_for(engine).await;
    }
}

async fn a_task_s_log_is_read_from_its_project_s_group_else_the_shared_one_for(engine: &str) {
    let (state, cloud) = state_with_project_log_group(engine);
    let (status, v) = post_json(
        api::router(state.clone()),
        "/runs/r1/task-logs/query",
        serde_json::json!({"task": "new"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["events"][0]["message"], "from the project group");
    // A stream the project group does not have (an older run's) is read from the shared group.
    let (status, v) = post_json(
        api::router(state.clone()),
        "/runs/r1/task-logs/query",
        serde_json::json!({"task": "old"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["events"][0]["message"], "log line for shared-stream");
    let group = "/biffix/project-demo/batch".to_string();
    assert_eq!(
        *cloud.group_reads.lock().unwrap(),
        vec![(group.clone(), "nf-stream".to_string()), (group, "shared-stream".to_string())]
    );
    // The coordinator always logs to the shared group.
    cloud.group_reads.lock().unwrap().clear();
    let (status, v) = get_json(api::router(state), "/runs/r1/coordinator-logs").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["events"][0]["message"], "log line for coord-stream");
    assert!(cloud.group_reads.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_nextflow_run_of_a_project_without_a_known_group_reads_the_shared_group_only() {
    // Only the default project has a task log group, so another project's run never reads it,
    // not even for a stream the group happens to hold.
    let (mut state, cloud) = state_with_project_log_group("nextflow");
    let demo = biffix_backend::config::projects::ProjectContext {
        task_log_group: "/biffix/project-demo/batch".into(),
        ..testing::project("demo", "bucket-demo")
    };
    state.projects = testing::fixed_projects(
        biffix_backend::config::projects::Projects::new(
            vec![demo, testing::project("other", "bucket-other")],
            "demo",
        )
        .unwrap(),
    );
    let mut detail = testing::run_detail("r1", "failed");
    detail.run.engine = Some("nextflow".into());
    detail.run.project = Some("other".into());
    // It ran in `other` (a run with no recorded prefix predates routing: the default project's).
    detail.output_prefix = Some("s3://bucket-other/biffix/runs/r1/".into());
    detail.tasks = vec![task_view("new", "running", Some("nf-stream"), Some("job-N"))];
    let store = MemStore::new();
    store.insert(detail);
    state.store = Arc::new(store);
    let (status, v) = post_json(
        api::router(state.clone()),
        "/runs/r1/task-logs/query",
        serde_json::json!({"task": "new"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["events"][0]["message"], "log line for nf-stream");
    assert!(cloud.group_reads.lock().unwrap().is_empty());
    let (_, v) = post_json(api::router(state), "/runs/r1/tasks/query", serde_json::json!({})).await;
    assert!(v.get("log_group").is_none(), "{v}");
}

#[tokio::test]
async fn a_project_group_that_cannot_be_read_is_an_error_not_a_fallback() {
    // Only a stream the group does not have falls back to the shared group; any other error
    // (here, a missing grant) is the answer, so it is never hidden behind the shared group.
    let (state, cloud) = state_with_project_log_group("nextflow");
    *cloud.fail_group_reads.lock().unwrap() = Some("no grant on the group".into());
    let (status, v) = post_json(
        api::router(state),
        "/runs/r1/task-logs/query",
        serde_json::json!({"task": "new"}),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{v}");
    let reads: Vec<_> = cloud.log_reads.lock().unwrap().iter().map(|r| r.0.clone()).collect();
    assert_eq!(reads, ["nf-stream"], "the shared group is not read");
}

#[tokio::test]
async fn a_nextflow_run_s_failure_summary_reads_its_task_logs_from_the_project_group() {
    let (mut state, cloud) = state_with_project_log_group("nextflow");
    let mut detail = testing::run_detail("r1", "failed");
    detail.run.engine = Some("nextflow".into());
    detail.tasks = vec![failed_task("ALIGN (A)", "ALIGN", 10.0, 1, "nf-stream")];
    let store = MemStore::new();
    store.insert(detail);
    state.store = Arc::new(store);
    let (status, v) = get_json(api::router(state), "/runs/r1/failure-summary").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["items"][0]["log_tail"], serde_json::json!(["from the project group"]));
    assert_eq!(
        *cloud.group_reads.lock().unwrap(),
        vec![("/biffix/project-demo/batch".to_string(), "nf-stream".to_string())]
    );
}

#[tokio::test]
async fn log_pages_pass_the_cursor_direction_and_a_clamped_limit_to_cloudwatch() {
    use biffix_backend::aws::logs::LogDirection::{Backward, Forward};
    let cloud = Arc::new(FakeCloud::new());
    let state = TestState::from(state_with_attempts()).with_cloud(cloud.clone()).build();
    let read = |body: serde_json::Value| {
        let (state, cloud) = (state.clone(), cloud.clone());
        async move {
            cloud.log_reads.lock().unwrap().clear();
            let (status, v) =
                post_json(api::router(state), "/runs/r1/task-logs/query", body.clone()).await;
            assert_eq!(status, StatusCode::OK, "{body}: {v}");
            cloud.log_reads.lock().unwrap().clone()
        }
    };
    let page = |stream: &str, cursor: Option<&str>, direction, limit| {
        vec![(stream.to_string(), cursor.map(String::from), direction, limit)]
    };
    // No paging parameters: the tail, 200 lines.
    assert_eq!(
        read(serde_json::json!({"task": "started"})).await,
        page("stream-A", None, Backward, 200)
    );
    assert_eq!(
        read(serde_json::json!({"task": "started", "cursor": "f/next", "direction": "forward", "limit": 5}))
            .await,
        page("stream-A", Some("f/next"), Forward, 5)
    );
    // The limit is clamped to 1..=1000; `backward` is the tail, as no direction is.
    assert_eq!(
        read(serde_json::json!({"task": "started", "limit": 5000, "direction": "backward"})).await,
        page("stream-A", None, Backward, 1000)
    );
    // A direction biffix does not know is the client's bug: refused, nothing read.
    cloud.log_reads.lock().unwrap().clear();
    let (status, v) = post_json(
        api::router(state.clone()),
        "/runs/r1/task-logs/query",
        serde_json::json!({"task": "started", "direction": "Forward"}),
    )
    .await;
    assert_eq!(
        (status, v),
        (
            StatusCode::BAD_REQUEST,
            serde_json::json!("unknown direction \"Forward\": `forward` or `backward`")
        )
    );
    assert!(cloud.log_reads.lock().unwrap().is_empty());
    assert_eq!(
        read(serde_json::json!({"task": "started", "limit": 0})).await,
        page("stream-A", None, Backward, 1)
    );
}

#[tokio::test]
async fn the_task_page_names_the_task_log_group_for_either_engine() {
    // The UI's CloudWatch link opens a task's stream in the group the page names.
    for engine in ["nextflow", "snakemake"] {
        let (state, _) = state_with_project_log_group(engine);
        let (status, v) =
            post_json(api::router(state), "/runs/r1/tasks/query", serde_json::json!({})).await;
        assert_eq!(status, StatusCode::OK, "{engine}");
        assert_eq!(v["log_group"], "/biffix/project-demo/batch", "{engine}");
    }
}

/// One run `r1` whose coordinator log stream is `stream` (None ⇒ not recorded).
fn state_with_coordinator_stream(stream: Option<&str>) -> AppState {
    let store = MemStore::new();
    let mut detail = testing::run_detail("r1", "failed");
    detail.coordinator_log_stream = stream.map(String::from);
    store.insert(detail);
    TestState::new().with_store(Arc::new(store)).build()
}

#[tokio::test]
async fn coordinator_logs_serves_the_recorded_stream() {
    let (status, v) = get_json(
        api::router(state_with_coordinator_stream(Some("coord-stream"))),
        "/runs/r1/coordinator-logs",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["log_stream"], "coord-stream");
    assert_eq!(v["events"][0]["message"], "log line for coord-stream");
}

#[tokio::test]
async fn coordinator_logs_empty_when_not_recorded() {
    let (status, v) =
        get_json(api::router(state_with_coordinator_stream(None)), "/runs/r1/coordinator-logs")
            .await;
    assert_eq!(status, StatusCode::OK);
    assert!(v["log_stream"].is_null());
    assert_eq!(v["events"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn coordinator_logs_404_for_unknown_run_or_expired_stream() {
    let (status, _) = get_json(
        api::router(state_with_coordinator_stream(Some("coord-stream"))),
        "/runs/nope/coordinator-logs",
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = get_json(
        api::router(state_with_coordinator_stream(Some("missing"))),
        "/runs/r1/coordinator-logs",
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
