//! [`StatusStore`] for [`PgStore`]: the status probe's samples and a connectivity check.

use async_trait::async_trait;
use sqlx::Row;

use super::PgStore;
use crate::store::status::{
    StatusBucketView, StatusCarriedView, StatusComponentView, StatusStore, GRACE_SECS,
    UNPROMISED_SECS,
};
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
        // Each sample promises the next within its `next_within_secs` (the hour without one); a
        // sample that came later than the one before it promised, plus the grace, marks a gap. The
        // samples are read from one promise (plus grace) before the window, so the window's first
        // sample is judged against the one before it.
        let rows = sqlx::query(
            "WITH s AS (SELECT sampled_at, component, status, \
               EXTRACT(EPOCH FROM sampled_at)::float8 AS at, \
               EXTRACT(EPOCH FROM sampled_at)::float8 + COALESCE(next_within_secs::float8, $6::float8) AS until \
             FROM status_samples \
             WHERE sampled_at > now() - make_interval(secs => $1 + $6::float8 + $7::float8)), \
             w AS (SELECT *, LAG(until) OVER (PARTITION BY component ORDER BY sampled_at) AS prev_until \
             FROM s) \
             SELECT EXTRACT(EPOCH FROM \
               date_bin(make_interval(secs => $2), sampled_at, TIMESTAMPTZ 'epoch'))::bigint AS bucket, \
             component, \
             COUNT(*) FILTER (WHERE status = $3) AS ok, \
             COUNT(*) FILTER (WHERE status = $4) AS degraded, \
             COUNT(*) FILTER (WHERE status = $5) AS down, \
             floor((array_agg(until ORDER BY sampled_at DESC))[1])::bigint AS covered_until, \
             bool_or(prev_until IS NULL OR at > prev_until + $7::float8) AS lapsed, \
             (array_agg(status ORDER BY sampled_at DESC))[1] AS last \
             FROM w \
             WHERE sampled_at > now() - make_interval(secs => $1) \
             GROUP BY 1, 2 ORDER BY 1, component COLLATE \"C\"",
        )
        .bind(window_secs)
        .bind(bucket_secs)
        .bind(biffix_api::HealthStatus::Ok.as_str())
        .bind(biffix_api::HealthStatus::Degraded.as_str())
        .bind(biffix_api::HealthStatus::Down.as_str())
        .bind(UNPROMISED_SECS as f64)
        .bind(GRACE_SECS as f64)
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
                covered_until: r.get("covered_until"),
                lapsed: r.get("lapsed"),
                last: r.get("last"),
            })
            .collect())
    }

    async fn status_carried_in(
        &self,
        window_secs: i64,
    ) -> Result<Vec<StatusCarriedView>, StoreError> {
        let window_secs = window_secs.clamp(60, 90 * 86400) as f64;
        // The newest sample per component in the stretch before the window from which a promise
        // (the hour at most, as the probe writes them) plus the grace still reaches into it.
        let rows = sqlx::query(
            "SELECT * FROM (SELECT DISTINCT ON (component) component, status, \
               floor(EXTRACT(EPOCH FROM sampled_at)::float8 \
                 + COALESCE(next_within_secs::float8, $2::float8))::bigint AS covered_until \
             FROM status_samples \
             WHERE sampled_at <= now() - make_interval(secs => $1) \
               AND sampled_at > now() - make_interval(secs => $1 + $2::float8 + $3::float8) \
             ORDER BY component, sampled_at DESC) latest \
             ORDER BY component COLLATE \"C\"",
        )
        .bind(window_secs)
        .bind(UNPROMISED_SECS as f64)
        .bind(GRACE_SECS as f64)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| StatusCarriedView {
                component: r.get("component"),
                covered_until: r.get("covered_until"),
                last: r.get("status"),
            })
            .collect())
    }

    async fn health_check(&self) -> Result<(), StoreError> {
        // Trivial connectivity probe — no projection scan.
        sqlx::query("SELECT 1").execute(&self.pool).await?;
        Ok(())
    }
}
