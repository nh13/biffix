//! The engine-plugin wire event (`source: engine`).
//!
//! One shared shape emitted by every engine plugin — the Snakemake logger plugin (interface ≥ 9.0)
//! and the nf-biffix Nextflow observer — since both report the same
//! engine-owned *semantics*: `task_key` (rule/process + sorted wildcards/tag), progress, the
//! dependency graph, and cache hits. AWS still owns ground truth (exit code, terminal fact). A
//! per-engine translator ([`crate::translate`]) maps this shape into the normalized RunEvent and
//! stamps the engine [`crate::source::EventSource`]. Versioned independently.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::ids::{Attempt, BatchJobId, RunId, TaskKey};
use crate::time::Timestamp;

/// Independent schema version for the engine-plugin wire event.
pub const SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct EngineEvent {
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    pub ts: Timestamp,
    pub run_id: RunId,
    #[serde(flatten)]
    pub body: EngineBody,
}

/// The engine-native event vocabulary (shared across engines). `kind` is the tag.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EngineBody {
    /// The engine began executing (coordinator's engine process started).
    RunStarted {
        /// The engine's own session id (Nextflow `session.uniqueId`), when the engine has one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_id: Option<String>,
    },
    /// Completed / total logical-task counts.
    RunProgress { completed: u64, total: u64 },
    /// The engine reported the whole run finished.
    RunSucceeded,
    /// The engine reported the whole run failed.
    RunFailed,
    /// The run's group-level dependency graph (rule/process graph), emitted once when known. Folds
    /// as a set-union in the reducer.
    RunDag {
        #[serde(default)]
        dag: crate::event::Dag,
    },
    /// A logical task was registered (may be unsubmitted). `group` is the graph node (rule/process)
    /// it belongs to, for rolling task states up onto the DAG.
    TaskCreated {
        task_key: TaskKey,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        group: Option<String>,
        /// Task keys of the in-run tasks that produced this task's inputs (its upstream).
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        deps: Vec<String>,
    },
    /// An attempt was submitted to Batch — binds `batch_job_id → (run_id, task_key)`.
    TaskSubmitted {
        task_key: TaskKey,
        attempt: Attempt,
        batch_job_id: BatchJobId,
        /// The container image the Batch job runs, as the engine resolved it (the tenant's copy,
        /// `<registry>/<repo>@sha256:…` where the engine knows the digest). Optional + additive:
        /// the task's image provenance on a run with more than one image.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        image: Option<String>,
    },
    /// A **local** task (a rule run on the coordinator, never submitted to Batch — e.g. an
    /// aggregation target) finished successfully. A Batch task's terminal is AWS's ground truth
    /// (`from_aws_batch`); a local rule has no Batch job, so the engine is its only terminal
    /// source. Carries no `batch_job_id`.
    ///
    /// `attempt` is the engine's attempt number for the rule, when it retries local rules: each
    /// attempt's terminal is then a distinct fact (its dedup key carries the attempt), so a retried
    /// rule's second failure is not dropped as a redelivery of the first. Optional + additive:
    /// absent ⇒ no attempt slot in the key.
    ///
    /// `execution_id` is the engine's id for this one execution of the task, for an engine that
    /// can re-run a task without advancing `attempt` (Nextflow's trace `task_id`, new for every
    /// execution, including an executor-level retry that keeps the attempt number). It makes each
    /// execution's terminal a distinct fact (the dedup key's discriminator) while a redelivery of
    /// one still dedupes. Optional + additive: absent ⇒ no discriminator in the key.
    TaskSucceeded {
        task_key: TaskKey,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        attempt: Option<Attempt>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        execution_id: Option<String>,
    },
    /// A task was satisfied from the engine cache (e.g. Nextflow `-resume`): it produced its
    /// outputs on a prior run and was never submitted to Batch this time. Folds to a succeeded
    /// terminal but is flagged as cached, so cost/runtime views don't count it as work run. Like
    /// [`EngineBody::TaskSucceeded`], carries no `batch_job_id` and an optional `attempt` and
    /// `execution_id`.
    TaskCached {
        task_key: TaskKey,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        attempt: Option<Attempt>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        execution_id: Option<String>,
    },
    /// A **local** task failed (engine-reported). Same rationale, `attempt` and `execution_id` as
    /// [`EngineBody::TaskSucceeded`]: with `execution_id`, a failure at an attempt the engine
    /// re-ran without advancing it is not dropped as a redelivery of the first.
    TaskFailed {
        task_key: TaskKey,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        attempt: Option<Attempt>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        execution_id: Option<String>,
    },
    /// A new attempt of a task is starting; links old → new.
    TaskRetrying { task_key: TaskKey, prev_attempt: Attempt, next_attempt: Attempt },
    /// The outputs of one task attempt, as a set (one event per task, attempt and role). With no
    /// `task_key` the outputs are run-level (e.g. a Nextflow publish that no task can be credited
    /// with). Folds as a set-union keyed on `s3_uri`.
    TaskOutputs {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task_key: Option<TaskKey>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        attempt: Option<Attempt>,
        /// The role of every ref in this batch; absent ⇒ `output`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        role: Option<crate::event::OutputRole>,
        /// True when the outputs were satisfied from the engine cache (Nextflow `-resume`), not
        /// produced by this run.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        reused: bool,
        outputs: Vec<crate::event::OutputRef>,
    },
    /// The engine's verdict on a task attempt — today only `failed`: the engine failed an attempt
    /// whose Batch job may have exited 0 (Nextflow: missing declared outputs, an exit status
    /// outside `validExitStatus`). Outranks AWS's `SUCCEEDED` for the task; the attempt keeps its
    /// AWS facts. `attempt` names the attempt (the one its `task_submitted` carried); absent ⇒
    /// every attempt. Either way it covers only submissions at or before `ts`, so a later re-run
    /// under the same attempt number is not rejected. A new kind, so it is named in the engine namespace
    /// ([`crate::translate::ENGINE_KIND_PREFIX`]): an older backend stores it unfolded.
    EngineTaskVerdict {
        task_key: TaskKey,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        attempt: Option<Attempt>,
        verdict: crate::event::TaskVerdict,
        /// `missing_output`, `app_error`, … (the open `TerminationCategory` vocabulary).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        category: Option<crate::event::TerminationCategory>,
        /// The engine's explanation, shown with the failure.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
}

