//! The backend's persistence port: one trait per concern, and [`Store`], all of them at once,
//! which [`crate::AppState::store`] holds.
//!
//! - [`runs::RunReads`]: the run list, a run's detail and sample facts, its state and engine, the
//!   runs it resumed, per-workflow run counts;
//! - [`tasks::TaskReads`]: a run's tasks, their samples and their Batch attempts;
//! - [`experiments::ExperimentReads`]: an experiment's runs, their counts and summed cost;
//! - [`cost::CostReads`]: run costs, usage, and the past runs a launch estimate or typical run
//!   reads;
//! - [`outputs::OutputStore`]: a run's outputs, their cached S3 HEADs, and the output-prefix claim
//!   a submit holds;
//! - [`event_log::EventLogReads`] and [`provenance::ProvenanceStore`]: the two reads of the raw log
//!   (below), and the provenance copies' marks;
//! - [`pending::PendingLaunchStore`]: launches parked until their images are ready, and the launch
//!   claim;
//! - [`notifications::NotificationStore`]: a user's notifications, reads and dismissals;
//! - [`status::StatusStore`]: the status page's samples and the store's own health;
//! - [`EventSink`]: the write side, appending to the log.
//!
//! The registry, site settings, reference bundles and experiments themselves have their own stores
//! here ([`registry`], [`settings`], [`references`], [`experiments::ExperimentStore`]).
//!
//! The API reads projections, not the raw log, with two named exceptions that fold nothing:
//! [`event_log::EventLogReads::run_event`] reads one of a run's control-plane singletons as it was
//! appended — its `run_submitted` anchor ([`event_log::EventLogReads::run_anchor`]), its
//! `run_preparing`, or the control plane's own `run_failed` — for the facts only that event records
//! (input versions, git sha, submit-time images, why the control plane failed the run), which the
//! run page and provenance crate show and no projection column is worth keeping for; and
//! [`provenance::ProvenanceStore::provenance_due`] reads ended runs' newest log `seq` to find the
//! provenance copies that are stale. The write side answers one read of its own,
//! [`EventSink::job_claims`]: which runs the log already binds to some Batch jobs, so engine ingest
//! can refuse a conflicting claim before appending it.
//!
//! A trait method has a default only where it derives from the trait's other reads (a light read
//! from the full one, a page from the list), which a store overrides where it can do better. A read
//! or write with no such derivation has none: an empty default hides a whole feature from every
//! store that forgets it (see [`runs::RunReads::run_stats_by_workflow`]).
//!
//! The store returns backend **view DTOs** (not the reducer's internal `RunProjection`), because
//! the projection tables hold denormalized state strings, and the API maps these straight to its
//! JSON responses. Postgres backs this (`biffix_db`'s projection tables, which Aurora hosts in
//! prod; `crate::pg`, one file per trait); tests use the in-memory `testing::MemStore`.

pub mod cost;
pub mod event_log;
pub mod experiments;
pub mod notifications;
pub mod outputs;
pub mod pending;
pub mod provenance;
pub mod references;
pub mod registry;
pub mod runs;
pub mod settings;
pub mod status;
pub mod tasks;

use async_trait::async_trait;

// The read side returns the shared HTTP wire types directly. `RunSummary`/`Task` are pure
// projection reads with no store-vs-wire divergence, so a parallel backend DTO would be nothing but
// a field-for-field clone kept in sync by boilerplate `From` impls. `RunDetailView` below is NOT
// collapsed: it carries store-only fields (`params`, `dag_nodes`/`dag_edges`) and a real transform.
use biffix_api::{Host, RunSummary, Task};
use biffix_core::event::RunEvent;
use biffix_core::ids::{BatchJobId, CoordinatorJobId, RunId};

use cost::CostReads;
use event_log::EventLogReads;
use experiments::ExperimentReads;
use notifications::NotificationStore;
use outputs::OutputStore;
use pending::PendingLaunchStore;
use provenance::ProvenanceStore;
use runs::RunReads;
use status::StatusStore;
use tasks::TaskReads;

/// A classified store failure. Callers (and the API status mapping) distinguish these so a
/// uniqueness conflict, a corrupt stored value, and an unreachable database no longer collapse
/// into one opaque "backend error" that every handler renders as 503.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// The requested row does not exist.
    #[error("not found")]
    NotFound,
    /// A uniqueness or precondition conflict (e.g. re-registering an existing version).
    #[error("conflict: {0}")]
    Conflict(String),
    /// The backing store is unreachable or returned an operational error.
    #[error("store unavailable: {0}")]
    Unavailable(String),
    /// A stored value could not be (de)serialized — corrupt or unexpected data shape, not a
    /// caller error.
    #[error("serialization: {0}")]
    Serialization(String),
    /// The store refused a value the request carried, as data or against a constraint (a NUL in
    /// text, a number out of range, a missing reference): the caller's, and retrying the same
    /// value fails the same way.
    #[error("the store refused a value: {0}")]
    Rejected(String),
}

