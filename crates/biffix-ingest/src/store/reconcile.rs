//! The DescribeJobs backstop's queries: stale attempts and coordinators to describe; jobs that
//! finished in the last 7 days whose log holds no execution end, fewer execution ends than
//! starts, or no Batch start of the job, or that a describe left unmarked; what each describe
//! answered (`reconcile_describe`); and the reaper's ended runs and live children.

use std::collections::HashMap;

use biffix_core::event::{Kind, COORDINATOR_TERMINAL_KINDS, TASK_TERMINAL_KINDS};
use biffix_core::source::EventSource;
use sqlx::postgres::PgPool;
// `AssertSqlSafe` marks the queries whose text is assembled with `format!`: every piece is a code
// constant (column lists, `EventSource`/kind names, bind placeholders), never a runtime value.
use sqlx::AssertSqlSafe;

use biffix_db::rules::{
    batch_fact_predicate, batch_job_known_fact_kinds, batch_job_start_kinds,
    batch_job_terminal_kinds, ended_run_states, kind_strings,
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

/// One stale job: a task attempt (`task_submitted` binding) or a coordinator
/// (`coordinator_submitted` binding) with no authoritative AWS terminal event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StaleAttempt {
    pub batch_job_id: String,
    pub last_observed_micros: i64,
    /// The dedup keys of the job's AWS Batch starts (either shape) and execution events the log
    /// already holds ([`known_fact_keys_of`]).
    pub known_fact_keys: Vec<String>,
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
/// **Known facts.** Each candidate carries the dedup keys of the AWS Batch starts and execution
/// events the log already holds for the job ([`known_fact_keys_of`]), so the backstop enqueues a
/// start or an execution event DescribeJobs reports only while the log lacks that fact, rather
/// than on every rotation of a job that runs for hours, and still backfills what is new (a Batch
/// retry's start and each later execution have their own keys) when only the earlier ones reached
/// the log.
///
/// **Bound.** Only jobs bound at or after `bound_since_micros` are candidates: the query starts
/// from those bindings (`run_events_job_binding`) and probes each job's own facts
/// (`run_events_batch_lifecycle`), so a poll costs what that window holds, not the whole log. The
/// probe reads only columns that index holds, so it can stay an index-only scan; the known fact
/// keys (`dedup_key` is not in it) come from a second query over the returned candidates alone.
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
        terminal = batch_fact_predicate(Some("e"), "$4"),
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
    let ids: Vec<String> = rows.iter().map(|(id, _)| id.clone()).collect();
    let mut known_fact_keys = known_fact_keys_of(pool, &ids).await?;
    Ok(rows
        .into_iter()
        .map(|(batch_job_id, last_observed_micros)| StaleAttempt {
            known_fact_keys: known_fact_keys.remove(&batch_job_id).unwrap_or_default(),
            batch_job_id,
            last_observed_micros,
        })
        .collect())
}

/// The dedup keys of the AWS Batch starts (of either shape) and execution events the log holds for
/// each of `ids` ([`batch_job_known_fact_kinds`]), sorted per job; a job with none has no entry.
/// One query, restricted to those kinds, over the `run_events_batch_lifecycle` index (its
/// `batch_job_id`, `source` and `kind`), so only those rows themselves are read from the table.
/// The keys are sorted here rather than by the database, whose text order follows its collation.
pub async fn known_fact_keys_of(
    pool: &PgPool,
    ids: &[String],
) -> Result<HashMap<String, Vec<String>>, DbError> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let sql = format!(
        r"
        SELECT e.batch_job_id, e.dedup_key
          FROM run_events e
         WHERE e.batch_job_id = ANY($1::text[]) AND {known_fact}
        ",
        known_fact = batch_fact_predicate(Some("e"), "$2"),
    );
    let rows: Vec<(String, String)> = sqlx::query_as(AssertSqlSafe(sql))
        .bind(ids)
        .bind(batch_job_known_fact_kinds())
        .fetch_all(pool)
        .await?;
    let mut keys: HashMap<String, Vec<String>> = HashMap::new();
    for (batch_job_id, dedup_key) in rows {
        keys.entry(batch_job_id).or_default().push(dedup_key);
    }
    for job_keys in keys.values_mut() {
        job_keys.sort_unstable();
    }
    Ok(keys)
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

