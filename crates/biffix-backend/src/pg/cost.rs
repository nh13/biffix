//! [`CostReads`] for [`PgStore`]: run costs from the priced jobs (`attempt_cost`), usage, and the
//! past runs an estimate reads.

use std::collections::HashMap;

use async_trait::async_trait;
use futures::TryFutureExt;
// `AssertSqlSafe` marks query text assembled with `format!`: every interpolated piece is a code
// constant (column lists, enum-derived literals, `$n` placeholders), never a runtime value; values
// are always bound.
use sqlx::{AssertSqlSafe, Row};

use biffix_core::ids::RunId;

use super::runs::filter_sql;
use super::PgStore;
use crate::store::cost::CostReads;
use crate::store::runs::RunFilter;
use crate::store::StoreError;

impl PgStore {
    /// The run's cost estimate from its priced jobs, given the batch job ids of each task's final
    /// attempt, plus the priced rows themselves.
    pub(super) async fn run_cost_rows(
        &self,
        run_id: &RunId,
        final_attempts: &std::collections::BTreeSet<String>,
    ) -> Result<(Option<biffix_api::RunCost>, Vec<crate::store::cost::CostRow>), StoreError> {
        // Every priced job of the run, with its role and (for a task job) its task, in job id order:
        // `run_cost` adds them up in this order, as `run_costs` does, so the run page's estimate
        // and the list's are the same float.
        let rows = sqlx::query(
            "SELECT c.batch_job_id, a.task_key, c.role, c.instance_type, c.lifecycle, c.hours, \
             c.price_per_hour, c.price_source, c.compute_usd, c.ebs_usd \
             FROM attempt_cost c \
             LEFT JOIN attempt_projection a \
               ON a.run_id = c.run_id AND a.batch_job_id = c.batch_job_id \
             WHERE c.run_id = $1 \
             ORDER BY c.batch_job_id COLLATE \"C\"",
        )
        .bind(run_id.as_str())
        .fetch_all(&self.pool);
        let pending_sql = pending_jobs_sql();
        let pending = sqlx::query_scalar::<_, i64>(AssertSqlSafe(pending_sql))
            .bind(run_id.as_str())
            .bind(pricing_window_start())
            .fetch_one(&self.pool);
        let (rows, pending_jobs) =
            tokio::try_join!(rows.err_into::<StoreError>(), pending.err_into::<StoreError>())?;
        let cost_rows: Vec<crate::store::cost::CostRow> = rows
            .into_iter()
            .map(|r| {
                let coordinator =
                    r.get::<String, _>("role") == biffix_db::rules::JobRole::Coordinator.as_str();
                crate::store::cost::CostRow {
                    batch_job_id: r.get("batch_job_id"),
                    coordinator,
                    // NULL for a task job whose attempt row is gone: see `CostRow::task_key`.
                    task_key: if coordinator { None } else { r.get("task_key") },
                    instance_type: r.get("instance_type"),
                    lifecycle: r.get("lifecycle"),
                    hours: r.get("hours"),
                    price_per_hour: r.get("price_per_hour"),
                    price_source: r.get("price_source"),
                    compute_usd: r.get("compute_usd"),
                    ebs_usd: r.get("ebs_usd"),
                }
            })
            .collect();
        let cost = crate::store::cost::run_cost(&cost_rows, final_attempts, pending_jobs);
        Ok((cost, cost_rows))
    }
}

/// How far back the pricer prices a finished job ([`biffix_db::rules::PRICING_WINDOW`]): a job
/// that finished earlier is never priced, so it is not pending either.
const PRICING_WINDOW_MICROS: i64 = biffix_db::rules::PRICING_WINDOW.as_micros() as i64;

/// The start of the pricing window, now: a job that ended before it is never priced.
fn pricing_window_start() -> i64 {
    biffix_core::clock::epoch_micros() - PRICING_WINDOW_MICROS
}

/// The number of the run (`$1`)'s jobs not priced yet that the pricer still can price (see
/// [`pending_jobs_from`]), with `$2` the pricing window's start.
fn pending_jobs_sql() -> String {
    let (attempts, coordinator) = pending_jobs_from("$1", "$2");
    format!("SELECT (SELECT COUNT(*) {attempts}) + (SELECT COUNT(*) {coordinator}) AS pending")
}

