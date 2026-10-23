//! [`OutputStore`] for [`PgStore`]: a run's outputs (`run_output`, with the S3 HEADs cached in
//! `s3_object_meta`) and the output-prefix claim.

use async_trait::async_trait;
// `AssertSqlSafe` marks query text assembled with `format!`: every interpolated piece is a code
// constant (column lists, enum-derived literals, `$n` placeholders), never a runtime value; values
// are always bound.
use sqlx::{AssertSqlSafe, Row};

use biffix_core::ids::RunId;

use super::PgStore;
use crate::store::outputs::{
    ObjectMeta, OutputPage, OutputQuery, OutputRow, OutputScope, OutputStore,
    MISSING_RECHECK_MICROS,
};
use crate::store::StoreError;

/// Map a `run_output` row.
fn output_row(r: &sqlx::postgres::PgRow) -> OutputRow {
    OutputRow {
        id: r.get("id"),
        s3_uri: r.get("s3_uri"),
        path: r.get("path"),
        task_key: r.get("task_key"),
        attempt: r.get::<Option<i32>, _>("attempt").map(i64::from),
        role: r.get("role"),
        reused: r.get("reused"),
        size: r.get("size"),
        etag: r.get("etag"),
        run_id: r.get::<String, _>("run_id").into(),
        meta: r.get::<Option<bool>, _>("meta_present").map(|present| ObjectMeta {
            present,
            size: r.get("meta_size"),
            etag: r.get("meta_etag"),
            last_modified: r.get("meta_last_modified"),
            checked_at: r.get("meta_checked_at"),
        }),
    }
}

/// The `run_output` columns plus its cached HEAD, for [`output_row`]. `o` is `run_output`, `m` the
/// LEFT JOINed `s3_object_meta`.
const OUTPUT_COLUMNS: &str =
    "o.run_id, o.id, o.s3_uri, o.path, o.task_key, o.attempt, o.role, o.reused, \
     o.size, o.etag, m.present AS meta_present, m.size AS meta_size, m.etag AS meta_etag, \
     m.last_modified AS meta_last_modified, m.checked_at AS meta_checked_at";

/// The [`OutputScope`] predicate on `run_output o`, with `$1` its runs (`text[]`, the run itself
/// first) and `$<prefix>` its shared prefix.
///
/// A lone run's is the plain `o.run_id = ($1::text[])[1]`, which leaves `$<prefix>` unreferenced:
/// bind it with [`bind_shared_prefix`], which binds nothing then (Postgres cannot type a parameter
/// no clause names). Equality on the leading column keeps the ordered scan of
/// `run_output_uri_bytes` (`run_id, s3_uri COLLATE "C"`), so a page stops at its `LIMIT`.
///
/// A resume chain's: a row of the run itself, or of a run it resumed under the shared prefix, that
/// no nearer run's row for the same URI hides (a hiding row is under the prefix too, being the
/// same URI). The hiding probe names the scope's runs (`n.run_id = ANY($1)`), never a slice built
/// from the outer row, so the planner reads only the scope's rows (an index probe per row, or a
/// hash anti-join over the scope's index entries) rather than the whole table.
fn output_scope_sql(scope: &OutputScope, prefix: usize) -> String {
    if scope.is_lone() {
        return "o.run_id = ($1::text[])[1]".to_string();
    }
    format!(
        "o.run_id = ANY($1::text[]) \
         AND (o.run_id = ($1::text[])[1] OR starts_with(o.s3_uri, ${prefix}::text)) \
         AND NOT EXISTS (SELECT 1 FROM run_output n \
             WHERE n.run_id = ANY($1::text[]) \
               AND n.s3_uri COLLATE \"C\" = o.s3_uri COLLATE \"C\" \
               AND array_position($1::text[], n.run_id) < array_position($1::text[], o.run_id))"
    )
}

/// The output list's filter: [`output_scope_sql`] plus `$2` task, `$3` roles (NULL ⇒ all but
/// intermediate; holding `store::outputs::ALL_ROLES` ⇒ all) and `$4` URI prefix, with the shared prefix
/// `$<prefix>`. One predicate for the page and the counts.
fn output_filter_sql(scope: &OutputScope, prefix: usize) -> String {
    format!(
        "{} \
         AND ($2::text IS NULL OR o.task_key = $2) \
         AND (CASE WHEN $3::text[] IS NULL THEN o.role <> 'intermediate' \
                   ELSE o.role = ANY($3) OR '*' = ANY($3) END) \
         AND ($4::text IS NULL OR starts_with(o.s3_uri, $4))",
        output_scope_sql(scope, prefix)
    )
}

/// The output list's `FROM`: `run_output o` with its cached HEAD `m`.
const OUTPUTS_FROM: &str = "run_output o LEFT JOIN s3_object_meta m ON m.s3_uri = o.s3_uri";

