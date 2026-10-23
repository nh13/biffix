//! `POST /runs/{id}/cancel`: records who asked, terminates the coordinator, and ends a run that is
//! not yet running at once (with in-memory doubles; no AWS, no DB).
//!
//! The runs whose cancel depends on the state the cancel's own events fold to (a run not yet
//! running, a retried cancel) are seeded from events through a folding sink
//! ([`MemEventSink::folding_into`]), so the store holds the fold of the log as it would in Postgres.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use axum::http::StatusCode;

use biffix_backend::api;
use biffix_backend::auth::rbac::Role;
use biffix_backend::store::notifications::NotificationStore;
use biffix_backend::store::EventSink;
use biffix_backend::testing::{self, task_view, FakeCloud, MemEventSink, MemStore, TestState};
use biffix_core::event::EventData;
use biffix_core::ids::{BatchJobId, CoordinatorJobId, RunId};
use biffix_core::state::RunState;
use biffix_core::time::Timestamp;

/// An app over `store` and `events`, and its cloud.
fn app_with(store: Arc<MemStore>, events: Arc<MemEventSink>) -> (axum::Router, Arc<FakeCloud>) {
    let cloud = Arc::new(FakeCloud::new());
    let app = api::router(
        TestState::new().with_store(store).with_events(events).with_cloud(cloud.clone()).build(),
    );
    (app, cloud)
}

/// An app over `store` with a recording (not folding) sink, and its event sink and cloud.
fn app_over(store: Arc<MemStore>) -> (axum::Router, Arc<MemEventSink>, Arc<FakeCloud>) {
    let events = Arc::new(MemEventSink::new());
    let (app, cloud) = app_with(store, events.clone());
    (app, events, cloud)
}

/// An app over `store` whose sink folds into it, and its event sink and cloud.
fn folding_app_over(store: Arc<MemStore>) -> (axum::Router, Arc<MemEventSink>, Arc<FakeCloud>) {
    let events = Arc::new(MemEventSink::folding_into(store.clone()));
    let (app, cloud) = app_with(store, events.clone());
    (app, events, cloud)
}

/// A store holding run `r1` in `state` with coordinator `coord-1` (seeded directly: for runs whose
/// cancel does not depend on what its events fold to).
fn app_with_run(state: &str) -> (axum::Router, Arc<MemEventSink>, Arc<FakeCloud>) {
    let store = MemStore::new();
    let mut detail = testing::run_detail("r1", state);
    detail.coordinator_job_id = Some(CoordinatorJobId::new("coord-1"));
    store.insert(detail);
    app_over(Arc::new(store))
}

fn r1() -> RunId {
    RunId::new("r1")
}

fn ts(secs: f64) -> Timestamp {
    Timestamp::from_epoch_seconds(secs)
}

/// The launch sweep's (or submit's) record of `r1` launched: its anchor, and its coordinator's
/// binding when it has one.
async fn record_launched(events: &MemEventSink, coordinator: Option<&str>) {
    let mut launched = vec![biffix_core::event::run_submitted(
        &r1(),
        ts(2.0),
        biffix_core::event::RunSubmitted::new(
            "wf",
            "sha",
            "sha256:d",
            serde_json::json!({}),
            vec![],
        ),
    )];
    if let Some(job) = coordinator {
        launched.push(biffix_core::event::coordinator_submitted(
            &r1(),
            ts(2.0),
            &BatchJobId::new(job),
        ));
    }
    events.record_all(&launched).await.unwrap();
}

/// Submit's record of `r1` accepted to wait for its images (`preparing_images`).
async fn record_preparing(events: &MemEventSink) {
    let preparing = biffix_core::event::run_preparing(
        &r1(),
        ts(1.0),
        biffix_core::event::RunPreparing {
            workflow_version_id: "wf".into(),
            params: serde_json::json!({}),
            input_uris: vec![],
            submitter: None,
            submitter_label: None,
            run_name: None,
            output_prefix: None,
            resumed_from: None,
            images: vec![],
            experiment_id: None,
        },
    );
    events.record_all(&[preparing]).await.unwrap();
}

