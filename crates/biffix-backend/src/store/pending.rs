//! [`PendingLaunchStore`]: launches parked until their images are ready, and the launch claim.

use async_trait::async_trait;
use biffix_core::ids::RunId;

use super::{Claim, StoreError};

/// A run waiting for its submit-time images: its launch request as given, parked until the sweep
/// launches it (or the run fails or is cancelled). Not part of the event log; nothing folds it.
///
/// A run that will never launch while a coordinator an earlier launch of it may have left running
/// cannot be stopped keeps its request, for cleanup only (`runs::launch`'s `release`, until that
/// cleanup succeeds): its plan is then scrubbed to what that cleanup reads, marked
/// [`PENDING_CLEANUP_KEY`], so the run's params (secret values) are not kept with it.
#[derive(Clone, Debug, PartialEq)]
pub struct PendingLaunch {
    pub run_id: RunId,
    /// When the request was parked (epoch micros): the start of the preparation bound.
    pub created_at_micros: i64,
    /// The launch plan, verbatim (params unredacted: the coordinator needs them). Beside it, under
    /// [`PENDING_ORPHAN_KEY`], the coordinator a launch left unbound
    /// ([`PendingLaunchStore::mark_pending_launch_orphan`]). Once kept for cleanup only
    /// ([`PendingLaunch::cleanup_only`]), only the queues its launches were submitted to
    /// (`attempted_queues`), its single-job use (`single_job`, which names the deploy's queue for
    /// it now), that coordinator, and whether the cleanup's failure was escalated
    /// ([`PENDING_ESCALATED_KEY`]).
    pub plan: serde_json::Value,
}

/// The key of a parked launch's plan under which [`PendingLaunchStore::mark_pending_launch_orphan`]
/// records a coordinator that launched but could not be recorded (or stopped).
pub(crate) const PENDING_ORPHAN_KEY: &str = "orphan_coordinator_job_id";

/// The key (`true`) of a parked launch's plan that marks it kept for cleanup only: the run will
/// never launch (its end, or failure, is recorded), and the plan was scrubbed of all else.
pub(crate) const PENDING_CLEANUP_KEY: &str = "cleanup_only";

/// The key (`true`) of a plan kept for cleanup only that marks it escalated: its cleanup still
/// failed `runs::launch`'s `CLEANUP_ESCALATE_AFTER` after the run was parked, which was logged as
/// an error once. The request is kept, and the cleanup tried, all the same.
pub(crate) const PENDING_ESCALATED_KEY: &str = "cleanup_escalated";

/// The key (epoch micros) of a plan kept for cleanup only under which each launch sweep records
/// when it last tried the cleanup, before it does: the sweep steps kept requests least recently
/// tried first, so one whose cleanup takes a sweep's whole time cannot starve another.
pub(crate) const PENDING_CLEANUP_TRIED_KEY: &str = "cleanup_tried_at_micros";

impl PendingLaunch {
    /// The coordinator a launch of this run left unbound, if one did.
    pub fn orphan_job_id(&self) -> Option<&str> {
        self.plan.get(PENDING_ORPHAN_KEY).and_then(|v| v.as_str())
    }

    /// Whether the request is kept for cleanup only ([`PENDING_CLEANUP_KEY`]): never launched,
    /// nor failed again.
    pub fn cleanup_only(&self) -> bool {
        self.plan.get(PENDING_CLEANUP_KEY).and_then(serde_json::Value::as_bool) == Some(true)
    }

    /// When a sweep last tried the cleanup of a request kept for it ([`PENDING_CLEANUP_TRIED_KEY`]):
    /// `None` when none has.
    pub fn cleanup_tried_at_micros(&self) -> Option<i64> {
        self.plan.get(PENDING_CLEANUP_TRIED_KEY).and_then(serde_json::Value::as_i64)
    }

    /// Whether the cleanup's failure was escalated already ([`PENDING_ESCALATED_KEY`]).
    pub fn cleanup_escalated(&self) -> bool {
        self.plan.get(PENDING_ESCALATED_KEY).and_then(serde_json::Value::as_bool) == Some(true)
    }
}

/// Parked launch requests (`pending_launch`) and the claim that makes launching a run, or
/// cancelling it, exclusive.
#[async_trait]
pub trait PendingLaunchStore: Send + Sync {
    /// Park a launch request, replacing any earlier one for the same run.
    async fn put_pending_launch(&self, pending: &PendingLaunch) -> Result<(), StoreError>;

    /// Every parked launch request, oldest first (then by run id).
    async fn list_pending_launches(&self) -> Result<Vec<PendingLaunch>, StoreError>;

    /// The run's parked launch request as it is now, if any: what a launcher reads once it holds
    /// the run's claim ([`Self::claim_run_launch`]), since a listing it took earlier may predate
    /// another launcher's mark ([`Self::mark_pending_launch_orphan`]) or drop.
    async fn get_pending_launch(&self, run_id: &RunId)
        -> Result<Option<PendingLaunch>, StoreError>;

    /// Drop the run's parked launch request, if any.
    async fn delete_pending_launch(&self, run_id: &RunId) -> Result<(), StoreError>;

    /// Record on the run's parked launch (under [`PENDING_ORPHAN_KEY`] in its plan) that launching
    /// it started coordinator `job_id` which could be neither recorded nor stopped, so no sweep
    /// launches the run again: the next one stops that coordinator and fails the run. A no-op
    /// when the run has no parked launch.
    async fn mark_pending_launch_orphan(
        &self,
        run_id: &RunId,
        job_id: &str,
    ) -> Result<(), StoreError>;

    /// Claim the launch of `run_id`: the mutex between the sweeps that may launch a parked run and
    /// a cancel of it. Held until the returned guard is dropped; `None` when another holder has it
    /// now (a try-claim: it never waits).
    ///
    /// The sweep holds it from reading the run's state through the launch and its recording, so
    /// two sweeps cannot both launch it. A cancel of a run with no coordinator takes it too (retrying
    /// briefly while a sweep holds it), then re-reads the run: a coordinator the sweep bound
    /// meanwhile is stopped, else the run ends while the claim is held, its parked launch left for
    /// the sweep to release (stopping any coordinator an earlier launch of it may have left
    /// running; it never launches a cancelled run). In Postgres the claim pins one pooled
    /// connection (an open transaction holding an advisory lock) for as long as it is held — the
    /// whole launch, AWS calls included — which is bounded by the sweep handling its rows one at a
    /// time.
    async fn claim_run_launch(&self, run_id: &RunId) -> Result<Option<Claim>, StoreError>;
}
