//! The status probe's queries: the read model's lag and the runs stuck before launch, which it
//! classifies (`crate::status`), and the `status_samples` it writes.

use biffix_core::state::RunState;
use sqlx::PgPool;

use biffix_db::DbError;

use crate::status::{Health, STUCK_RUN_MAX_SECS, STUCK_RUN_SECS};

/// Seconds the oldest un-folded event has been waiting (`None` when the read model is fully caught
/// up). The one append path folds in the transaction of every append, so no committed row sits past
/// the cursor today: this guards the read model against a writer that ever appends outside that
/// transaction (or a fold that stops advancing the cursor).
pub async fn unfolded_age_secs(pool: &PgPool) -> Result<Option<i64>, DbError> {
    Ok(sqlx::query_scalar(
        "SELECT EXTRACT(EPOCH FROM (now() - MIN(received_at)))::bigint AS age \
         FROM run_events \
         WHERE seq > COALESCE((SELECT last_seq FROM projection_cursors WHERE projection = $1), 0)",
    )
    .bind(biffix_db::projection::RUN_TASK_CURSOR)
    .fetch_one(pool)
    .await?)
}

/// Recently-launched runs that have sat in `submitted` past the stale window without ever starting
/// — a live signal of Batch not placing work now (capacity, bad AMI, broken launch template).
/// Bounded to a recent window so long-dead zombie runs don't perma-degrade the signal. Measured
/// from when the run launched (`launched_at`), not when it was submitted: a run that waited for its
/// submit-time images reached Batch only at launch; a `submitted` run with no launch time (the
/// control plane had not stamped one) is measured from its submission instead.
pub async fn stuck_run_count(pool: &PgPool) -> Result<i64, DbError> {
    let now = biffix_core::clock::since_epoch().as_secs() as i64;
    // Submitted longer ago than STUCK_RUN_SECS (the "should have placed by now" cutoff) but within
    // STUCK_RUN_MAX_SECS (older = an abandoned zombie, not a live Batch signal).
    let stuck_before = (now - STUCK_RUN_SECS).saturating_mul(1_000_000);
    let recent_after = (now - STUCK_RUN_MAX_SECS).saturating_mul(1_000_000);
    Ok(sqlx::query_scalar(
        "SELECT COUNT(*) FROM run_projection \
         WHERE state = $3 \
           AND COALESCE(launched_at, submitted_at) < $1 \
           AND COALESCE(launched_at, submitted_at) >= $2",
    )
    .bind(stuck_before)
    .bind(recent_after)
    .bind(RunState::Submitted.as_str())
    .fetch_one(pool)
    .await?)
}

/// How long `status_samples` rows are kept: 10 days of slack past the 90-day view so the window is
/// never ragged.
pub const STATUS_RETENTION_DAYS: i32 = 100;

/// Write one `status_samples` row per `(component, health, detail)` at a common timestamp, each
/// promising the next sample within `next_within_secs`, then prune samples older than
/// [`STATUS_RETENTION_DAYS`].
///
/// One transaction, so every row carries the same `sampled_at`: Postgres `now()` is the
/// transaction's start time, where autocommit inserts would each get their own. A second row for a
/// component at that timestamp is dropped (`ON CONFLICT DO NOTHING`) rather than failing, and so
/// losing, the whole sample.
pub async fn record_samples(
    pool: &PgPool,
    rows: &[(&str, Health, Option<String>)],
    next_within_secs: i64,
) -> Result<(), DbError> {
    let components: Vec<&str> = rows.iter().map(|(component, _, _)| *component).collect();
    let statuses: Vec<&str> = rows.iter().map(|(_, health, _)| health.as_str()).collect();
    let details: Vec<Option<&str>> = rows.iter().map(|(_, _, detail)| detail.as_deref()).collect();
    let mut tx = pool.begin().await?;
    sqlx::query(
        "INSERT INTO status_samples (sampled_at, component, status, detail, next_within_secs) \
         SELECT now(), c, s, d, $4::bigint FROM UNNEST($1::text[], $2::text[], $3::text[]) AS t(c, s, d) \
         ON CONFLICT (component, sampled_at) DO NOTHING",
    )
    .bind(components)
    .bind(statuses)
    .bind(details)
    .bind(next_within_secs)
    .execute(&mut *tx)
    .await?;
    sqlx::query("DELETE FROM status_samples WHERE sampled_at < now() - make_interval(days => $1)")
        .bind(STATUS_RETENTION_DAYS)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}