/// Park `r1`'s launch request, as submit does for a run waiting for its images.
fn park_launch(store: &MemStore) {
    store.pending.lock().unwrap().insert(
        "r1".into(),
        biffix_backend::store::pending::PendingLaunch {
            run_id: r1(),
            created_at_micros: 1,
            plan: serde_json::json!({}),
        },
    );
}

async fn cancel(app: axum::Router, run: &str) -> (StatusCode, String) {
    cancel_as(app, run, None).await
}

/// `POST /runs/{run}/cancel` as `bearer`: the status and the body as text (an error's message).
async fn cancel_as(app: axum::Router, run: &str, bearer: Option<&str>) -> (StatusCode, String) {
    let uri = format!("/runs/{run}/cancel");
    let (status, v) = testing::call_as(app, "POST", &uri, None, bearer).await;
    let body = match v {
        serde_json::Value::String(text) => text,
        v => v.to_string(),
    };
    (status, body)
}

/// The recorded events' kinds, in order.
fn kinds(events: &MemEventSink) -> Vec<&'static str> {
    events
        .events
        .lock()
        .unwrap()
        .iter()
        .map(|e| match e.data {
            EventData::RunPreparing(_) => "run_preparing",
            EventData::RunSubmitted(_) => "run_submitted",
            EventData::CoordinatorSubmitted {} => "coordinator_submitted",
            EventData::CancelRequested { .. } => "cancel_requested",
            EventData::RunCancelled {} => "run_cancelled",
            _ => "other",
        })
        .collect()
}

/// The actor of the recorded `cancel_requested`, if one was recorded.
fn cancel_actor(events: &MemEventSink) -> Option<String> {
    events.events.lock().unwrap().iter().find_map(|e| match &e.data {
        EventData::CancelRequested { actor, .. } => Some(actor.clone()),
        _ => None,
    })
}

#[tokio::test]
async fn cancelling_a_running_run_terminates_its_coordinator() {
    let (app, events, cloud) = app_with_run("running");
    let (status, body) = cancel(app, "r1").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    // The request is latched; the run ends when the terminated coordinator exits (→ CANCELLED).
    // With auth disabled the one asking is `local`.
    assert_eq!(kinds(&events), vec!["cancel_requested"]);
    assert_eq!(cancel_actor(&events).as_deref(), Some("local"));
    let terminated = cloud.terminated.lock().unwrap();
    assert_eq!(
        *terminated,
        vec![("coord-1".to_string(), "cancelled via biffix by local".to_string())]
    );
}

const SECRET: &[u8] = b"cancel-api-secret";
const ISS: &str = "https://cognito-idp.us-east-1.amazonaws.com/us-east-1_cancel";
const AUD: &str = "biffix-web";

/// A token for `sub` in `group`.
fn token(sub: &str, group: &str) -> String {
    testing::user_token(SECRET, ISS, AUD, sub, &[group])
}

#[tokio::test]
async fn a_cancel_records_who_asked_and_names_them_in_the_terminate() {
    let store = MemStore::new();
    let mut detail = testing::run_detail("r1", "running");
    detail.coordinator_job_id = Some(CoordinatorJobId::new("coord-1"));
    store.insert(detail);
    let auth = testing::static_auth(
        SECRET,
        ISS,
        vec![AUD.into()],
        &[("viewers", Role::Viewer), ("operators", Role::Operator)],
    );
    let events = Arc::new(MemEventSink::new());
    let cloud = Arc::new(FakeCloud::new());
    let app = api::router(
        TestState::new()
            .with_store(Arc::new(store))
            .with_events(events.clone())
            .with_cloud(cloud.clone())
            .with_auth(auth)
            .build(),
    );

    // A viewer may not cancel: nothing is recorded or terminated.
    let (status, body) = cancel_as(app.clone(), "r1", Some(&token("u-1", "viewers"))).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert!(events.events.lock().unwrap().is_empty());
    assert!(cloud.terminated.lock().unwrap().is_empty());

    let (status, body) = cancel_as(app, "r1", Some(&token("u-7", "operators"))).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    assert_eq!(cancel_actor(&events).as_deref(), Some("u-7"));
    assert_eq!(
        *cloud.terminated.lock().unwrap(),
        vec![("coord-1".to_string(), "cancelled via biffix by u-7".to_string())]
    );
}

