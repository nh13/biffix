//! The run level of the fold: a run ([`RunProjection`]) reduced over its control-plane, coordinator
//! and engine events; its task counts; and the notification its terminal transition produces.

use std::collections::{BTreeMap, BTreeSet};

use crate::event::{submitter_user, OutputRef, OutputRole};
use crate::ids::{Attempt, RunId};
use crate::reducer::keep::{keep_min_micros, Stamp};
use crate::reducer::{OutputProjection, TaskProjection};
use crate::state::{RunState, TaskState};

/// How far a `cancel_requested` may be stamped after the failure it caused and still count as its
/// cause. The request is stamped by the backend's clock, the failure by AWS Batch's `stoppedAt` or
/// the coordinator's clock; this absorbs the skew between them, so a cancel that really came first
/// is not read as coming after.
const CANCEL_CLOCK_SKEW_MICROS: i64 = 1_000_000;

/// The id of a run's notification of reaching `state`: `<run id>:<state>`. Deterministic, so a
/// replay rebuilds the same id and a user's read of it survives the replay (and a read can be
/// written ahead of it). Reads and dismissals in the database are keyed by it, so changing its
/// spelling orphans every stored read and dismissal.
pub fn notification_id(run_id: &str, state: RunState) -> String {
    format!("{run_id}:{}", state.as_str())
}

