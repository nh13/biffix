//! [`StatusStore`] for [`PgStore`]: the status probe's samples and a connectivity check.

use async_trait::async_trait;
use sqlx::Row;

use super::PgStore;
use crate::store::status::{StatusBucketView, StatusComponentView, StatusStore};
use crate::store::StoreError;

#[async_trait]
impl StatusStore for PgStore {
    async fn status_components(&self) -> Result<Vec<StatusComponentView>, StoreError> {
        // Latest sample per component. DISTINCT ON keeps the newest row per component (Postgres).
        let rows = sqlx::query(
            "SELECT * FROM (SELECT DISTINCT ON (component) component, status, detail, next_within_secs, \
             EXTRACT(EPOCH FROM sampled_at)::float8 AS at \
             FROM status_samples ORDER BY component, sampled_at DESC) latest \
             ORDER BY component COLLATE \"C\"",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| StatusComponentView {
                name: r.get("component"),
                status: r.get("status"),
                detail: r.get("detail"),
                sampled_at: r.get("at"),
                next_within_secs: r.get("next_within_secs"),
            })
            .collect())
    }

    async fn status_history(
        &self,
        window_secs: i64,
        bucket_secs: i64,
    ) -> Result<Vec<StatusBucketView>, StoreError> {
        let window_secs = window_secs.clamp(60, 90 * 86400) as f64;
        let bucket_secs = bucket_secs.clamp(60, 86400) as f64;
        // Bucket to `bucket_secs` aligned to the epoch origin (date_bin) so bucket starts are stable
        // integers the client can match without timezone ambiguity.
        // The tiers' spellings are the API's (`HealthStatus::as_str`), bound, not spelled here.
        let rows = sqlx::query(
            "SELECT EXTRACT(EPOCH FROM \
               date_bin(make_interval(secs => $2), sampled_at, TIMESTAMPTZ 'epoch'))::bigint AS bucket, \
             component, \
             COUNT(*) FILTER (WHERE status = $3) AS ok, \
             COUNT(*) FILTER (WHERE status = $4) AS degraded, \
             COUNT(*) FILTER (WHERE status = $5) AS down \
             FROM status_samples \
             WHERE sampled_at > now() - make_interval(secs => $1) \
             GROUP BY 1, 2 ORDER BY 1, component COLLATE \"C\"",
        )
        .bind(window_secs)
        .bind(bucket_secs)
        .bind(biffix_api::HealthStatus::Ok.as_str())
        .bind(biffix_api::HealthStatus::Degraded.as_str())
        .bind(biffix_api::HealthStatus::Down.as_str())
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| StatusBucketView {
                bucket: r.get("bucket"),
                component: r.get("component"),
                ok: r.get("ok"),
                degraded: r.get("degraded"),
                down: r.get("down"),
            })
            .collect())
    }

    async fn health_check(&self) -> Result<(), StoreError> {
        // Trivial connectivity probe — no projection scan.
        sqlx::query("SELECT 1").execute(&self.pool).await?;
        Ok(())
    }
}