/// The `FROM … WHERE` of the run `run` (an SQL expression, never over an alias `r`, which the
/// coordinator part uses itself)'s jobs not priced yet that the pricer still can price, as two
/// parts (its task attempts, its coordinator): a job that is not finished yet while its run has
/// not ended, or one that finished (after `window`, the pricing window's start, an SQL expression)
/// on a host the registry knows the instance type of. The eligibility is the pricer's own ([`biffix_db::rules`]), so a job
/// the pricer can never price (terminated before it got a host, or on a host that predates the
/// registry, or finished before the window) is not "pending" forever.
///
/// The coordinator's run time is judged from its own Batch facts, by the window the pricer prices
/// it from ([`biffix_db::rules::coordinator_window`]), so one whose terminal never reached the log
/// (which the pricer can never price) is not pending once its run has ended.
fn pending_jobs_from(run: &str, window: &str) -> (String, String) {
    use biffix_core::state::AttemptState;
    use biffix_db::rules;
    // Enum-derived literals (no user input), so inlining them is injection-safe.
    fn sql_list<'a>(values: impl IntoIterator<Item = &'a str>) -> String {
        values.into_iter().map(|v| format!("'{v}'")).collect::<Vec<_>>().join(", ")
    }
    // The attempt states `AttemptState::is_terminal` admits.
    let finished_attempt =
        sql_list([AttemptState::Succeeded, AttemptState::Failed].map(AttemptState::as_str));
    let ended_run =
        sql_list(RunFilter::Completed.states().unwrap_or_default().iter().map(|s| s.as_str()));
    let attempt_unpriced = rules::not_yet_priced("a.run_id", "a.batch_job_id");
    let attempt_priceable = rules::priceable("h", "a.finished_at", window);
    // An attempt still open counts only while its run has not ended, as the coordinator below:
    // one whose terminal never arrives (lost, and given up on by the reconciler) would otherwise
    // keep an ended run "pending" for good. A terminal that arrives late makes it pending again,
    // when the pricer can price it.
    let attempts = format!(
        "FROM attempt_projection a \
         JOIN run_projection ar ON ar.run_id = a.run_id \
         LEFT JOIN host_registry h ON h.container_instance_id = a.instance_id \
         WHERE a.run_id = {run} AND {attempt_unpriced} \
           AND ((a.state NOT IN ({finished_attempt}) AND ar.state NOT IN ({ended_run})) \
                OR (a.state IN ({finished_attempt}) AND a.started_at IS NOT NULL \
                    AND {attempt_priceable}))"
    );
    let coordinator_unpriced = rules::not_yet_priced("r.run_id", "r.coordinator_job_id");
    let coordinator_window = rules::coordinator_window("r.coordinator_job_id");
    let coordinator_priceable = rules::priceable("h", "w.finished", window);
    let coordinator = format!(
        "FROM run_projection r \
         LEFT JOIN host_registry h ON h.container_instance_id = r.coordinator_host_id \
         LEFT JOIN LATERAL ({coordinator_window}) w ON TRUE \
         WHERE r.run_id = {run} AND r.coordinator_job_id IS NOT NULL AND {coordinator_unpriced} \
           AND (r.state NOT IN ({ended_run}) \
                OR (w.started IS NOT NULL AND {coordinator_priceable}))"
    );
    (attempts, coordinator)
}

/// Which past runs a basis query reads (see [`basis_sql`]).
#[derive(Clone, Copy, PartialEq)]
enum Basis {
    /// A typical run: any sample count, its cost NULL while not fully priced. Of one workflow
    /// (`$3` project, `$4` name) or, per (project, name), of every workflow.
    Typical { one: bool },
    /// A launch estimate of one workflow (`$3`, `$4`): fully priced runs with a sample count.
    Estimate,
}