fn task(key: &str, state: &str, job: Option<&str>) -> biffix_api::Task {
    task_view(key, state, None, job)
}

#[tokio::test]
async fn cancelling_terminates_the_live_child_jobs_too() {
    // The engine may leave its children running when its coordinator is terminated (Nextflow
    // does), so cancel terminates every running/queued task's job itself — but not finished ones.
    let store = MemStore::new();
    let mut detail = testing::run_detail("r1", "running");
    detail.coordinator_job_id = Some(CoordinatorJobId::new("coord-1"));
    detail.tasks = vec![
        task("a", "running", Some("job-a")),
        task("b", "queued", Some("job-b")),
        task("c", "succeeded", Some("job-c")),
        task("d", "running", None), // a local task has no Batch job
    ];
    store.insert(detail);
    let (app, _, cloud) = app_over(Arc::new(store));
    let (status, _) = cancel(app, "r1").await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let mut jobs: Vec<String> =
        cloud.terminated.lock().unwrap().iter().map(|(job, _)| job.clone()).collect();
    jobs.sort();
    assert_eq!(jobs, vec!["coord-1", "job-a", "job-b"]);
}

#[tokio::test]
async fn a_cancel_terminates_at_most_200_children_inline() {
    // The rest are the ended-run sweep's, so the handler stays inside its Lambda timeout.
    let store = MemStore::new();
    let mut detail = testing::run_detail("r1", "running");
    detail.coordinator_job_id = Some(CoordinatorJobId::new("coord-1"));
    detail.tasks = (0..250)
        .map(|i| task(&format!("t{i:03}"), "running", Some(&format!("job-{i:03}"))))
        .collect();
    store.insert(detail);
    let (app, _, cloud) = app_over(Arc::new(store));
    let (status, body) = cancel(app, "r1").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let mut jobs: Vec<String> =
        cloud.terminated.lock().unwrap().iter().map(|(job, _)| job.clone()).collect();
    jobs.sort();
    // The coordinator, then the first 200 live children in task order.
    let want: Vec<String> = std::iter::once("coord-1".to_string())
        .chain((0..200).map(|i| format!("job-{i:03}")))
        .collect();
    assert_eq!(jobs, want);
}

#[tokio::test]
async fn cancelling_a_submitted_run_ends_it_now_and_terminates_its_coordinator() {
    // The coordinator may never start (or never report), so the run is ended immediately too.
    let store = Arc::new(MemStore::new());
    let (app, events, cloud) = folding_app_over(store.clone());
    record_launched(&events, Some("coord-1")).await;
    assert_eq!(events.folded_state("r1"), Some(RunState::Submitted));
    let (status, _) = cancel(app, "r1").await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(
        kinds(&events),
        vec!["run_submitted", "coordinator_submitted", "cancel_requested", "run_cancelled"]
    );
    assert_eq!(events.folded_state("r1"), Some(RunState::Cancelled));
    assert_eq!(
        *cloud.terminated.lock().unwrap(),
        vec![("coord-1".to_string(), "cancelled via biffix by local".to_string())]
    );
}

#[tokio::test]
async fn cancelling_a_finished_run_is_a_no_op() {
    for state in ["succeeded", "failed", "cancelled"] {
        let (app, events, cloud) = app_with_run(state);
        let (status, _) = cancel(app, "r1").await;
        assert_eq!(status, StatusCode::ACCEPTED, "{state}");
        assert!(kinds(&events).is_empty(), "{state}");
        assert!(cloud.terminated.lock().unwrap().is_empty(), "{state}");
    }
}