impl EngineBody {
    /// Every engine wire `kind` tag this build knows (one per [`EngineBody`] variant). An event of
    /// one of these kinds is parsed strictly — a malformed body is rejected — while any other kind
    /// is a newer producer's and is stored unfolded (`translate::from_engine_value`). A test pins
    /// this list to the enum's serde tags.
    pub const KINDS: &'static [&'static str] = &[
        "run_started",
        "run_progress",
        "run_succeeded",
        "run_failed",
        "run_dag",
        "task_created",
        "task_submitted",
        "task_succeeded",
        "task_cached",
        "task_failed",
        "task_retrying",
        "task_outputs",
        "engine_task_verdict",
    ];
}

fn default_schema_version() -> u32 {
    SCHEMA_VERSION
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_are_exactly_the_engine_body_tags() {
        // serde names every variant it accepts in its unknown-variant error; compare that list to
        // KINDS so a new variant cannot be left out (it would be misfiled as an unknown kind).
        let err = serde_json::from_value::<EngineBody>(serde_json::json!({"kind": "__none__"}))
            .unwrap_err()
            .to_string();
        let expected = err
            .split_once("expected one of ")
            .unwrap_or_else(|| panic!("unexpected serde error: {err}"))
            .1;
        let tags: Vec<&str> = expected
            .split(", ")
            .map(|t| t.trim_matches(|c: char| c == '`' || !c.is_ascii_graphic()))
            .map(|t| t.trim_end_matches('`'))
            .collect();
        assert_eq!(tags, EngineBody::KINDS);
    }
}
