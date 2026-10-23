//! [`RunReads`] for [`PgStore`]: the run list (`run_projection` with its workflow), a run's detail
//! and sample facts.

use std::collections::HashMap;

use async_trait::async_trait;
use futures::TryFutureExt;
// `AssertSqlSafe` marks query text assembled with `format!`: every interpolated piece is a code
// constant (column lists, enum-derived literals, `$n` placeholders), never a runtime value; values
// are always bound.
use sqlx::{AssertSqlSafe, Row};

use biffix_api::{RunSummary, Task};
use biffix_core::ids::RunId;

use super::notifications::undismissed_failed_of_run;
use super::tasks::{host_from_row, HOST_COLUMNS};
use super::{micros_to_secs, PgStore};
use crate::store::runs::{ResumeOrigin, RunFilter, RunPage, RunQuery, RunReads};
use crate::store::{RunDetailView, StoreError};

/// The SQL state predicate for a filter, built from [`RunFilter::states`] so it cannot drift from
/// the Rust-side `matches`. The interpolated values are `RunState::as_str` literals (no user
/// input), so inlining them is injection-safe.
pub(super) fn filter_sql(filter: RunFilter) -> String {
    match filter.states() {
        None => "TRUE".to_string(),
        Some(states) => {
            let list = states
                .iter()
                .map(|state| format!("'{}'", state.as_str()))
                .collect::<Vec<_>>()
                .join(",");
            format!("r.state IN ({list})")
        }
    }
}

/// The run list's sort key in SQL: [`crate::store::runs::run_sort_key`] over `r.submitted_at` (a
/// run with no anchor yet sorts as `i64::MAX`, the newest). Must stay the same expression as the
/// `run_projection_submitted_order` index (in the schema migrations), or the list stops using it.
const RUN_SORT_KEY: &str = "COALESCE(r.submitted_at, 9223372036854775807)";

// Per-run task-outcome counts, materialized on run_projection during the fold and read as plain
// columns — no per-read `COUNT(*) FILTER (...)` aggregation over task_projection. Aliased to the
// historical `n_*` names the row mapper expects.
const TASK_COUNTS_COLS: &str = "r.tasks_observed AS n_obs, r.tasks_succeeded AS n_ok, \
  r.tasks_running AS n_run, r.tasks_queued AS n_queued, r.tasks_failed AS n_fail, \
  r.tasks_cached AS n_cached, r.outputs_total, r.outputs_reused, r.samples_total, \
  r.samples_done, r.samples_failed, r.samples_declared";

/// The other `RunSummary` columns the row mapper reads beside [`TASK_COUNTS_COLS`]: the run's
/// experiment (`run_projection.experiment_id`) and whether it resumed another (the one place
/// `RunSummary::resumed` is derived).
const RUN_EXTRA_COLS: &str = "r.experiment_id, r.resumed_from IS NOT NULL AS resumed";

/// The run's pipeline title (`RunSummary::workflow_title`, `Notification::workflow_title`): its
/// frozen form's `title` (joined as `w`), trimmed of whitespace and absent when blank, as
/// `register::form_title` reads it for a version. Plain `btrim` would trim spaces only, so a
/// YAML block-scalar title would keep its newline here and not there. Forms are frozen with the
/// title trimmed, but a form frozen earlier may not be.
pub(super) const WORKFLOW_TITLE_SQL: &str =
    "NULLIF(btrim(w.form_schema->>'title', E' \\t\\r\\n\\f\\x0B'), '')";

/// A `task_projection t` row as a [`biffix_core::sample_progress::TaskFact`] reads it (see
/// [`row_to_task_fact`]), with whether the task wrote an output. The single-run and the batched
/// sample reads share it.
const TASK_FACT_COLS: &str = "t.sample, t.\"group\", t.state, t.cached, t.attempts, \
  EXISTS (SELECT 1 FROM run_output o WHERE o.run_id = t.run_id AND o.task_key = t.task_key) \
  AS outputs";

/// A row selected with [`TASK_FACT_COLS`] as a task fact.
fn row_to_task_fact(r: &sqlx::postgres::PgRow) -> biffix_core::sample_progress::TaskFact {
    biffix_core::sample_progress::TaskFact {
        sample: r.get("sample"),
        group: r.get("group"),
        state: r.get("state"),
        cached: r.get("cached"),
        attempts: r.get::<i32, _>("attempts").into(),
        outputs: r.get("outputs"),
    }
}