#[tokio::test]
async fn a_failed_terminate_is_reported_but_the_request_stays_recorded() {
    let (app, events, cloud) = app_with_run("running");
    cloud.fail_terminate.store(true, Ordering::SeqCst);
    let (status, body) = cancel(app, "r1").await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert!(body.contains("terminating the coordinator failed"), "{body}");
    assert_eq!(kinds(&events), vec!["cancel_requested"]);
}

#[tokio::test]
async fn a_submitted_run_whose_terminate_fails_stays_open_and_a_retry_terminates_it() {
    // The run must not be ended (terminal) while its coordinator may still start: a retry has to
    // reach the terminate again.
    let store = Arc::new(MemStore::new());
    let (app, events, cloud) = folding_app_over(store.clone());
    record_launched(&events, Some("coord-1")).await;
    cloud.fail_terminate.store(true, Ordering::SeqCst);
    let (status, body) = cancel(app.clone(), "r1").await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
    assert!(body.contains("retry"), "{body}");
    assert_eq!(kinds(&events), vec!["run_submitted", "coordinator_submitted", "cancel_requested"]);
    assert!(cloud.terminated.lock().unwrap().is_empty());

    // A `cancel_requested` alone ends nothing: the run the retry reads (the fold of the log) is
    // still `submitted`, so the retry terminates the coordinator and only then ends the run.
    assert_eq!(events.folded_state("r1"), Some(RunState::Submitted));
    cloud.fail_terminate.store(false, Ordering::SeqCst);
    let (status, body) = cancel(app, "r1").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let terminated = cloud.terminated.lock().unwrap();
    assert_eq!(terminated.len(), 1);
    assert_eq!(terminated[0].0, "coord-1");
    // The retry's `cancel_requested` dedups onto the first.
    assert_eq!(
        kinds(&events),
        vec!["run_submitted", "coordinator_submitted", "cancel_requested", "run_cancelled"]
    );
    assert_eq!(events.folded_state("r1"), Some(RunState::Cancelled));
}

#[tokio::test]
async fn cancelling_a_submitted_run_without_a_coordinator_ends_it_at_once() {
    let store = Arc::new(MemStore::new());
    let (app, events, cloud) = folding_app_over(store.clone());
    record_launched(&events, None).await;
    let (status, _) = cancel(app, "r1").await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(kinds(&events), vec!["run_submitted", "cancel_requested", "run_cancelled"]);
    assert_eq!(events.folded_state("r1"), Some(RunState::Cancelled));
    assert!(cloud.terminated.lock().unwrap().is_empty());
}

#[tokio::test]
async fn cancelling_a_run_preparing_its_images_ends_it_and_drops_its_parked_launch() {
    let store = Arc::new(MemStore::new());
    park_launch(&store);
    let (app, events, cloud) = folding_app_over(store.clone());
    record_preparing(&events).await;
    assert_eq!(events.folded_state("r1"), Some(RunState::PreparingImages));
    let (status, body) = cancel(app, "r1").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    assert_eq!(kinds(&events), vec!["run_preparing", "cancel_requested", "run_cancelled"]);
    assert_eq!(events.folded_state("r1"), Some(RunState::Cancelled));
    assert!(store.pending.lock().unwrap().is_empty());
    assert!(cloud.terminated.lock().unwrap().is_empty());
}

#[tokio::test]
async fn cancelling_an_unknown_run_is_404() {
    let (app, _, cloud) = app_with_run("running");
    let (status, _) = cancel(app, "nope").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(cloud.terminated.lock().unwrap().is_empty());
}

