//! The Postgres-backed [`ExperimentStore`] (the `experiments` table).

use async_trait::async_trait;
// `AssertSqlSafe` marks query text assembled with `format!`: every interpolated piece is a code
// constant (column lists, enum-derived literals, `$n` placeholders), never a runtime value; values
// are always bound.
use sqlx::{AssertSqlSafe, PgPool, Row};

use crate::store::experiments::{
    name_conflict, ExperimentChange, ExperimentRecord, ExperimentStore,
};
use std::collections::HashMap;

use biffix_api::RunSummary;

use super::notifications::undismissed_failed_of_run;
use super::PgStore;
use crate::store::experiments::ExperimentReads;
use crate::store::runs::{RunPage, RunQuery};
use crate::store::StoreError;

/// Postgres-backed [`ExperimentStore`].
pub struct PgExperimentStore {
    pool: PgPool,
}

impl PgExperimentStore {
    /// A store over `pool`.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

const COLUMNS: &str =
    "id, project, name, description, created_by, created_by_label, created_at, archived_at";

fn row_to_record(r: &sqlx::postgres::PgRow) -> ExperimentRecord {
    ExperimentRecord {
        id: r.get("id"),
        project: r.get("project"),
        name: r.get("name"),
        description: r.get("description"),
        created_by: r.get("created_by"),
        created_by_label: r.get("created_by_label"),
        created_at_micros: r.get("created_at"),
        archived_at_micros: r.get("archived_at"),
    }
}

/// Whether `e` is a violation of the live-name unique index (`experiments_live_name`), as opposed to
/// the primary key.
///
/// Case folding: the index compares Postgres `lower(name)`, while
/// [`crate::testing::MemExperimentStore`] compares Rust `str::to_lowercase`. They agree exactly for ASCII names, which is the guarantee:
/// "Batch" and "batch" clash in both. Beyond ASCII they can differ (Postgres folds by the database's
/// collation/ctype, Rust by full Unicode rules, e.g. for 'ẞ' or Greek final sigma), so a non-ASCII
/// pair may clash in one store and not the other; Postgres is the deployed rule.
fn is_name_clash(e: &sqlx::Error) -> bool {
    matches!(e, sqlx::Error::Database(db)
        if db.is_unique_violation() && db.constraint() == Some("experiments_live_name"))
}

#[async_trait]
impl ExperimentStore for PgExperimentStore {
    async fn list(
        &self,
        project: Option<&str>,
        include_archived: bool,
    ) -> Result<Vec<ExperimentRecord>, StoreError> {
        let rows = sqlx::query(AssertSqlSafe(format!(
            "SELECT {COLUMNS} FROM experiments \
             WHERE ($1::text IS NULL OR project = $1) AND ($2 OR archived_at IS NULL)"
        )))
        .bind(project)
        .bind(include_archived)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.iter().map(row_to_record).collect())
    }

    async fn get(&self, id: &str) -> Result<ExperimentRecord, StoreError> {
        let row =
            sqlx::query(AssertSqlSafe(format!("SELECT {COLUMNS} FROM experiments WHERE id = $1")))
                .bind(id)
                .fetch_optional(&self.pool)
                .await?
                .ok_or(StoreError::NotFound)?;
        Ok(row_to_record(&row))
    }

    async fn create(&self, e: &ExperimentRecord) -> Result<(), StoreError> {
        sqlx::query(AssertSqlSafe(format!(
            "INSERT INTO experiments ({COLUMNS}) VALUES ($1,$2,$3,$4,$5,$6,$7,$8)"
        )))
        .bind(&e.id)
        .bind(&e.project)
        .bind(&e.name)
        .bind(&e.description)
        .bind(&e.created_by)
        .bind(&e.created_by_label)
        .bind(e.created_at_micros)
        .bind(e.archived_at_micros)
        .execute(&self.pool)
        .await
        .map_err(|err| {
            if is_name_clash(&err) {
                name_conflict(&e.project, &e.name)
            } else {
                StoreError::from(err)
            }
        })?;
        Ok(())
    }

