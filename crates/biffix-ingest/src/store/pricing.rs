//! Cost estimates: the finished executions and jobs to price, and their recorded cost. A job priced
//! per execution ([`prices_by_execution`]) gets one `execution_cost` line per execution (or an
//! `unpriceable_execution` row); any other job one legacy job-level `attempt_cost` line (or an
//! `unpriceable_job` row).

use sqlx::postgres::PgPool;
// `AssertSqlSafe` marks the queries whose text is assembled with `format!`: every piece is a code
// constant (column lists, `EventSource`/kind names, bind placeholders), never a runtime value.
use sqlx::{AssertSqlSafe, Row};

use biffix_db::rules::{
    coordinator_window, execution_not_yet_priced, execution_to_price, not_yet_priced, priceable,
    priced_from, prices_by_execution, START_UNKNOWN,
};
use biffix_db::DbError;

pub use biffix_db::rules::JobRole;

/// A finished execution, or a finished job priced the legacy way, not yet priced, with what is
/// known about its host and run time.
#[derive(Clone, Debug, PartialEq)]
pub struct UnpricedJob {
    pub run_id: String,
    pub batch_job_id: String,
    /// The execution (its ECS task ARN) this is, priced on its own host from its own start to its
    /// own stop; `None` for a job priced the legacy way, over its span.
    pub execution_id: Option<String>,
    pub role: JobRole,
    /// The host's container-instance id (its `host_registry` key).
    pub instance_id: String,
    pub instance_type: String,
    pub availability_zone: Option<String>,
    /// The host's EC2 lifecycle (`spot`, `on_demand`), when resolved.
    pub lifecycle: Option<String>,
    /// When the host was first seen (epoch micros): while it is recent, an unresolved lifecycle
    /// may still resolve, so the pricer waits rather than guessing.
    pub host_first_seen_micros: i64,
    /// The Batch job queue the job ran on (for the queue's capacity and volume spec): an
    /// execution's own, a legacy task's from its attempt, a legacy coordinator's from its own
    /// `aws-batch` events.
    pub queue: Option<String>,
    pub started_micros: i64,
    pub finished_micros: i64,
}

/// Finished jobs of `role` priced the legacy way (not [`prices_by_execution`]) with a known host
/// type that have no cost yet, are not recorded as unpriceable, and finished at or after
/// `since_micros` (`biffix_db::rules::PRICING_WINDOW` before now), by the rules the backend's
/// "pending" count applies too ([`biffix_db::rules`]): task attempts from `attempt_projection`, and
/// run coordinators (their window and queue from their own Batch events, [`coordinator_window`]).
///
/// Oldest first, paged as [`unpriced_executions`] is.
pub async fn unpriced_jobs(
    pool: &PgPool,
    role: JobRole,
    since_micros: i64,
    after: Option<&UnpricedJob>,
    limit: i64,
) -> Result<Vec<UnpricedJob>, DbError> {
    // The role's jobs in the columns `page` reads; `$1` is the pricing window's start.
    let jobs = match role {
        JobRole::Task => format!(
            r"SELECT a.run_id, a.batch_job_id, NULL::text AS execution_id, '{role}' AS role,
                      a.instance_id, h.instance_type, h.availability_zone, h.lifecycle,
                      h.first_seen_at, a.queue, a.started_at AS started, a.finished_at AS finished
                 FROM attempt_projection a
                 JOIN host_registry h ON h.container_instance_id = a.instance_id
                WHERE {unpriced} AND NOT {by_execution}
                  AND a.started_at IS NOT NULL AND {priceable}",
            role = role.as_str(),
            unpriced = not_yet_priced("a.run_id", "a.batch_job_id"),
            by_execution = prices_by_execution("a.run_id", "a.batch_job_id"),
            priceable = priceable("h", "a.finished_at", "$1"),
        ),
        JobRole::Coordinator => format!(
            r"SELECT r.run_id, r.coordinator_job_id AS batch_job_id, NULL::text AS execution_id,
                      '{role}' AS role, r.coordinator_host_id AS instance_id, h.instance_type,
                      h.availability_zone, h.lifecycle, h.first_seen_at, w.queue, w.started,
                      w.finished
                 FROM run_projection r
                 JOIN host_registry h ON h.container_instance_id = r.coordinator_host_id
                 JOIN LATERAL ({window}) w ON TRUE
                WHERE r.coordinator_job_id IS NOT NULL AND {unpriced} AND NOT {by_execution}
                  AND w.started IS NOT NULL AND {priceable}",
            role = role.as_str(),
            window = coordinator_window("r.coordinator_job_id"),
            unpriced = not_yet_priced("r.run_id", "r.coordinator_job_id"),
            by_execution = prices_by_execution("r.run_id", "r.coordinator_job_id"),
            priceable = priceable("h", "w.finished", "$1"),
        ),
    };
    page(pool, &jobs, since_micros, after, limit).await
}