/// An app over a store holding run `r1` preparing its images (from its events), its launch parked
/// and claimed (a sweep is launching it), and the store and folding sink.
async fn launching_run() -> (axum::Router, Arc<MemStore>, Arc<MemEventSink>, Arc<FakeCloud>) {
    let store = Arc::new(MemStore::new());
    park_launch(&store);
    let (app, events, cloud) = folding_app_over(store.clone());
    record_preparing(&events).await;
    store.launch_claims.lock().unwrap().insert("r1".into());
    (app, store, events, cloud)
}

#[tokio::test(start_paused = true)]
async fn a_cancel_waits_out_a_launch_and_stops_the_coordinator_it_bound() {
    let (app, store, events, cloud) = launching_run().await;
    // The sweep finishes launching a moment later: the run is bound to its coordinator (its anchor
    // and binding recorded), its parked launch dropped, and the claim released.
    let sweep = {
        let (store, events) = (store.clone(), events.clone());
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            record_launched(&events, Some("coord-1")).await;
            store.pending.lock().unwrap().clear();
            store.launch_claims.lock().unwrap().clear();
        })
    };
    let (status, body) = cancel(app, "r1").await;
    sweep.await.unwrap();
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    // Seen under the claim: the coordinator the sweep bound is stopped (once), then the run ends.
    let terminated = cloud.terminated.lock().unwrap();
    assert_eq!(terminated.len(), 1, "{terminated:?}");
    assert_eq!(terminated[0].0, "coord-1");
    assert_eq!(
        kinds(&events),
        vec![
            "run_preparing",
            "run_submitted",
            "coordinator_submitted",
            "cancel_requested",
            "run_cancelled"
        ]
    );
    assert_eq!(events.folded_state("r1"), Some(RunState::Cancelled));
}

/// The API Lambda's timeout (`infra/biffix_infra/constructs/backend_api.py`).
const API_LAMBDA_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(29);

#[tokio::test(start_paused = true)]
async fn a_cancel_asks_to_be_retried_while_a_launch_holds_the_run() {
    let (app, store, events, cloud) = launching_run().await;
    let started = tokio::time::Instant::now();
    let (status, body) = cancel(app, "r1").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert!(body.contains("retry"), "{body}");
    // In time for the caller to read it: well inside the Lambda's timeout, with room to spare.
    let waited = started.elapsed();
    assert!(waited + std::time::Duration::from_secs(5) < API_LAMBDA_TIMEOUT, "{waited:?}");
    // Nothing recorded, nothing dropped: the launch in progress decides.
    assert_eq!(kinds(&events), vec!["run_preparing"]);
    assert!(cloud.terminated.lock().unwrap().is_empty());
    assert!(store.pending.lock().unwrap().contains_key("r1"));
}