/// A page of `scope`'s outputs ([`OutputStore::list_outputs_in`]): [`output_filter_sql`] with
/// `$5` the keyset cursor, `$6` the page size and `$7` the shared prefix. Public only so a
/// Postgres test can check its plan (a lone run's page is an ordered index scan).
#[doc(hidden)]
pub fn output_page_sql(scope: &OutputScope) -> String {
    format!(
        "SELECT {OUTPUT_COLUMNS} FROM {OUTPUTS_FROM} \
         WHERE {} AND o.s3_uri COLLATE \"C\" > $5 \
         ORDER BY o.s3_uri COLLATE \"C\" LIMIT $6",
        output_filter_sql(scope, 7)
    )
}

/// An [`OutputScope`]'s runs as the `text[]` [`output_scope_sql`] binds as `$1`.
fn scope_runs(scope: &OutputScope) -> Vec<&str> {
    scope.runs().iter().map(RunId::as_str).collect()
}

/// Bind `scope`'s shared prefix as the last parameter of a query built on [`output_scope_sql`]:
/// nothing for a lone run, whose predicate does not name it.
fn bind_shared_prefix<'q>(
    query: sqlx::query::Query<'q, sqlx::Postgres, sqlx::postgres::PgArguments>,
    scope: &'q OutputScope,
) -> sqlx::query::Query<'q, sqlx::Postgres, sqlx::postgres::PgArguments> {
    match scope.shared_prefix() {
        Some(prefix) if !scope.is_lone() => query.bind(prefix),
        _ => query,
    }
}

#[async_trait]
impl OutputStore for PgStore {
    async fn list_outputs_in(
        &self,
        scope: &OutputScope,
        query: &OutputQuery,
    ) -> Result<OutputPage, StoreError> {
        // One filter for the page and the counts ([`output_filter_sql`]): $1 the scope's runs, $2
        // task, $3 roles, $4 uri prefix, and the scope's shared prefix last ($7 in the page, $8 in
        // the counts; unbound for a lone run).
        let runs = scope_runs(scope);
        let page_sql = output_page_sql(scope);
        let page = sqlx::query(AssertSqlSafe(page_sql))
            .bind(&runs)
            .bind(query.task.as_deref())
            .bind(query.roles.as_deref())
            .bind(query.uri_prefix.as_deref())
            // URIs are non-empty, so `> ''` matches everything on the first page.
            .bind(query.after.as_deref().unwrap_or(""))
            .bind(query.limit.max(0));
        let rows = bind_shared_prefix(page, scope).fetch_all(&self.pool).await?;
        // `unchecked` mirrors `store::outputs::needs_head` (never checked, or a stale "missing") over the
        // objects `store::outputs::checkable` admits: $6 is `s3://<bucket>/`, $7 the staged-source prefix
        // under it; a NULL bucket makes nothing checkable.
        let counts_sql = format!(
            "SELECT count(*) AS total, \
                    count(*) FILTER (WHERE (m.s3_uri IS NULL \
                            OR (NOT m.present AND m.checked_at <= $5)) \
                        AND starts_with(o.s3_uri, $6) AND length(o.s3_uri) > length($6) \
                        AND NOT starts_with(o.s3_uri, $7) AND right(o.s3_uri, 1) <> '/') \
                        AS unchecked \
             FROM {OUTPUTS_FROM} WHERE {}",
            output_filter_sql(scope, 8)
        );
        let counts = sqlx::query(AssertSqlSafe(counts_sql))
            .bind(&runs)
            .bind(query.task.as_deref())
            .bind(query.roles.as_deref())
            .bind(query.uri_prefix.as_deref())
            .bind(biffix_core::clock::epoch_micros() - MISSING_RECHECK_MICROS)
            .bind(
                query
                    .checkable_bucket
                    .as_deref()
                    .filter(|b| !b.is_empty())
                    .map(|b| format!("s3://{b}/")),
            )
            .bind(
                query
                    .checkable_bucket
                    .as_deref()
                    .map(|b| format!("s3://{b}/{}", crate::aws::objects::SOURCE_PREFIX)),
            );
        let counts = bind_shared_prefix(counts, scope).fetch_one(&self.pool).await?;
        Ok(OutputPage {
            rows: rows.iter().map(output_row).collect(),
            total: counts.get("total"),
            unchecked: counts.get("unchecked"),
        })
    }

    async fn outputs_like_in(
        &self,
        scope: &OutputScope,
        patterns: &[String],
        limit: i64,
    ) -> Result<Vec<OutputRow>, StoreError> {
        if patterns.is_empty() {
            return Ok(Vec::new());
        }
        let roles = crate::store::outputs::REPORT_ROLES.map(|r| format!("'{r}'")).join(", ");
        let sql = format!(
            "SELECT {OUTPUT_COLUMNS} \
             FROM {OUTPUTS_FROM} \
             WHERE {} AND o.role IN ({roles}) \
               AND o.s3_uri LIKE ANY($2) \
             ORDER BY o.s3_uri COLLATE \"C\" LIMIT $3",
            output_scope_sql(scope, 4)
        );
        let q = sqlx::query(AssertSqlSafe(sql))
            .bind(scope_runs(scope))
            .bind(patterns)
            .bind(limit.max(0));
        let rows = bind_shared_prefix(q, scope).fetch_all(&self.pool).await?;
        Ok(rows.iter().map(output_row).collect())
    }

