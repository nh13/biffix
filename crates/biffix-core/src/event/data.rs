//! [`EventData`], the typed kind-tagged payload of a RunEvent, and its published JSON Schema.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::event::{
    Dag, Kind, OutputRef, OutputRole, RunPreparing, RunSubmitted, TaskVerdict, TerminationCategory,
};
use crate::ids::Attempt;
use crate::time::Timestamp;

// Named only by the doc links of `EventData::RunFailed::stage`.
#[cfg(doc)]
use crate::event::{FAILURE_STAGE_IMAGE_PREPARATION, FAILURE_STAGE_LAUNCH};

/// The typed, kind-tagged payload of a RunEvent.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EventData {
    // ── run-level ──
    // Boxed because it is by far the largest payload (~550 bytes inline vs ~100 for the next
    // largest) and there is one per run: inline, it would set the size of every event in a
    // replayed log. Serde and the JSON Schema see through the box, so the wire shape is the flat
    // `{"kind": "run_submitted", ...fields}` object either way.
    /// The control plane accepted the run — its provenance anchor (see [`RunSubmitted`]).
    RunSubmitted(Box<RunSubmitted>),
    RunStarted {
        /// The engine's own session id, reported by the engine plugin at start (Nextflow
        /// `session.uniqueId`). A cross-check on the anchor's `engine_session_id`; `None` from the
        /// Snakemake logger and on events from before this field existed.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_id: Option<String>,
    },
    RunProgress {
        completed: u64,
        total: u64,
    },
    RunSucceeded {},
    RunFailed {
        /// Why the control plane failed the run before it launched (e.g. a submit-time image that
        /// could not be prepared). `None` from the engine, and on events from before this field
        /// existed. Optional + additive.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        /// Which step of the control plane failed it: an open vocabulary (see
        /// [`FAILURE_STAGE_IMAGE_PREPARATION`], [`FAILURE_STAGE_LAUNCH`]); a reader treats a value
        /// it does not know, or none, as a launch failure. `None` from the engine, and on events
        /// from before this field existed. Optional + additive.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stage: Option<String>,
    },
    RunCancelled {},

    // ── control-plane ──
    CancelRequested {
        actor: String,
        /// A human label for the actor (email/name), snapshotted when the cancel was asked for, as
        /// a run's anchor snapshots its submitter's. `None` when none was known, and on events
        /// from before this field existed. Optional + additive.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        actor_label: Option<String>,
    },
    /// The run's intermediate files (a Nextflow run's `work/`) were deleted by the retention sweep;
    /// its results are kept, but it can no longer be resumed.
    WorkDeleted {},
    /// The run was accepted but waits for its submit-time images before it launches (see
    /// [`RunPreparing`]). Boxed like the anchor it precedes.
    RunPreparing(Box<RunPreparing>),
    /// The run was put in an experiment (`experiment_id`), moved to another, or taken out of one
    /// (`None`). The latest by `(ts, seq)` wins (see `RunProjection::experiment_id`); it overrides
    /// the experiment the anchor was launched into.
    ExperimentSet {
        #[serde(default)]
        experiment_id: Option<String>,
        /// Who made the change (the actor id `authorize` returned). Optional + additive.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        actor: Option<String>,
    },
    /// The run was given a new human name (`run_name`), or its name was cleared (`None`: readers
    /// fall back to a derived one). The latest by `(ts, seq)` wins and overrides the name it was
    /// launched with (see `RunProjection::run_name`).
    RunRenamed {
        #[serde(default)]
        run_name: Option<String>,
        /// Who renamed it (the actor id `authorize` returned). Optional + additive.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        actor: Option<String>,
    },

    // ── coordinator-level ──
    CoordinatorSubmitted {},
    // The coordinator's CloudWatch log stream, from the Batch event (absent before the job
    // starts). Optional + additive.
    CoordinatorRunning {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        log_stream: Option<String>,
    },
    CoordinatorSucceeded {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        log_stream: Option<String>,
    },
    /// The coordinator job stopped without succeeding. Batch's account of why (as on
    /// [`EventData::TaskFailed`]) is what the run's failure summary has to go on when the engine
    /// never started or wrote no log (an image that could not be pulled, a lost host, an
    /// out-of-memory kill). Each is optional + additive: absent when Batch reported none.
    CoordinatorFailed {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        log_stream: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exit_code: Option<i32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        status_reason: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        termination_category: Option<TerminationCategory>,
    },

    /// The run's group-level dependency graph (rule graph for Snakemake, process graph for
    /// Nextflow), emitted when the engine knows it. Folds as a set-union so re-emission or
    /// out-of-order delivery converges: its dedup key carries a hash of the node and
    /// edge sets, so a redelivery of the same graph dedupes while a grown or changed graph is a
    /// distinct fact that reaches the fold. Group-level, not per-task-instance — the graph people
    /// read.
    RunDag {
        dag: Dag,
    },

    // ── task-level ──
    TaskCreated {
        /// The graph node (rule/process) this task instance belongs to, for rolling task states up
        /// onto the DAG. Optional + additive; `None` on events from before this field existed.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        group: Option<String>,
        /// The task keys this task depends on in this run: the tasks that produced its inputs.
        /// Additive; empty on events from before this field existed and for a task with no
        /// in-run upstream.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        deps: Vec<String>,
    },
    TaskSubmitted {
        /// The container image the attempt's Batch job runs, as the engine resolved it (the
        /// tenant's copy: `<registry>/<repo>@sha256:…`, or a tag when the engine has no digest).
        /// Per-task image provenance for a multi-image run. Optional + additive: `None` from a
        /// plugin that predates it, and on events from before this field existed.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        image: Option<String>,
    },
    TaskQueued {},
    TaskStarted {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        started_at: Option<Timestamp>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        log_stream: Option<String>,
    },
    TaskSucceeded {
        exit_code: i32,
        /// The attempt's CloudWatch log stream, from the Batch terminal (the final container's, so
        /// an attempt seen only through its terminal still has its log). `None` for an
        /// engine-reported local-rule terminal and on events from before this field existed.
        /// Optional + additive.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        log_stream: Option<String>,
    },
    /// A task was satisfied from the engine's cache — it produced its outputs on a prior run and
    /// was never submitted to Batch. Folds to a SUCCEEDED terminal (it counts as done) but is
    /// flagged `cached` on the projection, because a cache-hit never became a Batch job and so must not be
    /// counted as work run this time (cost/runtime views). Engine-reported; carries no exit code.
    TaskCached {},
    TaskFailed {
        exit_code: i32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        termination_category: Option<TerminationCategory>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        status_reason: Option<String>,
        /// As on [`EventData::TaskSucceeded`]. Optional + additive.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        log_stream: Option<String>,
    },
    TaskRetrying {
        prev_attempt: Attempt,
        next_attempt: Attempt,
    },
    /// The engine's verdict on one attempt of a task (`task_key` + `attempt` in the correlation
    /// fields), reported by the engine plugin. A `failed` verdict outranks AWS Batch's `SUCCEEDED`
    /// for the *task*: the attempt no longer counts as the task's success (so the task fails, or,
    /// with a retry announced, waits for the retry), while the attempt itself keeps its AWS facts
    /// (state, exit code, host, log stream). With no `attempt`, it applies to every attempt
    /// submitted at or before its `ts`. It changes nothing for a task with no Batch attempt (a
    /// local task's terminal is its engine `task_failed`), and an unknown verdict folds to nothing.
    /// Named in the engine namespace (`engine_*`), so an older backend stores it unfolded.
    EngineTaskVerdict {
        verdict: TaskVerdict,
        /// Why, as a category ([`TerminationCategory::MissingOutput`] for missing outputs,
        /// [`TerminationCategory::AppError`] for a rejected exit status). Optional.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        category: Option<TerminationCategory>,
        /// The engine's own explanation, for display. Optional.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },

    // ── infra / data ──
    SpotReclaimWarning {},
    /// The outputs of one task attempt (or, with no task key, run-level outputs), as a set. Folds
    /// as a union keyed on `s3_uri`, so a re-emission, a partial batch or out-of-order delivery
    /// converges. The task key and attempt ride in the RunEvent correlation fields.
    TaskOutputs {
        /// The engine's role for every ref in this batch. `None` ⇒ [`OutputRole::Output`].
        #[serde(default, skip_serializing_if = "Option::is_none")]
        role: Option<OutputRole>,
        /// True when the outputs were satisfied from the engine cache, not produced by this run.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        reused: bool,
        outputs: Vec<OutputRef>,
    },

    // ── builds (images, engine runtimes) ──
    // Not about a run: `run_id` is unset. `subject` names what is built (`runtime:<key>`,
    // `image:<version>/<name>`, …) and is the fold key; one subject may be built more than once,
    // and its latest build is the one that counts.
    /// biffix started a CodeBuild build (the control plane, right after `StartBuild`).
    BuildRequested {
        subject: String,
        build_arn: String,
        /// What was asked for (the recipe, the inputs), for the record.
        #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
        request: serde_json::Value,
    },
    BuildStarted {
        subject: String,
        build_arn: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        logs_url: Option<String>,
    },
    BuildSucceeded {
        subject: String,
        build_arn: String,
        /// The buildspec's exported variables: what the build produced (a digest, …).
        #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
        outputs: std::collections::BTreeMap<String, String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        logs_url: Option<String>,
    },
    BuildFailed {
        subject: String,
        build_arn: String,
        /// The build's final status (`FAILED`, `STOPPED`, `TIMED_OUT`, `FAULT`) and, when known,
        /// the failed phase's message.
        reason: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        logs_url: Option<String>,
    },

    /// An unknown kind — preserved verbatim, stored + audited but not folded. The translators
    /// write it as `{"kind": <tag>, "raw": <the wire event>}` ([`EventData::unknown`]), so the
    /// stored kind is never empty and the wire event's own `ts`/`schema_version`/… cannot collide
    /// with the RunEvent envelope's. Being serde's untagged fallback, it also captures a *known*
    /// kind with a malformed payload (see the module doc and
    /// [`EventData::is_degraded_known_kind`]).
    #[serde(untagged)]
    Unknown(serde_json::Value),
}

