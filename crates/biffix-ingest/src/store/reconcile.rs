//! The DescribeJobs backstop's queries: stale attempts and coordinators to
//! describe, what each describe answered (`reconcile_describe`), and the reaper's ended runs and
//! live children.

use biffix_core::event::Kind;
use sqlx::postgres::PgPool;
// `AssertSqlSafe` marks the queries whose text is assembled with `format!`: every piece is a code
// constant (column lists, `EventSource`/kind names, bind placeholders), never a runtime value.
use sqlx::AssertSqlSafe;

use biffix_db::rules::{
    batch_job_terminal_kinds, batch_terminal_predicate, ended_run_states, kind_strings,
};
use biffix_db::DbError;

/// Which stale candidates one poll asks for (see [`stale_attempts`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StaleWindow {
    /// A job whose newest lifecycle fact is at or before this (epoch micros) is stale.
    pub cutoff_micros: i64,
    /// Only jobs bound at or after this (epoch micros) are candidates.
    pub bound_since_micros: i64,
    /// A job DescribeJobs has omitted since at or before this (epoch micros) is given up on.
    pub give_up_before_micros: i64,
    /// The most candidates returned.
    pub limit: i64,
}

/// One attempt that has a `task_submitted` binding but no authoritative AWS terminal event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StaleAttempt {
    pub batch_job_id: String,
    pub last_observed_micros: i64,
}

/// The stale candidates of one variant: the jobs bound by a `bound_by` event whose latest
/// `observed` fact is at or before `cutoff_micros` and that have no authoritative terminal of either
/// shape ([`batch_job_terminal_kinds`]: a coordinator on a queue the deploy does not list ends
/// task-shaped, and the fold still ends it), as [`StaleAttempt`]s, rotated and bounded by the
/// shared rule over `reconcile_describe`.
///
/// **Rotation.** Candidates come back least-recently-described first (a never-described job first),
/// then oldest observation first. A bounded page therefore walks every candidate over successive
/// polls, instead of re-describing the same oldest `limit` jobs forever: jobs that legitimately
/// run for hours, or that Batch no longer knows, can no longer starve a newer missed terminal.
///
/// **Give-up.** A job DescribeJobs has omitted since at or before `give_up_before_micros` is not
/// returned: Batch has forgotten it (it keeps a finished job about a week), so asking again cannot
/// produce its terminal. Nothing is fabricated for it; it only stops holding a candidate slot.
///
/// **Bound.** Only jobs bound at or after `bound_since_micros` are candidates: the query starts
/// from those bindings (`run_events_job_binding`) and probes each job's own facts
/// (`run_events_batch_lifecycle`), so a poll costs what that window holds, not the whole log.
async fn fetch_stale(
    pool: &PgPool,
    bound_by: Kind,
    observed: &[Kind],
    window: StaleWindow,
) -> Result<Vec<StaleAttempt>, DbError> {
    // `bound_by` is spelled into the text (a code constant) rather than bound, so the planner can
    // match the partial index's `kind IN (…)` predicate in a generic plan too.
    let sql = format!(
        r"
        SELECT c.batch_job_id, f.last_observed_micros
          FROM (
              SELECT DISTINCT b.batch_job_id FROM run_events b
               WHERE b.kind = '{bound_by}' AND b.batch_job_id IS NOT NULL AND b.ts_micros >= $5
                 AND NOT EXISTS (
                     SELECT 1 FROM reconcile_describe g
                      WHERE g.batch_job_id = b.batch_job_id AND g.missing_since_micros <= $3
                 )
          ) c
          CROSS JOIN LATERAL (
              SELECT MAX(e.ts_micros) FILTER (WHERE e.kind = ANY($6::text[]))
                         AS last_observed_micros,
                     BOOL_OR({terminal}) AS ended
                FROM run_events e
               WHERE e.batch_job_id = c.batch_job_id
          ) f
          LEFT JOIN reconcile_describe d ON d.batch_job_id = c.batch_job_id
         WHERE NOT f.ended AND f.last_observed_micros <= $1
         ORDER BY COALESCE(d.last_described_micros, 0), f.last_observed_micros, c.batch_job_id
         LIMIT $2
        ",
        bound_by = bound_by.as_str(),
        terminal = batch_terminal_predicate(Some("e"), "$4"),
    );
    let rows: Vec<(String, i64)> = sqlx::query_as(AssertSqlSafe(sql))
        .bind(window.cutoff_micros)
        .bind(window.limit.max(0))
        .bind(window.give_up_before_micros)
        .bind(batch_job_terminal_kinds())
        .bind(window.bound_since_micros)
        .bind(kind_strings(observed))
        .fetch_all(pool)
        .await?;
    Ok(rows
        .into_iter()
        .map(|(batch_job_id, last_observed_micros)| StaleAttempt {
            batch_job_id,
            last_observed_micros,
        })
        .collect())
}

