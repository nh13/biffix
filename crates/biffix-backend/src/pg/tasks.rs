//! [`TaskReads`] for [`PgStore`]: a run's tasks (`task_projection`) and their attempts
//! (`attempt_projection`, with their hosts and costs).

use async_trait::async_trait;
use futures::TryFutureExt;
// `AssertSqlSafe` marks query text assembled with `format!`: every interpolated piece is a code
// constant (column lists, enum-derived literals, `$n` placeholders), never a runtime value; values
// are always bound.
use sqlx::{AssertSqlSafe, Row};

use biffix_api::{Host, Task, TaskAttempt};
use biffix_core::ids::{RunId, TaskKey};

use super::{micros_to_secs, PgStore};
use crate::store::tasks::TaskReads;
use crate::store::StoreError;

impl PgStore {
    /// The run's tasks ([`TASK_SELECT_ORDER`]): all of them, or only those with these keys.
    pub(super) async fn select_tasks(
        &self,
        run_id: &RunId,
        keys: Option<&[String]>,
    ) -> Result<Vec<Task>, StoreError> {
        let sql = format!(
            "{} WHERE t.run_id = $1 AND ($2::text[] IS NULL OR t.task_key = ANY($2)) \
             {TASK_SELECT_ORDER}",
            *TASK_SELECT
        );
        Ok(sqlx::query(AssertSqlSafe(sql))
            .bind(run_id.as_str())
            .bind(keys)
            .fetch_all(&self.pool)
            .await?
            .iter()
            .map(task_from_row)
            .collect())
    }

    /// Each of these tasks' estimated cost: every priced attempt of it.
    pub(super) async fn task_costs(
        &self,
        run_id: &RunId,
        keys: &[String],
    ) -> Result<std::collections::HashMap<String, f64>, StoreError> {
        Ok(sqlx::query(
            "SELECT a.task_key, sum(c.compute_usd + c.ebs_usd) AS usd FROM attempt_cost c \
             JOIN attempt_projection a ON a.run_id = c.run_id AND a.batch_job_id = c.batch_job_id \
             WHERE c.run_id = $1 AND c.role <> $3 AND a.task_key = ANY($2) \
             GROUP BY a.task_key",
        )
        .bind(run_id.as_str())
        .bind(keys)
        .bind(biffix_db::rules::JobRole::Coordinator.as_str())
        .fetch_all(&self.pool)
        .await?
        .into_iter()
        .map(|r| (r.get("task_key"), r.get("usd")))
        .collect())
    }
}

#[async_trait]
impl TaskReads for PgStore {
    async fn get_task(&self, run_id: &RunId, task_key: &str) -> Result<Option<Task>, StoreError> {
        let keys = [task_key.to_string()];
        let (mut tasks, mut costs) = tokio::try_join!(
            self.select_tasks(run_id, Some(&keys)),
            self.task_costs(run_id, &keys)
        )?;
        let Some(mut task) = tasks.pop() else {
            return Ok(None);
        };
        task.cost_usd = costs.remove(task_key);
        Ok(Some(task))
    }