/// The notification a run's terminal transition produces ([`RunProjection::notification`]): the
/// `notifications` projection row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunNotification {
    /// [`notification_id`] of the run and its terminal state.
    pub id: String,
    /// Whom it is for: the submitting user's id (the `sub` `/me` endpoints key on).
    pub user_sub: String,
    /// The terminal state reached.
    pub kind: RunState,
    /// When the run first reached a terminal state (epoch micros).
    pub at_micros: i64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RunProjection {
    pub run_id: RunId,
    /// Derived from the monotonic flags below via [`RunProjection::recompute_state`]; never
    /// mutated directly, so the value is a pure function of the observed event set.
    /// Private so nothing outside the fold can set it; read it through [`RunProjection::state`].
    state: RunState,
    /// Tasks keyed by `task_key`.
    pub tasks: BTreeMap<String, TaskProjection>,
    pub completed: Option<u64>,
    pub total: Option<u64>,
    /// Source-time (epoch micros) the user submitted the run: the earliest of its `run_submitted`
    /// anchor and a `run_preparing` (a run that waited for its submit-time images).
    pub submitted_at_micros: Option<i64>,
    /// Source-time (epoch micros) the run launched: the earliest of its anchor and its coordinator
    /// lifecycle events. Equals `submitted_at_micros` for a run that did not wait for its images;
    /// what "stuck in submitted" and wall-time estimates measure from.
    pub launched_at_micros: Option<i64>,
    /// Source-time (epoch micros) the run first reached a terminal state.
    pub completed_at_micros: Option<i64>,
    /// Who or what triggered the run (`user:<id>` for a person, `watcher:<id>`), from the
    /// anchor's [`crate::event::RunSubmitted::submitter`] (or a `run_preparing`'s, before the
    /// anchor folds).
    pub submitter: Option<String>,
    /// A human label for the submitter (email/name), from the anchor's `submitter_label`.
    pub submitter_label: Option<String>,
    /// The coordinator (head) job's Batch id, for a console/log link on the run detail.
    pub coordinator_job_id: Option<String>,
    /// The coordinator's CloudWatch log stream, from its Batch lifecycle events. Set-once, with the
    /// lexicographic min on a conflict (a coordinator job has one stream, so a conflict is a data
    /// bug; min keeps the fold order-independent).
    pub coordinator_log_stream: Option<String>,
    /// The ECS container-instance id the coordinator ran on (joins `host_registry`). Set-once, min
    /// on a conflict, like `coordinator_log_stream`.
    pub coordinator_host_id: Option<String>,
    /// The coordinator job's exit code, from its `coordinator_failed` (absent when Batch reported
    /// none, as for a container that never started). The lowest on a conflict, so the fold is
    /// order-independent.
    pub coordinator_exit_code: Option<i32>,
    /// Batch's reason for stopping the coordinator job, from its `coordinator_failed` (for one that
    /// never started, e.g. `CannotPullContainerError: …`). Set-once, min on a conflict.
    pub coordinator_status_reason: Option<String>,
    /// The termination category the translator read from that reason (`oom`, `spot`,
    /// `host_terminated`, …), as its wire string. Set-once, min on a conflict.
    pub coordinator_termination_category: Option<String>,
    /// The workflow version id this run executed (from the anchor).
    pub workflow_version_id: Option<String>,
    /// The input URIs declared at submit (from the anchor's `input_uris`).
    pub input_uris: Vec<String>,
    /// The effective output prefix the workflow's outputs land under (from the anchor). `None`
    /// until the anchor folds, or when the run had nowhere to write (no project bucket).
    pub output_prefix: Option<String>,
    /// The session biffix launched the engine with, from the anchor (Nextflow: the cloud-cache key
    /// under `<output_prefix>.nextflow/cache/`). `None` for Snakemake and pre-resume runs.
    pub engine_session_id: Option<String>,
    /// The session the engine itself reported on `run_started`. Kept apart from the anchor's value
    /// so both stay pure set-once facts; readers prefer this one via
    /// [`RunProjection::effective_session_id`] (it is what the engine actually ran as).
    pub reported_session_id: Option<String>,
    /// The run this one resumed (`--resume-from`), from the anchor.
    pub resumed_from: Option<String>,
    /// The run's human name: the latest `run_renamed`'s (by `(ts, seq)`; `None` when it cleared the
    /// name), else the one it was launched with (the anchor's, else the `run_preparing`'s).
    /// Derived after every fold step from the fields below, so it stays order-independent.
    pub run_name: Option<String>,
    /// The name the run was launched with (anchor, else `run_preparing`). Fold-internal.
    pub(super) launched_run_name: Option<String>,
    /// The latest `run_renamed`'s name (`Some(None)`: cleared) and the `(ts, seq)` it came from.
    /// Fold-internal: read through `run_name`.
    pub(super) run_renamed: Option<Option<String>>,
    pub(super) run_renamed_at: Option<Stamp>,
    /// Samplesheets biffix wrote for the run, from the anchor.
    pub input_tables: Option<BTreeMap<String, crate::event::InputTable>>,
    /// The reference bundle the run was launched with, from the anchor.
    pub reference_bundle: Option<crate::event::ReferenceUse>,
    /// How many samples the run was launched with, from the anchor.
    pub sample_count: Option<u32>,
    /// The run's samples and the hint for matching tasks to them, from the anchor.
    pub samples: Option<crate::event::SampleSpec>,
    /// What happens to the run's intermediate files, from the anchor (`None` ⇒ the default).
    pub retention: Option<crate::event::Retention>,
    /// When the retention sweep deleted the run's intermediate files (`work_deleted`).
    pub work_deleted_at_micros: Option<i64>,
    /// Who asked to cancel the run (the `cancel_requested` actor) and their label when it recorded
    /// one. Fold-internal: read through [`RunProjection::cancelled_by`].
    pub(super) cancel_actor: Option<String>,
    pub(super) cancel_actor_label: Option<String>,
    /// The experiment the anchor and the `run_preparing` say the run was launched into, each kept
    /// on its own so the fold is order-independent. Fold-internal: read through
    /// [`RunProjection::experiment_id`].
    pub(super) anchor_experiment_id: Option<String>,
    pub(super) preparing_experiment_id: Option<String>,
    /// The latest `experiment_set`'s experiment (`Some(None)`: taken out of its experiment) and the
    /// `(ts, seq)` it came from. Fold-internal: read through [`RunProjection::experiment_id`].
    pub(super) experiment_set: Option<Option<String>>,
    pub(super) experiment_set_at: Option<Stamp>,
    /// The resolved params at submit, already secret-redacted (from the anchor's `params`).
    pub params: serde_json::Value,
    /// The run's group-level dependency graph, folded from `run_dag` events as set-unions (order-
    /// independent). Node ids are rule/process names; edges are directed `from → to`.
    pub dag_nodes: BTreeSet<String>,
    pub dag_edges: BTreeSet<(String, String)>,
    /// The run's registered outputs keyed by `s3_uri`, folded as a set-union (order-independent).
    pub outputs: BTreeMap<String, OutputProjection>,
    // ── monotonic flags driving Run terminality ──
    // Each latches on and never clears, so `state` is order-independent: incremental and replay
    // folds converge regardless of delivery order.
    /// A `run_started` (engine) or coordinator RUNNING was seen.
    pub(super) running_seen: bool,
    /// A `run_preparing` was seen: the run was accepted but waited for its submit-time images.
    pub(super) preparing_seen: bool,
    /// The run was launched: its `run_submitted` anchor or any coordinator lifecycle event was
    /// seen. Ends PREPARING_IMAGES whichever order the events fold in.
    pub(super) launched_seen: bool,
    /// An explicit `run_cancelled` terminal was seen: the run is CANCELLED unless it succeeded.
    pub(super) run_cancelled: bool,
    /// A `run_succeeded` / `coordinator_succeeded` terminal was seen.
    pub(super) succeeded: bool,
    /// Source-time (epoch micros) of the earliest failure terminal (`run_failed` /
    /// `coordinator_failed`). Min-latched, so order-independent. With [`Self::cancel_order`], tells
    /// a failure a cancel caused (the cancel was requested first) from a run that failed on its own
    /// before anyone asked to cancel it.
    pub(super) failed_at_micros: Option<i64>,
    /// The `(ts, seq)` of the anchor the anchor-derived fields came from. A run has exactly one
    /// anchor (its dedup key carries no discriminator); should two ever fold, the earliest in log
    /// order supplies every anchor field, so the fields never mix and the fold is
    /// order-independent.
    pub(super) anchor_order: Option<Stamp>,
    /// The `(ts, seq)` of the `cancel_requested` the cancel actor came from: one per run (its dedup
    /// key carries no actor), but the earliest wins should two fold, as for the anchor.
    pub(super) cancel_order: Option<Stamp>,
}

