//! The attempt level of the fold: one Batch job's lifecycle ([`AttemptProjection`]), joined from
//! the engine's and AWS's streams.

use crate::event::TerminationCategory;
use crate::ids::{Attempt, BatchJobId, InstanceId};
use crate::reducer::keep::Stamp;
use crate::state::AttemptState;

#[derive(Clone, Debug, PartialEq)]
pub struct AttemptProjection {
    pub batch_job_id: BatchJobId,
    pub attempt: Option<Attempt>,
    pub state: AttemptState,
    /// The winning terminal's exit code; `None` when no exit code was reported (a failure the
    /// translator recorded with its `-1` sentinel).
    pub exit_code: Option<i32>,
    pub termination_category: Option<TerminationCategory>,
    /// Batch's human-readable reason for a failed attempt (e.g. "Host EC2 (instance i-…)
    /// terminated."). Owned by the winning terminal, like `termination_category`.
    pub status_reason: Option<String>,
    /// The Batch job queue (name) the attempt ran on; the fallback for spot vs on-demand when the
    /// host's own lifecycle is unknown. Invariant per job; lexicographic min on a conflict keeps it
    /// order-independent.
    pub queue: Option<String>,
    /// The vCPUs the attempt's Batch job requested (not its host's capacity). Invariant per job (a
    /// job's resource requirements are fixed at submit, Batch-internal retries included; an engine
    /// retry that asks for more is a new job and so a new attempt). On a conflict the minimum wins,
    /// not the first value seen: "first" depends on fold order, the min does not, so any fold
    /// order, including a replay, yields the same value. The cost of that choice is that a conflict
    /// would show the smaller request, i.e. err towards "undersized".
    pub requested_vcpus: Option<f64>,
    /// The memory (MiB) the attempt's Batch job requested. As `requested_vcpus`.
    pub requested_memory_mib: Option<i64>,
    /// The attempt's container log stream: the winning terminal's if it names one, else the one
    /// from the latest `(ts, seq)` `TaskStarted` (a Batch retry within the job
    /// starts a new container, so the latest start names the current one). Derived from the
    /// per-source streams below after every event (`AttemptProjection::refresh_log_stream`), so a
    /// losing terminal's stream never lingers: a Failed terminal's stream is dropped once a
    /// Succeeded one folds, whichever order they arrive in.
    pub log_stream: Option<String>,
    /// The container image the attempt's Batch job ran, from its `task_submitted` (lexicographic
    /// min on a conflict: one job runs one image).
    pub image: Option<String>,
    /// The host this attempt ran on: the value from the latest `(ts, seq)` attributed event that
    /// carries one. A Batch retry within the job can land on another host, so a last-write set
    /// would depend on fold order; latest-wins agrees with a replay in log order.
    pub instance_id: Option<InstanceId>,
    /// Source-time (epoch micros) the attempt first started running: the **earliest** `TaskStarted`
    /// observed (min-latched). AWS `STARTING` and `RUNNING` both translate to `TaskStarted`, so an
    /// attempt can see two — an unconditional overwrite would make this depend on fold order, so
    /// incremental and replay disagreed. The min over the observed set is order-independent.
    pub started_at_micros: Option<i64>,
    /// Source-time (epoch micros) the attempt reached its terminal: the winning terminal's time.
    /// Success outranks failure (see `AttemptState::rank`) and owns this field, so it is a pure
    /// function of the observed set regardless of the order the two terminals fold.
    pub finished_at_micros: Option<i64>,
    /// Source-time (epoch micros) of the earliest `task_submitted` for this job (min-latched, so
    /// order-independent). Lets [`TaskProjection::state`](crate::reducer::TaskProjection::state)
    /// recognize a retry's resubmission even when the engine reuses an attempt number.
    pub(super) submitted_at_micros: Option<i64>,
    /// The stream from the latest `(ts, seq)` `TaskStarted`.
    pub(super) live_log_stream: Option<String>,
    /// Where `live_log_stream` came from, for [`keep_latest`](crate::reducer::keep::keep_latest).
    pub(super) log_stream_at: Option<Stamp>,
    /// The streams the Succeeded and Failed terminals named (min on a conflict). The winning
    /// terminal's is the final container's (a Batch retry within the job starts a new one), so it
    /// wins over `live_log_stream`; the losing terminal's is never shown.
    pub(super) succeeded_log_stream: Option<String>,
    pub(super) failed_log_stream: Option<String>,
    /// Where `instance_id` came from, for [`keep_latest`](crate::reducer::keep::keep_latest).
    pub(super) instance_at: Option<Stamp>,
}

impl AttemptProjection {
    pub(super) fn new(batch_job_id: BatchJobId) -> Self {
        Self {
            batch_job_id,
            attempt: None,
            state: AttemptState::Created,
            exit_code: None,
            termination_category: None,
            status_reason: None,
            queue: None,
            requested_vcpus: None,
            requested_memory_mib: None,
            log_stream: None,
            image: None,
            instance_id: None,
            started_at_micros: None,
            finished_at_micros: None,
            submitted_at_micros: None,
            live_log_stream: None,
            log_stream_at: None,
            succeeded_log_stream: None,
            failed_log_stream: None,
            instance_at: None,
        }
    }

    /// Re-derive `log_stream`: the winning terminal's stream when it named one, else the live one.
    /// A pure function of the per-source streams and the state, so order-independent.
    pub(super) fn refresh_log_stream(&mut self) {
        let terminal = match self.state {
            AttemptState::Succeeded => self.succeeded_log_stream.as_ref(),
            AttemptState::Failed => self.failed_log_stream.as_ref(),
            _ => None,
        };
        self.log_stream = terminal.or(self.live_log_stream.as_ref()).cloned();
    }

    /// Advance state only forward. Enrichment fields may still be filled on a
    /// late event even when the state itself does not move.
    pub(super) fn advance_to(&mut self, s: AttemptState) {
        if s.rank() > self.state.rank() {
            self.state = s;
        }
    }
}