/// One finished job whose executions or start the log lacks (see [`execution_candidates`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecutionCandidate {
    pub batch_job_id: String,
    /// Whether the control plane bound the job as a run's coordinator (`coordinator_submitted`),
    /// so a start backfilled for it is coordinator-shaped whatever queue it ran on, as the fold
    /// reads it.
    pub bound_coordinator: bool,
    /// The dedup keys of the job's AWS Batch starts and execution events the log already holds
    /// ([`known_fact_keys_of`]), so only the facts it lacks are enqueued.
    pub known_fact_keys: Vec<String>,
}

/// The finished jobs, tasks and coordinators alike, whose record looks incomplete in the log:
/// bound to a run, with an authoritative terminal of either shape ([`batch_job_terminal_kinds`])
/// logged at or after `finished_since_micros` (Batch keeps a job for a while after it finishes,
/// however long it ran), and any of
/// - no execution end at all,
/// - fewer execution ends than execution starts (an execution that ran has no recorded end),
/// - fewer AWS Batch starts of the job ([`batch_job_start_kinds`], of either shape) than
///   execution ends: each execution that started logs its own start of the job, so its terminal
///   arrived, perhaps carrying every execution's end, while a start was lost (the first one too:
///   the attempt's span starts at the job's first start), or
/// - an earlier describe that did not mark it ([`touch_described`]): a describe whose events were
///   not all accepted, or the stale backstop's while it ran. The counts above cannot catch every
///   missing fact (an execution that ended without starting offsets a start that was rejected),
///   so a job asked about without being marked is asked again until it is.
///
/// At most `limit` of them. Only AWS Batch's own events count, in all the tests, as in the fold
/// and in [`known_fact_keys_of`]: an engine's start is not Batch's.
///
/// The second and third tests count rather than pairing each start with its end, so that the
/// scans stay index-only: an execution that ended without starting (lost while its container
/// started) can offset one whose end is missing, and makes the third test ask about its job once.
/// The offset needs an end missing from a job whose Batch terminal is logged, which the paths that
/// write a terminal do not leave: the live terminal's record carries every execution end its
/// `attempts[]` lists and is appended whole, and the backstop holds a terminal back while any of
/// its job's execution events is unsent. What is left is not
/// repaired: a terminal whose `attempts[]` lacked an entry, and an entry without a task ARN,
/// which is never recorded at all (only warned of, `drain::warn_of_unidentified_executions`).
///
/// The stale backstop stops asking about a job once its terminal is logged, so the executions a
/// lost live event carried, and a start lost while the terminal arrived, would otherwise never be
/// recovered. Each of these jobs is described until one answer is fully sent: a finished job's
/// `attempts[]` is final, so one answer whose events were all enqueued is everything Batch will
/// ever say, and
/// [`mark_executions_described`] then takes the job out of this query for good. That is also what
/// bounds a job that never got an execution or a start (cancelled before it reached a host): it
/// has nothing to wait for, and is asked about once rather than on every poll of its window.
///
/// **Cost.** The query starts from the Batch terminals logged in the window
/// (`run_events_batch_terminal`, read for `batch_job_id` alone) and probes each job's own rows in
/// `run_events_batch_lifecycle`, as [`fetch_stale`] does. Everything the probe reads is in that
/// index (`ts_micros`, `source`, `kind`), so both scans can be index-only: the tests count kinds
/// rather than reading `dedup_key`, which would cost a heap read per job, and whether the job is
/// bound, and as a coordinator, is read there too (its `task_submitted` or `coordinator_submitted`
/// binding carries the job id). The known fact keys come from a second query over the returned
/// jobs alone.
///
/// **Order.** Least recently described first (a never-described job first), then, among jobs
/// described equally recently, the newest terminal first. So a job whose describe or send failed
/// ([`touch_described`]) rotates behind the ones not yet tried instead of holding a slot; but a
/// job the stale path described while it ran also waits behind every never-described job of a
/// backlog (the first deploy's is every job of the window), up to `limit` a tick.
pub async fn execution_candidates(
    pool: &PgPool,
    finished_since_micros: i64,
    limit: i64,
) -> Result<Vec<ExecutionCandidate>, DbError> {
    // The terminal kinds and source are spelled into the outer scan's text (code constants)
    // rather than bound, so the planner can match `run_events_batch_terminal`'s predicate in a
    // generic plan too; the migration names the same kinds.
    let sql = format!(
        r"
        SELECT c.batch_job_id, f.coordinator
          FROM (SELECT DISTINCT t.batch_job_id FROM run_events t
                 WHERE t.source = '{aws_batch_source}' AND t.kind IN ({terminal_kinds})
                   AND t.batch_job_id IS NOT NULL AND t.ts_micros >= $1) c
          CROSS JOIN LATERAL (
                SELECT COALESCE(BOOL_OR({terminal}), false) AS job_ended,
                       COALESCE(BOOL_OR(e.kind IN ('{task_submitted}', '{coordinator_submitted}')),
                                false) AS bound,
                       COALESCE(BOOL_OR(e.kind = '{coordinator_submitted}'), false) AS coordinator,
                       COUNT(*) FILTER (WHERE {aws_batch} AND e.kind = '{execution_ended}')
                           AS ended,
                       COUNT(*) FILTER (WHERE {aws_batch} AND e.kind = '{execution_started}')
                           AS started,
                       COUNT(*) FILTER (WHERE {job_start}) AS job_starts,
                       MAX(e.ts_micros) FILTER (WHERE {terminal}) AS ended_at
                  FROM run_events e WHERE e.batch_job_id = c.batch_job_id) f
          LEFT JOIN reconcile_describe d ON d.batch_job_id = c.batch_job_id
         WHERE f.job_ended AND f.bound
           AND (f.ended = 0 OR f.ended < f.started OR f.job_starts < f.ended
                OR d.last_described_micros IS NOT NULL)
           AND d.executions_described_micros IS NULL
         ORDER BY COALESCE(d.last_described_micros, 0), f.ended_at DESC, c.batch_job_id
         LIMIT $2
        ",
        task_submitted = Kind::TaskSubmitted.as_str(),
        coordinator_submitted = Kind::CoordinatorSubmitted.as_str(),
        execution_ended = Kind::BatchExecutionEnded.as_str(),
        execution_started = Kind::BatchExecutionStarted.as_str(),
        aws_batch = format_args!("e.source = '{}'", EventSource::AwsBatch.as_str()),
        aws_batch_source = EventSource::AwsBatch.as_str(),
        terminal_kinds = batch_terminal_kind_list(),
        terminal = batch_fact_predicate(Some("e"), "$3"),
        job_start = batch_fact_predicate(Some("e"), "$4"),
    );
    let rows: Vec<(String, bool)> = sqlx::query_as(AssertSqlSafe(sql))
        .bind(finished_since_micros)
        .bind(limit.max(0))
        .bind(batch_job_terminal_kinds())
        .bind(batch_job_start_kinds())
        .fetch_all(pool)
        .await?;
    let ids: Vec<String> = rows.iter().map(|(id, _)| id.clone()).collect();
    let mut known_fact_keys = known_fact_keys_of(pool, &ids).await?;
    Ok(rows
        .into_iter()
        .map(|(batch_job_id, bound_coordinator)| ExecutionCandidate {
            known_fact_keys: known_fact_keys.remove(&batch_job_id).unwrap_or_default(),
            batch_job_id,
            bound_coordinator,
        })
        .collect())
}

