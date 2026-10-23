//! The normalized **RunEvent** — the log + fold contract.
//!
//! Every wire event translates into exactly one RunEvent (see [`crate::translate`]). The log
//! stores RunEvents; the reducers fold them. The reducer sees one vocabulary of [`Kind`],
//! never the per-source wire vocabularies ([`crate::wire`]).
//!
//! The `data` payload is a **typed enum keyed by kind**, so each kind's required fields are
//! compile-time-enforced, with an [`EventData::Unknown`] escape hatch that preserves unknown kinds
//! (store + audit, don't fold).
//!
//! Consequence of serde's untagged fallback: `Unknown` also absorbs a *known* kind whose payload is
//! malformed/incomplete (a `run_progress` missing `total`, say) — it degrades to `Unknown` rather
//! than raising a parse error, so the fact would be stored but not folded. The wire layer is the
//! validating boundary that keeps this from happening at write time: producers POST the typed
//! per-source wire events ([`crate::wire`]), a known engine kind with a malformed body is rejected
//! at ingest, and the translators only ever write `Unknown` under a kind this build does not know.
//! A normalized `RunEvent` is otherwise only deserialized from a DB payload a translator wrote.
//!
//! The remaining risk is stored-event schema evolution. Nothing reads [`SCHEMA_VERSION`] back to
//! gate it; the protection is the additive-only evolution policy — never add a required
//! field to an existing kind, never close an open vocabulary, or previously-stored events degrade
//! to `Unknown` on read — plus [`EventData::is_degraded_known_kind`], which lets a reader detect (and
//! log) a stored known-kind event that no longer parses as its variant instead of silently folding
//! without it.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::ids::{Attempt, BatchJobId, CoordinatorJobId, InstanceId, RunId, TaskKey};
use crate::source::EventSource;
use crate::time::Timestamp;

mod anchor;
mod control_plane;
mod data;
mod kind;
mod payload;
mod samples;
#[cfg(test)]
mod tests;

pub(crate) use anchor::submitter_user;
pub use anchor::{
    user_submitter, InputTable, ReferenceUse, Retention, RunPreparing, RunSubmitted, RuntimeUse,
    SubmitImage, USER_SUBMITTER_PREFIX,
};
pub use control_plane::{
    build_requested, cancel_requested, coordinator_submitted, experiment_set, run_cancelled,
    run_failed, run_preparing, run_renamed, run_submitted, work_deleted,
    FAILURE_STAGE_IMAGE_PREPARATION, FAILURE_STAGE_LAUNCH,
};
pub use data::EventData;
pub use kind::{Kind, COORDINATOR_TERMINAL_KINDS, TASK_TERMINAL_KINDS};
pub use payload::{Dag, DagEdge, DagNode, OutputRef, OutputRole, TaskVerdict, TerminationCategory};
pub use samples::{ReusedSample, SampleHint, SampleSpec, StepScope};

/// Contract version for the normalized RunEvent.
pub const SCHEMA_VERSION: u32 = 1;

/// A stored event's position in the append-only log — the `BIGSERIAL` the persistence layer
/// assigns at append: monotonic, unique, and the log's authoritative *total* order.
///
/// It is the tiebreaker for events that share a source `ts`. Threading it onto the event makes the
/// reducer's ordering intrinsic to the log rather than emergent from a caller's `ORDER BY` plus a
/// stable sort — so `replay` folds in one well-defined order for *every* caller.
///
/// It is deliberately **not** part of the wire contract: a producer never knows its future log
/// position, so `RunEvent::seq` is skipped in (de)serialization and absent from the JSON Schema.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LogSeq(pub i64);

/// The normalized event every producer's wire event maps into and every consumer folds.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RunEvent {
    pub schema_version: u32,
    /// Producer-computed, deterministic; the log dedupes on it.
    pub dedup_key: String,
    /// Source time of the fact.
    pub ts: Timestamp,
    pub source: EventSource,
    /// The log position assigned at append (see [`LogSeq`]). `None` before the event is persisted
    /// (a freshly built or test event); `Some` once read back from the log, where it is the fold's
    /// total-order tiebreaker. A persistence fact, not part of the normalized contract, so it is
    /// skipped on the wire and omitted from the JSON Schema.
    #[serde(skip)]
    pub seq: Option<LogSeq>,
    // ── correlation / identity; all optional except where a kind requires them ──
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<RunId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_key: Option<TaskKey>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt: Option<Attempt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub batch_job_id: Option<BatchJobId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance_id: Option<InstanceId>,
    /// The coordinator's Batch job id, set only on `coordinator_submitted` (always equal to its
    /// `batch_job_id`). Informational: the fold joins on `batch_job_id`, and nothing in biffix
    /// reads this back. It stays a published contract field because stored events carry it and
    /// external consumers may name the coordinator by it (additive-only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coordinator_job_id: Option<CoordinatorJobId>,
    /// The AWS Batch job queue (its name, not ARN) an `aws-batch` event's job ran on. Like
    /// `instance_id`, a fact about where the attempt ran; it tells a spot attempt from an on-demand
    /// one when the host's own lifecycle is unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue: Option<String>,
    /// The vCPUs an `aws-batch` event's job asked Batch for (its `VCPU` resource requirement): what
    /// the attempt requested, not what its host has. Like `queue`, a fact about the attempt,
    /// carried by the Batch state-change events that name it (not by a reaped terminal, which has
    /// no container); the reducer keeps the minimum across the events that carry it. `f64` end to
    /// end (wire, column, API), so a fractional request reads back exactly. Optional + additive:
    /// absent on events from before this field and on jobs whose event names no requirement.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_vcpus: Option<f64>,
    /// The memory (MiB) an `aws-batch` event's job asked Batch for (its `MEMORY` resource
    /// requirement). As `requested_vcpus`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_memory_mib: Option<i64>,
    /// Kind-specific payload; the `kind` tag lives inside.
    #[serde(flatten)]
    #[schemars(schema_with = "data::event_data_schema")]
    pub data: EventData,
}

impl RunEvent {
    /// The event's place in log order: source time, then log sequence (an unsequenced event sorts
    /// before a sequenced one at the same time).
    pub fn log_order(&self) -> (i64, Option<LogSeq>) {
        (self.ts.as_micros(), self.seq)
    }

    /// Build a normalized event from its identity and payload, with every correlation field unset.
    ///
    /// Each builder and translator starts here and then fills, via struct-update syntax, only the
    /// correlation fields its kind actually populates — so the all-`None` boilerplate lives in one
    /// place and a future correlation field is wired into the default exactly once.
    pub(crate) fn new(
        dedup_key: String,
        ts: Timestamp,
        source: EventSource,
        data: EventData,
    ) -> Self {
        RunEvent {
            schema_version: SCHEMA_VERSION,
            dedup_key,
            ts,
            source,
            seq: None,
            run_id: None,
            task_key: None,
            attempt: None,
            batch_job_id: None,
            instance_id: None,
            coordinator_job_id: None,
            queue: None,
            requested_vcpus: None,
            requested_memory_mib: None,
            data,
        }
    }

    /// The `kind` string of this event (including for [`EventData::Unknown`]).
    pub fn kind(&self) -> Kind {
        self.data.kind()
    }
}