/// A corrupt or unexpected stored JSON value.
impl From<serde_json::Error> for StoreError {
    fn from(e: serde_json::Error) -> Self {
        StoreError::Serialization(e.to_string())
    }
}

/// `Ok(None)` for a lookup that found nothing; any other error is the caller's.
pub(crate) fn found<T>(res: Result<T, StoreError>) -> Result<Option<T>, StoreError> {
    match res {
        Ok(v) => Ok(Some(v)),
        Err(StoreError::NotFound) => Ok(None),
        Err(e) => Err(e),
    }
}

/// A run's detail = its summary + its task rows + detail-only fields. The summary and task rows are
/// the shared wire types ([`RunSummary`]/[`Task`]); the store-only fields below are what earns
/// this its own type.
#[derive(Clone, Debug, PartialEq)]
pub struct RunDetailView {
    pub run: RunSummary,
    pub tasks: Vec<Task>,
    /// Resolved params at submit, already secret-redacted. Detail-only.
    pub params: serde_json::Value,
    /// The coordinator (head) job's Batch id, for a console/log link. Detail-only.
    pub coordinator_job_id: Option<CoordinatorJobId>,
    /// The run's group-level dependency graph node ids (rule/process names), from `run_dag`.
    pub dag_nodes: Vec<String>,
    /// The graph's directed dependency edges `(from, to)`.
    pub dag_edges: Vec<(String, String)>,
    /// The run's recorded effective output prefix (from the anchor). `None` before the run
    /// launched, or in a composition with no project bucket.
    pub output_prefix: Option<String>,
    /// The coordinator's CloudWatch log stream, when its Batch events recorded one.
    pub coordinator_log_stream: Option<String>,
    /// Batch's account of a failed coordinator's stop (exit code, reason, category), from its
    /// `coordinator_failed`; all unset when it did not fail or Batch said nothing. The failure
    /// summary reads it for a run that failed before any step did. Detail-only.
    pub coordinator_stop: biffix_core::failure::FailedAttempt,
    /// The host the coordinator ran on, when registered.
    pub coordinator_host: Option<Host>,
    /// The engine session the run executed as (Nextflow: its cloud-cache key). `None` for
    /// Snakemake runs and runs recorded before resume support.
    pub engine_session_id: Option<String>,
    /// The engine reported a different session than the one biffix launched it with.
    pub session_mismatch: bool,
    /// The run this one resumed (`--resume-from`).
    pub resumed_from: Option<RunId>,
    /// Samplesheets biffix wrote for the run from submitted rows (param → uri, etag, rows).
    pub input_tables: Option<serde_json::Value>,
    /// The reference bundle the run was launched with (`{id, uris}`).
    pub reference_bundle: Option<serde_json::Value>,
    /// What happens to the run's intermediate files (`None` ⇒ the default).
    pub(crate) retention: Option<biffix_core::event::Retention>,
    /// When the retention sweep deleted the run's intermediate files.
    pub work_deleted_at_micros: Option<i64>,
    /// When the run's coordinator was (first) launched; `None` before launch.
    pub(crate) launched_at_micros: Option<i64>,
    /// The run's estimated cost (see [`cost::run_cost`]); `None` until a job of the run has run.
    pub cost: Option<biffix_api::RunCost>,
    /// How many tasks each graph step has in each state — the DAG's per-step counts, so a load
    /// without the task table can still draw them. Empty ⇒ derive them from `tasks`.
    pub task_groups: Vec<TaskGroupCount>,
    /// Who cancelled the run, for a person (folded: `RunProjection::cancelled_by`). `None` unless
    /// it ended cancelled on a recorded request.
    pub cancelled_by: Option<String>,
    /// When the run's workflow version was registered, epoch microseconds (`workflow_versions`,
    /// read with the run's row). `None` when the registry no longer has the version.
    pub workflow_registered_at_micros: Option<i64>,
}

/// The number of a run's tasks in one (group, state, cached) bucket.
#[derive(Clone, Debug, PartialEq)]
pub struct TaskGroupCount {
    pub(crate) group: String,
    pub(crate) state: String,
    pub(crate) cached: bool,
    pub(crate) count: i64,
}

/// [`TaskGroupCount`]s derived from a task list (tasks without a group are not on the graph).
pub(crate) fn task_group_counts(tasks: &[Task]) -> Vec<TaskGroupCount> {
    let mut counts: std::collections::BTreeMap<(&str, &str, bool), i64> = Default::default();
    for t in tasks {
        if let Some(group) = &t.group {
            *counts.entry((group, &t.state, t.cached)).or_default() += 1;
        }
    }
    counts
        .into_iter()
        .map(|((group, state, cached), count)| TaskGroupCount {
            group: group.to_owned(),
            state: state.to_owned(),
            cached,
            count,
        })
        .collect()
}

