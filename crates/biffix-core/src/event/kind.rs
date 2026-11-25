//! The `kind` vocabulary: the open [`Kind`] enum every RunEvent is tagged with, declared from one
//! table, and the terminal kinds reconciliation keys on.

use serde::{Deserialize, Serialize};

/// Declares the `kind` vocabulary from ONE `Variant => "tag"` table: the [`Kind`] enum (with its
/// serde tags and open `Unknown` fallback), [`Kind::as_str`], [`Kind::KNOWN`] and the schema's
/// documented examples are all generated from it, so they cannot drift from one another.
macro_rules! kind_vocabulary {
    ($($variant:ident => $tag:literal),* $(,)?) => {
        /// The `kind` vocabulary (open enum). Known kinds are matched; unknowns hit a generic
        /// path.
        #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
        pub enum Kind {
            $(
                #[serde(rename = $tag)]
                $variant,
            )*
            #[serde(untagged)]
            Unknown(String),
        }

        crate::impl_open_enum_schema!(
            Kind,
            "Kind",
            "The RunEvent kind vocabulary (open enum).",
            [$($tag),*]
        );

        impl Kind {
            /// Every known kind, in declaration order.
            pub const KNOWN: &'static [Kind] = &[$(Kind::$variant),*];

            /// The canonical wire string for this kind (its serde tag). Consumers that need the
            /// string in SQL or logs use it rather than hardcoding a literal that could drift.
            pub fn as_str(&self) -> &str {
                match self {
                    $(Kind::$variant => $tag,)*
                    Kind::Unknown(raw) => raw,
                }
            }
        }
    };
}

kind_vocabulary! {
    RunSubmitted => "run_submitted",
    RunStarted => "run_started",
    RunProgress => "run_progress",
    RunSucceeded => "run_succeeded",
    RunFailed => "run_failed",
    RunCancelled => "run_cancelled",
    CancelRequested => "cancel_requested",
    WorkDeleted => "work_deleted",
    RunPreparing => "run_preparing",
    ExperimentSet => "experiment_set",
    RunRenamed => "run_renamed",
    CoordinatorSubmitted => "coordinator_submitted",
    CoordinatorRunning => "coordinator_running",
    CoordinatorSucceeded => "coordinator_succeeded",
    CoordinatorFailed => "coordinator_failed",
    RunDag => "run_dag",
    TaskCreated => "task_created",
    TaskSubmitted => "task_submitted",
    TaskQueued => "task_queued",
    TaskStarted => "task_started",
    TaskSucceeded => "task_succeeded",
    TaskCached => "task_cached",
    TaskFailed => "task_failed",
    TaskRetrying => "task_retrying",
    EngineTaskVerdict => "engine_task_verdict",
    SpotReclaimWarning => "spot_reclaim_warning",
    TaskOutputs => "task_outputs",
    BuildRequested => "build_requested",
    BuildStarted => "build_started",
    BuildSucceeded => "build_succeeded",
    BuildFailed => "build_failed",
    BatchExecutionStarted => "batch_execution_started",
    BatchExecutionEnded => "batch_execution_ended",
}

impl Kind {
    /// The kind a wire tag names: a known variant, else [`Kind::Unknown`] carrying the tag.
    pub(crate) fn from_tag(tag: &str) -> Kind {
        Kind::KNOWN
            .iter()
            .find(|k| k.as_str() == tag)
            .cloned()
            .unwrap_or_else(|| Kind::Unknown(tag.to_string()))
    }

    /// Whether this build knows the kind (it is not [`Kind::Unknown`]).
    pub(crate) fn is_known(&self) -> bool {
        !matches!(self, Kind::Unknown(_))
    }
}

/// The authoritative child-task terminal kinds. AWS Batch (not the engine) is the source of a
/// task's exit fact, so reconciliation keys terminality on these (paired with
/// `EventSource::AwsBatch`). Shared so the reconcile SQL cannot drift from the fold's own terminal
/// logic.
pub const TASK_TERMINAL_KINDS: [Kind; 2] = [Kind::TaskSucceeded, Kind::TaskFailed];

/// The coordinator (head-job) terminal kinds, for the same reason as [`TASK_TERMINAL_KINDS`].
pub const COORDINATOR_TERMINAL_KINDS: [Kind; 2] =
    [Kind::CoordinatorSucceeded, Kind::CoordinatorFailed];

/// The kinds of a Batch job's start fact, of either shape: the translator maps STARTING and
/// RUNNING to `task_started`, or to `coordinator_running` for a job on a listed coordinator queue.
/// Shared so the reconcile SQL and the reconcile tick read starts as the translator writes them.
pub const BATCH_JOB_START_KINDS: [Kind; 2] = [Kind::TaskStarted, Kind::CoordinatorRunning];

/// The kinds of one AWS Batch execution (one ECS task: one run of a job on a host). A job that
/// Batch retries itself has several executions under one `batch_job_id`, so these are facts about
/// an execution, never about the job: they are deliberately in none of [`TASK_TERMINAL_KINDS`],
/// [`COORDINATOR_TERMINAL_KINDS`] or [`BATCH_JOB_START_KINDS`], which keeps an execution's end
/// from reading as a task's or coordinator's terminal and its start from reading as the job's.
pub const BATCH_EXECUTION_KINDS: [Kind; 2] =
    [Kind::BatchExecutionStarted, Kind::BatchExecutionEnded];
