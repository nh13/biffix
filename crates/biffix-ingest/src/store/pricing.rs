//! Cost estimates (`attempt_cost`, `unpriceable_job`): the finished jobs to price, and their
//! recorded cost.

use sqlx::postgres::PgPool;
// `AssertSqlSafe` marks the queries whose text is assembled with `format!`: every piece is a code
// constant (column lists, `EventSource`/kind names, bind placeholders), never a runtime value.
use sqlx::{AssertSqlSafe, Row};

use biffix_db::rules::{coordinator_window, not_yet_priced, priceable};
use biffix_db::DbError;

pub use biffix_db::rules::JobRole;

/// A finished job not yet priced, with what is known about its host and run time.
#[derive(Clone, Debug, PartialEq)]
pub struct UnpricedJob {
    pub run_id: String,
    pub batch_job_id: String,
    pub role: JobRole,
    pub instance_type: String,
    pub availability_zone: Option<String>,
    /// The host's EC2 lifecycle (`spot`, `on_demand`), when resolved.
    pub lifecycle: Option<String>,
    /// When the host was first seen (epoch micros): while it is recent, an unresolved lifecycle
    /// may still resolve, so the pricer waits rather than guessing.
    pub host_first_seen_micros: i64,
    /// The Batch job queue the job ran on (for the queue's capacity and volume spec): a task's from
    /// its attempt, a coordinator's from its own `aws-batch` events.
    pub queue: Option<String>,
    pub started_micros: i64,
    pub finished_micros: i64,
}

/// Finished jobs of `role` with a known host type that have no cost yet, are not recorded as
/// unpriceable, and finished at or after `since_micros` (`biffix_db::rules::PRICING_WINDOW` before
/// now), by the rules the backend's "pending" count applies too ([`biffix_db::rules`]): task
/// attempts from `attempt_projection`, and run coordinators (their window and queue from their own
/// Batch events, [`coordinator_window`]).
///
/// Oldest first (by finish, then job and run id), at most `limit`, after `after` (the last job of
/// the previous page, `None` for the first): the pricer pages past the jobs it leaves for a later
/// tick, so a pile of them at the head of the queue cannot keep newer jobs from being priced.
pub async fn unpriced_jobs(
    pool: &PgPool,
    role: JobRole,
    since_micros: i64,
    after: Option<&UnpricedJob>,
    limit: i64,
) -> Result<Vec<UnpricedJob>, DbError> {
    // The role's jobs as `run_id, batch_job_id, instance_type, availability_zone, lifecycle,
    // first_seen_at, queue, started, finished`; `$1` is the pricing window's start.
    let jobs = match role {
        JobRole::Task => format!(
            r"SELECT a.run_id, a.batch_job_id, h.instance_type, h.availability_zone, h.lifecycle,
                      h.first_seen_at, a.queue, a.started_at AS started, a.finished_at AS finished
                 FROM attempt_projection a
                 JOIN host_registry h ON h.container_instance_id = a.instance_id
                WHERE {unpriced}
                  AND a.started_at IS NOT NULL AND {priceable}",
            unpriced = not_yet_priced("a.run_id", "a.batch_job_id"),
            priceable = priceable("h", "a.finished_at", "$1"),
        ),
        JobRole::Coordinator => format!(
            r"SELECT r.run_id, r.coordinator_job_id AS batch_job_id, h.instance_type,
                      h.availability_zone, h.lifecycle, h.first_seen_at, w.queue, w.started,
                      w.finished
                 FROM run_projection r
                 JOIN host_registry h ON h.container_instance_id = r.coordinator_host_id
                 JOIN LATERAL ({window}) w ON TRUE
                WHERE r.coordinator_job_id IS NOT NULL AND {unpriced}
                  AND w.started IS NOT NULL AND {priceable}",
            window = coordinator_window("r.coordinator_job_id"),
            unpriced = not_yet_priced("r.run_id", "r.coordinator_job_id"),
            priceable = priceable("h", "w.finished", "$1"),
        ),
    };
    let rows = sqlx::query(AssertSqlSafe(format!(
        r"SELECT * FROM ({jobs}) j
           WHERE $3::bigint IS NULL OR (j.finished, j.batch_job_id, j.run_id) > ($3, $4, $5)
           ORDER BY j.finished, j.batch_job_id, j.run_id
           LIMIT $2"
    )))
    .bind(since_micros)
    .bind(limit.max(1))
    .bind(after.map(|job| job.finished_micros))
    .bind(after.map(|job| job.batch_job_id.as_str()))
    .bind(after.map(|job| job.run_id.as_str()))
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| UnpricedJob {
            run_id: r.get("run_id"),
            batch_job_id: r.get("batch_job_id"),
            role,
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

/// Record a job's estimated cost. Written once: a re-price of an already-priced job is a no-op.
pub async fn record_job_cost(
    pool: &PgPool,
    job: &UnpricedJob,
    lifecycle: Option<&str>,
    cost: &crate::pricing::JobCost,
) -> Result<(), DbError> {
    sqlx::query(
        r"INSERT INTO attempt_cost
             (run_id, batch_job_id, role, instance_type, availability_zone, lifecycle, hours,
              price_per_hour, price_source, compute_usd, ebs_usd)
           VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)
           ON CONFLICT (run_id, batch_job_id) DO NOTHING",
    )
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
    .bind(cost.ebs_usd)
    .execute(pool)
    .await?;
    Ok(())
}

/// Record that a job can never be priced (`reason` says why), so [`unpriced_jobs`] stops returning
/// it and it no longer holds a slot at the head of the oldest-first queue. Idempotent.
pub async fn record_unpriceable(
    pool: &PgPool,
    job: &UnpricedJob,
    reason: &str,
) -> Result<(), DbError> {
    sqlx::query(
        r"INSERT INTO unpriceable_job (run_id, batch_job_id, reason) VALUES ($1, $2, $3)
           ON CONFLICT (run_id, batch_job_id) DO NOTHING",
    )
    .bind(&job.run_id)
    .bind(&job.batch_job_id)
    .bind(reason)
    .execute(pool)
    .await?;
    Ok(())
}