/// A held claim ([`OutputStore::claim_output_prefix`], [`PendingLaunchStore::claim_run_launch`]):
/// dropping it releases what it claims. Opaque; each store keeps whatever holds its claim (a
/// Postgres transaction, an in-memory entry) inside.
///
/// In Postgres a claim is a transaction-scoped advisory lock, so a held claim is a pooled
/// connection left idle in transaction while its holder calls S3 and Batch (a resume's submit, a
/// sweep's or a cancel's launch). This is accepted, not an oversight:
/// - Each held claim pins one connection of its process's pool (8 per backend Lambda), and a
///   process holds at most one at a time: an API instance serves one request at once, and the
///   sweep claims its parked runs one by one. Concurrent resumes or sweeps therefore spread over
///   instances, each pinning one of its own connections, not a shared pool.
/// - A hold lasts no longer than its handler: the API Lambda's 29 s, the sweep's 5 min. A holder
///   that dies or times out loses its connection, and Postgres ends the transaction and releases
///   the lock with it.
/// - biffix sets no `idle_in_transaction_session_timeout` (Aurora's default is off). A deployment
///   that sets one shorter than those handler timeouts would drop a lock mid-submit, letting two
///   resumes share a prefix or two sweeps launch one run, so keep it off, or above 5 min, on the
///   biffix database.
///
/// A session-level lock on a dedicated connection with an explicit unlock would avoid the idle
/// transaction; it is not worth its extra failure modes (a lock leaked by a missed unlock) at this
/// concurrency.
pub struct Claim(
    #[expect(dead_code, reason = "held only for its Drop, which releases the claim")] Box<dyn Send>,
);

impl Claim {
    /// A claim held by `holder` until it is dropped.
    pub(crate) fn new(holder: impl Send + 'static) -> Self {
        Self(Box::new(holder))
    }
}

impl std::fmt::Debug for Claim {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Claim")
    }
}

/// Every store trait at once: what [`crate::AppState::store`] holds, so a handler reaches any
/// concern through one handle. Implemented for every type that implements them all.
pub trait Store:
    RunReads
    + TaskReads
    + ExperimentReads
    + CostReads
    + OutputStore
    + EventLogReads
    + ProvenanceStore
    + PendingLaunchStore
    + NotificationStore
    + StatusStore
{
}

impl<T> Store for T where
    T: RunReads
        + TaskReads
        + ExperimentReads
        + CostReads
        + OutputStore
        + EventLogReads
        + ProvenanceStore
        + PendingLaunchStore
        + NotificationStore
        + StatusStore
{
}

/// Write side for the control-plane's own events (the `run_submitted` anchor at submit).
///
/// The backend is a first-class RunEvent producer: it mints the run id and the provenance anchor,
/// then appends it to the append-only log and folds it into the projection so the run is
/// immediately readable. This is a trait so the submit handler is testable without a database;
/// the production impl reuses the single ingest writer + projection fold.
#[async_trait]
pub trait EventSink: Send + Sync {
    /// Append several events **atomically** — all persist or none — then make them readable, and
    /// return how many were *newly* appended (the rest were dedup no-ops on `dedup_key`). Submit
    /// uses this so its `run_submitted` anchor and `coordinator_submitted` binding can never be
    /// split by a crash, which would strand the run "submitted" forever; the ingest route uses the
    /// returned count to tell a plugin's retry from a fresh append, and folds a whole engine batch
    /// in one pass instead of one transaction per event.
    async fn record_all(&self, evs: &[RunEvent]) -> Result<usize, StoreError>;

    /// Append `ev` idempotently (dedup on `dedup_key`) and advance the projection.
    async fn record(&self, ev: &RunEvent) -> Result<(), StoreError> {
        self.record_all(std::slice::from_ref(ev)).await.map(|_| ())
    }

    /// Every `(batch_job_id, run_id)` pair the log already binds among `job_ids`: a
    /// `task_submitted` or `coordinator_submitted` naming one of them, with the run it names. The
    /// one read the write side answers: the engine ingest route refuses a claim on a job another
    /// run already holds, and skips re-checking one its own run holds (a plugin retry or the
    /// coordinator's end-of-run replay), before it appends. The default (a sink that cannot read
    /// its log) fails, so the route fails closed.
    async fn job_claims(
        &self,
        _job_ids: &[BatchJobId],
    ) -> Result<Vec<(String, RunId)>, StoreError> {
        Err(StoreError::Unavailable("this event sink cannot read job claims".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_not_found_is_absent() {
        assert_eq!(found(Ok::<_, StoreError>(1)).unwrap(), Some(1));
        assert_eq!(found::<i32>(Err(StoreError::NotFound)).unwrap(), None);
        assert!(found::<i32>(Err(StoreError::Unavailable("down".into()))).is_err());
    }
}