    async fn list_tasks(
        &self,
        run_id: &RunId,
        query: &crate::runs::task_page::TaskQuery,
        now: f64,
    ) -> Result<biffix_api::TaskPage, StoreError> {
        use crate::runs::task_page::TaskStateFilter;
        // Group and search narrow the rows and the chip counts; the state filter only the rows —
        // the same semantics as `task_page::page_tasks`, which the tests hold this to.
        // `$key` is the exact-key filter's parameter, whose number differs per statement.
        let narrow = |key: usize| {
            format!(
                "t.run_id = $1 AND ($2::text IS NULL OR t.\"group\" = $2) \
                 AND ($3::text IS NULL OR strpos(lower(t.task_key), $3) > 0) \
                 AND (${key}::text IS NULL OR t.task_key = ${key})"
            )
        };
        let task_key = query.task_key.as_deref();
        let needle =
            query.q.as_deref().map(str::trim).filter(|q| !q.is_empty()).map(str::to_lowercase);
        let group = query.group.as_deref();
        let counts_sql = format!(
            "SELECT count(*) AS n_all, \
                    count(*) FILTER (WHERE {}) AS n_active, \
                    count(*) FILTER (WHERE {}) AS n_ok, \
                    count(*) FILTER (WHERE {}) AS n_failed \
             FROM task_projection t WHERE {}",
            task_state_sql(TaskStateFilter::Active),
            task_state_sql(TaskStateFilter::Succeeded),
            task_state_sql(TaskStateFilter::Failed),
            narrow(4)
        );
        let counts = sqlx::query(AssertSqlSafe(counts_sql))
            .bind(run_id.as_str())
            .bind(group)
            .bind(needle.as_deref())
            .bind(task_key)
            .fetch_one(&self.pool);
        let state_sql = task_state_sql(query.state);
        let now_micros = (now * 1_000_000.0) as i64;
        let keys_sql = format!(
            "SELECT t.task_key FROM task_projection t WHERE {} AND {state_sql} \
             ORDER BY {} LIMIT $4 OFFSET $5",
            narrow(7),
            task_order_by(query.sort, query.reverse)
        );
        let keys = sqlx::query_scalar::<_, String>(AssertSqlSafe(keys_sql))
            .bind(run_id.as_str())
            .bind(group)
            .bind(needle.as_deref())
            .bind(query.limit.max(0))
            .bind(query.offset.max(0))
            .bind(now_micros)
            .bind(task_key)
            .fetch_all(&self.pool);
        // The chip counts and the page's keys are independent reads.
        let (counts, keys) =
            tokio::try_join!(counts.err_into::<StoreError>(), keys.err_into::<StoreError>())?;
        let counts = biffix_api::TaskFilterCounts {
            all: counts.get("n_all"),
            active: counts.get("n_active"),
            succeeded: counts.get("n_ok"),
            failed: counts.get("n_failed"),
        };
        let total = match query.state {
            TaskStateFilter::All => counts.all,
            TaskStateFilter::Active => counts.active,
            TaskStateFilter::Succeeded => counts.succeeded,
            TaskStateFilter::Failed => counts.failed,
        };
        // The page's rows in full, its costs and its dependents: one query each, concurrently.
        let feeds_of = sqlx::query(
            "SELECT d.dep, t.task_key FROM task_projection t \
             CROSS JOIN LATERAL unnest(t.deps) AS d(dep) \
             WHERE t.run_id = $1 AND d.dep = ANY($2) AND d.dep <> t.task_key",
        )
        .bind(run_id.as_str())
        .bind(&keys)
        .fetch_all(&self.pool);
        let (page, mut costs, feed_rows) = tokio::try_join!(
            self.select_tasks(run_id, Some(&keys)),
            self.task_costs(run_id, &keys),
            feeds_of.err_into::<StoreError>(),
        )?;
        let mut by_key: std::collections::HashMap<String, Task> =
            page.into_iter().map(|t| (t.task_key.as_str().to_string(), t)).collect();
        let mut feeds: std::collections::HashMap<String, Vec<String>> = Default::default();
        for r in feed_rows {
            feeds.entry(r.get("dep")).or_default().push(r.get("task_key"));
        }
        let tasks = keys
            .iter()
            .filter_map(|k| {
                let mut t = by_key.remove(k)?;
                t.cost_usd = costs.remove(k);
                let mut f = feeds.remove(k).unwrap_or_default();
                f.sort();
                f.dedup();
                t.feeds = f;
                Some(t)
            })
            .collect();
        Ok(biffix_api::TaskPage {
            tasks,
            total,
            offset: query.offset.max(0),
            limit: query.limit,
            counts,
            log_group: None,
        })
    }

    async fn task_samples(
        &self,
        run_id: &RunId,
        task_keys: &[String],
    ) -> Result<std::collections::BTreeMap<String, String>, StoreError> {
        let rows = sqlx::query(
            "SELECT task_key, sample FROM task_projection \
             WHERE run_id = $1 AND task_key = ANY($2) AND sample IS NOT NULL",
        )
        .bind(run_id.as_str())
        .bind(task_keys)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.iter().map(|r| (r.get("task_key"), r.get("sample"))).collect())
    }

    async fn get_attempts(
        &self,
        run_id: &RunId,
        task_key: &TaskKey,
    ) -> Result<Vec<TaskAttempt>, StoreError> {
        let rows = sqlx::query(AssertSqlSafe(format!(
            "SELECT a.batch_job_id, a.attempt, a.state, a.exit_code, a.termination_category, \
             a.status_reason, a.log_stream, a.queue, a.instance_id, a.started_at, a.finished_at, \
             a.requested_vcpus, a.requested_memory_mib, {HOST_COLUMNS}, {SPOT_WARNED_AT}, \
             c.hours AS cost_hours, c.price_per_hour AS cost_price_per_hour, \
             c.price_source AS cost_price_source, c.compute_usd AS cost_compute_usd, \
             c.ebs_usd AS cost_ebs_usd \
             FROM attempt_projection a \
             LEFT JOIN host_registry h ON h.container_instance_id = a.instance_id \
             LEFT JOIN attempt_cost c \
               ON c.run_id = a.run_id AND c.batch_job_id = a.batch_job_id \
             WHERE a.run_id = $1 AND a.task_key = $2 \
             ORDER BY a.attempt NULLS LAST, a.batch_job_id COLLATE \"C\""
        )))
        .bind(run_id.as_str())
        .bind(task_key.as_str())
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| TaskAttempt {
                batch_job_id: r.get::<String, _>("batch_job_id").into(),
                attempt: r.get::<Option<i32>, _>("attempt").map(i64::from),
                state: r.get("state"),
                exit_code: r.get::<Option<i32>, _>("exit_code").map(i64::from),
                termination_category: r.get("termination_category"),
                status_reason: r.get("status_reason"),
                log_stream: r.get("log_stream"),
                queue: r.get("queue"),
                instance_id: r.get("instance_id"),
                started_at: micros_to_secs(r.get("started_at")),
                finished_at: micros_to_secs(r.get("finished_at")),
                display: false,
                host: host_from_row(&r, r.get("queue")),
                spot_warned_at: micros_to_secs(r.get("spot_warned_at")),
                requested_vcpus: r.get("requested_vcpus"),
                requested_memory_mib: r.get("requested_memory_mib"),
                cost: r.get::<Option<String>, _>("cost_price_source").map(|price_source| {
                    let compute_usd: f64 = r.get("cost_compute_usd");
                    let ebs_usd: f64 = r.get("cost_ebs_usd");
                    biffix_api::JobCost {
                        usd: compute_usd + ebs_usd,
                        compute_usd,
                        ebs_usd,
                        hours: r.get("cost_hours"),
                        price_per_hour: r.get("cost_price_per_hour"),
                        price_source,
                    }
                }),
            })
            .collect())
    }
}

