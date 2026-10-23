//! The projection: a deterministic fold of the event log into the tables the API reads
//! (`run_projection`, `task_projection`, `attempt_projection`, `run_output`, `notifications`,
//! `build_projection`, `spot_interruption`), using `biffix_core::reducer`, which guarantees
//! `projection == fold`. A projection bug is fixed in the reducer and replayed
//! ([`refold_runs`]), never by patching rows.
//!
//! [`crate::log`] advances it incrementally, in the transaction of every append.

use biffix_core::event::LogSeq;
use biffix_core::event::RunEvent;
use sqlx::postgres::{PgPool, PgRow};
use sqlx::{PgConnection, Row};

use crate::error::DbError;

mod build;
mod run;
mod spot;

use build::{build_kinds, fold_build_subject, rebuild_build_projection};
use run::fold_run;
use spot::{derive_spot_interruption, rebuild_spot_interruption};

pub use run::RUN_ENDED_CATEGORY;

/// The `projection_cursors` row of the run, task and build fold: the `seq` it has folded through.
pub const RUN_TASK_CURSOR: &str = "run_task";

/// Advisory-lock key serializing concurrent projection advancers (the SQS drain batch and the
/// `POST /events` append path) so the incremental fold has a single writer. An
/// arbitrary fixed constant; it only has to be unique among biffix's advisory locks.
const PROJECTION_ADVISORY_LOCK: i64 = 0x0b1f_f180_7a5c_0001;

/// Deserialize `(seq, payload)` rows into seq-stamped [`RunEvent`]s, skipping any row whose payload
/// no longer deserializes.
///
/// Accepted tradeoff (availability over consistency): a single undeserializable row must NOT wedge
/// *all* projection advancement (a poison-pill halting the fold for every run), so the fold
/// proceeds without it. The cost is that the affected run is then persisted with a
/// silently-incomplete projection that later re-folds do not repair (each re-loads the whole set
/// and skips the same row). This is near-unreachable in a consistent deploy — a
/// `RunEvent`→JSON→`RunEvent` round-trip within one binary — so the skip is logged at `error!` (a
/// data-integrity event, not routine noise) rather than made fatal; a stored-but-unfoldable row is
/// a bug to fix, not a reason to stop tracking every other run.
///
/// A row whose `kind` is one this binary knows but whose body no longer matches that variant (e.g.
/// a required field missing) does NOT fail to deserialize: the open-enum fallback decays it into
/// [`EventData::Unknown`], which the fold ignores. That is the same silent incompleteness as a
/// skipped row, so it is counted and logged at `error!` the same way.
fn rows_to_events(rows: &[PgRow]) -> Vec<RunEvent> {
    let mut events = Vec::with_capacity(rows.len());
    let mut skipped = 0usize;
    let mut degraded = 0usize;
    for row in rows {
        let seq: i64 = row.get("seq");
        let payload: serde_json::Value = row.get("payload");
        match serde_json::from_value::<RunEvent>(payload) {
            // Stamp the log sequence (it lives in the row column, not the wire payload) so `replay`
            // orders by `(ts, seq)` intrinsically.
            Ok(mut ev) => {
                if ev.data.is_degraded_known_kind() {
                    degraded += 1;
                    tracing::warn!(seq, kind = %ev.kind().as_str(), "fold: run_events row of a known kind decayed into EventData::Unknown (its body does not match the variant); the fold ignores it");
                }
                ev.seq = Some(LogSeq(seq));
                events.push(ev);
            }
            Err(e) => {
                skipped += 1;
                tracing::warn!(seq, error = %e, "fold: skipping run_events row whose payload does not deserialize into RunEvent");
            }
        }
    }
    if skipped > 0 {
        tracing::error!(
            skipped,
            "the projection fold read an incomplete event set (undeserializable rows skipped) — \
             affected runs are persisted with a silently-incomplete projection; investigate the \
             stored payloads"
        );
    }
    if degraded > 0 {
        tracing::error!(
            degraded,
            "the projection fold read an incomplete event set (known-kind rows decayed into \
             EventData::Unknown and were ignored by the fold) — affected runs are persisted with a \
             silently-incomplete projection; investigate the stored payloads"
        );
    }
    events
}

/// The most runs one [`refold_runs`] page re-folds: a page holds the projection lock, and so every
/// ingest batch and `POST /events`, for as long as it takes.
pub const REFOLD_PAGE_MAX: i64 = 200;

/// One page of a [`refold_runs`] backfill.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RefoldPage {
    /// How many runs this page refolded.
    pub refolded: usize,
    /// The last run id refolded — pass it back as `after` for the next page. `None` when the page
    /// was empty (the backfill is done).
    pub last_run_id: Option<String>,
}

