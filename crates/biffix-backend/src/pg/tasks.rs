//! [`TaskReads`] for [`PgStore`]: a run's tasks (`task_projection`) and their attempts
//! (`attempt_projection`, with their hosts and costs).

use async_trait::async_trait;
use futures::TryFutureExt;
// `AssertSqlSafe` marks query text assembled with `format!`: every interpolated piece is a code
// constant (column lists, enum-derived literals, `$n` placeholders), never a runtime value; values
// are always bound.
use sqlx::{AssertSqlSafe, Row};

use biffix_api::{Host, JobCost, JobExecution, Task, TaskAttempt};
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

    /// Each of these tasks' estimated cost: every cost line of every attempt of it.
    pub(super) async fn task_costs(
        &self,
        run_id: &RunId,
        keys: &[String],
    ) -> Result<std::collections::HashMap<String, f64>, StoreError> {
        Ok(sqlx::query(
            "SELECT a.task_key, sum(c.compute_usd + c.ebs_usd) AS usd FROM cost_line c \
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
        let attempts = sqlx::query(AssertSqlSafe(format!(
            "SELECT a.batch_job_id, a.attempt, a.state, a.exit_code, a.termination_category, \
             a.status_reason, a.log_stream, a.queue, a.instance_id, a.started_at, a.finished_at, \
             a.requested_vcpus, a.requested_memory_mib, a.run_micros, a.lost_micros, \
             a.wait_micros, {HOST_COLUMNS}, {} AS spot_warned_at, \
             c.hours AS cost_hours, c.price_source AS cost_price_source, \
             c.compute_usd AS cost_compute_usd, \
             c.ebs_usd AS cost_ebs_usd \
             FROM attempt_projection a \
             LEFT JOIN host_registry h ON h.container_instance_id = a.instance_id \
             LEFT JOIN LATERAL ({ATTEMPT_COST}) c ON TRUE \
             WHERE a.run_id = $1 AND a.task_key = $2 \
             ORDER BY a.attempt NULLS LAST, a.batch_job_id COLLATE \"C\"",
            spot_warned_sql("a", "finished_at")
        )))
        .bind(run_id.as_str())
        .bind(task_key.as_str())
        .fetch_all(&self.pool);
        // Each attempt's executions, by job, in order: only the jobs of this task's attempts.
        let executions = sqlx::query(AssertSqlSafe(format!(
            "SELECT {} FROM {EXECUTION_FROM} \
             WHERE x.run_id = $1 AND x.role = $3 AND x.batch_job_id IN \
               (SELECT a.batch_job_id FROM attempt_projection a \
                WHERE a.run_id = $1 AND a.task_key = $2) \
             ORDER BY x.ordinal",
            *EXECUTION_COLUMNS
        )))
        .bind(run_id.as_str())
        .bind(task_key.as_str())
        .bind(biffix_db::rules::JobRole::Task.as_str())
        .fetch_all(&self.pool);
        let (rows, executions) = tokio::try_join!(attempts, executions)?;
        let mut by_job: std::collections::HashMap<String, Vec<JobExecution>> = Default::default();
        for r in &executions {
            by_job.entry(r.get("batch_job_id")).or_default().push(execution_from_row(r));
        }
        Ok(rows
            .into_iter()
            .map(|r| TaskAttempt {
                executions: by_job.remove(&r.get::<String, _>("batch_job_id")).unwrap_or_default(),
                run_seconds: micros_to_secs(r.get("run_micros")),
                lost_seconds: micros_to_secs(r.get("lost_micros")),
                wait_seconds: micros_to_secs(r.get("wait_micros")),
                // Filled by the handler, which knows the task's state.
                awaiting_host: false,
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
                    let hours: f64 = r.get("cost_hours");
                    let compute_usd: f64 = r.get("cost_compute_usd");
                    // Averaged over the run time, as the pricer prices each line (so one line
                    // keeps its own price to the bit).
                    let price_per_hour = if hours > 0.0 { compute_usd / hours } else { 0.0 };
                    job_cost(&r, price_per_hour, price_source)
                }),
            })
            .collect())
    }

    async fn run_executions(
        &self,
        run_id: &RunId,
        coordinator_job: Option<&str>,
    ) -> Result<crate::store::tasks::RunExecutions, StoreError> {
        // One query for the coordinator job's executions and every execution of the run that lost
        // its host, in job then ordinal order (so a sum over them is the same from one read to
        // the next).
        let coordinator = biffix_db::rules::JobRole::Coordinator.as_str();
        let rows = sqlx::query(AssertSqlSafe(format!(
            "SELECT x.role, {} FROM {EXECUTION_FROM} \
             WHERE x.run_id = $1 \
               AND ((x.role = $2 AND x.batch_job_id = $3) OR {}) \
             ORDER BY x.batch_job_id COLLATE \"C\", x.ordinal, x.execution_id COLLATE \"C\"",
            *EXECUTION_COLUMNS,
            host_lost_sql("x")
        )))
        .bind(run_id.as_str())
        .bind(coordinator)
        .bind(coordinator_job)
        .fetch_all(&self.pool)
        .await?;
        let mut out = crate::store::tasks::RunExecutions::default();
        for r in &rows {
            let x = execution_from_row(r);
            let job: String = r.get("batch_job_id");
            if x.interruption.is_some() {
                out.lost.push(x.clone());
            }
            if r.get::<String, _>("role") == coordinator && Some(job.as_str()) == coordinator_job {
                out.coordinator.push(x);
            }
        }
        Ok(out)
    }
}