/// Find attempts whose latest known non-terminal lifecycle fact is older than `cutoff_micros`,
/// rotated and bounded as [`fetch_stale`] describes.
///
/// This query intentionally keys terminality only on `source = aws-batch`: an engine-reported
/// success/failure does not replace Batch's authoritative exit fact. The binding is
/// required, so unrelated/coordinator Batch events are not treated as task Attempts.
pub async fn stale_attempts(
    pool: &PgPool,
    window: StaleWindow,
) -> Result<Vec<StaleAttempt>, DbError> {
    fetch_stale(
        pool,
        Kind::TaskSubmitted,
        &[Kind::TaskSubmitted, Kind::TaskQueued, Kind::TaskStarted],
        window,
    )
    .await
}

/// Stale COORDINATOR jobs to reconcile: a run's coordinator was bound (`coordinator_submitted`)
/// but no coordinator terminal has been observed, and the binding is older than the cutoff. Without
/// this, a coordinator terminal that never reached the log (missed EventBridge) leaves the run
/// stuck forever — `stale_attempts` only backstops child tasks (`task_submitted`), never the
/// coordinator. Rotated and bounded as [`fetch_stale`] describes.
pub async fn stale_coordinators(
    pool: &PgPool,
    window: StaleWindow,
) -> Result<Vec<StaleAttempt>, DbError> {
    fetch_stale(pool, Kind::CoordinatorSubmitted, &[Kind::CoordinatorSubmitted], window).await
}

/// Record one DescribeJobs answer at `now_micros`: every id in `present` was returned (its omission
/// streak, if any, ends), every id in `missing` was asked about but left out (its streak starts now
/// unless it already had one). Returns, for each `missing` id, since when Batch has omitted it.
///
/// Also forgets rows no stale query needs any more: a job last described before
/// `forget_before_micros` that Batch still knew. Rotation re-describes every live candidate far
/// sooner, so such a row belongs to a job whose terminal has since landed. Rows of omitted jobs are
/// kept, so a given-up job stays given up.
pub async fn record_describe_results(
    pool: &PgPool,
    present: &[String],
    missing: &[String],
    now_micros: i64,
    forget_before_micros: i64,
) -> Result<std::collections::HashMap<String, i64>, DbError> {
    let mut tx = pool.begin().await?;
    if !present.is_empty() {
        sqlx::query(
            r"INSERT INTO reconcile_describe (batch_job_id, last_described_micros)
               SELECT id, $2 FROM UNNEST($1::text[]) AS id
               ON CONFLICT (batch_job_id) DO UPDATE
                  SET last_described_micros = EXCLUDED.last_described_micros,
                      missing_since_micros = NULL",
        )
        .bind(present)
        .bind(now_micros)
        .execute(&mut *tx)
        .await?;
    }
    let missing_since = if missing.is_empty() {
        std::collections::HashMap::new()
    } else {
        sqlx::query_as::<_, (String, i64)>(
            r"INSERT INTO reconcile_describe
                   (batch_job_id, last_described_micros, missing_since_micros)
               SELECT id, $2, $2 FROM UNNEST($1::text[]) AS id
               ON CONFLICT (batch_job_id) DO UPDATE
                  SET last_described_micros = EXCLUDED.last_described_micros,
                      missing_since_micros = COALESCE(reconcile_describe.missing_since_micros,
                                                      EXCLUDED.missing_since_micros)
               RETURNING batch_job_id, missing_since_micros",
        )
        .bind(missing)
        .bind(now_micros)
        .fetch_all(&mut *tx)
        .await?
        .into_iter()
        .collect()
    };
    sqlx::query(
        "DELETE FROM reconcile_describe \
          WHERE missing_since_micros IS NULL AND last_described_micros < $1",
    )
    .bind(forget_before_micros)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(missing_since)
}

/// Move `ids` to the back of the stale rotation without recording an answer: their DescribeJobs
/// call failed, and a request Batch rejects every time (say, for one malformed id) must not pin the
/// same page of candidates at the head of every poll. An omission streak is left as it was.
pub async fn touch_described(
    pool: &PgPool,
    ids: &[String],
    now_micros: i64,
) -> Result<(), DbError> {
    if ids.is_empty() {
        return Ok(());
    }
    sqlx::query(
        r"INSERT INTO reconcile_describe (batch_job_id, last_described_micros)
           SELECT id, $2 FROM UNNEST($1::text[]) AS id
           ON CONFLICT (batch_job_id) DO UPDATE
              SET last_described_micros = EXCLUDED.last_described_micros",
    )
    .bind(ids)
    .bind(now_micros)
    .execute(pool)
    .await?;
    Ok(())
}