impl RunProjection {
    /// The run's lifecycle state, derived from the monotonic flags.
    pub fn state(&self) -> RunState {
        self.state
    }

    /// The engine session this run executed as: the id the engine reported on `run_started`,
    /// else the one biffix launched it with. A pure function of two set-once fields.
    pub fn effective_session_id(&self) -> Option<&str> {
        self.reported_session_id.as_deref().or(self.engine_session_id.as_deref())
    }

    /// True when the engine reported a session other than the one biffix launched it with — i.e.
    /// the `NXF_UUID` pre-choice did not take. Surfaced on the run detail as a watch on that
    /// assumption.
    pub fn session_mismatch(&self) -> bool {
        matches!(
            (&self.engine_session_id, &self.reported_session_id),
            (Some(launched), Some(reported)) if launched != reported
        )
    }

    pub fn new(run_id: RunId) -> Self {
        Self {
            run_id,
            state: RunState::Submitted,
            tasks: BTreeMap::new(),
            completed: None,
            total: None,
            submitted_at_micros: None,
            launched_at_micros: None,
            completed_at_micros: None,
            submitter: None,
            submitter_label: None,
            coordinator_job_id: None,
            coordinator_log_stream: None,
            coordinator_host_id: None,
            coordinator_exit_code: None,
            coordinator_status_reason: None,
            coordinator_termination_category: None,
            workflow_version_id: None,
            input_uris: Vec::new(),
            output_prefix: None,
            engine_session_id: None,
            reported_session_id: None,
            resumed_from: None,
            run_name: None,
            launched_run_name: None,
            run_renamed: None,
            run_renamed_at: None,
            input_tables: None,
            reference_bundle: None,
            sample_count: None,
            samples: None,
            retention: None,
            work_deleted_at_micros: None,
            cancel_actor: None,
            cancel_actor_label: None,
            anchor_experiment_id: None,
            preparing_experiment_id: None,
            experiment_set: None,
            experiment_set_at: None,
            params: serde_json::Value::Null,
            dag_nodes: BTreeSet::new(),
            dag_edges: BTreeSet::new(),
            outputs: BTreeMap::new(),
            running_seen: false,
            preparing_seen: false,
            launched_seen: false,
            run_cancelled: false,
            succeeded: false,
            failed_at_micros: None,
            anchor_order: None,
            cancel_order: None,
        }
    }