/// The executions, of tasks and coordinators alike, the pricer still owes a line
/// ([`execution_to_price`], with `since_micros` the pricing window's start,
/// `biffix_db::rules::PRICING_WINDOW` before now), by the rule the backend's "pending" count
/// applies too: each priced from its own start to its own stop, on its own host and queue.
///
/// Oldest first (by finish, then job, execution and run id), at most `limit`, after `after` (the
/// last of the previous page, `None` for the first): the pricer pages past the ones it leaves for a
/// later tick, so a pile of them at the head of the queue cannot keep newer ones from being priced.
pub async fn unpriced_executions(
    pool: &PgPool,
    since_micros: i64,
    after: Option<&UnpricedJob>,
    limit: i64,
) -> Result<Vec<UnpricedJob>, DbError> {
    let executions = format!(
        r"SELECT x.run_id, x.batch_job_id, x.execution_id, x.role, x.instance_id, h.instance_type,
                  h.availability_zone, h.lifecycle, h.first_seen_at, x.queue,
                  x.started_at AS started, x.stopped_at AS finished
             FROM execution_projection x
             JOIN host_registry h ON h.container_instance_id = x.instance_id
            WHERE {to_price}",
        to_price = execution_to_price("x", "h", "$1"),
    );
    page(pool, &executions, since_micros, after, limit).await
}

/// A page of `jobs` (a query of `run_id, batch_job_id, execution_id, role, instance_id,
/// instance_type, availability_zone, lifecycle, first_seen_at, queue, started, finished`, with `$1`
/// the pricing window's start), oldest first by `(finished, batch_job_id, execution_id, run_id)`,
/// after `after`.
async fn page(
    pool: &PgPool,
    jobs: &str,
    since_micros: i64,
    after: Option<&UnpricedJob>,
    limit: i64,
) -> Result<Vec<UnpricedJob>, DbError> {
    // A legacy job has no execution id: it sorts as the empty one.
    let rows = sqlx::query(AssertSqlSafe(format!(
        r"SELECT * FROM ({jobs}) j
           WHERE $3::bigint IS NULL
              OR (j.finished, j.batch_job_id, COALESCE(j.execution_id, ''), j.run_id)
                 > ($3, $4, $6, $5)
           ORDER BY j.finished, j.batch_job_id, COALESCE(j.execution_id, ''), j.run_id
           LIMIT $2"
    )))
    .bind(since_micros)
    .bind(limit.max(1))
    .bind(after.map(|job| job.finished_micros))
    .bind(after.map(|job| job.batch_job_id.as_str()))
    .bind(after.map(|job| job.run_id.as_str()))
    .bind(after.map(|job| job.execution_id.as_deref().unwrap_or("")))
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| UnpricedJob {
            run_id: r.get("run_id"),
            batch_job_id: r.get("batch_job_id"),
            execution_id: r.get("execution_id"),
            role: JobRole::of(r.get("role")),
            instance_id: r.get("instance_id"),
            instance_type: r.get("instance_type"),
            availability_zone: r.get("availability_zone"),
            lifecycle: r.get("lifecycle"),
            host_first_seen_micros: r.get("first_seen_at"),
            queue: r.get("queue"),
            started_micros: r.get("started"),
            finished_micros: r.get("finished"),
        })
        .collect())
}

