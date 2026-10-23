//! Shared pg test helpers: a migrated, schema-isolated pool (`biffix_db::testing`), so the pg
//! integration tests run in PARALLEL, the locked append path, and the events several suites build.

use biffix_core::event::RunEvent;
use biffix_core::ids::{BatchJobId, RunId};
use biffix_core::time::Timestamp;
use sqlx::PgPool;

/// A MIGRATED pool pinned to a private schema derived from `label` (`biffix_db::testing`, under
/// this crate's `ing` prefix, so it never collides with the backend suite's schemas). `label` must
/// be unique per test (reusing one within a run makes two tests share a schema). The schema is
/// dropped and recreated first, then the production migrations (`biffix_db::migrate::run`) are
/// applied into it.
pub(crate) async fn isolated_pool(label: &str) -> PgPool {
    let pool = biffix_db::testing::isolated_pool("ing", label).await;
    biffix_db::migrate::run(&pool).await.unwrap();
    pool
}

/// Append one event through the single writer (`append_and_advance`, which also folds it), and say
/// whether it was new. The raw append takes no projection lock, so tests go through the same locked
/// path production does. Each call is its own transaction and fold; use
/// `biffix_db::log::ingest_batch` to fold several events as one batch.
pub(crate) async fn append(pool: &PgPool, ev: &RunEvent) -> Result<bool, biffix_db::DbError> {
    Ok(biffix_db::log::append_and_advance(pool, std::slice::from_ref(ev)).await? == 1)
}

/// Log `payload` as `ev`'s row past the fold cursor without folding it, as a batch whose append
/// committed but whose fold did not would leave it. Returns its `seq`.
pub(crate) async fn log_unfolded(pool: &PgPool, ev: &RunEvent, payload: serde_json::Value) -> i64 {
    sqlx::query_scalar(
        "INSERT INTO run_events (dedup_key, ts_micros, source, kind, run_id, batch_job_id, \
         schema_version, payload) VALUES ($1, $2, $3, $4, $5, $6, 1, $7) RETURNING seq",
    )
    .bind(&ev.dedup_key)
    .bind(ev.ts.as_micros())
    .bind(ev.source.as_str())
    .bind(ev.kind().as_str())
    .bind(ev.run_id.as_ref().map(RunId::as_str))
    .bind(ev.batch_job_id.as_ref().map(BatchJobId::as_str))
    .bind(payload)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// The control plane's binding of coordinator `job` to `run` at `ts` (epoch seconds), under `dedup`.
pub(crate) fn coordinator_bound(run: &str, ts: f64, job: &str, dedup: &str) -> RunEvent {
    RunEvent {
        dedup_key: dedup.into(),
        ..biffix_core::event::coordinator_submitted(
            &RunId::new(run),
            Timestamp::from_epoch_seconds(ts),
            &BatchJobId::new(job),
        )
    }
}

/// `run` accepted at `ts` (epoch seconds) to wait for its submit-time images, with placeholder
/// fields.
pub(crate) fn preparing(run: &str, ts: f64) -> RunEvent {
    biffix_core::event::run_preparing(
        &RunId::new(run),
        Timestamp::from_epoch_seconds(ts),
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
    )
}