/// The newest `$2` past successful runs (per workflow) a [`Basis`] reads, newest first, as
/// `project, name, sample_count, launched_at, completed_at, usd`.
///
/// Only fresh runs that launched count. A resumed run (`resumed_from`) skipped its cached work, so
/// its time and cost are not a run's; a run with no launch time has no run time (its wall time is
/// from launch: waiting for submit-time images is not run time). `usd` is the run's priced cost,
/// NULL while any of its jobs is still pending pricing ([`pending_jobs_from`], with `$1` the
/// pricing window's start) or none is priced, so a partly priced run never pulls a median down; an
/// estimate, which is all about cost, leaves such runs out.
fn basis_sql(basis: Basis) -> String {
    let succeeded = filter_sql(RunFilter::Succeeded);
    let (attempts, coordinator) = pending_jobs_from("b.run_id", "$1");
    let usd = format!(
        "CASE WHEN EXISTS (SELECT 1 {attempts}) OR EXISTS (SELECT 1 {coordinator}) THEN NULL \
           ELSE (SELECT sum(c.compute_usd + c.ebs_usd) FROM attempt_cost c \
                  WHERE c.run_id = b.run_id) END"
    );
    let runs = |scope: &str, extra: &str| {
        format!(
            "SELECT w.project, w.name, r.run_id, r.sample_count, r.launched_at, r.completed_at \
               {extra} \
             FROM run_projection r JOIN workflow_versions w ON r.workflow_version = w.id \
             WHERE {succeeded} {scope} \
               AND r.completed_at IS NOT NULL AND r.launched_at IS NOT NULL \
               AND r.resumed_from IS NULL"
        )
    };
    let one = "AND w.project = $3 AND w.name = $4";
    match basis {
        // Ranked before the cost is read, so only the runs kept are priced.
        Basis::Typical { one: scoped } => {
            let rank = ", row_number() OVER (PARTITION BY w.project, w.name \
                 ORDER BY r.completed_at DESC, r.run_id COLLATE \"C\") AS rn";
            let runs = runs(if scoped { one } else { "" }, rank);
            format!(
                "SELECT b.project, b.name, b.sample_count, b.launched_at, b.completed_at, \
                   {usd} AS usd \
                 FROM ({runs}) b WHERE b.rn <= $2 \
                 ORDER BY b.project COLLATE \"C\", b.name COLLATE \"C\", b.completed_at DESC, \
                   b.run_id COLLATE \"C\""
            )
        }
        // Filtered on the cost before the limit, so the basis is the newest priced runs.
        Basis::Estimate => {
            let runs = runs(&format!("{one} AND r.sample_count > 0"), "");
            format!(
                "SELECT * FROM (SELECT b.*, {usd} AS usd FROM ({runs}) b) p \
                 WHERE p.usd IS NOT NULL \
                 ORDER BY p.completed_at DESC, p.run_id COLLATE \"C\" LIMIT $2"
            )
        }
    }
}

/// A basis row's wall time, from launch to completion (both are set: see [`basis_sql`]).
fn wall_secs_of(r: &sqlx::postgres::PgRow) -> f64 {
    let launched: i64 = r.get("launched_at");
    let completed: i64 = r.get("completed_at");
    (completed - launched) as f64 / 1e6
}

/// A typical-basis row ([`basis_sql`]) as a past run.
fn row_to_past_wall(r: &sqlx::postgres::PgRow) -> crate::runs::estimate::PastWall {
    crate::runs::estimate::PastWall {
        wall_secs: wall_secs_of(r),
        usd: r.get::<Option<f64>, _>("usd"),
    }
}

#[async_trait]
impl CostReads for PgStore {
    async fn run_costs(
        &self,
        run_ids: &[RunId],
    ) -> Result<HashMap<String, crate::store::cost::ListCost>, StoreError> {
        // Per run, in one query: its priced jobs' compute and root volume, each summed in job id
        // order as `run_cost_rows` reads them (so `compute + ebs` is the detail's estimate, the same
        // float), and whether any job is still pending (`pending_jobs_from`: EXISTS stops at the
        // first, where the detail counts them all).
        let (attempts, coordinator) = pending_jobs_from("ids.run_id", "$2");
        let sql = format!(
            "SELECT ids.run_id, c.n, c.compute, c.ebs, \
                    (EXISTS (SELECT 1 {attempts}) OR EXISTS (SELECT 1 {coordinator})) AS pending \
             FROM unnest($1::text[]) AS ids(run_id) \
             CROSS JOIN LATERAL ( \
               SELECT count(*) AS n, \
                      sum(compute_usd ORDER BY batch_job_id COLLATE \"C\")::float8 AS compute, \
                      sum(ebs_usd ORDER BY batch_job_id COLLATE \"C\")::float8 AS ebs \
               FROM attempt_cost WHERE run_id = ids.run_id) c"
        );
        let ids: Vec<&str> = run_ids.iter().map(RunId::as_str).collect();
        let rows = sqlx::query(AssertSqlSafe(sql))
            .bind(&ids)
            .bind(pricing_window_start())
            .fetch_all(&self.pool)
            .await?;
        Ok(rows
            .into_iter()
            .filter_map(|r| {
                let priced = r.get::<i64, _>("n") > 0;
                let cost = crate::store::cost::ListCost {
                    usd: priced.then(|| r.get::<f64, _>("compute") + r.get::<f64, _>("ebs")),
                    pending: r.get("pending"),
                };
                (priced || cost.pending).then(|| (r.get("run_id"), cost))
            })
            .collect())
    }

