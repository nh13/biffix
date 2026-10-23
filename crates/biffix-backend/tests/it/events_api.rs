//! `POST /events` — the engine ingest route.
//!
//! The token is the engine's only credential and this route is edge-reachable, so the tests that
//! matter are the authorization ones: no token, a forged or expired token, a token for a DIFFERENT
//! run or for a cancelled one (and a revocation check that cannot be made), and a token whose run
//! claims a Batch job it does not own.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{json, Value};
use tower::ServiceExt;

use biffix_backend::auth::capability::CapabilityVerifier;
use biffix_backend::aws::AwsError;
use biffix_backend::testing::{self, FakeCloud, MemEventSink, MemStore, TestState};
use biffix_backend::{api, AppState};

const SECRET: &[u8] = b"deployment-signing-secret";

fn state() -> (AppState, Arc<MemEventSink>, Arc<CapabilityVerifier>) {
    state_with(MemStore::new())
}

/// Build app state around a caller-seeded store (so a test can put a run in a chosen state and
/// exercise the revocation-by-recheck path).
fn state_with(store: MemStore) -> (AppState, Arc<MemEventSink>, Arc<CapabilityVerifier>) {
    state_with_cloud(store, batch_cloud())
}

/// A Batch that knows `job-1`, tagged for `run-1`, and `job-b`, tagged for `run-b`, both created at
/// the epoch (so the fixtures' small `ts` values are within their lifetime).
fn batch_cloud() -> Arc<FakeCloud> {
    let cloud = FakeCloud::new();
    cloud.add_batch_job("job-1", Some("run-1"), 0.0);
    cloud.add_batch_job("job-b", Some("run-b"), 0.0);
    Arc::new(cloud)
}

fn state_with_cloud(
    store: MemStore,
    cloud: Arc<FakeCloud>,
) -> (AppState, Arc<MemEventSink>, Arc<CapabilityVerifier>) {
    let events = Arc::new(MemEventSink::new());
    let capability = Arc::new(CapabilityVerifier::new(SECRET.to_vec()));
    let state = TestState::new()
        .with_store(Arc::new(store))
        .with_events(events.clone())
        .with_cloud(cloud)
        .with_capability(capability.clone())
        .build();
    (state, events, capability)
}

/// A minimal projected run in a given derived state, for the revocation tests.
fn seeded_store(run_id: &str, run_state: &str) -> MemStore {
    let store = MemStore::new();
    store.insert(testing::run_detail(run_id, run_state));
    store
}

fn batch(run: &str) -> Value {
    json!({"events": [
        {"schema_version": 1, "ts": 1.0, "run_id": run, "kind": "run_started"},
        {"schema_version": 1, "ts": 2.0, "run_id": run, "kind": "task_created",
         "task_key": "align:sample=A"},
        {"schema_version": 1, "ts": 3.0, "run_id": run, "kind": "task_submitted",
         "task_key": "align:sample=A", "attempt": 1, "batch_job_id": "job-1"}
    ]})
}

fn post(token: Option<&str>, body: &Value) -> Request<Body> {
    let mut b =
        Request::builder().method("POST").uri("/events").header("content-type", "application/json");
    if let Some(t) = token {
        b = b.header("authorization", format!("Bearer {t}"));
    }
    b.body(Body::from(body.to_string())).unwrap()
}

#[tokio::test]
async fn a_valid_token_appends_the_batch() {
    let (state, events, cap) = state();
    let token = cap.mint_for(&biffix_core::ids::RunId::new("run-1"));
    let res = api::router(state).oneshot(post(Some(&token), &batch("run-1"))).await.unwrap();
    assert_eq!(res.status(), StatusCode::ACCEPTED);
    // All three engine events reached the log, including the task_submitted binding that joins the
    // engine and AWS streams.
    let recorded = events.events.lock().unwrap();
    assert_eq!(recorded.len(), 3);
    assert!(recorded
        .iter()
        .any(|e| e.kind() == biffix_core::event::Kind::TaskSubmitted && e.batch_job_id.is_some()));
}