/// The JSON Schema of [`RunEvent::data`](crate::event::RunEvent::data): the derived
/// [`EventData`] schema with its `Unknown` branch corrected.
///
/// schemars models the untagged fallback as the bare `serde_json::Value` schema (any document) in
/// an `anyOf`, which would make the published schema admit anything, a known kind with a malformed
/// payload included. The branch is rewritten to what the contract preserves: an object whose
/// `kind` is a string *other than* the known kinds. Excluding the known kinds makes the branches
/// exclusive (a known kind matches only its own variant's branch), so they are published as a
/// `oneOf`.
pub(super) fn event_data_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
    use serde_json::{json, Value};

    let mut schema = <EventData as JsonSchema>::json_schema(generator);
    let Some(obj) = schema.as_object_mut() else {
        return schema;
    };
    let Some(mut branches) = obj.remove("anyOf").or_else(|| obj.remove("oneOf")) else {
        return schema;
    };
    let known: Vec<&str> = Kind::KNOWN.iter().map(Kind::as_str).collect();
    for branch in branches.as_array_mut().into_iter().flatten() {
        // A tagged variant pins `kind` with `const` (`enum: [tag]` before schemars 1).
        let tag = branch.pointer("/properties/kind").and_then(|kind| {
            kind.get("const")
                .or_else(|| kind.get("enum").and_then(|e| e.get(0)))
                .and_then(Value::as_str)
        });
        if tag.is_none_or(|tag| tag == "unknown") {
            let description = branch.get("description").cloned();
            *branch = json!({
                "type": "object",
                "properties": { "kind": { "type": "string", "not": { "enum": known } } },
                "required": ["kind"],
            });
            if let Some(description) = description {
                branch["description"] = description;
            }
        }
    }
    obj.insert("oneOf".into(), branches);
    schema
}

