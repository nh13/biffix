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
    /// Epoch seconds by which the bucket's newest sample promised the next one (its time plus its
    /// `next_within_secs`, or plus [`UNPROMISED_SECS`] without one), before the grace.
    pub covered_until: i64,
    /// A sample in the bucket came more than [`GRACE_SECS`] after the sample before it promised
    /// the next, or had no sample before it within [`UNPROMISED_SECS`] plus the grace.
    pub lapsed: bool,
    /// The tier of the bucket's newest sample, as recorded.
    pub last: String,
}

/// A component's newest sample before a history window: how far its promise runs into it.
#[derive(Clone, Debug, PartialEq)]
pub struct StatusCarriedView {
    pub component: String,
    /// As [`StatusBucketView::covered_until`].
    pub covered_until: i64,
    /// The sample's tier, as recorded.
    pub last: String,
}

/// The promise a sample written before samples carried one is held to: the idle, hourly cadence.
pub const UNPROMISED_SECS: i64 = 3_600;

/// How long past its promised next sample a sample still counts: one tick, for the probe running
/// late in its tick (an Aurora resume, a slow reconcile step).
pub const GRACE_SECS: i64 = 300;

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

    /// Per component, the newest sample before the last `window_secs` (clamped as
    /// [`StatusStore::status_history`] clamps it) whose promise, plus [`GRACE_SECS`], could still
    /// reach into the window: what covers the window's start until its first sample.
    async fn status_carried_in(
        &self,
        window_secs: i64,
    ) -> Result<Vec<StatusCarriedView>, StoreError>;

    /// Cheap store-reachability probe for readiness/status. Must NOT scan the projection tables:
    /// readiness is polled frequently, so a full `list_runs` per probe is wasteful. DB-backed stores
    /// use a trivial connectivity query; in-memory doubles are always ready.
    async fn health_check(&self) -> Result<(), StoreError>;
}