    /// Who cancelled the run, for a person: the label the `cancel_requested` recorded, else the
    /// submitter's label when the submitter cancelled their own run. Never the raw actor id (an
    /// opaque IdP `sub` means nothing to a reader), so `None` when no label is known; also `None`
    /// unless the run ended CANCELLED on a recorded request (a request the run outlived, by
    /// succeeding, cancelled nothing). A pure function of folded fields, so order-independent.
    pub fn cancelled_by(&self) -> Option<String> {
        if self.state != RunState::Cancelled {
            return None;
        }
        let actor = self.cancel_actor.as_deref()?;
        let own_run = self.submitter.as_deref().and_then(submitter_user) == Some(actor);
        self.cancel_actor_label
            .clone()
            .or_else(|| own_run.then(|| self.submitter_label.clone()).flatten())
    }

    /// The experiment the run is in: the latest `experiment_set`'s (by `(ts, seq)`; `None` when it
    /// took the run out), else the one it was launched into: the anchor's, else the
    /// `run_preparing`'s. An `experiment_set` can only be recorded for a run that exists, so any of
    /// them postdates the launch and overrides it. Only an `experiment_set` takes a run out: an
    /// anchor without `experiment_id` means "not known", not "none", because a build from before
    /// experiments (a rollback mid-deploy) can launch a run a newer build parked, and its anchor
    /// drops the parked plan's experiment; the `run_preparing` still names it. A pure function of
    /// the folded event set, so order-independent.
    pub fn experiment_id(&self) -> Option<String> {
        match &self.experiment_set {
            Some(set) => set.clone(),
            None => {
                self.anchor_experiment_id.clone().or_else(|| self.preparing_experiment_id.clone())
            }
        }
    }

    /// Re-derive `run_name`: a recorded rename (even one that cleared the name) overrides the
    /// launch name. A rename can only be recorded for a run that exists, so it postdates the
    /// launch. A pure function of folded fields, so order-independent.
    pub(super) fn derive_run_name(&mut self) {
        self.run_name = match &self.run_renamed {
            Some(renamed) => renamed.clone(),
            None => self.launched_run_name.clone(),
        };
    }

    /// The in-app notification this run's fold produces: one when it has reached a terminal state
    /// and a user submitted it (a watcher-triggered run has no one to tell), addressed to
    /// that user, at the time it first ended. A pure function of the fold, so a replay rebuilds it.
    pub fn notification(&self) -> Option<RunNotification> {
        let state = self.state;
        if !state.is_terminal() {
            return None;
        }
        let user_sub = submitter_user(self.submitter.as_deref()?)?;
        Some(RunNotification {
            id: notification_id(self.run_id.as_str(), state),
            user_sub: user_sub.to_string(),
            kind: state,
            at_micros: self.completed_at_micros?,
        })
    }

    /// Whether the run's failure terminal is the cancel's doing: a `cancel_requested` stamped at or
    /// before the earliest failure (within [`CANCEL_CLOCK_SKEW_MICROS`]). The cancel handler records
    /// the request before it terminates the coordinator, so the failure that follows is stamped
    /// later; a run that failed on its own while the cancel was in flight (its failure had not yet
    /// reached the projection, so the cancel was accepted) stays FAILED.
    fn failure_was_cancelled(&self) -> bool {
        match (self.cancel_order, self.failed_at_micros) {
            (Some((cancel_at, _)), Some(failed_at)) => {
                cancel_at <= failed_at.saturating_add(CANCEL_CLOCK_SKEW_MICROS)
            }
            _ => false,
        }
    }