    async fn usage(
        &self,
        from_micros: i64,
        to_micros: i64,
        group: crate::runs::usage::UsageGroup,
    ) -> Result<crate::runs::usage::UsageReport, StoreError> {
        use crate::runs::usage::{UsageGroup, UsageReport, UsageTotals};
        // Each key is a fixed expression chosen by the enum, never caller text.
        let (key, order) = match group {
            UsageGroup::Month => (
                "to_char(to_timestamp(r.submitted_at / 1000000.0) AT TIME ZONE 'UTC', 'YYYY-MM')",
                "{k} COLLATE \"C\"",
            ),
            // A workflow is a name within a project: same-named workflows of two projects are two
            // rows (and two keys).
            UsageGroup::Workflow => (
                "COALESCE(w.project || '/' || w.name, '(unknown)')",
                "total DESC, {k} COLLATE \"C\"",
            ),
            UsageGroup::Submitter => (
                "COALESCE(r.submitter_label, r.submitter, '(unknown)')",
                "total DESC, {k} COLLATE \"C\"",
            ),
            UsageGroup::InstanceType => ("c.instance_type", "total DESC, {k} COLLATE \"C\""),
            UsageGroup::Project => {
                ("COALESCE(w.project, '(unknown)')", "total DESC, {k} COLLATE \"C\"")
            }
        };
        // Ties by key in byte order, as the in-memory store breaks them (an output column cannot
        // carry a collation in ORDER BY, so the key's expression does).
        let order = order.replace("{k}", key);
        const FROM: &str = "FROM attempt_cost c JOIN run_projection r ON r.run_id = c.run_id \
             LEFT JOIN workflow_versions w ON w.id = r.workflow_version \
             WHERE r.submitted_at >= $1 AND r.submitted_at < $2";
        let totals = |r: &sqlx::postgres::PgRow, key: String| UsageTotals {
            key,
            runs: r.get::<i64, _>("runs"),
            compute_usd: r.get::<Option<f64>, _>("compute_usd").unwrap_or(0.0),
            ebs_usd: r.get::<Option<f64>, _>("ebs_usd").unwrap_or(0.0),
        };
        let rows = sqlx::query(AssertSqlSafe(format!(
            "SELECT {key} AS key, count(DISTINCT r.run_id) AS runs, \
               sum(c.compute_usd) AS compute_usd, sum(c.ebs_usd) AS ebs_usd, \
               sum(c.compute_usd + c.ebs_usd) AS total \
             {FROM} GROUP BY 1 ORDER BY {order}"
        )))
        .bind(from_micros)
        .bind(to_micros)
        .fetch_all(&self.pool)
        .await?;
        let total = sqlx::query(AssertSqlSafe(format!(
            "SELECT count(DISTINCT r.run_id) AS runs, sum(c.compute_usd) AS compute_usd, \
               sum(c.ebs_usd) AS ebs_usd {FROM}"
        )))
        .bind(from_micros)
        .bind(to_micros)
        .fetch_one(&self.pool)
        .await?;
        Ok(UsageReport {
            rows: rows
                .iter()
                .map(|r| totals(r, r.get::<Option<String>, _>("key").unwrap_or_default()))
                .collect(),
            total: totals(&total, "total".into()),
        })
    }

    async fn estimate_basis(
        &self,
        project: &str,
        workflow_name: &str,
        limit: i64,
    ) -> Result<Vec<crate::runs::estimate::PastRun>, StoreError> {
        let rows = sqlx::query(AssertSqlSafe(basis_sql(Basis::Estimate)))
            .bind(pricing_window_start())
            .bind(limit)
            .bind(project)
            .bind(workflow_name)
            .fetch_all(&self.pool)
            .await?;
        Ok(rows
            .iter()
            .map(|r| crate::runs::estimate::PastRun {
                usd: r.get::<Option<f64>, _>("usd").unwrap_or(0.0),
                samples: r.get::<i32, _>("sample_count").max(0) as u32,
                wall_secs: wall_secs_of(r),
            })
            .collect())
    }

    async fn typical_basis(
        &self,
        project: &str,
        workflow_name: &str,
        limit: i64,
    ) -> Result<Vec<crate::runs::estimate::PastWall>, StoreError> {
        let rows = sqlx::query(AssertSqlSafe(basis_sql(Basis::Typical { one: true })))
            .bind(pricing_window_start())
            .bind(limit)
            .bind(project)
            .bind(workflow_name)
            .fetch_all(&self.pool)
            .await?;
        Ok(rows.iter().map(row_to_past_wall).collect())
    }

    async fn typical_bases(
        &self,
        limit: i64,
    ) -> Result<HashMap<(String, String), Vec<crate::runs::estimate::PastWall>>, StoreError> {
        let rows = sqlx::query(AssertSqlSafe(basis_sql(Basis::Typical { one: false })))
            .bind(pricing_window_start())
            .bind(limit)
            .fetch_all(&self.pool)
            .await?;
        let mut bases: HashMap<(String, String), Vec<crate::runs::estimate::PastWall>> =
            HashMap::new();
        for r in &rows {
            bases.entry((r.get("project"), r.get("name"))).or_default().push(row_to_past_wall(r));
        }
        Ok(bases)
    }
}