// ── Row mapping and the SQL fragments the task reads share ─────────────────────────────────

/// A [`JobCost`] from a row's `cost_hours`, `cost_compute_usd` and `cost_ebs_usd`.
fn job_cost(r: &sqlx::postgres::PgRow, price_per_hour: f64, price_source: String) -> JobCost {
    let compute_usd: f64 = r.get("cost_compute_usd");
    let ebs_usd: f64 = r.get("cost_ebs_usd");
    JobCost {
        usd: compute_usd + ebs_usd,
        compute_usd,
        ebs_usd,
        hours: r.get("cost_hours"),
        price_per_hour,
        price_source,
    }
}

/// The columns of an execution row (with its host and cost), for [`execution_from_row`], over
/// [`EXECUTION_FROM`].
static EXECUTION_COLUMNS: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
    format!(
        "x.batch_job_id, x.execution_id, x.ordinal, x.ended, x.is_final, x.exit_code, \
             x.termination_category, x.status_reason, x.log_stream, x.instance_id, x.queue, \
             x.started_at, x.stopped_at, {HOST_COLUMNS}, \
             {} AS spot_warned_at, \
             c.hours AS cost_hours, c.price_per_hour AS cost_price_per_hour, \
             c.price_source AS cost_price_source, c.compute_usd AS cost_compute_usd, \
             c.ebs_usd AS cost_ebs_usd",
        spot_warned_sql("x", "stopped_at")
    )
});

/// The execution rows (aliased `x`) with their host (`h`) and their own cost line (`c`), for
/// [`EXECUTION_COLUMNS`]; a query appends its `WHERE`.
const EXECUTION_FROM: &str = "execution_projection x \
     LEFT JOIN host_registry h ON h.container_instance_id = x.instance_id \
     LEFT JOIN execution_cost c ON c.run_id = x.run_id AND c.execution_id = x.execution_id";

/// A [`JobExecution`] from a row of [`EXECUTION_COLUMNS`].
fn execution_from_row(r: &sqlx::postgres::PgRow) -> JobExecution {
    let spot_warned_at = micros_to_secs(r.get("spot_warned_at"));
    let host = host_from_row(r, r.get("queue"));
    let termination_category: Option<String> = r.get("termination_category");
    // The registry's lifecycle, not `Host.capacity`, which the handler may fill from the queue.
    let lifecycle: Option<String> = r.get("h_lifecycle");
    let interruption = crate::runs::executions::interruption(
        termination_category.as_deref(),
        lifecycle.as_deref(),
        spot_warned_at.is_some(),
    );
    JobExecution {
        execution_id: r.get("execution_id"),
        ordinal: i64::from(r.get::<i32, _>("ordinal")),
        ended: r.get("ended"),
        final_execution: r.get("is_final"),
        exit_code: r.get::<Option<i32>, _>("exit_code").map(i64::from),
        termination_category,
        status_reason: r.get("status_reason"),
        log_stream: r.get("log_stream"),
        instance_id: r.get("instance_id"),
        started_at: micros_to_secs(r.get("started_at")),
        stopped_at: micros_to_secs(r.get("stopped_at")),
        host,
        spot_warned_at,
        interruption: interruption.map(str::to_string),
        cost: r
            .get::<Option<String>, _>("cost_price_source")
            .map(|source| job_cost(r, r.get("cost_price_per_hour"), source)),
    }
}

/// The cost of the attempt aliased `a`, for a `LEFT JOIN LATERAL`: its job's cost lines
/// (`cost_line`: one per execution, or its one legacy line), summed in execution id order, as
/// `hours`, `compute_usd`, `ebs_usd` and `price_source` (the lines' common one, else `mixed`;
/// NULL with no line, when the attempt has no cost).
const ATTEMPT_COST: &str = "SELECT \
       sum(l.hours ORDER BY l.execution_id COLLATE \"C\") AS hours, \
       sum(l.compute_usd ORDER BY l.execution_id COLLATE \"C\") AS compute_usd, \
       sum(l.ebs_usd ORDER BY l.execution_id COLLATE \"C\") AS ebs_usd, \
       CASE WHEN count(*) = 0 THEN NULL \
            WHEN count(DISTINCT l.price_source) = 1 THEN min(l.price_source) \
            ELSE 'mixed' END AS price_source \
     FROM cost_line l WHERE l.run_id = a.run_id AND l.batch_job_id = a.batch_job_id";