/// A stored run DAG (`run_projection.dag`): `{"nodes":[id], "edges":[[from,to]]}`.
#[derive(serde::Deserialize, Default)]
struct StoredDag {
    #[serde(default)]
    nodes: Vec<String>,
    #[serde(default)]
    edges: Vec<(String, String)>,
}

/// Map a `run_projection r` row (with the workflow join + task-count columns) to a `RunSummary`.
fn row_to_run_view(r: &sqlx::postgres::PgRow) -> RunSummary {
    RunSummary {
        run_id: r.get::<String, _>("run_id").into(),
        state: r.get("state"),
        // Filled by the API handler, which owns the lifecycle rule (`api::runs`).
        terminal: false,
        completed: r.get("completed"),
        total: r.get("total"),
        submitted_at: micros_to_secs(r.get("submitted_at")),
        completed_at: micros_to_secs(r.get("completed_at")),
        submitter: r.get("submitter"),
        submitter_label: r.get("submitter_label"),
        run_name: r.get("run_name"),
        workflow: r.get::<Option<String>, _>("workflow_version").map(Into::into),
        workflow_name: r.get("workflow_name"),
        workflow_title: r.get("workflow_title"),
        engine: r.get("engine"),
        project: r.get("project"),
        tasks_observed: r.get("n_obs"),
        tasks_succeeded: r.get("n_ok"),
        tasks_running: r.get("n_run"),
        tasks_queued: r.get("n_queued"),
        tasks_failed: r.get("n_fail"),
        tasks_cached: r.get("n_cached"),
        outputs_total: r.get("outputs_total"),
        outputs_reused: r.get("outputs_reused"),
        resumed: r.get("resumed"),
        // Native Postgres text[] ↔ Vec<String> directly — no JSON re-parse per row.
        input_uris: r.get("input_uris"),
        samples: r.get::<Option<i32>, _>("samples_total").map(|total| {
            biffix_api::RunSampleCounts {
                total: total.into(),
                done: r.get::<Option<i32>, _>("samples_done").unwrap_or(0).into(),
                failed: r.get::<Option<i32>, _>("samples_failed").unwrap_or(0).into(),
                declared: r.get::<Option<bool>, _>("samples_declared").unwrap_or(false),
            }
        }),
        experiment_id: r.get("experiment_id"),
        // Filled by the list handler from `CostReads::run_costs`, one query for the page.
        cost_usd: None,
        cost_pending: false,
    }
}

