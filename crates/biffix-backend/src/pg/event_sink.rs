//! Postgres [`EventSink`] (behind `feature = "aws"`): the control plane's own appends, through the
//! single event-log writer (`biffix_db::log`), and the job-claim lookup engine ingest checks before
//! appending.

use async_trait::async_trait;
use sqlx::postgres::PgPool;

use biffix_core::event::{Kind, RunEvent};
use biffix_core::ids::{BatchJobId, RunId};

use crate::store::{EventSink, StoreError};

/// Postgres [`EventSink`]: append the control-plane's own event and re-fold the projection.
///
/// Reuses the single event-log writer (`biffix_db::log::append_and_advance`: appends idempotent
/// on `dedup_key`, then the projection fold, under one lock). The fold is a pure function of the
/// full event set and re-folds from zero, so appending the `run_submitted` anchor here and folding
/// makes the run immediately readable, and a concurrent ingest fold converges to the identical
/// state. The append's dedup key makes a re-delivery a no-op.
pub struct PgEventSink {
    pool: PgPool,
}

impl PgEventSink {
    /// A sink over `pool`.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl EventSink for PgEventSink {
    async fn record_all(&self, evs: &[RunEvent]) -> Result<usize, StoreError> {
        // Append every event AND fold in ONE lock-holding transaction so a crash can never persist
        // a subset — the submit anchor and its coordinator binding land together or not at all —
        // and holding the single-writer advisory lock across the commit keeps seq-allocation
        // order equal to commit order, so a concurrent advance can never leap one of these seqs.
        // The append is idempotent on dedup_key, so a retried submit is a no-op. One fold pass for
        // the whole batch, and the returned count lets the ingest route report
        // appended-vs-duplicate without a transaction per event.
        biffix_db::log::append_and_advance(&self.pool, evs).await.map_err(StoreError::from)
    }

    async fn job_claims(&self, job_ids: &[BatchJobId]) -> Result<Vec<(String, RunId)>, StoreError> {
        if job_ids.is_empty() {
            return Ok(Vec::new());
        }
        let ids: Vec<&str> = job_ids.iter().map(biffix_core::ids::BatchJobId::as_str).collect();
        // The bindings only: a claim is a task_submitted or coordinator_submitted with a run id.
        // Served by the partial `run_events_batch_lifecycle` index (batch_job_id leading).
        let rows: Vec<(String, String)> = sqlx::query_as(
            "SELECT DISTINCT batch_job_id, run_id FROM run_events \
             WHERE batch_job_id = ANY($1) AND run_id IS NOT NULL AND kind = ANY($2)",
        )
        .bind(&ids)
        .bind([Kind::TaskSubmitted.as_str(), Kind::CoordinatorSubmitted.as_str()])
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(|(job, run)| (job, RunId::new(run))).collect())
    }
}
