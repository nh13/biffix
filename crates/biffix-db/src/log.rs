//! The event log's writer: the ONE append path (`run_events`), which advances the projection in the
//! same transaction.
//!
//! Every producer's events land here: the SQS drain (EventBridge facts), the engine plugins' events
//! (`POST /events`) and the backend's control-plane events (the submit anchor, cancel, …). An append
//! is idempotent on `dedup_key`, so a redelivery is a no-op.

use biffix_core::event::RunEvent;
use biffix_core::eventbridge::decode::HostFact;
use sqlx::postgres::PgPool;

use crate::error::DbError;
use crate::hosts::record_host;
use crate::projection::{advance_projection_in, take_projection_lock};

/// Append one RunEvent. Idempotent on `dedup_key` — a re-delivery is a no-op.
/// Returns whether a new row was inserted.
///
/// Private on purpose: it takes no lock, and an append committed outside the projection lock can
/// land a lower `seq` after an advance has already folded past it, stranding that event (see
/// [`append_and_advance`]). Every caller goes through [`ingest_batch`], which holds the lock across
/// the appends and the fold. Generic over the executor so it runs inside that transaction.
async fn append<'e, E>(executor: E, ev: &RunEvent) -> Result<bool, DbError>
where
    E: sqlx::PgExecutor<'e>,
{
    let payload = serde_json::to_value(ev)?;
    // Persist the canonical snake_case `kind`/`source` (the contract vocabulary) via the enums' own
    // `as_str` — the single source of truth — rather than Rust's `Debug` form or a JSON round-trip.
    // Otherwise queries filtering `kind` must match `RunSubmitted`/`Unknown("x")` instead of
    // `run_submitted`, diverging from the wire contract.
    let kind = ev.kind();
    let kind = kind.as_str();
    let source = ev.source.as_str();
    let res = sqlx::query(
        r"
        INSERT INTO run_events
            (dedup_key, ts_micros, source, kind, run_id, task_key, batch_job_id, schema_version, payload)
        VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)
        ON CONFLICT (dedup_key) DO NOTHING
        ",
    )
    .bind(&ev.dedup_key)
    .bind(ev.ts.as_micros())
    .bind(source)
    .bind(kind)
    .bind(ev.run_id.as_ref().map(biffix_core::ids::RunId::as_str))
    .bind(ev.task_key.as_ref().map(biffix_core::ids::TaskKey::as_str))
    .bind(ev.batch_job_id.as_ref().map(biffix_core::ids::BatchJobId::as_str))
    .bind(ev.schema_version as i32)
    .bind(payload)
    .execute(executor)
    .await?;
    Ok(res.rows_affected() > 0)
}

/// Append `events` idempotently AND advance the projection in ONE transaction that holds the
/// single-writer advisory lock from before the first INSERT through commit. Returns how many events
/// were newly inserted (a redelivery is a no-op). With no events it only advances the projection
/// (folds whatever is past the cursor).
///
/// This is the SOLE append path — the SQS drain and the backend `POST /events` / submit
/// anchor both route through it. Holding the lock across the appends' commit makes seq-allocation
/// order equal commit order, so the fold's `MAX(seq)` high-water mark can never leap a
/// still-in-flight lower seq. Without it (append and advance in separate transactions), under
/// READ COMMITTED a concurrent advancer could read a committed higher seq while a lower one is
/// still uncommitted, fold past it, and permanently strand a run whose last event was that skipped
/// seq (`seq > cursor` never revisits it, and no reconcile backstop catches an already-logged
/// terminal).
pub async fn append_and_advance(pool: &PgPool, events: &[RunEvent]) -> Result<usize, DbError> {
    ingest_batch(pool, events, &[]).await
}

/// [`append_and_advance`] plus recording `hosts` into `host_registry`, all in the one transaction,
/// so an SQS batch mixing both kinds of message commits (or retries) as a unit. Host rows are not
/// part of the event log or the fold; they are upserted order-independently (see
/// `hosts::record_host`).
pub async fn ingest_batch(
    pool: &PgPool,
    events: &[RunEvent],
    hosts: &[HostFact],
) -> Result<usize, DbError> {
    let mut tx = pool.begin().await?;
    take_projection_lock(&mut tx).await?;
    for host in hosts {
        record_host(&mut tx, host).await?;
    }
    let mut appended = 0usize;
    for ev in events {
        if append(&mut *tx, ev).await? {
            appended += 1;
        }
    }
    advance_projection_in(&mut tx).await?;
    tx.commit().await?;
    Ok(appended)
}