// ── Row mapping and the SQL fragments the task reads share ─────────────────────────────────

/// The `host_registry` columns a query selects (aliased `h_*`) for [`host_from_row`]; the query
/// must `LEFT JOIN host_registry h`.
pub(super) const HOST_COLUMNS: &str = "h.ec2_instance_id AS h_ec2_instance_id, \
     h.instance_type AS h_instance_type, h.availability_zone AS h_availability_zone, \
     h.cpu_architecture AS h_cpu_architecture, h.registered_vcpus AS h_registered_vcpus, \
     h.registered_memory_mib AS h_registered_memory_mib, h.lifecycle AS h_lifecycle";

/// The earliest EC2 spot interruption warning for the attempt aliased `a` (on the host aliased `h`)
/// that arrived while it ran, as `spot_warned_at` (epoch micros), for a query that `LEFT JOIN`s
/// `host_registry h`. The EC2 id comes from the host registry, else from the id Batch names in the
/// attempt's status_reason ("Host EC2 (instance i-…) terminated."), which also covers attempts
/// whose host predates the registry.
const SPOT_WARNED_AT: &str = "(SELECT MIN(s.warned_at) FROM spot_interruption s \
     WHERE s.ec2_instance_id = COALESCE(h.ec2_instance_id, \
                                        substring(a.status_reason from 'instance (i-[0-9a-f]+)')) \
       AND s.warned_at >= a.started_at \
       AND (a.finished_at IS NULL OR s.warned_at <= a.finished_at)) AS spot_warned_at";

/// The columns of a task row (with its host, [`HOST_COLUMNS`], and its [`SPOT_WARNED_AT`]), for
/// [`task_from_row`]; built once. A query appends its `WHERE`.
static TASK_SELECT: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
    format!(
        "SELECT t.task_key, t.state, t.attempts, t.exit_code, t.batch_job_id, \
         t.log_stream, t.termination_category, t.instance_id, t.started_at, t.finished_at, \
         t.cached, t.\"group\", t.deps, t.image, t.verdict_reason, a.queue, a.requested_vcpus, \
         a.requested_memory_mib, {HOST_COLUMNS}, \
         {SPOT_WARNED_AT} \
         FROM task_projection t \
         LEFT JOIN host_registry h ON h.container_instance_id = t.instance_id \
         LEFT JOIN attempt_projection a \
           ON a.run_id = t.run_id AND a.batch_job_id = t.batch_job_id"
    )
});

/// How [`PgStore::select_tasks`] orders a run's tasks.
const TASK_SELECT_ORDER: &str = "ORDER BY t.task_key COLLATE \"C\"";

fn task_from_row(r: &sqlx::postgres::PgRow) -> Task {
    Task {
        task_key: r.get::<String, _>("task_key").into(),
        state: r.get("state"),
        attempts: r.get::<i32, _>("attempts") as i64,
        exit_code: r.get::<Option<i32>, _>("exit_code").map(|c| c as i64),
        batch_job_id: r.get::<Option<String>, _>("batch_job_id").map(Into::into),
        log_stream: r.get("log_stream"),
        termination_category: r.get("termination_category"),
        instance_id: r.get("instance_id"),
        started_at: micros_to_secs(r.get("started_at")),
        finished_at: micros_to_secs(r.get("finished_at")),
        cached: r.get("cached"),
        group: r.get("group"),
        deps: r.get("deps"),
        feeds: vec![],
        cost_usd: None,
        host: host_from_row(r, r.get("queue")),
        spot_warned_at: micros_to_secs(r.get("spot_warned_at")),
        image: r.get("image"),
        verdict_reason: r.get("verdict_reason"),
        requested_vcpus: r.get("requested_vcpus"),
        requested_memory_mib: r.get("requested_memory_mib"),
    }
}