impl EventData {
    pub fn kind(&self) -> Kind {
        match self {
            EventData::RunSubmitted(_) => Kind::RunSubmitted,
            EventData::RunStarted { .. } => Kind::RunStarted,
            EventData::RunProgress { .. } => Kind::RunProgress,
            EventData::RunSucceeded {} => Kind::RunSucceeded,
            EventData::RunFailed { .. } => Kind::RunFailed,
            EventData::RunCancelled {} => Kind::RunCancelled,
            EventData::CancelRequested { .. } => Kind::CancelRequested,
            EventData::WorkDeleted {} => Kind::WorkDeleted,
            EventData::RunPreparing(_) => Kind::RunPreparing,
            EventData::ExperimentSet { .. } => Kind::ExperimentSet,
            EventData::RunRenamed { .. } => Kind::RunRenamed,
            EventData::CoordinatorSubmitted {} => Kind::CoordinatorSubmitted,
            EventData::CoordinatorRunning { .. } => Kind::CoordinatorRunning,
            EventData::CoordinatorSucceeded { .. } => Kind::CoordinatorSucceeded,
            EventData::CoordinatorFailed { .. } => Kind::CoordinatorFailed,
            EventData::RunDag { .. } => Kind::RunDag,
            EventData::TaskCreated { .. } => Kind::TaskCreated,
            EventData::TaskSubmitted { .. } => Kind::TaskSubmitted,
            EventData::TaskQueued {} => Kind::TaskQueued,
            EventData::TaskStarted { .. } => Kind::TaskStarted,
            EventData::TaskSucceeded { .. } => Kind::TaskSucceeded,
            EventData::TaskCached {} => Kind::TaskCached,
            EventData::TaskFailed { .. } => Kind::TaskFailed,
            EventData::TaskRetrying { .. } => Kind::TaskRetrying,
            EventData::EngineTaskVerdict { .. } => Kind::EngineTaskVerdict,
            EventData::SpotReclaimWarning {} => Kind::SpotReclaimWarning,
            EventData::TaskOutputs { .. } => Kind::TaskOutputs,
            EventData::BuildRequested { .. } => Kind::BuildRequested,
            EventData::BuildStarted { .. } => Kind::BuildStarted,
            EventData::BuildSucceeded { .. } => Kind::BuildSucceeded,
            EventData::BuildFailed { .. } => Kind::BuildFailed,
            EventData::Unknown(v) => {
                let k = v.get("kind").and_then(|k| k.as_str()).unwrap_or("").to_string();
                Kind::Unknown(k)
            }
        }
    }