/// The `host_registry` columns a query selects (aliased `h_*`) for [`host_from_row`]; the query
/// must `LEFT JOIN host_registry h`.
pub(super) const HOST_COLUMNS: &str = "h.ec2_instance_id AS h_ec2_instance_id, \
     h.instance_type AS h_instance_type, h.availability_zone AS h_availability_zone, \
     h.cpu_architecture AS h_cpu_architecture, h.registered_vcpus AS h_registered_vcpus, \
     h.registered_memory_mib AS h_registered_memory_mib, h.lifecycle AS h_lifecycle";

/// The earliest EC2 spot interruption warning for the attempt or execution aliased `row` (on the
/// host aliased `h`, from a `LEFT JOIN host_registry h`) that arrived while it ran, from its
/// `started_at` to its `stop` column: an epoch-micros expression, one rule for attempts and
/// executions. The EC2 id comes from the host registry, else from the id Batch names in the
/// row's status_reason ("Host EC2 (instance i-…) terminated."), which also covers hosts that
/// predate the registry. With no recorded start, any warning before the stop counts (a host
/// reclaimed while the container was starting); with neither, none does.
fn spot_warned_sql(row: &str, stop: &str) -> String {
    format!(
        "(SELECT MIN(s.warned_at) FROM spot_interruption s \
         WHERE s.ec2_instance_id = COALESCE(h.ec2_instance_id, \
                                           substring({row}.status_reason \
                                                     from 'instance (i-[0-9a-f]+)')) \
           AND ({row}.started_at IS NOT NULL OR {row}.{stop} IS NOT NULL) \
           AND ({row}.started_at IS NULL OR s.warned_at >= {row}.started_at) \
           AND ({row}.{stop} IS NULL OR s.warned_at <= {row}.{stop}))"
    )
}

/// The columns of a task row (with its host, [`HOST_COLUMNS`], its [`spot_warned_sql`], and its
/// display attempt's execution counts and span split), for [`task_from_row`]; built once. A query
/// appends its `WHERE`.
static TASK_SELECT: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
    format!(
        "SELECT t.task_key, t.state, t.attempts, t.exit_code, t.batch_job_id, \
         t.log_stream, t.termination_category, t.instance_id, t.started_at, t.finished_at, \
         t.cached, t.\"group\", t.deps, t.image, t.verdict_reason, a.queue, a.requested_vcpus, \
         a.requested_memory_mib, a.run_micros, a.lost_micros, a.wait_micros, \
         xn.executions, xn.lost_executions, {HOST_COLUMNS}, \
         {} AS spot_warned_at \
         FROM task_projection t \
         LEFT JOIN host_registry h ON h.container_instance_id = t.instance_id \
         LEFT JOIN attempt_projection a \
           ON a.run_id = t.run_id AND a.batch_job_id = t.batch_job_id \
         LEFT JOIN LATERAL (SELECT count(*) AS executions, \
             count(*) FILTER (WHERE {}) AS lost_executions \
           FROM execution_projection x \
           WHERE x.run_id = t.run_id AND x.batch_job_id = t.batch_job_id \
             AND x.role = '{}') xn ON TRUE",
        spot_warned_sql("a", "finished_at"),
        host_lost_sql("x"),
        biffix_db::rules::JobRole::Task.as_str()
    )
});

/// Whether the execution aliased `x` lost its host: its termination category is one of
/// [`TerminationCategory::HOST_LOST`] (code constants, not input).
///
/// [`TerminationCategory::HOST_LOST`]: biffix_core::event::TerminationCategory::HOST_LOST
pub(super) fn host_lost_sql(x: &str) -> String {
    let categories: Vec<String> = biffix_core::event::TerminationCategory::HOST_LOST
        .iter()
        .map(|c| format!("'{}'", c.as_str()))
        .collect();
    format!("{x}.termination_category IN ({})", categories.join(", "))
}

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
        executions: r.get::<Option<i64>, _>("executions").unwrap_or(0),
        lost_executions: r.get::<Option<i64>, _>("lost_executions").unwrap_or(0),
        run_seconds: micros_to_secs(r.get("run_micros")),
        lost_seconds: micros_to_secs(r.get("lost_micros")),
        wait_seconds: micros_to_secs(r.get("wait_micros")),
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
            "CASE t.state WHEN 'queued' THEN 2 WHEN 'running' THEN 1 WHEN 'succeeded' THEN 4 \
             WHEN 'failed' THEN 0 WHEN 'cancelled' THEN 3 ELSE 5 END"
        );
    }
}