    async fn get_output_in(
        &self,
        scope: &OutputScope,
        id: &str,
    ) -> Result<Option<OutputRow>, StoreError> {
        // An id names one run's row (it hashes the run id), so at most one row matches.
        let sql = format!(
            "SELECT {OUTPUT_COLUMNS} FROM {OUTPUTS_FROM} \
             WHERE {} AND o.id = $2",
            output_scope_sql(scope, 3)
        );
        let q = sqlx::query(AssertSqlSafe(sql)).bind(scope_runs(scope)).bind(id);
        let row = bind_shared_prefix(q, scope).fetch_optional(&self.pool).await?;
        Ok(row.as_ref().map(output_row))
    }

    async fn output_at_in(
        &self,
        scope: &OutputScope,
        uri: &str,
    ) -> Result<Option<OutputRow>, StoreError> {
        // The scope keeps only the nearest run's row for a URI, so at most one row matches; the
        // `COLLATE "C"` equality probes `run_output_uri_bytes` once per scope run.
        let sql = format!(
            "SELECT {OUTPUT_COLUMNS} FROM {OUTPUTS_FROM} \
             WHERE {} AND o.s3_uri COLLATE \"C\" = $2::text COLLATE \"C\"",
            output_scope_sql(scope, 3)
        );
        let q = sqlx::query(AssertSqlSafe(sql)).bind(scope_runs(scope)).bind(uri);
        let row = bind_shared_prefix(q, scope).fetch_optional(&self.pool).await?;
        Ok(row.as_ref().map(output_row))
    }

    async fn put_object_meta(&self, metas: &[(String, ObjectMeta)]) -> Result<(), StoreError> {
        if metas.is_empty() {
            return Ok(());
        }
        let uris: Vec<&str> = metas.iter().map(|(u, _)| u.as_str()).collect();
        let present: Vec<bool> = metas.iter().map(|(_, m)| m.present).collect();
        let sizes: Vec<Option<i64>> = metas.iter().map(|(_, m)| m.size).collect();
        let etags: Vec<Option<&str>> = metas.iter().map(|(_, m)| m.etag.as_deref()).collect();
        let modified: Vec<Option<i64>> = metas.iter().map(|(_, m)| m.last_modified).collect();
        let checked: Vec<i64> = metas.iter().map(|(_, m)| m.checked_at).collect();
        sqlx::query(
            "INSERT INTO s3_object_meta (s3_uri, present, size, etag, last_modified, checked_at) \
             SELECT * FROM UNNEST($1::text[], $2::bool[], $3::bigint[], $4::text[], \
                                  $5::bigint[], $6::bigint[]) \
             ON CONFLICT (s3_uri) DO UPDATE SET present = EXCLUDED.present, \
                size = EXCLUDED.size, etag = EXCLUDED.etag, \
                last_modified = EXCLUDED.last_modified, checked_at = EXCLUDED.checked_at \
             WHERE s3_object_meta.checked_at <= EXCLUDED.checked_at",
        )
        .bind(&uris)
        .bind(&present)
        .bind(&sizes)
        .bind(&etags)
        .bind(&modified)
        .bind(&checked)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn active_run_in_prefix(&self, output_prefix: &str) -> Result<Option<RunId>, StoreError> {
        // Not ended: not in a terminal state (RunFilter::Completed's, from the Rust-side table).
        let ended = super::runs::filter_sql(crate::store::runs::RunFilter::Completed);
        let row = sqlx::query(AssertSqlSafe(format!(
            "SELECT run_id FROM run_projection r \
             WHERE r.output_prefix = $1 AND NOT ({ended}) \
             ORDER BY r.run_id COLLATE \"C\" LIMIT 1"
        )))
        .bind(output_prefix)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| RunId::new(r.get::<String, _>("run_id"))))
    }

    async fn claim_output_prefix(
        &self,
        output_prefix: &str,
    ) -> Result<Option<crate::store::Claim>, StoreError> {
        // A transaction-scoped advisory lock on the prefix, held by an open transaction the claim
        // owns: dropping the claim rolls the transaction back, which releases the lock (also if
        // the handler holding it errors out or its connection dies). The key space is shared with
        // the ingest single-writer lock, so the prefix hash is namespaced by a salt. The connection
        // sits idle in transaction while the claim is held: `crate::store::Claim` says why that is
        // accepted and what bounds it.
        let mut tx = self.pool.begin().await?;
        let claimed: bool = sqlx::query_scalar(
            "SELECT pg_try_advisory_xact_lock(hashtextextended('biffix:output-prefix:' || $1, 0))",
        )
        .bind(output_prefix)
        .fetch_one(&mut *tx)
        .await?;
        Ok(claimed.then(|| crate::store::Claim::new(tx)))
    }
}