    async fn update(
        &self,
        id: &str,
        change: &ExperimentChange,
    ) -> Result<ExperimentRecord, StoreError> {
        // $2 name (NULL keeps it); $3 whether to set the description, $4 to what; $5 archive
        // (true), restore (false) or keep (NULL), $6 the archive time. An archived experiment
        // archived again keeps its time, as the in-memory store does.
        let res = sqlx::query(AssertSqlSafe(format!(
            "UPDATE experiments SET \
               name = COALESCE($2, name), \
               description = CASE WHEN $3 THEN $4 ELSE description END, \
               archived_at = CASE WHEN $5::boolean IS NULL THEN archived_at \
                                  WHEN $5 THEN COALESCE(archived_at, $6) ELSE NULL END \
             WHERE id = $1 RETURNING {COLUMNS}"
        )))
        .bind(id)
        .bind(change.name.as_deref())
        .bind(change.description.is_some())
        .bind(change.description.as_ref().and_then(Option::as_deref))
        .bind(change.archived)
        .bind(change.now_micros)
        .fetch_optional(&self.pool)
        .await;
        match res {
            Ok(Some(row)) => Ok(row_to_record(&row)),
            Ok(None) => Err(StoreError::NotFound),
            Err(err) if is_name_clash(&err) => {
                // Name the clash as the result would have had it.
                let current = self.get(id).await?;
                let name = change.name.clone().unwrap_or(current.name);
                Err(name_conflict(&current.project, &name))
            }
            Err(err) => Err(StoreError::from(err)),
        }
    }
}

#[async_trait]
impl ExperimentReads for PgStore {
    async fn experiment_runs(
        &self,
        experiment_id: &str,
        limit: i64,
    ) -> Result<Vec<RunSummary>, StoreError> {
        let query =
            RunQuery { experiment: Some(experiment_id.to_string()), limit, ..Default::default() };
        Ok(self.run_page(&query, false).await?.runs)
    }

    async fn experiment_run_page(
        &self,
        experiment_id: &str,
        after: Option<String>,
    ) -> Result<RunPage, StoreError> {
        let query = RunQuery {
            experiment: Some(experiment_id.to_string()),
            limit: crate::store::runs::MAX_RUN_PAGE,
            after,
            ..Default::default()
        };
        self.run_page(&query, false).await
    }

    async fn experiment_run_stats(
        &self,
        experiment_ids: &[String],
    ) -> Result<HashMap<String, crate::store::runs::RunStats>, StoreError> {
        if experiment_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let rows = sqlx::query(
            "SELECT experiment_id, COUNT(*)::bigint AS n, MAX(submitted_at) AS last_submitted \
             FROM run_projection WHERE experiment_id = ANY($1) GROUP BY experiment_id",
        )
        .bind(experiment_ids)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| {
                let stats = crate::store::runs::RunStats {
                    count: r.get("n"),
                    last_submitted_micros: r.get("last_submitted"),
                };
                (r.get::<String, _>("experiment_id"), stats)
            })
            .collect())
    }

    async fn experiment_runs_of(
        &self,
        submitter: &str,
        user: &str,
        experiment: Option<&str>,
    ) -> Result<HashMap<String, crate::store::experiments::SubmitterRuns>, StoreError> {
        // Reads the partial `run_projection(submitter, experiment_id)` index: the
        // submitter's experiment runs alone, whatever everyone else has in experiments.
        let sql = format!(
            "SELECT r.experiment_id, COUNT(*)::bigint AS n, \
               (COUNT(*) FILTER (WHERE r.state = 'failed' AND {}))::bigint AS attention \
             FROM run_projection r \
             WHERE r.experiment_id IS NOT NULL AND r.submitter = $1 \
               AND ($3::text IS NULL OR r.experiment_id = $3) \
             GROUP BY r.experiment_id",
            undismissed_failed_of_run("$2")
        );
        let rows = sqlx::query(AssertSqlSafe(sql))
            .bind(submitter)
            .bind(user)
            .bind(experiment)
            .fetch_all(&self.pool)
            .await?;
        Ok(rows
            .into_iter()
            .map(|r| {
                let runs = crate::store::experiments::SubmitterRuns {
                    runs: r.get("n"),
                    attention: r.get("attention"),
                };
                (r.get::<String, _>("experiment_id"), runs)
            })
            .collect())
    }

    async fn experiment_priced_usd(&self, experiment_id: &str) -> Result<Option<f64>, StoreError> {
        // Every priced job of every run in it, summed as `run_cost` sums a run's: Σcompute + Σebs.
        let row = sqlx::query(
            "SELECT count(*) AS n, \
                    (COALESCE(sum(c.compute_usd), 0) + COALESCE(sum(c.ebs_usd), 0))::float8 AS usd \
             FROM attempt_cost c JOIN run_projection r ON r.run_id = c.run_id \
             WHERE r.experiment_id = $1",
        )
        .bind(experiment_id)
        .fetch_one(&self.pool)
        .await?;
        Ok((row.get::<i64, _>("n") > 0).then(|| row.get("usd")))
    }
}