impl PgStore {
    /// One page of the run list ([`RunReads::list_runs`]); its `total` is counted only when
    /// `with_total` (else 0), so a caller that has the count already skips the `COUNT(*)`.
    pub(super) async fn run_page(
        &self,
        query: &RunQuery,
        with_total: bool,
    ) -> Result<RunPage, StoreError> {
        let state_cond = filter_sql(query.filter);
        // Escape LIKE metacharacters so a literal % or _ in the search term isn't a wildcard.
        let like = query.q.as_ref().map(|s| {
            let escaped = s.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_");
            format!("%{escaped}%")
        });
        // `$1::text IS NULL` short-circuits to "all rows" when there's no search term. The joins
        // bring engine/project from the frozen workflow version and observed task-outcome counts.
        // $1 = fuzzy search (NULL ⇒ all), $2 = exact workflow-version scope (NULL ⇒ all versions),
        // $3 = exact submitter scope (NULL ⇒ everyone's runs), $4 = the user whose attention the
        // run needs (NULL ⇒ any run), $5 = exact experiment scope (NULL ⇒ any run), $6 = only these
        // run ids (NULL ⇒ any run).
        // The searched fields are exactly those `RunQuery::q` documents.
        let attention_of_run = undismissed_failed_of_run("$4");
        let where_clause = format!(
            "FROM run_projection r LEFT JOIN workflow_versions w ON r.workflow_version = w.id \
             WHERE {state_cond} AND ($1::text IS NULL \
               OR r.run_id ILIKE $1 OR r.run_name ILIKE $1 OR w.name ILIKE $1 \
               OR r.workflow_version ILIKE $1 OR r.submitter ILIKE $1 OR r.submitter_label ILIKE $1 \
               OR w.project ILIKE $1 OR w.engine ILIKE $1 OR {WORKFLOW_TITLE_SQL} ILIKE $1) \
             AND ($2::text IS NULL OR r.workflow_version = $2) \
             AND ($3::text IS NULL OR r.submitter = $3) \
             AND ($4::text IS NULL OR (r.state = 'failed' AND {attention_of_run})) \
             AND ($5::text IS NULL OR r.experiment_id = $5) \
             AND ($6::text[] IS NULL OR r.run_id = ANY($6))"
        );

        let total: i64 = if with_total {
            sqlx::query_scalar(AssertSqlSafe(format!("SELECT COUNT(*) {where_clause}")))
                .bind(like.as_deref())
                .bind(query.workflow.as_deref())
                .bind(query.submitter.as_deref())
                .bind(query.attention.as_deref())
                .bind(query.experiment.as_deref())
                .bind(query.ids.as_deref())
                .fetch_one(&self.pool)
                .await?
        } else {
            0
        };

        let limit = query.page_size();
        // Keyset pagination on (RUN_SORT_KEY, run_id): deep pages stay O(limit), not O(offset).
        // `after` reads the next (older) page: rows strictly ordered after the cursor, still DESC.
        // `before` reads the previous (newer) page: rows after the cursor in ASC order (so we
        // get the ones adjacent to it), then reversed back to the list's DESC display order.
        // `after` wins when both are set; a token that does not decode is no cursor, i.e. the first
        // page in the normal order.
        let cursor = match (&query.after, &query.before) {
            (Some(after), _) => crate::store::runs::decode_cursor(after),
            (None, Some(before)) => crate::store::runs::decode_cursor(before),
            (None, None) => None,
        };
        let backward = query.after.is_none() && cursor.is_some();
        let (keyset_pred, order, limit_param) = match (&cursor, backward) {
            (Some(_), true) => (
                format!("AND ({RUN_SORT_KEY}, r.run_id COLLATE \"C\") > ($7, $8)"),
                format!("{RUN_SORT_KEY} ASC, r.run_id COLLATE \"C\" ASC"),
                "$9",
            ),
            (Some(_), false) => (
                format!("AND ({RUN_SORT_KEY}, r.run_id COLLATE \"C\") < ($7, $8)"),
                format!("{RUN_SORT_KEY} DESC, r.run_id COLLATE \"C\" DESC"),
                "$9",
            ),
            (None, _) => {
                (String::new(), format!("{RUN_SORT_KEY} DESC, r.run_id COLLATE \"C\" DESC"), "$7")
            }
        };
        let page_sql = format!(
            "SELECT r.run_id, r.state, r.completed, r.total, r.submitted_at, r.completed_at, \
             r.submitter, r.submitter_label, r.run_name, r.workflow_version, r.input_uris, w.engine, \
             w.project, w.name AS workflow_name, {WORKFLOW_TITLE_SQL} AS workflow_title, \
             {RUN_SORT_KEY} AS sort_key, {TASK_COUNTS_COLS}, \
             {RUN_EXTRA_COLS} \
             {where_clause} {keyset_pred} ORDER BY {order} LIMIT {limit_param}"
        );
        let mut page = sqlx::query(AssertSqlSafe(page_sql))
            .bind(like.as_deref())
            .bind(query.workflow.as_deref())
            .bind(query.submitter.as_deref())
            .bind(query.attention.as_deref())
            .bind(query.experiment.as_deref())
            .bind(query.ids.as_deref());
        if let Some((key, run_id)) = &cursor {
            page = page.bind(key).bind(run_id);
        }
        let mut rows = page.bind(limit).fetch_all(&self.pool).await?;
        // A `before` page was fetched ascending (adjacent to the cursor); flip it to the list's
        // DESC display order so cursors and rows are computed in the order the caller sees.
        if backward {
            rows.reverse();
        }

        let cursor_of = |r: &sqlx::postgres::PgRow| {
            crate::store::runs::encode_cursor(r.get("sort_key"), &r.get::<String, _>("run_id"))
        };
        let first_cursor = rows.first().map(cursor_of);
        let last_cursor = rows.last().map(cursor_of);
        let runs = rows.iter().map(row_to_run_view).collect();
        Ok(RunPage { runs, total, first_cursor, last_cursor })
    }