/// Record an execution's or a legacy job's estimated cost (`execution_cost` or `attempt_cost`, by
/// [`UnpricedJob::execution_id`]). A legacy line is written once: a re-price of an already-priced
/// job is a no-op. An execution's line records the host, queue, start and stop it was priced from,
/// and its role, and replaces the execution's line or unpriceable record from inputs (or a role)
/// the fold has since changed ([`execution_not_yet_priced`]), so the execution keeps one, from the
/// inputs it was last priced from.
pub async fn record_job_cost(
    pool: &PgPool,
    job: &UnpricedJob,
    lifecycle: Option<&str>,
    cost: &crate::pricing::JobCost,
) -> Result<(), DbError> {
    // An execution's line also binds its id and inputs, as `$12` to `$16`.
    let sql = match job.execution_id {
        Some(_) => {
            r"WITH replaced AS (
                DELETE FROM unpriceable_execution WHERE run_id = $1 AND execution_id = $12
              )
              INSERT INTO execution_cost
                 (run_id, batch_job_id, role, instance_type, availability_zone, lifecycle, hours,
                  price_per_hour, price_source, compute_usd, ebs_usd, execution_id, instance_id,
                  queue, started_at, stopped_at)
               VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16)
               ON CONFLICT (run_id, execution_id) DO UPDATE SET
                 batch_job_id = EXCLUDED.batch_job_id, role = EXCLUDED.role,
                 instance_type = EXCLUDED.instance_type,
                 availability_zone = EXCLUDED.availability_zone, lifecycle = EXCLUDED.lifecycle,
                 hours = EXCLUDED.hours, price_per_hour = EXCLUDED.price_per_hour,
                 price_source = EXCLUDED.price_source, compute_usd = EXCLUDED.compute_usd,
                 ebs_usd = EXCLUDED.ebs_usd, instance_id = EXCLUDED.instance_id,
                 queue = EXCLUDED.queue, started_at = EXCLUDED.started_at,
                 stopped_at = EXCLUDED.stopped_at, priced_at = now()"
        }
        None => {
            r"INSERT INTO attempt_cost
                 (run_id, batch_job_id, role, instance_type, availability_zone, lifecycle, hours,
                  price_per_hour, price_source, compute_usd, ebs_usd)
               VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)
               ON CONFLICT (run_id, batch_job_id) DO NOTHING"
        }
    };
    let query = sqlx::query(sql)
        .bind(&job.run_id)
        .bind(&job.batch_job_id)
        .bind(job.role.as_str())
        .bind(&job.instance_type)
        .bind(job.availability_zone.as_deref())
        .bind(lifecycle)
        .bind(cost.hours)
        .bind(cost.price_per_hour)
        .bind(cost.price_source.as_str())
        .bind(cost.compute_usd)
        .bind(cost.ebs_usd);
    match &job.execution_id {
        Some(execution_id) => query
            .bind(execution_id)
            .bind(&job.instance_id)
            .bind(job.queue.as_deref())
            .bind(job.started_micros)
            .bind(job.finished_micros),
        None => query,
    }
    .execute(pool)
    .await?;
    Ok(())
}