#[tokio::test]
async fn ingest_response_reports_appended_then_duplicate_counts() {
    // The IngestSummary body is the plugin's only signal for how much of a buffered flush actually
    // landed vs. was a retry no-op, so assert it — and exercise the dedup path a second POST hits.
    let (state, _events, cap) = state();
    let app = api::router(state);
    let token = cap.mint_for(&biffix_core::ids::RunId::new("run-1"));

    let first = app.clone().oneshot(post(Some(&token), &batch("run-1"))).await.unwrap();
    assert_eq!(first.status(), StatusCode::ACCEPTED);
    let body = body_json(first).await;
    assert_eq!(body["appended"], 3, "all three fresh events append");
    assert_eq!(body["duplicates"], 0);

    // Re-POST the identical batch: every event dedups on its key, none is newly appended.
    let second = app.oneshot(post(Some(&token), &batch("run-1"))).await.unwrap();
    assert_eq!(second.status(), StatusCode::ACCEPTED);
    let body = body_json(second).await;
    assert_eq!(body["appended"], 0, "a retried batch appends nothing");
    assert_eq!(body["duplicates"], 3, "and reports all three as duplicates");
}

#[tokio::test]
async fn no_token_is_rejected() {
    let (state, events, _) = state();
    let res = api::router(state).oneshot(post(None, &batch("run-1"))).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    assert!(events.events.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_forged_token_is_rejected() {
    let (state, events, _) = state();
    let forged = CapabilityVerifier::new(b"attacker".to_vec())
        .mint_for(&biffix_core::ids::RunId::new("run-1"));
    let res = api::router(state).oneshot(post(Some(&forged), &batch("run-1"))).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    assert!(events.events.lock().unwrap().is_empty());
}

#[tokio::test]
async fn an_expired_token_is_rejected() {
    // Validly signed for the right run, but past its expiry (a token expires at its `exp` second).
    let (state, events, _) = state();
    let expired = biffix_core::capability::mint(
        SECRET,
        &biffix_core::ids::RunId::new("run-1"),
        testing::now(),
    );
    let res = api::router(state).oneshot(post(Some(&expired), &batch("run-1"))).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    assert!(events.events.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_revocation_check_that_cannot_read_the_run_fails_closed() {
    // A store outage must not let a cancelled run's token through: the append is refused as
    // retryable (503), and nothing lands.
    let store = seeded_store("run-1", "cancelled");
    store.fail_run_state.store(true, std::sync::atomic::Ordering::SeqCst);
    let (state, events, cap) = state_with(store);
    let token = cap.mint_for(&biffix_core::ids::RunId::new("run-1"));
    let res = api::router(state).oneshot(post(Some(&token), &batch("run-1"))).await.unwrap();
    assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(events.events.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_token_cannot_append_to_another_run() {
    // The load-bearing property: run A's coordinator must not be able to forge run B's provenance.
    let (state, events, cap) = state();
    let token = cap.mint_for(&biffix_core::ids::RunId::new("run-a"));
    let res = api::router(state).oneshot(post(Some(&token), &batch("run-b"))).await.unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        body_json(res).await,
        "capability token does not authorize the run these events claim"
    );
    assert!(
        events.events.lock().unwrap().is_empty(),
        "a rejected batch must not be partially applied"
    );
}

#[tokio::test]
async fn a_cancelled_run_revokes_its_token() {
    // Revocation-by-recheck: once a run is cancelled, its capability token no longer authorizes
    // appends — cancel == revoke — even though the token itself is still cryptographically valid
    // and scoped to that run. This is the incident-response lever for a leaked token.
    let (state, events, cap) = state_with(seeded_store("run-1", "cancelled"));
    let token = cap.mint_for(&biffix_core::ids::RunId::new("run-1"));
    let res = api::router(state).oneshot(post(Some(&token), &batch("run-1"))).await.unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
    assert!(
        events.events.lock().unwrap().is_empty(),
        "a revoked (cancelled) run must not accept any appends"
    );
}

#[tokio::test]
async fn a_terminal_but_not_cancelled_run_still_appends() {
    // Only CANCELLED is revoked, not succeeded/failed — otherwise the coordinator's at-least-once
    // events.jsonl replay backstop (which re-POSTs on exit, after the run is already terminal)
    // would be rejected and dropped events would never be recovered.
    for run_state in ["succeeded", "failed"] {
        let (state, events, cap) = state_with(seeded_store("run-1", run_state));
        let token = cap.mint_for(&biffix_core::ids::RunId::new("run-1"));
        let res = api::router(state).oneshot(post(Some(&token), &batch("run-1"))).await.unwrap();
        assert_eq!(res.status(), StatusCode::ACCEPTED, "{run_state}");
        assert_eq!(events.events.lock().unwrap().len(), 3, "{run_state}");
    }
}

#[tokio::test]
async fn ingest_fails_closed_when_no_secret_is_configured() {
    let (mut state, events, cap) = state();
    let token = cap.mint_for(&biffix_core::ids::RunId::new("run-1"));
    state.capability = None; // deployment without a signing secret
    let res = api::router(state).oneshot(post(Some(&token), &batch("run-1"))).await.unwrap();
    assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(events.events.lock().unwrap().is_empty());
}

#[tokio::test]
async fn an_oversized_batch_is_rejected() {
    let (state, events, cap) = state();
    let token = cap.mint_for(&biffix_core::ids::RunId::new("run-1"));
    let events_json: Vec<Value> = (0..501)
        .map(|i| json!({"schema_version": 1, "ts": i as f64, "run_id": "run-1", "kind": "run_started"}))
        .collect();
    let res = api::router(state)
        .oneshot(post(Some(&token), &json!({"events": events_json})))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert!(events.events.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_batch_at_the_exact_max_is_accepted() {
    // The accept boundary: exactly MAX_BATCH (500) events must pass, not just "< 501". Without this
    // an off-by-one in the `>`/`>=` size guard would reject a legitimate full flush and go
    // unnoticed.
    let (state, _events, cap) = state();
    let token = cap.mint_for(&biffix_core::ids::RunId::new("run-1"));
    let events_json: Vec<Value> = (0..500)
        .map(|i| {
            json!({"schema_version": 1, "ts": i as f64, "run_id": "run-1",
                        "kind": "task_created", "task_key": format!("rule:i={i}")})
        })
        .collect();
    let res = api::router(state)
        .oneshot(post(Some(&token), &json!({"events": events_json})))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::ACCEPTED);
    let body = body_json(res).await;
    assert_eq!(body["appended"], 500, "all 500 distinct events append");
}

#[tokio::test]
async fn an_unknown_kind_is_stored_and_does_not_reject_the_batch() {
    // A newer plugin's kind must not cost the batch its known events: they append (and Postgres
    // folds them), and the unknown one is stored under its own kind, never folded (the open-enum
    // contract). The sink here only records, so this checks what is appended, not the fold.
    let (state, events, cap) = state();
    let token = cap.mint_for(&biffix_core::ids::RunId::new("run-1"));
    let mut body = batch("run-1");
    body["events"].as_array_mut().unwrap().push(json!({
        "schema_version": 2, "ts": 4.0, "run_id": "run-1",
        "kind": "engine_task_memory_sample", "task_key": "align:sample=A", "rss": 1
    }));
    let res = api::router(state).oneshot(post(Some(&token), &body)).await.unwrap();
    assert_eq!(res.status(), StatusCode::ACCEPTED);
    assert_eq!(body_json(res).await["appended"], 4);
    let recorded = events.events.lock().unwrap();
    let unknown = recorded
        .iter()
        .find(|e| e.kind() == biffix_core::event::Kind::Unknown("engine_task_memory_sample".into()))
        .expect("the unknown kind is stored");
    assert!(matches!(unknown.data, biffix_core::event::EventData::Unknown(_)));
    assert_eq!(unknown.run_id, Some(biffix_core::ids::RunId::new("run-1")));
    assert!(recorded.iter().any(|e| e.kind() == biffix_core::event::Kind::TaskSubmitted));
}

#[tokio::test]
async fn an_unknown_kind_is_still_bound_to_the_token_s_run() {
    let (state, events, cap) = state();
    let token = cap.mint_for(&biffix_core::ids::RunId::new("run-1"));
    let mut body = batch("run-1");
    body["events"].as_array_mut().unwrap().push(json!({
        "ts": 4.0, "run_id": "run-2", "kind": "engine_task_memory_sample"
    }));
    let res = api::router(state).oneshot(post(Some(&token), &body)).await.unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
    assert!(events.events.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_malformed_known_kind_rejects_the_batch() {
    // Known kinds are still validated: a broken body is a producer bug, not version skew, and the
    // batch is rejected whole (nothing appended). So is a kind no engine may send, an event with no
    // run id, and an unknown kind that is not a snake_case tag.
    for bad in [
        json!({"schema_version": 1, "ts": 4.0, "run_id": "run-1", "kind": "run_progress",
               "completed": 1}),
        json!({"ts": 4.0, "run_id": "run-1", "kind": "run_submitted"}),
        json!({"ts": 4.0, "kind": "run_started"}),
        json!({"ts": 4.0, "run_id": "run-1", "kind": "bad:kind"}),
        // An unknown kind outside the engine namespace: a later build could add it as a
        // control-plane fact and fold this row as one.
        json!({"ts": 4.0, "run_id": "run-1", "kind": "run_approved"}),
    ] {
        let (state, events, cap) = state();
        let token = cap.mint_for(&biffix_core::ids::RunId::new("run-1"));
        let mut body = batch("run-1");
        body["events"].as_array_mut().unwrap().push(bad.clone());
        let res = api::router(state).oneshot(post(Some(&token), &body)).await.unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST, "{bad}");
        assert!(events.events.lock().unwrap().is_empty(), "{bad}");
    }
}

// ── Batch job claims (a task_submitted binds a job's AWS facts to the claiming run) ──

/// One `task_submitted` from `run` claiming `job` at `ts`.
fn claim(run: &str, job: &str, ts: f64) -> Value {
    json!({"schema_version": 1, "ts": ts, "run_id": run, "kind": "task_submitted",
           "task_key": "align:sample=A", "attempt": 1, "batch_job_id": job})
}

/// POST `events` with a token for `run`; returns the status.
async fn post_as(
    state: AppState,
    cap: &CapabilityVerifier,
    run: &str,
    events: Vec<Value>,
) -> StatusCode {
    post_for_body(state, cap, run, events).await.0
}

/// POST `events` with a token for `run`; returns the status and the JSON body.
async fn post_for_body(
    state: AppState,
    cap: &CapabilityVerifier,
    run: &str,
    events: Vec<Value>,
) -> (StatusCode, Value) {
    let token = cap.mint_for(&biffix_core::ids::RunId::new(run));
    let res =
        api::router(state).oneshot(post(Some(&token), &json!({ "events": events }))).await.unwrap();
    let status = res.status();
    (status, body_json(res).await)
}

/// The kinds the sink recorded, in order.
fn recorded_kinds(events: &MemEventSink) -> Vec<String> {
    events.events.lock().unwrap().iter().map(|e| e.kind().as_str().to_string()).collect()
}

#[tokio::test]
async fn a_token_cannot_claim_another_run_s_batch_job() {
    // Run A naming run B's worker job in its own task_submitted would, folded first, route B's
    // attempt state, exit code, host and cost to A for good (the log is immutable). The job's run
    // tag says it is B's, so the claim is refused, named by its index; the rest of the batch lands
    // (a 403 would make the plugin drop all of it).
    let (state, events, cap) = state();
    let (status, body) = post_for_body(
        state,
        &cap,
        "run-a",
        vec![
            json!({"schema_version": 1, "ts": 1.0, "run_id": "run-a", "kind": "run_started"}),
            claim("run-a", "job-b", 2.0),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(
        body,
        json!({"appended": 1, "duplicates": 0, "refused": [
            {"index": 1, "reason": "Batch job job-b is not a job of this run"}
        ]})
    );
    assert_eq!(recorded_kinds(&events), ["run_started"]);
}

#[tokio::test]
async fn an_untagged_or_unknown_job_cannot_be_claimed() {
    // A job with no run tag (or one Batch does not know) cannot be shown to be the claimant's, so
    // the claim fails closed.
    for job in ["job-untagged", "job-nobody-knows"] {
        let cloud = batch_cloud();
        cloud.add_batch_job("job-untagged", None, 0.0);
        let (state, events, cap) = state_with_cloud(MemStore::new(), cloud);
        let (status, body) =
            post_for_body(state, &cap, "run-1", vec![claim("run-1", job, 2.0)]).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{job}");
        assert_eq!(
            body["refused"],
            json!([{"index": 0, "reason": format!("Batch job {job} is not a job of this run")}]),
            "{job}"
        );
        assert!(events.events.lock().unwrap().is_empty(), "{job}");
    }
}

#[tokio::test]
async fn a_job_another_run_already_claims_is_refused_whatever_its_tag() {
    // Defence in depth: even with a tag that names the claimant (a retagged job), a job the log
    // already binds to another run is not claimable.
    let cloud = batch_cloud();
    cloud.add_batch_job("job-x", Some("run-a"), 0.0);
    let (state, events, cap) = state_with_cloud(MemStore::new(), cloud);
    let mut owner = biffix_core::translate::from_engine_value(
        biffix_core::source::EventSource::Snakemake,
        claim("run-b", "job-x", 1.0),
    )
    .unwrap();
    owner.dedup_key = "run-b-claim".into();
    events.events.lock().unwrap().push(owner);
    let (status, body) =
        post_for_body(state, &cap, "run-a", vec![claim("run-a", "job-x", 2.0)]).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(
        body["refused"],
        json!([{"index": 0, "reason": "Batch job job-x is already claimed by another run"}])
    );
    assert_eq!(events.events.lock().unwrap().len(), 1, "only run B's own claim");
}

#[tokio::test]
async fn a_claim_cannot_predate_its_job_or_lie_in_the_future() {
    // The fold binds a job to its first claim in ts order, so a claim backdated ahead of the
    // owner's would win it; a claim cannot be older than its job (beyond clock skew). Nor can it
    // be dated past now (beyond the same 300 s skew).
    let now = biffix_core::clock::epoch_secs();
    for ts in [1_000.0 - 301.0, now + 301.0, now + 3600.0] {
        let cloud = batch_cloud();
        cloud.add_batch_job("job-late", Some("run-1"), 1_000.0);
        let (state, events, cap) = state_with_cloud(MemStore::new(), cloud);
        let (status, body) =
            post_for_body(state, &cap, "run-1", vec![claim("run-1", "job-late", ts)]).await;
        assert_eq!(status, StatusCode::ACCEPTED, "ts {ts}");
        assert_eq!(
            body["refused"],
            json!([{"index": 0,
                    "reason": "task_submitted for Batch job job-late has a ts outside the job's lifetime"}]),
            "ts {ts}"
        );
        assert!(events.events.lock().unwrap().is_empty(), "ts {ts}");
    }
    // Within the skew (up to exactly 300 s before the job), any time after the job was created,
    // and a little ahead of this clock, a claim is fine: a plugin's buffered flush, a late retry,
    // or a producer whose clock runs fast.
    for ts in [1_000.0 - 300.0, 1_000.0 - 299.0, 50_000.0, now + 60.0] {
        let cloud = batch_cloud();
        cloud.add_batch_job("job-late", Some("run-1"), 1_000.0);
        let (state, events, cap) = state_with_cloud(MemStore::new(), cloud);
        let (status, body) =
            post_for_body(state, &cap, "run-1", vec![claim("run-1", "job-late", ts)]).await;
        assert_eq!(status, StatusCode::ACCEPTED, "ts {ts}");
        assert_eq!(body["refused"], json!([]), "ts {ts}");
        assert_eq!(recorded_kinds(&events), ["task_submitted"], "ts {ts}");
    }
}

#[tokio::test]
async fn a_claim_fails_closed_when_batch_cannot_answer() {
    // Batch down or the deploy's grant missing: a 503 the plugin retries, never an unchecked
    // append (and never a 403, which the plugin would take as a bad token and drop). A job id
    // Batch refuses as malformed (its `ClientException`, which the provider reads as
    // `AwsError::Invalid`: `aws::provider::errors::classify_invalid`) is the plugin's fault.
    for (err, want) in [
        (AwsError::Unavailable("throttled".into()), StatusCode::SERVICE_UNAVAILABLE),
        (AwsError::Forbidden("no batch:DescribeJobs".into()), StatusCode::SERVICE_UNAVAILABLE),
        (AwsError::Unsupported("no batch".into()), StatusCode::SERVICE_UNAVAILABLE),
        (AwsError::Invalid("bad job id".into()), StatusCode::BAD_REQUEST),
    ] {
        let cloud = batch_cloud();
        *cloud.fail_describe_jobs.lock().unwrap() = Some(err);
        let (state, events, cap) = state_with_cloud(MemStore::new(), cloud);
        let status = post_as(state, &cap, "run-1", vec![claim("run-1", "job-1", 3.0)]).await;
        assert_eq!(status, want);
        assert!(events.events.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn a_claim_the_log_already_holds_for_the_run_is_not_described_again() {
    // A plugin retry or the coordinator's end-of-run replay re-sends claims already accepted; they
    // land as duplicates without asking Batch again (whose retention may have dropped the job).
    let cloud = batch_cloud();
    let (state, _events, cap) = state_with_cloud(MemStore::new(), cloud.clone());
    let app = api::router(state);
    let token = cap.mint_for(&biffix_core::ids::RunId::new("run-1"));
    let first = app.clone().oneshot(post(Some(&token), &batch("run-1"))).await.unwrap();
    assert_eq!(first.status(), StatusCode::ACCEPTED);
    *cloud.fail_describe_jobs.lock().unwrap() = Some(AwsError::Unavailable("gone".into()));
    let again = app.oneshot(post(Some(&token), &batch("run-1"))).await.unwrap();
    assert_eq!(again.status(), StatusCode::ACCEPTED);
    assert_eq!(body_json(again).await["duplicates"], 3);
}

#[tokio::test]
async fn a_mixed_batch_is_rejected_whole() {
    // The foreign event is LAST: a check of only the first event's run would let it through.
    let (state, events, cap) = state();
    let mut body = batch("run-1");
    body["events"]
        .as_array_mut()
        .unwrap()
        .push(json!({"schema_version": 1, "ts": 4.0, "run_id": "run-b", "kind": "run_started"}));
    let token = cap.mint_for(&biffix_core::ids::RunId::new("run-1"));
    let res = api::router(state).oneshot(post(Some(&token), &body)).await.unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
    assert!(events.events.lock().unwrap().is_empty());
}

#[tokio::test]
async fn events_are_recorded_under_the_run_s_engine() {
    // The engine wire shape is engine-agnostic, so the source comes from the run's registered
    // engine: a Nextflow run's events are Nextflow's, and a run with no known engine falls back to
    // Snakemake.
    use biffix_core::source::EventSource;
    for (engine, want) in [
        (Some("nextflow"), EventSource::Nextflow),
        (Some("snakemake"), EventSource::Snakemake),
        (None, EventSource::Snakemake),
    ] {
        let store = MemStore::new();
        let mut run = testing::run_detail("run-1", "running");
        run.run.engine = engine.map(String::from);
        store.insert(run);
        let (state, events, cap) = state_with(store);
        let token = cap.mint_for(&biffix_core::ids::RunId::new("run-1"));
        let res = api::router(state).oneshot(post(Some(&token), &batch("run-1"))).await.unwrap();
        assert_eq!(res.status(), StatusCode::ACCEPTED);
        let recorded = events.events.lock().unwrap();
        assert_eq!(recorded.len(), 3);
        assert!(
            recorded.iter().all(|e| e.source == want),
            "{engine:?}: {:?}",
            recorded.iter().map(|e| &e.source).collect::<Vec<_>>()
        );
    }
}

#[tokio::test]
async fn a_large_batch_fits_under_the_explicit_body_limit() {
    // axum's default 2 MiB would refuse a legitimate flush of big events (a gather task's
    // task_created lists every upstream key); the route sets its own limit.
    let (app, events, cap) = state();
    let blob = "x".repeat(3 * 1024 * 1024);
    let status = post_as(
        app,
        &cap,
        "run-1",
        vec![json!({"schema_version": 1, "ts": 1.0, "run_id": "run-1",
                     "kind": "engine_big_fact", "blob": blob})],
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(events.events.lock().unwrap().len(), 1);

    // Past the limit, the body is refused before it is parsed.
    let (app, events, cap) = state();
    let blob = "x".repeat(biffix_core::ingest_limits::MAX_BODY_BYTES);
    let status = post_as(
        app,
        &cap,
        "run-1",
        vec![json!({"schema_version": 1, "ts": 1.0, "run_id": "run-1",
                     "kind": "engine_big_fact", "blob": blob})],
    )
    .await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    assert!(events.events.lock().unwrap().is_empty());
}

/// A response's JSON body ([`testing::response_json`]).
async fn body_json(res: axum::response::Response) -> Value {
    testing::response_json(res).await.1
}

#[tokio::test]
async fn a_body_is_read_only_once_its_token_verifies() {
    let (state, events, cap) = state();
    let raw = |token: Option<&str>, content_type: &str, body: &str| {
        let mut b =
            Request::builder().method("POST").uri("/events").header("content-type", content_type);
        if let Some(t) = token {
            b = b.header("authorization", format!("Bearer {t}"));
        }
        b.body(Body::from(body.to_string())).unwrap()
    };
    // No token: a 401 whatever the body, never a parse error telling the caller about it.
    for (content_type, body) in [
        ("application/json", "{not json"),
        ("application/json", r#"{"events": 7}"#),
        ("text/plain", "hello"),
    ] {
        let res = api::router(state.clone()).oneshot(raw(None, content_type, body)).await.unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED, "{body}");
    }
    // With a good token, a malformed body reads as the JSON extractor reads it.
    let token = cap.mint_for(&biffix_core::ids::RunId::new("run-1"));
    for (content_type, body, status) in [
        ("application/json", "{not json", StatusCode::BAD_REQUEST),
        ("application/json", r#"{"events": 7}"#, StatusCode::UNPROCESSABLE_ENTITY),
        ("text/plain", "hello", StatusCode::UNSUPPORTED_MEDIA_TYPE),
    ] {
        let res = api::router(state.clone())
            .oneshot(raw(Some(&token), content_type, body))
            .await
            .unwrap();
        assert_eq!(res.status(), status, "{body}");
    }
    assert!(events.events.lock().unwrap().is_empty());
}

/// An event sink that refuses every append as a value it cannot store.
struct RefusingSink;

#[async_trait::async_trait]
impl biffix_backend::store::EventSink for RefusingSink {
    async fn record_all(
        &self,
        _evs: &[biffix_core::event::RunEvent],
    ) -> Result<usize, biffix_backend::store::StoreError> {
        Err(biffix_backend::store::StoreError::Rejected(
            "invalid byte sequence for encoding \"UTF8\": 0x00".into(),
        ))
    }

    async fn job_claims(
        &self,
        _job_ids: &[biffix_core::ids::BatchJobId],
    ) -> Result<Vec<(String, biffix_core::ids::RunId)>, biffix_backend::store::StoreError> {
        Ok(Vec::new())
    }
}

#[tokio::test]
async fn a_value_the_log_refuses_is_a_400_the_plugin_isolates_not_a_retry() {
    // A 5xx is retried and then drops the whole batch; a 400 makes the plugin halve it until the
    // one event the log cannot store is alone.
    let (state, _events, cap) = state_with(seeded_store("run-1", "running"));
    let state = TestState::from(state).with_events(Arc::new(RefusingSink)).build();
    let token = cap.mint_for(&biffix_core::ids::RunId::new("run-1"));
    let res = api::router(state)
        .oneshot(post(
            Some(&token),
            &json!({"events": [{"schema_version": 1, "ts": 1.0, "run_id": "run-1",
                                "kind": "run_started"}]}),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        body_json(res).await,
        "append failed: the store refused a value: invalid byte sequence for encoding \"UTF8\": 0x00"
    );
}

/// An event sink whose appends, or job-claim reads, fail (the log is unavailable).
struct DownSink {
    inner: Arc<MemEventSink>,
    appends: bool,
    claims: bool,
}

#[async_trait::async_trait]
impl biffix_backend::store::EventSink for DownSink {
    async fn record_all(
        &self,
        evs: &[biffix_core::event::RunEvent],
    ) -> Result<usize, biffix_backend::store::StoreError> {
        if self.appends {
            return Err(biffix_backend::store::StoreError::Unavailable("down".into()));
        }
        self.inner.record_all(evs).await
    }

    async fn job_claims(
        &self,
        job_ids: &[biffix_core::ids::BatchJobId],
    ) -> Result<Vec<(String, biffix_core::ids::RunId)>, biffix_backend::store::StoreError> {
        if self.claims {
            return Err(biffix_backend::store::StoreError::Unavailable("down".into()));
        }
        self.inner.job_claims(job_ids).await
    }
}

#[tokio::test]
async fn a_log_that_cannot_answer_is_a_503_the_plugin_retries_with_nothing_appended() {
    use std::sync::atomic::Ordering::SeqCst;
    let token_for = |cap: &CapabilityVerifier| cap.mint_for(&biffix_core::ids::RunId::new("run-1"));
    // The run's state (the revocation check) or its engine cannot be read.
    for flag in ["run_state", "run_engine"] {
        let store = seeded_store("run-1", "running");
        match flag {
            "run_state" => store.fail_run_state.store(true, SeqCst),
            _ => store.fail_run_engine.store(true, SeqCst),
        }
        let (state, events, cap) = state_with(store);
        let res = api::router(state)
            .oneshot(post(Some(&token_for(&cap)), &batch("run-1")))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE, "{flag}");
        assert!(events.events.lock().unwrap().is_empty(), "{flag}");
    }
    // The job-claim read, or the append itself, fails.
    for (appends, claims) in [(false, true), (true, false)] {
        let (state, events, cap) = state_with(seeded_store("run-1", "running"));
        let state = TestState::from(state)
            .with_events(Arc::new(DownSink { inner: events.clone(), appends, claims }))
            .build();
        let res = api::router(state)
            .oneshot(post(Some(&token_for(&cap)), &batch("run-1")))
            .await
            .unwrap();
        assert_eq!(
            res.status(),
            StatusCode::SERVICE_UNAVAILABLE,
            "appends {appends} claims {claims}"
        );
        assert!(events.events.lock().unwrap().is_empty());
    }
}