#[tokio::test]
async fn a_cancel_holds_the_launch_while_it_ends_a_preparing_run() {
    let (app, store, events, _) = launching_run().await;
    store.launch_claims.lock().unwrap().clear();
    let (status, body) = cancel(app, "r1").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    assert_eq!(kinds(&events), vec!["run_preparing", "cancel_requested", "run_cancelled"]);
    assert!(store.pending.lock().unwrap().is_empty());
    // The run was ended and its parked launch dropped while the cancel held the run's launch, so
    // no sweep could launch it in between.
    assert_eq!(
        *events.recorded_under_claim.lock().unwrap(),
        vec![
            ("run_preparing".to_string(), false),
            ("cancel_requested".to_string(), true),
            ("run_cancelled".to_string(), true),
        ]
    );
    assert_eq!(*store.pending_deleted.lock().unwrap(), vec![("r1".to_string(), true)]);
    // Released once the cancel is done.
    assert!(store.launch_claims.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_cancelled_run_names_who_cancelled_it_by_their_label() {
    let store = Arc::new(MemStore::new());
    let auth =
        testing::static_auth(SECRET, ISS, vec![AUD.into()], &[("operators", Role::Operator)]);
    let events = Arc::new(MemEventSink::folding_into(store.clone()));
    let app = api::router(
        TestState::new().with_store(store).with_events(events.clone()).with_auth(auth).build(),
    );
    record_launched(&events, None).await;
    let with_email = testing::hs256_token(
        SECRET,
        &serde_json::json!({
            "sub": "u-7", "email": "ada@lab.example.org", "cognito:groups": ["operators"],
            "iss": ISS, "aud": AUD, "exp": testing::now() + 3600
        }),
    );
    let (status, body) = cancel_as(app.clone(), "r1", Some(&with_email)).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let label = events.events.lock().unwrap().iter().find_map(|e| match &e.data {
        EventData::CancelRequested { actor_label, .. } => Some(actor_label.clone()),
        _ => None,
    });
    assert_eq!(label, Some(Some("ada@lab.example.org".to_string())));
    let (status, v) = testing::call_as(app, "GET", "/runs/r1", None, Some(&with_email)).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["state"], "cancelled");
    assert_eq!(v["cancelled_by"], "ada@lab.example.org");
}

#[tokio::test]
async fn a_run_not_cancelled_names_no_one() {
    let store = Arc::new(MemStore::new());
    let (app, events, _) = folding_app_over(store);
    record_launched(&events, None).await;
    let (_, v) = testing::call_as(app, "GET", "/runs/r1", None, None).await;
    assert_eq!(v["state"], "submitted");
    assert!(v.get("cancelled_by").is_none(), "{v}");
}

/// Record `r1`'s anchor, submitted by `submitter` (no coordinator: a cancel ends it at once).
async fn record_submitted_by(events: &MemEventSink, submitter: &str) {
    let mut anchor = biffix_core::event::RunSubmitted::new(
        "wf",
        "sha",
        "sha256:d",
        serde_json::json!({}),
        vec![],
    );
    anchor.submitter = Some(submitter.into());
    events.record_all(&[biffix_core::event::run_submitted(&r1(), ts(2.0), anchor)]).await.unwrap();
}

#[tokio::test]
async fn cancelling_my_own_run_does_not_notify_me() {
    // Auth off: the caller is `local`, who submitted the run.
    let store = Arc::new(MemStore::new());
    let (app, events, _) = folding_app_over(store.clone());
    record_submitted_by(&events, "user:local").await;
    let (status, body) = cancel(app, "r1").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let list = store.list_notifications("local", 20).await.unwrap();
    let ids: Vec<(&str, bool)> = list.items.iter().map(|n| (n.id.as_str(), n.read)).collect();
    // The fold produced the cancelled notification, already read: it was the user's own action.
    assert_eq!(ids, [("r1:cancelled", true)]);
    assert_eq!((list.unread, list.unread_failed), (0, 0));
}

#[tokio::test]
async fn another_user_cancelling_my_run_notifies_me() {
    let store = Arc::new(MemStore::new());
    let auth =
        testing::static_auth(SECRET, ISS, vec![AUD.into()], &[("operators", Role::Operator)]);
    let events = Arc::new(MemEventSink::folding_into(store.clone()));
    let app = api::router(
        TestState::new()
            .with_store(store.clone())
            .with_events(events.clone())
            .with_auth(auth)
            .build(),
    );
    record_submitted_by(&events, "user:alice").await;
    let bob = testing::hs256_token(
        SECRET,
        &serde_json::json!({
            "sub": "bob", "cognito:groups": ["operators"],
            "iss": ISS, "aud": AUD, "exp": testing::now() + 3600
        }),
    );
    let (status, body) = cancel_as(app, "r1", Some(&bob)).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let list = store.list_notifications("alice", 20).await.unwrap();
    assert_eq!(list.items.len(), 1);
    assert_eq!((list.items[0].id.as_str(), list.items[0].read), ("r1:cancelled", false));
    assert_eq!(list.unread, 1);
    // Nothing was marked for the canceller either.
    assert!(store.notification_reads.lock().unwrap().is_empty());
}
