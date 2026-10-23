//! [`ProvenanceStore`]: which runs' provenance copies are stale, and marking them written.

use async_trait::async_trait;
use biffix_core::ids::RunId;

use super::StoreError;

/// How long a run whose provenance failed twice in a row waits before the next try (epoch micros);
/// each further failure doubles it ([`ProvenanceStore::mark_provenance_failed`]).
pub const PROVENANCE_RETRY_BASE_MICROS: i64 = 5 * 60 * 1_000_000;

/// The longest a run whose provenance keeps failing waits between tries: a day.
pub const PROVENANCE_RETRY_MAX_MICROS: i64 = 24 * 3600 * 1_000_000;

/// When a run whose provenance has now failed `attempts` times in a row (counting this one) is due
/// again, after failing at `at_micros`: at once after the first, then the base doubling, capped.
pub fn provenance_retry_after(attempts: i64, at_micros: i64) -> i64 {
    if attempts <= 1 {
        return at_micros;
    }
    let shift = u32::try_from((attempts - 2).min(20)).unwrap_or(20);
    at_micros + (PROVENANCE_RETRY_BASE_MICROS << shift).min(PROVENANCE_RETRY_MAX_MICROS)
}

/// The provenance copies in S3: which are due (a read of the raw log, see the [`crate::store`]
/// docs) and which are written.
#[async_trait]
pub trait ProvenanceStore: Send + Sync {
    /// Ended runs whose provenance copy in S3 is missing or older than their newest event, and
    /// whose newest event arrived more than `settle_secs` ago (so a late reconcile lands first),
    /// at most `limit`: each with the newest event's log `seq`. A run marked
    /// ([`Self::mark_provenance`]) at that `seq` or later is not due, nor is one whose last
    /// failure ([`Self::mark_provenance_failed`]) asks it to wait still. Runs that have not failed
    /// come first, then by how many times they have, each group oldest first.
    async fn provenance_due(
        &self,
        settle_secs: i64,
        limit: i64,
    ) -> Result<Vec<(RunId, i64)>, StoreError>;

    /// Record that writing the run's provenance failed, at `at_micros`: a first failure is due
    /// again at once (the next sweep), each one after [`PROVENANCE_RETRY_BASE_MICROS`] doubling per
    /// failure, at most [`PROVENANCE_RETRY_MAX_MICROS`].
    async fn mark_provenance_failed(
        &self,
        run_id: &RunId,
        at_micros: i64,
    ) -> Result<(), StoreError>;

    /// Record that the run's provenance, up to log `seq`, was written to `uri` (forgetting its
    /// failures).
    async fn mark_provenance(
        &self,
        run_id: &RunId,
        seq: i64,
        uri: &str,
        at_micros: i64,
    ) -> Result<(), StoreError>;
}