    /// The run's projection row (with its workflow and coordinator host); NotFound for an unknown
    /// run.
    async fn run_row(&self, run_id: &RunId) -> Result<sqlx::postgres::PgRow, StoreError> {
        sqlx::query(AssertSqlSafe(format!(
            "SELECT r.run_id, r.state, r.completed, r.total, r.submitted_at, r.completed_at, \
             r.submitter, r.submitter_label, r.run_name, r.coordinator_job_id, r.workflow_version, \
             r.input_uris, r.params, r.dag, r.output_prefix, r.coordinator_log_stream, \
             r.coordinator_exit_code, r.coordinator_status_reason, \
             r.coordinator_termination_category, r.engine_session_id, r.session_mismatch, r.resumed_from, r.input_tables, \
             r.reference_bundle, r.retention, r.work_deleted_at, r.launched_at, r.cancelled_by, \
             w.engine, w.project, w.name AS workflow_name, {WORKFLOW_TITLE_SQL} AS workflow_title, \
             w.created_at AS workflow_created_at, \
             {TASK_COUNTS_COLS}, {RUN_EXTRA_COLS}, {HOST_COLUMNS} \
             FROM run_projection r LEFT JOIN workflow_versions w ON r.workflow_version = w.id \
             LEFT JOIN host_registry h ON h.container_instance_id = r.coordinator_host_id \
             WHERE r.run_id = $1"
        )))
        .bind(run_id.as_str())
        .fetch_optional(&self.pool)
        .await?
        .ok_or(StoreError::NotFound)
    }
}

#[async_trait]
impl RunReads for PgStore {
    async fn list_runs(&self, query: &RunQuery) -> Result<RunPage, StoreError> {
        self.run_page(query, true).await
    }

    async fn get_run(&self, run_id: &RunId) -> Result<RunDetailView, StoreError> {
        // The run row and its tasks are independent reads; an unknown run is still NotFound (not an
        // empty task list), since `run_row` fails the join.
        let (run, mut tasks) =
            tokio::try_join!(self.run_row(run_id), self.select_tasks(run_id, None))?;
        let task_groups = crate::store::task_group_counts(&tasks);
        let final_attempts =
            tasks.iter().filter_map(|t| t.batch_job_id.as_ref().map(|j| j.0.clone())).collect();
        let (cost, cost_rows) = self.run_cost_rows(run_id, &final_attempts).await?;
        // Each task's cost: every priced attempt of it (the coordinator's rows carry no task).
        let mut task_cost: HashMap<&str, f64> = Default::default();
        for row in &cost_rows {
            if let Some(task) = &row.task_key {
                *task_cost.entry(task.as_str()).or_default() += row.usd();
            }
        }
        for task in &mut tasks {
            task.cost_usd = task_cost.get(task.task_key.as_str()).copied();
        }
        assemble_detail(&run, tasks, task_groups, cost)
    }

    async fn get_run_light(&self, run_id: &RunId) -> Result<RunDetailView, StoreError> {
        // The DAG's per-step counts and the final attempts (for the cost) straight from SQL,
        // without materializing every task. The groups come in `store::task_group_counts`' order
        // (byte order on group and state, then cached) so the light and full loads agree exactly.
        let task_groups = sqlx::query(
            "SELECT \"group\", state, cached, count(*) AS n FROM task_projection \
             WHERE run_id = $1 AND \"group\" IS NOT NULL GROUP BY 1, 2, 3 \
             ORDER BY \"group\" COLLATE \"C\", state COLLATE \"C\", cached",
        )
        .bind(run_id.as_str())
        .fetch_all(&self.pool);
        let final_attempts = sqlx::query_scalar::<_, String>(
            "SELECT batch_job_id FROM task_projection \
             WHERE run_id = $1 AND batch_job_id IS NOT NULL",
        )
        .bind(run_id.as_str())
        .fetch_all(&self.pool);
        let (run, task_groups, final_attempts) = tokio::try_join!(
            self.run_row(run_id),
            task_groups.err_into::<StoreError>(),
            final_attempts.err_into::<StoreError>(),
        )?;
        let task_groups = task_groups
            .into_iter()
            .map(|r| crate::store::TaskGroupCount {
                group: r.get("group"),
                state: r.get("state"),
                cached: r.get("cached"),
                count: r.get("n"),
            })
            .collect();
        let final_attempts = final_attempts.into_iter().collect();
        let (cost, _) = self.run_cost_rows(run_id, &final_attempts).await?;
        assemble_detail(&run, vec![], task_groups, cost)
    }