/// Record that an execution or a legacy job can never be priced (`reason` says why;
/// `unpriceable_execution` or `unpriceable_job`, by [`UnpricedJob::execution_id`]), so
/// [`unpriced_executions`] or [`unpriced_jobs`] stops returning it and it no longer holds a slot at
/// the head of the oldest-first queue. Idempotent. An execution's record keeps the inputs it was
/// decided from, and replaces the execution's line or record from inputs the fold has since
/// changed, as [`record_job_cost`]'s line does: a line priced from other inputs is no estimate of
/// this execution.
pub async fn record_unpriceable(
    pool: &PgPool,
    job: &UnpricedJob,
    reason: &str,
) -> Result<(), DbError> {
    let query = match &job.execution_id {
        Some(execution_id) => sqlx::query(
            r"WITH replaced AS (
                DELETE FROM execution_cost WHERE run_id = $1 AND execution_id = $2
              )
              INSERT INTO unpriceable_execution
                 (run_id, execution_id, instance_id, queue, started_at, stopped_at, reason)
               VALUES ($1, $2, $3, $4, $5, $6, $7)
               ON CONFLICT (run_id, execution_id) DO UPDATE SET
                 instance_id = EXCLUDED.instance_id, queue = EXCLUDED.queue,
                 started_at = EXCLUDED.started_at, stopped_at = EXCLUDED.stopped_at,
                 reason = EXCLUDED.reason, recorded_at = now()",
        )
        .bind(&job.run_id)
        .bind(execution_id)
        .bind(&job.instance_id)
        .bind(job.queue.as_deref())
        .bind(job.started_micros)
        .bind(job.finished_micros),
        None => sqlx::query(
            r"INSERT INTO unpriceable_job (run_id, batch_job_id, reason) VALUES ($1, $2, $3)
               ON CONFLICT (run_id, batch_job_id) DO NOTHING",
        )
        .bind(&job.run_id)
        .bind(&job.batch_job_id),
    };
    query.bind(reason).execute(pool).await?;
    Ok(())
}

/// Record why each ended execution that stopped at or after `since_micros` with no start has no
/// cost ([`START_UNKNOWN`]), and drop what no longer holds: every unpriceable record whose inputs
/// the fold has since changed ([`priced_from`], such as a start folded after its end), whose
/// execution the execution queue then offers again, and the line of every such execution priced
/// from a start the fold has since dropped. A start-unknown record never blocks pricing
/// ([`execution_not_yet_priced`]); it is what a reader is told. A describe of a finished job
/// yields its executions' ends alone, so such a start is rare. The job of an execution with no
/// start is priced the legacy way unless another of its executions ran ([`prices_by_execution`]).
/// Returns how many were recorded.
pub async fn mark_start_unknown_executions(
    pool: &PgPool,
    since_micros: i64,
) -> Result<u64, DbError> {
    let stale = format!(
        r"DELETE FROM unpriceable_execution u USING execution_projection x
           WHERE x.run_id = u.run_id AND x.execution_id = u.execution_id AND NOT ({current})",
        current = priced_from("u", "x"),
    );
    sqlx::query(AssertSqlSafe(stale)).execute(pool).await?;
    // A line priced from a start the fold has since dropped estimates no run time it has.
    sqlx::query(
        r"DELETE FROM execution_cost c USING execution_projection x
           WHERE x.run_id = c.run_id AND x.execution_id = c.execution_id
             AND x.ended AND x.started_at IS NULL AND x.stopped_at >= $1",
    )
    .bind(since_micros)
    .execute(pool)
    .await?;
    let sql = format!(
        r"INSERT INTO unpriceable_execution
             (run_id, execution_id, reason, instance_id, queue, started_at, stopped_at)
          SELECT x.run_id, x.execution_id, $2, x.instance_id, x.queue, x.started_at, x.stopped_at
            FROM execution_projection x
           WHERE x.ended AND x.started_at IS NULL AND x.stopped_at >= $1 AND {unpriced}
          ON CONFLICT (run_id, execution_id) DO NOTHING",
        unpriced = execution_not_yet_priced("x"),
    );
    let done = sqlx::query(AssertSqlSafe(sql))
        .bind(since_micros)
        .bind(START_UNKNOWN)
        .execute(pool)
        .await?;
    Ok(done.rows_affected())
}
