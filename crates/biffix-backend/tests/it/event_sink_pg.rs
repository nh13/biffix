//! `PgEventSink` against the real schema: `record_all` (the control plane's own appends, and the
//! engine ingest route's) and `job_claims` (the lookup `POST /events` uses to refuse a claim on a
//! Batch job another run already holds). `#[ignore]`d; needs `--features pg` (or
//! `--all-features`) and the Postgres harness:
//!
//! ```sh
//! docker compose -f dev/docker-compose.yml up -d postgres      # :5433
//! BIFFIX_TEST_PG=postgres://biffix:biffix@localhost:5433/biffix \
//!   cargo test -p biffix-backend --all-features --test it -- --include-ignored event_sink_pg::
//! ```

#![cfg(feature = "aws")]

use biffix_backend::pg::event_sink::PgEventSink;
use biffix_backend::store::EventSink;
use biffix_core::event::{coordinator_submitted, run_submitted};
use biffix_core::ids::{BatchJobId, RunId};
use biffix_core::source::EventSource;
use biffix_core::time::Timestamp;
use sqlx::Row;

use crate::pg_store::migrated_pool as fresh_pool;

fn anchor_events(run: &RunId, job: &BatchJobId) -> [biffix_core::event::RunEvent; 2] {
    let ts = Timestamp::from_epoch_seconds(1.0);
    [
        run_submitted(
            run,
            ts,
            biffix_core::event::RunSubmitted::new(
                "wf",
                "sha",
                "sha256:d",
                serde_json::json!({}),
                vec![],
            ),
        ),
        coordinator_submitted(run, ts, job),
    ]
}

async fn count(pool: &sqlx::PgPool, sql: &'static str) -> i64 {
    sqlx::query(sql).fetch_one(pool).await.unwrap().get(0)
}

#[tokio::test]
#[ignore = "requires --features pg + the Postgres harness"]
async fn record_all_persists_the_anchor_and_binding_and_a_retry_appends_nothing() {
    let pool = fresh_pool("record_all_anchor_and_binding").await;
    let sink = PgEventSink::new(pool.clone());
    let run = RunId::new("run-x");
    let job = BatchJobId::new("coord-1");

    // Both land, and the count says so (the ingest route reports it as `appended`).
    let appended = sink.record_all(&anchor_events(&run, &job)).await.unwrap();
    assert_eq!(appended, 2);
    let rows = "SELECT count(*) FROM run_events WHERE run_id = 'run-x'";
    assert_eq!(count(&pool, rows).await, 2);

    // The run is projected (readable) with its coordinator.
    let row =
        sqlx::query("SELECT state, coordinator_job_id FROM run_projection WHERE run_id='run-x'")
            .fetch_one(&pool)
            .await
            .unwrap();
    let state: String = row.get("state");
    let coord_job: Option<String> = row.get("coordinator_job_id");
    assert_eq!(state, "submitted");
    assert_eq!(coord_job.as_deref(), Some("coord-1"));

    // A retried submit re-records the identical events: a dedup no-op, and counted as none.
    let appended = sink.record_all(&anchor_events(&run, &job)).await.unwrap();
    assert_eq!(appended, 0, "a retry appends nothing (dedup on dedup_key)");
    assert_eq!(count(&pool, rows).await, 2);
}

#[tokio::test]
#[ignore = "requires --features pg + the Postgres harness"]
async fn record_all_is_atomic_a_batch_that_fails_part_way_leaves_nothing() {
    // The anchor and its binding land together or not at all: when a later event of the batch
    // fails at its insert (a NUL byte, which Postgres text refuses), the earlier ones already
    // inserted in the transaction are rolled back with it, and nothing is folded.
    let pool = fresh_pool("record_all_is_atomic").await;
    let sink = PgEventSink::new(pool.clone());
    let run = RunId::new("run-x");
    let [anchor, binding] = anchor_events(&run, &BatchJobId::new("coord-1"));
    let poisoned = biffix_core::event::run_cancelled(
        &RunId::new("run-\0y"),
        Timestamp::from_epoch_seconds(1.0),
    );
    let err = sink.record_all(&[anchor, binding, poisoned]).await;
    // The value is refused as the caller's (a 400 on `POST /events`, which the plugins answer by
    // isolating the event), not an outage to retry.
    assert!(matches!(err, Err(biffix_backend::store::StoreError::Rejected(_))), "{err:?}");
    assert_eq!(count(&pool, "SELECT count(*) FROM run_events").await, 0);
    assert_eq!(count(&pool, "SELECT count(*) FROM run_projection").await, 0);
}

fn claim(run: &str, job: &str, attempt: u32) -> biffix_core::event::RunEvent {
    biffix_core::translate::from_engine_value(
        EventSource::Snakemake,
        serde_json::json!({"schema_version": 1, "ts": 2.0, "run_id": run, "kind": "task_submitted",
                           "task_key": "align:sample=A", "attempt": attempt, "batch_job_id": job}),
    )
    .unwrap()
}

#[tokio::test]
#[ignore = "requires --features pg + the Postgres harness"]
async fn job_claims_reads_task_and_coordinator_bindings() {
    let pool = fresh_pool("job_claims").await;
    let sink = PgEventSink::new(pool);
    let run_a = RunId::new("run-a");
    let run_b = RunId::new("run-b");
    let appended = sink
        .record_all(&[
            claim("run-a", "job-1", 1),
            // A second claim on the same job by the same run (an attempt renumbered): its own
            // event, but one binding.
            claim("run-a", "job-1", 2),
            claim("run-b", "job-2", 1),
            // Two runs claiming one job: the conflict the ingest route refuses, so both show.
            claim("run-a", "job-3", 1),
            claim("run-b", "job-3", 1),
            coordinator_submitted(
                &run_b,
                Timestamp::from_epoch_seconds(1.0),
                &BatchJobId::new("coord-b"),
            ),
            // Not a binding: a run-level fact with no job.
            biffix_core::translate::from_engine_value(
                EventSource::Snakemake,
                serde_json::json!({"schema_version": 1, "ts": 1.0, "run_id": "run-a",
                                   "kind": "run_started"}),
            )
            .unwrap(),
        ])
        .await
        .unwrap();
    assert_eq!(appended, 7, "every event is its own row");

    let ids = |ids: &[&str]| ids.iter().map(|id| BatchJobId::new(*id)).collect::<Vec<_>>();
    let mut got = sink
        .job_claims(&ids(&["job-1", "job-2", "job-3", "coord-b", "job-unclaimed"]))
        .await
        .unwrap();
    got.sort_by(|a, b| (&a.0, a.1.as_str()).cmp(&(&b.0, b.1.as_str())));
    assert_eq!(
        got,
        vec![
            ("coord-b".to_string(), run_b.clone()),
            ("job-1".to_string(), run_a.clone()),
            ("job-2".to_string(), run_b.clone()),
            ("job-3".to_string(), run_a.clone()),
            ("job-3".to_string(), run_b.clone()),
        ]
    );
    assert!(sink.job_claims(&ids(&["job-unclaimed"])).await.unwrap().is_empty());
    assert!(sink.job_claims(&[]).await.unwrap().is_empty());
}