    async fn sample_facts(
        &self,
        run_id: &RunId,
    ) -> Result<
        (Option<biffix_core::event::SampleSpec>, Vec<biffix_core::sample_progress::TaskFact>),
        StoreError,
    > {
        let spec: Option<serde_json::Value> =
            sqlx::query_scalar("SELECT samples FROM run_projection WHERE run_id = $1")
                .bind(run_id.as_str())
                .fetch_optional(&self.pool)
                .await?
                .flatten();
        let spec = spec.map(serde_json::from_value).transpose()?;
        let rows = sqlx::query(AssertSqlSafe(format!(
            "SELECT {TASK_FACT_COLS} FROM task_projection t WHERE t.run_id = $1"
        )))
        .bind(run_id.as_str())
        .fetch_all(&self.pool)
        .await?;
        let tasks = rows.iter().map(row_to_task_fact).collect();
        Ok((spec, tasks))
    }

    async fn runs_sample_facts(
        &self,
        run_ids: &[RunId],
    ) -> Result<HashMap<String, crate::store::runs::RunSampleFacts>, StoreError> {
        if run_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let ids: Vec<&str> = run_ids.iter().map(RunId::as_str).collect();
        let runs =
            sqlx::query("SELECT run_id, samples, dag FROM run_projection WHERE run_id = ANY($1)")
                .bind(&ids)
                .fetch_all(&self.pool);
        let tasks_sql = format!(
            "SELECT t.run_id, {TASK_FACT_COLS} FROM task_projection t WHERE t.run_id = ANY($1)"
        );
        let tasks = sqlx::query(AssertSqlSafe(tasks_sql)).bind(&ids).fetch_all(&self.pool);
        let (runs, tasks) =
            tokio::try_join!(runs.err_into::<StoreError>(), tasks.err_into::<StoreError>())?;
        let mut out = HashMap::new();
        for r in &runs {
            let spec = r
                .get::<Option<serde_json::Value>, _>("samples")
                .map(serde_json::from_value)
                .transpose()?;
            let dag: StoredDag = serde_json::from_value(r.get("dag"))?;
            out.insert(
                r.get::<String, _>("run_id"),
                crate::store::runs::RunSampleFacts {
                    spec,
                    tasks: vec![],
                    dag_nodes: dag.nodes,
                    dag_edges: dag.edges,
                },
            );
        }
        for t in &tasks {
            if let Some(facts) = out.get_mut(t.get::<&str, _>("run_id")) {
                facts.tasks.push(row_to_task_fact(t));
            }
        }
        Ok(out)
    }

    async fn run_sample_ids(&self, run_id: &RunId) -> Result<Vec<String>, StoreError> {
        let spec: Option<serde_json::Value> =
            sqlx::query_scalar("SELECT samples FROM run_projection WHERE run_id = $1")
                .bind(run_id.as_str())
                .fetch_optional(&self.pool)
                .await?
                .flatten();
        let spec = spec
            .map(serde_json::from_value::<biffix_core::event::SampleSpec>)
            .transpose()?
            .unwrap_or_default();
        let attributed: Vec<String> = sqlx::query_scalar(
            "SELECT DISTINCT sample FROM task_projection \
             WHERE run_id = $1 AND sample IS NOT NULL",
        )
        .bind(run_id.as_str())
        .fetch_all(&self.pool)
        .await?;
        Ok(biffix_core::sample_progress::sample_ids(
            &spec.ids,
            attributed.iter().map(String::as_str).chain(spec.reused.iter().map(|r| r.id.as_str())),
        ))
    }