/// The Batch terminal kinds of either shape, as the SQL list `'a', 'b', …`: the kinds
/// `run_events_batch_terminal` (migration 0006) indexes, which a test ties to these constants.
fn batch_terminal_kind_list() -> String {
    [TASK_TERMINAL_KINDS, COORDINATOR_TERMINAL_KINDS]
        .concat()
        .iter()
        .map(|kind| format!("'{}'", kind.as_str()))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Record at `now_micros` that each of `ids` has had its one describe as a finished job, so
/// [`execution_candidates`] never returns it again. An omission streak is left as it was.
///
/// Only the finished-job backfill calls this, and only for a job whose described events were all
/// enqueued or that Batch no longer returns. The stale backstop and the reaper describe jobs that
/// have not finished, whose `attempts[]` can still grow, so their answers prove nothing final and
/// [`record_describe_results`] never sets the mark.
pub async fn mark_executions_described(
    pool: &PgPool,
    ids: &[String],
    now_micros: i64,
) -> Result<(), DbError> {
    if ids.is_empty() {
        return Ok(());
    }
    sqlx::query(
        r"INSERT INTO reconcile_describe
               (batch_job_id, last_described_micros, executions_described_micros)
           SELECT id, $2, $2 FROM UNNEST($1::text[]) AS id
           ON CONFLICT (batch_job_id) DO UPDATE
              SET last_described_micros = EXCLUDED.last_described_micros,
                  executions_described_micros = EXCLUDED.executions_described_micros",
    )
    .bind(ids)
    .bind(now_micros)
    .execute(pool)
    .await?;
    Ok(())
}

/// Record one DescribeJobs answer at `now_micros`: every id in `present` was returned (its omission
/// streak, if any, ends), every id in `missing` was asked about but left out (its streak starts now
/// unless it already had one). Returns, for each `missing` id, since when Batch has omitted it.
///
/// Also forgets rows no candidate query needs any more: a job last described before
/// `forget_before_micros` that Batch still knew. Rotation re-describes every live candidate far
/// sooner, so such a row belongs to a job whose terminal has since landed. Rows of omitted jobs are
/// kept, so a given-up job stays given up.
///
/// A forgotten row may carry a finished job's mark ([`mark_executions_described`]), which this
/// function never sets and never clears. Dropping it cannot bring the job back as an
/// [`execution_candidates`] candidate, provided the caller forgets a row only well after that
/// query's binding window has passed the job (the tick pins its two durations to that): by then
/// the job is no longer asked about, marked or not.
pub async fn record_describe_results(
    pool: &PgPool,
    present: &[String],
    missing: &[String],
    now_micros: i64,
    forget_before_micros: i64,
) -> Result<HashMap<String, i64>, DbError> {
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
        HashMap::new()
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

/// Move `ids` to the back of the rotation (the stale candidates' and [`execution_candidates`]')
/// without recording an answer: their DescribeJobs call failed, or events described for them are
/// about to be enqueued (so one SQS rejects leaves its job in the rotation until a describe is
/// fully sent and marks it), and a request that fails every time (say, for one malformed id) must
/// not pin the same page of candidates at the head of every poll. An omission streak is left as it was,
/// and so is a finished job's mark.
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
        terminal = batch_fact_predicate(Some("t"), "$3"),
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

/// One still-live child job of a run (see [`live_children_of_run`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LiveChild {
    pub batch_job_id: String,
    /// The dedup keys of the job's AWS Batch starts and execution events the log already holds
    /// ([`known_fact_keys_of`]), so the reaper enqueues a described start or execution event only
    /// while the log lacks it, as the stale path does.
    pub known_fact_keys: Vec<String>,
}

/// One keyset page of the still-live child jobs of a run: attempts with a `task_submitted` binding
/// but no authoritative `aws-batch` terminal, each with the keys of the starts and execution
/// events the log holds for it. These are the jobs to terminate when the run's coordinator dies,
/// so a crashed coordinator does not strand — and keep billing — orphaned child jobs.
/// Terminality is keyed on `source = 'aws-batch'` for the same reason as [`stale_attempts`]: an
/// engine-reported terminal is not Batch's authoritative exit.
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
) -> Result<Vec<LiveChild>, DbError> {
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
        terminal = batch_fact_predicate(Some("t"), "$4"),
    );
    let ids: Vec<String> = sqlx::query_scalar(AssertSqlSafe(sql))
        .bind(run_id)
        .bind(limit.max(0))
        // job ids are non-empty, so `> ''` matches all on the first page.
        .bind(after.unwrap_or(""))
        .bind(batch_job_terminal_kinds())
        .bind(Kind::TaskSubmitted.as_str())
        .fetch_all(pool)
        .await?;
    let mut known_fact_keys = known_fact_keys_of(pool, &ids).await?;
    Ok(ids
        .into_iter()
        .map(|batch_job_id| LiveChild {
            known_fact_keys: known_fact_keys.remove(&batch_job_id).unwrap_or_default(),
            batch_job_id,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_terminal_index_names_the_kinds_the_backfill_scans() {
        // The planner uses the partial index only when the query names the same kinds.
        let migration = include_str!("../../../biffix-db/migrations/0006_executions.sql");
        let source = biffix_core::source::EventSource::AwsBatch.as_str();
        for wanted in [
            format!("source = '{source}'"),
            format!("kind IN ({})", super::batch_terminal_kind_list()),
        ] {
            assert!(migration.contains(&wanted), "the migration does not say {wanted}");
        }
    }
}