/// Runs that ended at or after `since_micros` but still have live child jobs (a `task_submitted`
/// binding with no authoritative `aws-batch` terminal), with their coordinator job id, oldest end
/// first. This is the reaper's general sweep: however a run ended — cancelled (its coordinator
/// terminated), or failed with an executor that does not stop its children on exit — its still-
/// running children keep billing until terminated. A `succeeded` run is included: its "live"
/// children are usually terminals the log missed, which the reaper recovers by describing each
/// child before it terminates anything (it never marks a child Batch reports as finished).
/// Bounded to a recent window so finished history is not rescanned forever; a child whose
/// terminate failed is retried every poll inside it.
pub async fn ended_runs_with_live_children(
    pool: &PgPool,
    since_micros: i64,
    limit: i64,
) -> Result<Vec<(String, String)>, DbError> {
    let sql = format!(
        r"
        SELECT r.run_id, r.coordinator_job_id
          FROM run_projection r
         WHERE r.state = ANY($4::text[])
           AND r.completed_at >= $1
           AND r.coordinator_job_id IS NOT NULL
           AND EXISTS (
               SELECT 1 FROM run_events e
                WHERE e.run_id = r.run_id
                  AND e.kind = $5
                  AND e.batch_job_id IS NOT NULL
                  AND NOT EXISTS (
                      SELECT 1 FROM run_events t
                       WHERE t.batch_job_id = e.batch_job_id AND {terminal}
                  )
           )
         ORDER BY r.completed_at, r.run_id
         LIMIT $2
        ",
        terminal = batch_terminal_predicate(Some("t"), "$3"),
    );
    Ok(sqlx::query_as(AssertSqlSafe(sql))
        .bind(since_micros)
        .bind(limit.max(0))
        .bind(batch_job_terminal_kinds())
        .bind(ended_run_states())
        .bind(Kind::TaskSubmitted.as_str())
        .fetch_all(pool)
        .await?)
}

/// The run a coordinator job belongs to, from its `coordinator_submitted` binding.
///
/// Returns `None` when the coordinator id was never bound (e.g. the binding POST was lost). Used by
/// the reap sweep: a terminated coordinator's still-live children must be found by run.
pub async fn run_id_for_coordinator(
    pool: &PgPool,
    coordinator_job_id: &str,
) -> Result<Option<String>, DbError> {
    Ok(sqlx::query_scalar(
        r"SELECT run_id FROM run_events
            WHERE batch_job_id = $1 AND kind = $2 AND run_id IS NOT NULL
            LIMIT 1",
    )
    .bind(coordinator_job_id)
    .bind(Kind::CoordinatorSubmitted.as_str())
    .fetch_optional(pool)
    .await?)
}

/// One keyset page of the still-live child jobs of a run: attempts with a `task_submitted` binding
/// but no authoritative `aws-batch` terminal. These are the jobs to terminate when the run's
/// coordinator dies so a crashed coordinator does not strand — and keep billing —
/// orphaned child jobs. Terminality is keyed on `source = 'aws-batch'` for the same reason as
/// [`stale_attempts`]: an engine-reported terminal is not Batch's authoritative exit.
///
/// **Keyset-paginated** on `batch_job_id`: pass `after = None` for the first page, then the last id
/// of the previous page. The reaper must page to exhaustion — a run with more live children than
/// one `limit`-sized page would otherwise leak the remainder, because what the reaper enqueues is
/// not folded into `run_events` synchronously, so a re-query without a keyset cursor would return
/// the very same still-"live" children and never make progress.
pub async fn live_children_of_run(
    pool: &PgPool,
    run_id: &str,
    after: Option<&str>,
    limit: i64,
) -> Result<Vec<String>, DbError> {
    let sql = format!(
        r"
        SELECT DISTINCT e.batch_job_id
          FROM run_events e
         WHERE e.run_id = $1
           AND e.batch_job_id IS NOT NULL
           AND e.batch_job_id > $3
           AND e.kind = $5
           AND NOT EXISTS (
               SELECT 1 FROM run_events t
                WHERE t.batch_job_id = e.batch_job_id AND {terminal}
           )
         ORDER BY e.batch_job_id
         LIMIT $2
        ",
        terminal = batch_terminal_predicate(Some("t"), "$4"),
    );
    Ok(sqlx::query_scalar(AssertSqlSafe(sql))
        .bind(run_id)
        .bind(limit.max(0))
        // job ids are non-empty, so `> ''` matches all on the first page.
        .bind(after.unwrap_or(""))
        .bind(batch_job_terminal_kinds())
        .bind(Kind::TaskSubmitted.as_str())
        .fetch_all(pool)
        .await?)
}
