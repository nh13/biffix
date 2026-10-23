//! [`StatusStore`]: the status page's samples, and the store's own health.

use async_trait::async_trait;

use super::StoreError;

/// The latest recorded sample of one status component (service-status page).
#[derive(Clone, Debug, PartialEq)]
pub struct StatusComponentView {
    pub name: String,
    /// "ok" | "degraded" | "down" as recorded by the probe.
    pub status: String,
    pub detail: Option<String>,
    /// Epoch seconds the sample was written (for staleness detection).
    pub sampled_at: Option<f64>,
    /// The most seconds until the probe's next sample, as the tick that wrote this one promised
    /// (`None`: a sample written before samples carried a promise).
    pub next_within_secs: Option<i64>,
}

/// One time-bucket's status-sample rollup for a component (uptime history bars). The bucket size
/// depends on the requested window (5-min for 1h, hourly for 1d, etc.).
#[derive(Clone, Debug, PartialEq)]
pub struct StatusBucketView {
    /// Bucket start as epoch seconds (aligned to the bucket interval from the epoch origin), so the
    /// client can build a fixed timeline and match buckets by integer, TZ-free.
    pub bucket: i64,
    pub component: String,
    pub ok: i64,
    pub degraded: i64,
    pub down: i64,
}

/// The status probe's samples (latest and rolled up), and whether the store is reachable.
#[async_trait]
pub trait StatusStore: Send + Sync {
    /// The latest status sample per component (the probe writes these on the reconcile tick).
    async fn status_components(&self) -> Result<Vec<StatusComponentView>, StoreError>;

    /// Per-bucket, per-component status-sample rollup over the last `window_secs`, bucketed to
    /// `bucket_secs` (aligned to the epoch origin). A store may clamp both rather than reject them,
    /// so a caller must not assume the requested sizes were used verbatim: the Postgres store clamps
    /// `window_secs` to 60 s..=90 days and `bucket_secs` to 60 s..=1 day.
    async fn status_history(
        &self,
        window_secs: i64,
        bucket_secs: i64,
    ) -> Result<Vec<StatusBucketView>, StoreError>;

    /// Cheap store-reachability probe for readiness/status. Must NOT scan the projection tables:
    /// readiness is polled frequently, so a full `list_runs` per probe is wasteful. DB-backed stores
    /// use a trivial connectivity query; in-memory doubles are always ready.
    async fn health_check(&self) -> Result<(), StoreError>;
}
