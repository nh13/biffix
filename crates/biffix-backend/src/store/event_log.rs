//! [`EventLogReads`]: a run's control-plane singletons, read from the raw log.

use async_trait::async_trait;
use biffix_core::event::Kind;
use biffix_core::event::RunEvent;
use biffix_core::ids::RunId;

use super::StoreError;

/// The raw-log read of a run's control-plane singletons (see the [`crate::store`] docs).
#[async_trait]
pub trait EventLogReads: Send + Sync {
    /// The run's `run_submitted` anchor as it was appended, for the facts only the anchor records
    /// (input versions at submit, git sha, image digest). `None` when the run has no anchor. A
    /// [`Self::run_event`] read (see the module docs).
    async fn run_anchor(&self, run_id: &RunId) -> Result<Option<RunEvent>, StoreError> {
        self.run_event(run_id, &Kind::RunSubmitted).await
    }

    /// The run's earliest event of `kind` from the control plane (in fold order: `ts`, then `seq`)
    /// as it was appended, for a control-plane singleton's own facts (`run_submitted`,
    /// `run_preparing`, the control plane's `run_failed`; an engine's `run_failed` is never read).
    /// `None` when the run has none. One of the two raw-log reads (see the module
    /// docs): only for these singletons, never to rebuild what the projection folds.
    async fn run_event(&self, run_id: &RunId, kind: &Kind) -> Result<Option<RunEvent>, StoreError>;
}