    /// The payload a translator stores for a fact of a kind this build does not know:
    /// `{"kind": <kind>, "raw": <raw>}`. The raw wire event is nested (not flattened into the
    /// RunEvent), so its envelope keys cannot overwrite the RunEvent's own, and `kind` names the
    /// fact in the log. `kind` must not be a known [`Kind`] — that would make the fact look like a
    /// degraded known event ([`EventData::is_degraded_known_kind`]).
    pub fn unknown(kind: impl Into<String>, raw: serde_json::Value) -> EventData {
        let kind = kind.into();
        debug_assert!(
            !Kind::from_tag(&kind).is_known(),
            "an unknown payload must not claim the known kind `{kind}`"
        );
        EventData::Unknown(serde_json::json!({ "kind": kind, "raw": raw }))
    }

    /// True for an event whose `kind` this build knows but whose payload did not parse as that
    /// kind's variant, so it fell back to [`EventData::Unknown`] and the fold skips it (see the
    /// module doc). Translators never write such an event, so seeing one when reading the log means
    /// a stored payload and this build's contract disagree — a reader should log it loudly rather
    /// than fold without it.
    pub fn is_degraded_known_kind(&self) -> bool {
        match self {
            EventData::Unknown(v) => {
                v.get("kind").and_then(|k| k.as_str()).is_some_and(|k| Kind::from_tag(k).is_known())
            }
            _ => false,
        }
    }
}