/// Re-fold up to `limit` runs (at least 1, at most [`REFOLD_PAGE_MAX`]) whose id sorts after
/// `after`, in run-id order, and persist them.
///
/// For backfilling projection tables or columns added after runs were folded (e.g.
/// `attempt_projection`): the incremental advance only refolds runs with new events, so older runs
/// never gain the new rows. Paged so each call stays well inside a Lambda timeout, and it leaves
/// the projection cursor alone — a refold is idempotent (`projection == fold`), so it never races
/// or skips the live advance, which it serializes with via the same advisory lock. It is also the
/// replay that applies a reducer fix: each refolded run's rows are made to match the fold exactly.
/// The first page (`after == None`) also rebuilds the non-run-scoped projections from the whole
/// log:
/// `spot_interruption` and `build_projection` (so a `fold_builds` fix has a replay path too).
pub async fn refold_runs(
    pool: &PgPool,
    after: Option<&str>,
    limit: i64,
) -> Result<RefoldPage, DbError> {
    let mut tx = pool.begin().await?;
    take_projection_lock(&mut tx).await?;
    let run_ids: Vec<String> = sqlx::query(
        r"SELECT DISTINCT run_id FROM run_events
             WHERE run_id IS NOT NULL AND ($1::text IS NULL OR run_id > $1)
             ORDER BY run_id
             LIMIT $2",
    )
    .bind(after)
    .bind(limit.clamp(1, REFOLD_PAGE_MAX))
    .fetch_all(&mut *tx)
    .await?
    .into_iter()
    .map(|r| r.get::<String, _>("run_id"))
    .collect();
    for run_id in &run_ids {
        fold_run(&mut tx, run_id).await?;
    }
    // The non-run-scoped projections are rebuilt once per backfill, on its first page.
    if after.is_none() {
        rebuild_spot_interruption(&mut tx).await?;
        rebuild_build_projection(&mut tx).await?;
    }
    tx.commit().await?;
    Ok(RefoldPage { refolded: run_ids.len(), last_run_id: run_ids.last().cloned() })
}

/// Acquire the single-writer advisory lock on this transaction (released at commit/rollback), so
/// the incremental fold has a single writer and appends never interleave with an advance.
pub(crate) async fn take_projection_lock(conn: &mut PgConnection) -> Result<(), DbError> {
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(PROJECTION_ADVISORY_LOCK)
        .execute(conn)
        .await?;
    Ok(())
}

/// The projection fold, run on a connection that ALREADY holds the advisory lock (via
/// [`take_projection_lock`]) and does NOT commit — the caller owns the transaction boundary. Folds
/// only the runs touched since the cursor, each from its own complete event set, then advances the
/// cursor to the folded high-water mark.
///
/// Correctness rests on the event-sourcing invariant `projection == fold(a run's events)`:
/// re-folding one run from *all* its events yields the same projection a full-log replay would
/// for that run, so touching only affected runs is exact, not an approximation.
pub(crate) async fn advance_projection_in(conn: &mut PgConnection) -> Result<(), DbError> {
    let cursor: i64 = sqlx::query("SELECT last_seq FROM projection_cursors WHERE projection = $1")
        .bind(RUN_TASK_CURSOR)
        .fetch_optional(&mut *conn)
        .await?
        .map(|r| r.get::<i64, _>("last_seq"))
        .unwrap_or(0);

    let max_seq: Option<i64> = sqlx::query("SELECT MAX(seq) AS m FROM run_events")
        .fetch_one(&mut *conn)
        .await?
        .get::<Option<i64>, _>("m");
    let Some(max_seq) = max_seq.filter(|m| *m > cursor) else {
        // Nothing new past the cursor.
        return Ok(());
    };

    // Runs affected since the cursor: a run named directly by a new event, or one whose binding
    // ties it to a batch_job_id that a new (run_id-less AWS) event carries. Both arms are needed
    // because AWS lifecycle events land with a batch_job_id but no run_id (attributed via the
    // binding). They are `UNION` arms rather than one `seq > $1 OR batch_job_id IN (…)` predicate,
    // which Postgres can only evaluate by scanning every run-bound row of the log; as arms, the
    // first is a primary-key range scan past the cursor and the second probes
    // `run_events_batch_lifecycle` once per new job id. This runs on every append while holding the
    // projection lock.
    let affected: Vec<String> = sqlx::query(
        r"SELECT run_id FROM run_events WHERE seq > $1 AND run_id IS NOT NULL
           UNION
           SELECT b.run_id FROM run_events b
            WHERE b.run_id IS NOT NULL
              AND b.batch_job_id IN (
                      SELECT n.batch_job_id FROM run_events n
                       WHERE n.seq > $1 AND n.batch_job_id IS NOT NULL
                  )",
    )
    .bind(cursor)
    .fetch_all(&mut *conn)
    .await?
    .into_iter()
    .map(|r| r.get::<String, _>("run_id"))
    .collect();

    for run_id in &affected {
        fold_run(&mut *conn, run_id).await?;
    }

    // Build subjects affected since the cursor (build events carry a subject, not a run).
    let subjects: Vec<String> = sqlx::query(
        r"SELECT DISTINCT payload->>'subject' AS subject FROM run_events
             WHERE seq > $1 AND kind = ANY($2) AND payload->>'subject' IS NOT NULL",
    )
    .bind(cursor)
    .bind(build_kinds())
    .fetch_all(&mut *conn)
    .await?
    .into_iter()
    .map(|r| r.get::<String, _>("subject"))
    .collect();
    for subject in &subjects {
        fold_build_subject(&mut *conn, subject).await?;
    }

    derive_spot_interruption(&mut *conn, cursor, max_seq).await?;

    // Advance the cursor to the high-water mark we folded through.
    sqlx::query(
        r"INSERT INTO projection_cursors (projection, last_seq) VALUES ($2, $1)
           ON CONFLICT (projection) DO UPDATE SET last_seq = EXCLUDED.last_seq",
    )
    .bind(max_seq)
    .bind(RUN_TASK_CURSOR)
    .execute(&mut *conn)
    .await?;
    Ok(())
}
