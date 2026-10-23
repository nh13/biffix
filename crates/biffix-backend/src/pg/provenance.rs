//! [`ProvenanceStore`] for [`PgStore`]: the stale provenance copies (from `run_events`) and
//! `run_provenance`.

use async_trait::async_trait;
// `AssertSqlSafe` marks query text assembled with `format!`: every interpolated piece is a code
// constant (column lists, enum-derived literals, `$n` placeholders), never a runtime value; values
// are always bound.
use sqlx::{AssertSqlSafe, Row};

use biffix_core::ids::RunId;

use super::runs::filter_sql;
use super::PgStore;
use crate::store::provenance::ProvenanceStore;
use crate::store::runs::RunFilter;
use crate::store::StoreError;

#[async_trait]
impl ProvenanceStore for PgStore {
    async fn provenance_due(
        &self,
        settle_secs: i64,
        limit: i64,
    ) -> Result<Vec<(RunId, i64)>, StoreError> {
        // Due means the log has an event past the one the written crate describes: the run's
        // newest `seq` (one backward probe of `run_events_run_seq` per ended run) against
        // `written_seq`. Both are log positions, so nothing here compares the row's write time with
        // the writer's clock (which could miss an event folded while a crate was being written), and
        // a replay, which rewrites every row but no seq, makes nothing due. Only the due runs have
        // their newest arrival looked up for the settle window.
        let ended = filter_sql(RunFilter::Completed);
        let rows = sqlx::query(AssertSqlSafe(format!(
            r#"
            WITH cand AS (
                SELECT r.run_id, p.written_seq, COALESCE(f.attempts, 0) AS attempts,
                       (SELECT max(e.seq) FROM run_events e WHERE e.run_id = r.run_id) AS max_seq
                  FROM run_projection r
                  LEFT JOIN run_provenance p ON p.run_id = r.run_id
                  LEFT JOIN run_provenance_failure f ON f.run_id = r.run_id
                 WHERE {ended}
                   AND (f.retry_after IS NULL
                        OR f.retry_after <= (extract(epoch FROM now()) * 1000000)::bigint)
            )
            SELECT c.run_id, c.max_seq
              FROM cand c
              JOIN LATERAL (
                   SELECT max(received_at) AS last_at
                     FROM run_events e WHERE e.run_id = c.run_id
              ) m ON true
             WHERE c.max_seq > COALESCE(c.written_seq, 0)
               AND m.last_at < now() - make_interval(secs => $1)
             ORDER BY c.attempts, m.last_at, c.run_id COLLATE "C"
             LIMIT $2
            "#
        )))
        .bind(settle_secs as f64)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .iter()
            .map(|r| (RunId::new(r.get::<String, _>("run_id")), r.get("max_seq")))
            .collect())
    }

    async fn mark_provenance_failed(
        &self,
        run_id: &RunId,
        at_micros: i64,
    ) -> Result<(), StoreError> {
        // One statement (the count read and bumped in place); the wait is
        // `provenance_retry_after`'s, from the count after this failure.
        use crate::store::provenance::{PROVENANCE_RETRY_BASE_MICROS, PROVENANCE_RETRY_MAX_MICROS};
        sqlx::query(
            "INSERT INTO run_provenance_failure (run_id, attempts, retry_after, failed_at) \
             VALUES ($1, 1, $2, $2) \
             ON CONFLICT (run_id) DO UPDATE SET \
               attempts = run_provenance_failure.attempts + 1, \
               retry_after = $2 + LEAST($3::bigint << LEAST(run_provenance_failure.attempts - 1, 20), \
                                        $4::bigint), \
               failed_at = $2",
        )
        .bind(run_id.as_str())
        .bind(at_micros)
        .bind(PROVENANCE_RETRY_BASE_MICROS)
        .bind(PROVENANCE_RETRY_MAX_MICROS)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn mark_provenance(
        &self,
        run_id: &RunId,
        seq: i64,
        uri: &str,
        at_micros: i64,
    ) -> Result<(), StoreError> {
        sqlx::query(
            "WITH written AS (DELETE FROM run_provenance_failure WHERE run_id = $1) \
             INSERT INTO run_provenance (run_id, written_seq, uri, written_at) \
             VALUES ($1, $2, $3, $4) \
             ON CONFLICT (run_id) DO UPDATE SET written_seq = EXCLUDED.written_seq, \
               uri = EXCLUDED.uri, written_at = EXCLUDED.written_at",
        )
        .bind(run_id.as_str())
        .bind(seq)
        .bind(uri)
        .bind(at_micros)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}