    async fn run_state(&self, run_id: &RunId) -> Result<Option<String>, StoreError> {
        let row = sqlx::query("SELECT state FROM run_projection WHERE run_id = $1")
            .bind(run_id.as_str())
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(|r| r.get::<String, _>("state")))
    }

    async fn run_engine(&self, run_id: &RunId) -> Result<Option<String>, StoreError> {
        let row = sqlx::query(
            "SELECT w.engine FROM run_projection r \
             LEFT JOIN workflow_versions w ON r.workflow_version = w.id \
             WHERE r.run_id = $1",
        )
        .bind(run_id.as_str())
        .fetch_optional(&self.pool)
        .await?;
        // Outer row present but `engine` NULL (version not joined yet) collapses to None, same as
        // an unprojected run — both mean "engine unknown", which the caller defaults to Snakemake.
        Ok(row.and_then(|r| r.get::<Option<String>, _>("engine")))
    }

    async fn resume_origins(
        &self,
        run_id: &RunId,
        max_hops: usize,
    ) -> Result<Vec<ResumeOrigin>, StoreError> {
        // Walk `resumed_from` from the run; `seen` stops a cycle, `depth` the walk.
        let rows = sqlx::query(
            "WITH RECURSIVE chain(run_id, resumed_from, output_prefix, depth, seen) AS ( \
                 SELECT r.run_id, r.resumed_from, r.output_prefix, 0, ARRAY[r.run_id] \
                 FROM run_projection r WHERE r.run_id = $1 \
               UNION ALL \
                 SELECT r.run_id, r.resumed_from, r.output_prefix, c.depth + 1, c.seen || r.run_id \
                 FROM chain c JOIN run_projection r ON r.run_id = c.resumed_from \
                 WHERE c.depth < $2 AND r.run_id <> ALL(c.seen) \
             ) \
             SELECT run_id, output_prefix FROM chain WHERE depth > 0 ORDER BY depth",
        )
        .bind(run_id.as_str())
        .bind(i32::try_from(max_hops).unwrap_or(i32::MAX))
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .iter()
            .map(|r| ResumeOrigin {
                run_id: r.get::<String, _>("run_id").into(),
                output_prefix: r.get("output_prefix"),
            })
            .collect())
    }

    async fn run_stats_by_workflow(
        &self,
    ) -> Result<HashMap<String, crate::store::runs::RunStats>, StoreError> {
        let rows = sqlx::query(
            "SELECT workflow_version, COUNT(*)::bigint AS n, MAX(submitted_at) AS last_submitted \
             FROM run_projection WHERE workflow_version IS NOT NULL GROUP BY workflow_version",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| {
                let stats = crate::store::runs::RunStats {
                    count: r.get("n"),
                    last_submitted_micros: r.get("last_submitted"),
                };
                (r.get::<String, _>("workflow_version"), stats)
            })
            .collect())
    }
}

/// A run's detail from its row, tasks (possibly none), per-step counts and cost.
fn assemble_detail(
    run: &sqlx::postgres::PgRow,
    tasks: Vec<Task>,
    task_groups: Vec<crate::store::TaskGroupCount>,
    cost: Option<biffix_api::RunCost>,
) -> Result<RunDetailView, StoreError> {
    let dag: StoredDag = serde_json::from_value(run.get("dag"))?;

    Ok(RunDetailView {
        run: row_to_run_view(run),
        tasks,
        params: run.get("params"),
        coordinator_job_id: run.get::<Option<String>, _>("coordinator_job_id").map(Into::into),
        dag_nodes: dag.nodes,
        dag_edges: dag.edges,
        output_prefix: run.get("output_prefix"),
        coordinator_log_stream: run.get("coordinator_log_stream"),
        coordinator_stop: biffix_core::failure::FailedAttempt {
            exit_code: run.get::<Option<i32>, _>("coordinator_exit_code").map(i64::from),
            termination_category: run.get("coordinator_termination_category"),
            status_reason: run.get("coordinator_status_reason"),
            spot_warned: false,
        },
        coordinator_host: host_from_row(run, None),
        engine_session_id: run.get("engine_session_id"),
        session_mismatch: run.get("session_mismatch"),
        resumed_from: run.get::<Option<String>, _>("resumed_from").map(Into::into),
        input_tables: run.get("input_tables"),
        reference_bundle: run.get("reference_bundle"),
        // A stored retention this build cannot read is corrupt, not the default (`None`).
        retention: run
            .get::<Option<String>, _>("retention")
            .map(|r| {
                r.parse().map_err(|_| {
                    StoreError::Serialization(format!("unreadable stored retention {r:?}"))
                })
            })
            .transpose()?,
        work_deleted_at_micros: run.get("work_deleted_at"),
        launched_at_micros: run.get("launched_at"),
        cost,
        task_groups,
        cancelled_by: run.get("cancelled_by"),
        // From the same join as the workflow's name: absent when the registry lacks the version.
        workflow_registered_at_micros: run.get("workflow_created_at"),
    })
}