    /// Recompute `state` from the folded terminals. Precedence: a success wins; then an explicit
    /// `run_cancelled` is CANCELLED; a failure terminal is CANCELLED when a cancel was requested at
    /// or before it ([`Self::failure_was_cancelled`]), else FAILED; otherwise RUNNING once started,
    /// else PREPARING_IMAGES while a `run_preparing` run has not launched, else SUBMITTED. Every
    /// input is a monotonic flag or a min-latched time, so the state is a pure function of the
    /// folded event set, whatever order it folds in.
    pub(super) fn recompute_state(&mut self) {
        self.state = if self.succeeded {
            RunState::Succeeded
        } else if self.run_cancelled || self.failure_was_cancelled() {
            RunState::Cancelled
        } else if self.failed_at_micros.is_some() {
            RunState::Failed
        } else if self.running_seen {
            RunState::Running
        } else if self.preparing_seen && !self.launched_seen {
            RunState::PreparingImages
        } else {
            RunState::Submitted
        };
    }

    /// Latch the earliest terminal time (order-independent). A run commonly reaches a
    /// terminal via *two* events — an engine `run_succeeded` plus the coordinator's
    /// `coordinator_succeeded`, or a success terminal plus a non-success one — and both hit this
    /// field. First-write-wins made the recorded time depend on fold order (incremental vs replay
    /// diverged); the min over all terminal events is the deterministic "first reached a terminal
    /// state" the field means.
    pub(super) fn latch_completed_at(&mut self, ts_micros: i64) {
        keep_min_micros(&mut self.completed_at_micros, ts_micros);
    }

    /// Merge one reported output into the run's output set (see [`OutputProjection`]).
    pub(super) fn merge_output(
        &mut self,
        task_key: Option<&str>,
        attempt: Option<Attempt>,
        role: &OutputRole,
        reused: bool,
        r: &OutputRef,
    ) {
        // An empty URI names no object; drop it rather than project a row nobody can resolve.
        if r.s3_uri.is_empty() {
            return;
        }
        super::get_or_insert_with(&mut self.outputs, &r.s3_uri, || {
            OutputProjection::new(&r.s3_uri, role.clone(), reused)
        })
        .merge(task_key, attempt, role, reused, r);
    }

    /// Outputs a person would call the run's results: everything except work-dir intermediates.
    pub fn outputs_total(&self) -> u64 {
        self.outputs.values().filter(|o| o.role != OutputRole::Intermediate).count() as u64
    }

    /// A digest of the whole output set (every field of every object, in key order). Persistence
    /// compares it to the stored one so an advance that did not change the set skips rewriting it.
    pub fn outputs_digest(&self) -> String {
        use sha2::{Digest, Sha256};
        use std::fmt::Write as _;
        let mut h = Sha256::new();
        let mut line = String::new();
        for o in self.outputs.values() {
            // Debug of a flat struct of strings/numbers is deterministic within a binary; a digest
            // mismatch across binaries only costs one extra (idempotent) rewrite.
            line.clear();
            writeln!(line, "{o:?}").expect("writing to a String cannot fail");
            h.update(line.as_bytes());
        }
        hex::encode(h.finalize())
    }

    /// Of [`Self::outputs_total`], the objects satisfied from the engine cache, not produced by
    /// this run.
    pub fn outputs_reused(&self) -> u64 {
        self.outputs.values().filter(|o| o.role != OutputRole::Intermediate && o.reused).count()
            as u64
    }

    /// The sample each task belongs to (the Lab view's per-sample progress), by task key: from its
    /// key, its group and the anchor's sample spec, attributed in one pass by one
    /// [`crate::sample_progress::SampleMatcher`]. A pure function of the fold, so it is persisted
    /// with the task and a rule fix is a replay. `None` for a whole-study task.
    pub fn task_samples(&self) -> BTreeMap<String, Option<String>> {
        let default = crate::event::SampleSpec::default();
        let matcher =
            crate::sample_progress::SampleMatcher::new(self.samples.as_ref().unwrap_or(&default));
        self.tasks
            .iter()
            .map(|(key, t)| {
                let sample = matcher.attribute(t.task_key.as_str(), t.group.as_deref());
                (key.clone(), sample)
            })
            .collect()
    }