/// A [`Host`] from a row carrying [`HOST_COLUMNS`] and the attempt's `queue`, or `None` when
/// nothing is known. `capacity` is the instance's own lifecycle here; the handler falls back to the
/// queue's configured capacity when it is absent.
pub(super) fn host_from_row(r: &sqlx::postgres::PgRow, queue: Option<String>) -> Option<Host> {
    let lifecycle: Option<String> = r.get("h_lifecycle");
    let host = Host {
        ec2_instance_id: r.get("h_ec2_instance_id"),
        instance_type: r.get("h_instance_type"),
        availability_zone: r.get("h_availability_zone"),
        cpu_architecture: r.get("h_cpu_architecture"),
        registered_vcpus: r.get::<Option<f32>, _>("h_registered_vcpus").map(f64::from),
        registered_memory_mib: r.get("h_registered_memory_mib"),
        queue,
        capacity_source: lifecycle.as_ref().map(|_| "instance".to_string()),
        capacity: lifecycle,
    };
    (host != Host::default()).then_some(host)
}

/// The SQL predicate on `t.state` for a task-state filter, built from
/// [`TaskStateFilter::includes`] over every [`TaskState`] so it cannot drift from the Rust-side
/// reference (`task_page::page_tasks`); a state no [`TaskState`] spells passes only `All`, as
/// [`TaskStateFilter::matches`] has it. The interpolated values are `TaskState::as_str` literals
/// (no user input), so inlining them is injection-safe.
///
/// [`TaskState`]: biffix_core::state::TaskState
fn task_state_sql(filter: crate::runs::task_page::TaskStateFilter) -> String {
    if filter == crate::runs::task_page::TaskStateFilter::All {
        return "TRUE".to_string();
    }
    let states: Vec<String> = biffix_core::state::TaskState::ALL
        .into_iter()
        .filter(|s| filter.includes(*s))
        .map(|s| format!("'{}'", s.as_str()))
        .collect();
    if states.is_empty() {
        "FALSE".to_string()
    } else {
        format!("t.state IN ({})", states.join(", "))
    }
}

/// The `CASE` ranking `t.state` as `task_page::state_rank` does, one arm per [`TaskState`] (an
/// unknown state last).
///
/// [`TaskState`]: biffix_core::state::TaskState
fn task_state_rank_sql() -> String {
    let arms: String = biffix_core::state::TaskState::ALL
        .into_iter()
        .map(|s| {
            format!(
                "WHEN '{}' THEN {} ",
                s.as_str(),
                crate::runs::task_page::state_rank(s.as_str())
            )
        })
        .collect();
    format!("CASE t.state {arms}ELSE {} END", crate::runs::task_page::state_rank(""))
}

/// `ORDER BY` for a task page, matching `task_page::sort_tasks` (natural order, ties by key in byte
/// order — hence `COLLATE "C"`) and its reversal. `$6` is now, in epoch micros.
fn task_order_by(sort: crate::runs::task_page::TaskSort, reverse: bool) -> String {
    use crate::runs::task_page::TaskSort;
    let (asc, desc) = if reverse { ("DESC", "ASC") } else { ("ASC", "DESC") };
    let key = format!("t.task_key COLLATE \"C\" {asc}");
    match sort {
        TaskSort::Key => key,
        TaskSort::State => format!("{} {asc}, {key}", task_state_rank_sql()),
        TaskSort::Duration => format!(
            "(t.started_at IS NULL) {asc}, \
             (COALESCE(t.finished_at, $6) - t.started_at) {desc}, {key}"
        ),
        TaskSort::Started => format!("(t.started_at IS NULL) {asc}, t.started_at {desc}, {key}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runs::task_page::TaskStateFilter;

    #[test]
    fn the_task_state_sql_is_the_rust_side_tables() {
        assert_eq!(task_state_sql(TaskStateFilter::All), "TRUE");
        assert_eq!(task_state_sql(TaskStateFilter::Active), "t.state IN ('queued', 'running')");
        assert_eq!(task_state_sql(TaskStateFilter::Succeeded), "t.state IN ('succeeded')");
        assert_eq!(task_state_sql(TaskStateFilter::Failed), "t.state IN ('failed')");
        assert_eq!(
            task_state_rank_sql(),
            "CASE t.state WHEN 'queued' THEN 2 WHEN 'running' THEN 1 WHEN 'succeeded' THEN 3 \
             WHEN 'failed' THEN 0 ELSE 4 END"
        );
    }
}
