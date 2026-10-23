//! The retention sweep's queries: the Nextflow work dirs due for deletion.

use biffix_core::event::Retention;
use biffix_core::state::RunState;
use sqlx::postgres::PgPool;
// `AssertSqlSafe`: the query text is assembled from code constants (enum spellings) only.
use sqlx::{AssertSqlSafe, Row};

use biffix_db::rules::ended_run_states;
use biffix_db::DbError;

/// The work-dir groups due for deletion, as SQL: `$1` = the retention cutoff (NULL keeps work dirs
/// forever), `$2` = the limit, `$3` = one output prefix to judge alone (NULL for every group), `$4`
/// = the output prefix to list after (NULL from the first), in output-prefix order.
///
/// A group is admitted only when one of its runs owns the prefix in exactly the default per-run
/// shape `s3://<bucket>/biffix/runs/<run_id>/` (`biffix_core::s3_layout::RUNS_PREFIX`), compared by
/// equality rather than `LIKE`, so a run id's `_`/`%` are not wildcards and a nested prefix such as
/// `s3://b/team/biffix/runs/<id>/` is not admitted. Every row this returns therefore passes
/// `retention::work_location`, and none can hold a slot of the bounded page forever.
///
/// The run states and retention values are their enums' spellings. The workflow engine's
/// (`nextflow`) is spelled out: its enum is `biffix_workflow`'s, which this crate does not link.
fn work_due_sql() -> String {
    let ended =
        ended_run_states().iter().map(|state| format!("'{state}'")).collect::<Vec<_>>().join(", ");
    format!(
        r"
        SELECT r.output_prefix,
               array_agg(r.run_id ORDER BY r.run_id) FILTER (WHERE r.work_deleted_at IS NULL)
                 AS run_ids
          FROM run_projection r
          JOIN workflow_versions w ON w.id = r.workflow_version
         WHERE w.engine = 'nextflow'
           AND r.output_prefix LIKE 's3://%/biffix/runs/%/'
           AND ($3::TEXT IS NULL OR r.output_prefix = $3)
           AND ($4::TEXT IS NULL OR r.output_prefix > $4)
         GROUP BY r.output_prefix
        HAVING bool_or(r.work_deleted_at IS NULL)
           AND bool_or(r.output_prefix = 's3://' || split_part(substr(r.output_prefix, 6), '/', 1)
                                         || '/biffix/runs/' || r.run_id || '/')
           AND bool_and(r.state IN ({ended}) AND r.completed_at IS NOT NULL)
           AND bool_and(COALESCE(r.retention, '{default}') <> '{keep}')
           AND (($1::BIGINT IS NOT NULL AND max(r.completed_at) <= $1)
                OR ((array_agg(COALESCE(r.retention, '{default}')
                               ORDER BY r.completed_at DESC, r.run_id DESC))[1]
                      = '{delete_on_success}'
                    AND (array_agg(r.state ORDER BY r.completed_at DESC, r.run_id DESC))[1]
                      = '{succeeded}'))
         ORDER BY r.output_prefix
         LIMIT $2
        ",
        default = Retention::Default.as_str(),
        keep = Retention::Keep.as_str(),
        delete_on_success = Retention::DeleteOnSuccess.as_str(),
        succeeded = RunState::Succeeded.as_str(),
    )
}

/// Nextflow work dirs due for deletion, at most `limit`, in output-prefix order after `after` (the
/// last one a previous page held; `None` from the first). The sweep pages through them a tick at a
/// time and wraps around, so a work dir whose sweep fails every tick (its bucket is gone, say)
/// takes one slot per pass rather than one in every tick's page. A work dir is shared by a run and
/// every run that resumed it, so it is judged as a group: every run that uses it has ended, none
/// asked to keep it, and either the newest one succeeded asking for deletion on success, or the
/// newest ended at or before `cutoff_micros` (the deploy's retention period; `None` keeps them
/// forever). Only a run's own default prefix is swept (one of the group's runs owns it exactly, see
/// `work_due_sql`); groups whose deletion is already recorded for every run are skipped.
pub async fn work_due_for_deletion(
    pool: &PgPool,
    cutoff_micros: Option<i64>,
    limit: i64,
    after: Option<&str>,
) -> Result<Vec<crate::retention::WorkDue>, DbError> {
    let rows = sqlx::query(AssertSqlSafe(work_due_sql()))
        .bind(cutoff_micros)
        .bind(limit.max(0))
        .bind(None::<String>)
        .bind(after)
        .fetch_all(pool)
        .await?;
    Ok(rows
        .into_iter()
        .map(|r| crate::retention::WorkDue {
            output_prefix: r.get("output_prefix"),
            run_ids: r.get("run_ids"),
        })
        .collect())
}

/// Whether the work dir at `output_prefix` is still due for deletion now, by the same rule as
/// [`work_due_for_deletion`]. The sweep asks before each delete batch: a run that resumed the work
/// dir after the due list was read (it shares the dir, and is not ended) makes the group not due,
/// so its `work/` is never deleted under it.
pub async fn work_still_due(
    pool: &PgPool,
    output_prefix: &str,
    cutoff_micros: Option<i64>,
) -> Result<bool, DbError> {
    Ok(sqlx::query(AssertSqlSafe(work_due_sql()))
        .bind(cutoff_micros)
        .bind(1_i64)
        .bind(output_prefix)
        .bind(None::<String>)
        .fetch_optional(pool)
        .await?
        .is_some())
}