    /// The run's samples × steps, from its tasks (each at its effective state).
    pub fn sample_grid(&self) -> crate::sample_progress::SampleGrid {
        self.sample_grid_from(&self.task_samples())
    }

    /// [`Self::sample_grid`], given [`Self::task_samples`] already computed (a writer that also
    /// persists each task's sample attributes once).
    pub fn sample_grid_from(
        &self,
        task_samples: &BTreeMap<String, Option<String>>,
    ) -> crate::sample_progress::SampleGrid {
        let default = crate::event::SampleSpec::default();
        let spec = self.samples.as_ref().unwrap_or(&default);
        // The tasks credited with an output (as `run_output.task_key`, which the read side joins).
        let with_outputs: BTreeSet<&str> =
            self.outputs.values().filter_map(|o| o.task_key.as_deref()).collect();
        let tasks: Vec<crate::sample_progress::TaskFact> = self
            .tasks
            .iter()
            .map(|(key, t)| crate::sample_progress::TaskFact {
                sample: task_samples.get(key).cloned().flatten(),
                group: t.group.clone(),
                state: self.effective_task_state(t).as_str().to_string(),
                cached: t.cached,
                attempts: t.attempts.len() as i64,
                outputs: with_outputs.contains(t.task_key.as_str()),
            })
            .collect();
        crate::sample_progress::SampleGrid::build(
            spec,
            &tasks,
            (&self.dag_nodes, &self.dag_edges),
            crate::sample_progress::RunEnd::of(self.state()),
        )
    }

    /// A task's effective state for display/persistence. Once the *run* is terminal, a task the log
    /// left open cannot still be running — the run is over — so it surfaces as FAILED. This is a
    /// pure derivation over the observed event set (both run- and task-terminality are pure), not a
    /// fabricated event: it never invents a success, only resolves a task stranded `running` on an
    /// ended run (the "orphaned running task rows on terminal runs" the UI masked). A
    /// task that reached its own terminal keeps it — success included.
    pub fn effective_task_state(&self, task: &TaskProjection) -> TaskState {
        let state = task.state();
        if self.strands(state) {
            TaskState::Failed
        } else {
            state
        }
    }

    /// Whether `task` is stranded: still open (running or queued) in the log although the run has
    /// ended, so no result was ever reported for it. [`Self::effective_task_state`] surfaces such a
    /// task as FAILED; readers use this to say *why* ("no result reported") rather than implying
    /// the task itself failed.
    pub fn is_stranded(&self, task: &TaskProjection) -> bool {
        self.strands(task.state())
    }

    /// Whether a task whose own state is `state` is stranded by this run having ended.
    fn strands(&self, state: TaskState) -> bool {
        self.state.is_terminal() && matches!(state, TaskState::Running | TaskState::Queued)
    }

    /// The run's per-outcome task counts, using the same [`Self::effective_task_state`] the
    /// projection persists. Materialized onto `run_projection` during the fold so a run-list read
    /// need not aggregate `task_projection` per row. `succeeded` includes cache-hits
    /// (a cached task is a success); `cached` is the orthogonal subset that never ran as a job.
    pub fn task_counts(&self) -> TaskOutcomeCounts {
        let mut counts = TaskOutcomeCounts::default();
        for task in self.tasks.values() {
            counts.observed += 1;
            match self.effective_task_state(task) {
                TaskState::Succeeded => counts.succeeded += 1,
                TaskState::Running => counts.running += 1,
                TaskState::Queued => counts.queued += 1,
                TaskState::Failed => counts.failed += 1,
            }
            if task.cached {
                counts.cached += 1;
            }
        }
        counts
    }
}

/// A run's task counts by outcome, for list/summary views. `succeeded` counts every
/// done task including cache-hits; `cached` is the subset satisfied from cache (never a Batch job).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TaskOutcomeCounts {
    pub observed: u64,
    pub succeeded: u64,
    pub running: u64,
    pub queued: u64,
    pub failed: u64,
    pub cached: u64,
}
