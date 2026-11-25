//! The whole fold ([`Projection`]): routes each event to its run, task and attempt, binds AWS jobs
//! to the runs and tasks that own them, and holds the facts that arrive before their binding.

use std::borrow::Cow;
use std::collections::BTreeMap;

use crate::event::{
    EventData, OutputRole, RunEvent, RunPreparing, RunSubmitted, TaskVerdict,
    MAX_COORDINATOR_GENERATION,
};
use crate::ids::{Attempt, BatchJobId, InstanceId, RunId, TaskKey};
use crate::reducer::execution::merge_execution;
use crate::reducer::keep::{keep_latest, keep_min, keep_min_by, keep_min_micros};
use crate::reducer::{
    get_or_insert_with, AttemptProjection, Executions, RunProjection, TaskProjection, Verdict,
};
use crate::source::EventSource;
use crate::state::{AttemptState, TaskState};

/// The whole fold state: runs plus a pending set for unattributed AWS events.
#[derive(Clone, Debug, Default)]
pub struct Projection {
    pub runs: BTreeMap<String, RunProjection>,
    /// AWS events whose `batch_job_id` is not yet bound to a run. Keyed by `batch_job_id`.
    pending: BTreeMap<String, Vec<RunEvent>>,
    /// `batch_job_id → (run_id, task_key)` bindings published by `task_submitted`.
    ///
    /// First writer wins: a later `task_submitted` naming an already-bound job (another run's
    /// engine claiming a job it does not own) never re-routes that job's AWS facts. A job bound as
    /// a coordinator is never task-bound at all (see `coordinator_binding`). Two conflicting claims
    /// are a producer bug or a forgery; the reducer only keeps the first from hijacking the owner's
    /// facts — it cannot tell the true owner from a forger that folds first. Telling them apart is
    /// the ingest route's job (`biffix_backend::api::events`): before a claim is appended it checks the
    /// job's `biffix:run` Batch tag names the claiming run, that no other run already claims the
    /// job, and that the claim's `ts` does not predate the job, so a forged claim never reaches
    /// the log this folds.
    binding: BTreeMap<String, (RunId, TaskKey)>,
    /// `batch_job_id → run_id` for the COORDINATOR's job, published by the control plane at submit.
    ///
    /// Deliberately separate from `binding`: a coordinator has no `task_key`, and collapsing the
    /// two would let a coordinator event be folded as a task attempt. Without this map every
    /// `coordinator_*` event (which `from_aws_batch` emits with `run_id: None` by design) pends
    /// forever, so Run terminality — anchored on coordinator exit — never resolves.
    ///
    /// Authoritative over `binding`: the control plane (trusted) publishes it, while a task binding
    /// comes from an engine. It also decides coordinator-ness, not the translator's queue-name
    /// guess: an AWS event for a coordinator-bound job folds as coordinator lifecycle whatever kind
    /// it arrived as (see [`as_coordinator_event`]).
    coordinator_binding: BTreeMap<String, RunId>,
    /// Engine facts received before a mode anchor. Replayed once when a single-job anchor arrives;
    /// tasks-mode anchors discard the buffer because their eager aggregate already is correct.
    unanchored_engine: BTreeMap<String, BTreeMap<String, RunEvent>>,
}

/// Whether a single-job engine fact belongs to one coordinator generation, so a newer generation
/// replaces it. A Nextflow retry starts over from nothing (its work dir was on the lost host), so
/// all its facts are. A Snakemake retry keeps the work the lost host finished: each job's outputs
/// were uploaded to S3 as it ended and the run's metadata is restored from there, so the retry
/// skips those jobs and never reports them again. Its task facts (and their outputs) therefore
/// stay; only the run-level facts (progress, DAG, session, running and terminal state) are the
/// newest generation's.
fn generation_scoped(e: &RunEvent) -> bool {
    !(keeps_finished_tasks(&e.source)
        && (e.task_key.is_some() || matches!(e.data, EventData::TaskOutputs { .. })))
}

/// Whether a retried single-job coordinator of this engine keeps the tasks the lost host finished
/// (see [`generation_scoped`]).
fn keeps_finished_tasks(source: &EventSource) -> bool {
    *source == EventSource::Snakemake
}

/// The coordinator (head-job) lifecycle events. They bind on `coordinator_binding` and drive Run
/// terminality rather than a task attempt. Named once so the two sites that must agree
/// on the set — the binding step and the coordinator-job-id capture — cannot drift apart.
fn is_coordinator_lifecycle(data: &EventData) -> bool {
    matches!(
        data,
        EventData::CoordinatorSubmitted {}
            | EventData::CoordinatorRunning { .. }
            | EventData::CoordinatorSucceeded { .. }
            | EventData::CoordinatorFailed { .. }
    )
}

/// The two kinds that tell one execution of a Batch job (its start and its end). They fold only
/// into their job's executions: an execution is not its job's lifecycle, so the fold keeps them
/// off every path that moves an attempt's or a run's own facts. Named once so those sites cannot
/// drift apart.
fn is_batch_execution(data: &EventData) -> bool {
    matches!(data, EventData::BatchExecutionStarted { .. } | EventData::BatchExecutionEnded { .. })
}

/// The log stream a coordinator lifecycle event carries, if any.
fn coordinator_log_stream(data: &EventData) -> Option<&str> {
    match data {
        EventData::CoordinatorRunning { log_stream }
        | EventData::CoordinatorSucceeded { log_stream }
        | EventData::CoordinatorFailed { log_stream, .. } => log_stream.as_deref(),
        _ => None,
    }
}
/// Whether `e`'s source may produce its kind, for the kinds only the control plane, the Batch
/// translator or an engine publishes.
///
/// The engine ingest route refuses these kinds from an engine, and stores an unknown engine kind
/// only under the engine namespace (`translate::ENGINE_KIND_PREFIX`), so no engine can append one
/// today. This is the fold's own half of that rule, for rows the ingest check cannot reach: one an
/// earlier build stored as an unknown kind that this build knows as a control-plane fact. Such a
/// row would otherwise fold on replay as if the control plane had written it (an immutable log
/// cannot drop it). A new control-plane kind is added here with its producers.
///
/// `coordinator_submitted` has two producers: the control plane's binding at submit, and the
/// translation of the coordinator job's own AWS Batch `SUBMITTED` state change. A Batch job's
/// other lifecycle kinds fold from the Batch translator alone when they name a job (no producer but
/// it emits them today, so this is the same guard for a row the ingest check cannot reach). A Batch
/// job's executions fold from the Batch translator alone too, job named or not. Every other kind
/// folds from any source.
pub fn is_trusted_producer(e: &RunEvent) -> bool {
    match e.data {
        EventData::RunSubmitted(_)
        | EventData::RunPreparing(_)
        | EventData::RunCancelled {}
        | EventData::CancelRequested { .. }
        | EventData::ExperimentSet { .. }
        | EventData::RunRenamed { .. }
        | EventData::WorkDeleted {}
        | EventData::BuildRequested { .. } => e.source == EventSource::BiffixControlPlane,
        EventData::CoordinatorSubmitted {} => {
            matches!(e.source, EventSource::BiffixControlPlane | EventSource::AwsBatch)
        }
        // The engine's own verdict: only an engine speaks for the engine.
        EventData::EngineTaskVerdict { .. } => {
            matches!(e.source, EventSource::Nextflow | EventSource::Snakemake)
        }
        // A Batch job's lifecycle: the coordinator's, and a task's that names its job, come only
        // from the Batch translator (an engine names a job only on `task_submitted`; its own
        // terminals for local rules carry none). One that also carries a `run_id` folds straight
        // onto the job it names, so from another producer it could bind any job as the run's
        // coordinator (taking a task's facts away) or overwrite another run's attempt.
        EventData::CoordinatorRunning { .. }
        | EventData::CoordinatorSucceeded { .. }
        | EventData::CoordinatorFailed { .. } => e.source == EventSource::AwsBatch,
        EventData::TaskQueued {}
        | EventData::TaskStarted { .. }
        | EventData::TaskSucceeded { .. }
        | EventData::TaskFailed { .. }
            if e.batch_job_id.is_some() =>
        {
            e.source == EventSource::AwsBatch
        }
        // An execution is AWS's account of one run of a job on a host; nobody else has one.
        ref data if is_batch_execution(data) => e.source == EventSource::AwsBatch,
        _ => true,
    }
}

/// An AWS Batch event for a job the reducer knows is a coordinator, read as coordinator lifecycle.
///
/// The translator tells coordinator from worker by the job's queue name, but the control plane's
/// `coordinator_submitted` binding is authoritative: a coordinator launched on a queue the ingest
/// does not list arrives task-shaped, and must still drive run terminality. A failure keeps its
/// exit code, reason and category, so nothing the run projection reads is lost.
fn as_coordinator_event(e: &RunEvent) -> Cow<'_, RunEvent> {
    let data = match &e.data {
        EventData::TaskQueued {} => EventData::CoordinatorSubmitted {},
        EventData::TaskStarted { log_stream, .. } => {
            EventData::CoordinatorRunning { log_stream: log_stream.clone() }
        }
        EventData::TaskSucceeded { log_stream, .. } => {
            EventData::CoordinatorSucceeded { log_stream: log_stream.clone() }
        }
        EventData::TaskFailed { exit_code, termination_category, status_reason, log_stream } => {
            EventData::CoordinatorFailed {
                log_stream: log_stream.clone(),
                // The translator's "none reported" sentinel is no exit code.
                exit_code: (*exit_code >= 0).then_some(*exit_code),
                status_reason: status_reason.clone(),
                termination_category: termination_category.clone(),
            }
        }
        _ => return Cow::Borrowed(e),
    };
    Cow::Owned(RunEvent { data, ..e.clone() })
}

/// The inverse of [`as_coordinator_event`]: a coordinator-shaped AWS event for a job bound to a
/// task (a worker that ran on a queue the ingest lists as a coordinator queue), read as task
/// lifecycle. A terminal without an exit code takes the default the translator uses when Batch
/// reports none (`0` / `-1`).
fn as_task_event(e: &RunEvent) -> Cow<'_, RunEvent> {
    let data = match &e.data {
        EventData::CoordinatorSubmitted {} => EventData::TaskQueued {},
        EventData::CoordinatorRunning { log_stream } => {
            EventData::TaskStarted { started_at: None, log_stream: log_stream.clone() }
        }
        EventData::CoordinatorSucceeded { log_stream } => {
            EventData::TaskSucceeded { exit_code: 0, log_stream: log_stream.clone() }
        }
        EventData::CoordinatorFailed {
            log_stream,
            exit_code,
            status_reason,
            termination_category,
        } => EventData::TaskFailed {
            exit_code: exit_code.unwrap_or(-1),
            termination_category: termination_category.clone(),
            status_reason: status_reason.clone(),
            log_stream: log_stream.clone(),
        },
        _ => return Cow::Borrowed(e),
    };
    Cow::Owned(RunEvent { data, ..e.clone() })
}

impl Projection {
    pub fn new() -> Self {
        Self::default()
    }

    /// Replay fold: apply a whole log in `(ts, seq)` order.
    ///
    /// Ordering is intrinsic to the events, not to how the caller passed them: events are sorted by
    /// source time and then by log sequence ([`crate::event::LogSeq`]), so equal-`ts` ties break on
    /// the log's authoritative total order rather than on the input slice's order + a stable sort.
    /// Events with no assigned `seq` (freshly built or test events) sort
    /// before sequenced ones and, among themselves, keep their input order (the sort is stable).
    pub fn replay(events: &[RunEvent]) -> Self {
        let mut ordered: Vec<&RunEvent> = events.iter().collect();
        ordered.sort_by_key(|e| e.log_order());
        let mut p = Projection::new();
        for e in ordered {
            p.apply(e);
        }
        p
    }

    /// Incremental fold: apply one event. Idempotent and order-tolerant.
    pub fn apply(&mut self, e: &RunEvent) {
        // 0. A control-plane fact folds only from its producer ([`is_trusted_producer`]): a row an
        // engine planted under such a kind — as an unknown kind, before a later build learned it —
        // is kept by the log but moves nothing.
        if !is_trusted_producer(e) {
            return;
        }
        // 1. A binding published by task_submitted lets us stitch pending AWS events.
        if let EventData::TaskSubmitted { image } = &e.data {
            if let (Some(run), Some(task), Some(job)) = (&e.run_id, &e.task_key, &e.batch_job_id) {
                // The claim is recorded in the claiming run whatever the binding decides below, so
                // the claimer's own projection does not depend on which claim folded first.
                let ap = self.ensure_attempt(run, task, job, e.attempt, AttemptState::Submitted);
                keep_min_micros(&mut ap.submitted_at_micros, e.ts.as_micros());
                keep_min(&mut ap.image, image.as_deref());
                // Route the job's AWS facts here only if nobody owns the job yet: a coordinator
                // binding is authoritative, and an earlier task binding is never replaced.
                let owns = if self.coordinator_binding.contains_key(job.as_str()) {
                    false
                } else {
                    let (owner_run, owner_task) = self
                        .binding
                        .entry(job.as_str().to_string())
                        .or_insert_with(|| (run.clone(), task.clone()));
                    owner_run == run && owner_task == task
                };
                // flush any pending AWS events for this job
                if owns {
                    if let Some(pending) = self.pending.remove(job.as_str()) {
                        for pe in pending {
                            self.apply_attributed(run, task, &pe);
                        }
                    }
                }
            }
            return;
        }

        // 1b. The coordinator's analogue of task_submitted: the control plane knows both halves at
        // submit (it mints the run_id; SubmitJob returns the job id) and publishes the pairing.
        // Registered before the run_id fast path below, which would otherwise apply the event and
        // drop the binding on the floor.
        if is_coordinator_lifecycle(&e.data) {
            if let (Some(run), Some(job)) = (&e.run_id, &e.batch_job_id) {
                let owner =
                    get_or_insert_with(&mut self.coordinator_binding, job.as_str(), || run.clone())
                        .clone();
                // The coordinator binding is authoritative: a task claim on this job folded earlier
                // stops receiving its AWS facts from here on.
                self.binding.remove(job.as_str());
                // Flush anything that arrived before the binding. Every run-level flag is
                // monotonic, so applying these before the current event is order-safe.
                if let Some(pending) = self.pending.remove(job.as_str()) {
                    for pe in pending {
                        self.apply_run_scoped(&owner, &as_coordinator_event(&pe));
                    }
                }
            }
        }

        // 2. Events that carry a run_id apply directly. Not an execution's: it belongs to its
        // job and to whoever that job is bound to, so one that names a run all the same (no
        // producer does) cannot put an execution on a run that does not own the job.
        if let Some(run) = e.run_id.as_ref().filter(|_| !is_batch_execution(&e.data)) {
            self.apply_run_scoped(run, e);
            return;
        }

        // 3. AWS events with a batch_job_id but no run_id (and every execution event): attribute
        // via binding, else pend.
        if let Some(job) = &e.batch_job_id {
            if let Some(run) = self.coordinator_binding.get(job.as_str()).cloned() {
                // A coordinator job: run-level only, never a task attempt — whatever kind the
                // translator gave the event (the binding, not the queue name, decides).
                self.apply_run_scoped(&run, &as_coordinator_event(e));
            } else if let Some((run, task)) = self.binding.get(job.as_str()).cloned() {
                self.apply_attributed(&run, &task, e);
            } else {
                self.pending.entry(job.as_str().to_string()).or_default().push(e.clone());
            }
        }
        // Events with neither run_id nor batch_job_id (e.g. a bare spot warning) carry no
        // attribution; they are stored by the log but do not move a projection here.
    }

    fn run_mut(&mut self, run: &RunId) -> &mut RunProjection {
        get_or_insert_with(&mut self.runs, run.as_str(), || RunProjection::new(run.clone()))
    }

    /// Ensure the (run, task) rows exist and return the task projection.
    fn ensure_task(&mut self, run: &RunId, task: &TaskKey) -> &mut TaskProjection {
        get_or_insert_with(&mut self.run_mut(run).tasks, task.as_str(), || {
            TaskProjection::new(task.clone())
        })
    }

    /// Ensure the (run, task, attempt) rows exist, advance the attempt to `at_least`, and return
    /// the attempt projection — so callers that need it need not re-descend the maps (mirrors
    /// [`Self::ensure_task`]).
    fn ensure_attempt(
        &mut self,
        run: &RunId,
        task: &TaskKey,
        job: &BatchJobId,
        attempt: Option<Attempt>,
        at_least: AttemptState,
    ) -> &mut AttemptProjection {
        let ap =
            get_or_insert_with(&mut self.ensure_task(run, task).attempts, job.as_str(), || {
                AttemptProjection::new(job.clone())
            });
        if attempt.is_some() {
            ap.attempt = attempt;
        }
        ap.advance_to(at_least);
        // Retry-pending is *derived* from (max_next_attempt vs observed attempt numbers), not
        // toggled here — so a duplicate/out-of-order submit cannot regress the task state.
        ap
    }

    /// Apply an AWS/attempt event once its (run, task) is known.
    fn apply_attributed(&mut self, run: &RunId, task: &TaskKey, e: &RunEvent) {
        let Some(job) = &e.batch_job_id else {
            return;
        };
        // An execution folds into the job's executions and nowhere else. It stops here, before
        // the attempt's own latches: its host is its own, not the attempt's (the attempt keeps
        // the host its job's lifecycle named), and its start, stop, reason and exit code are one
        // execution's, never the attempt's state, span or terminal detail. It carries no engine
        // attempt number either, so none is taken from it. The queue is the job's, and the same
        // on every execution.
        if is_batch_execution(&e.data) {
            let ap = self.ensure_attempt(run, task, job, None, AttemptState::Created);
            keep_min(&mut ap.queue, e.queue.as_deref());
            merge_execution(&mut ap.executions, e);
            return;
        }
        // A task-bound job's AWS facts are task lifecycle even if the translator (going by queue
        // name) shaped them as coordinator events.
        let e = &*as_task_event(e);
        let ap = self.ensure_attempt(run, task, job, e.attempt, AttemptState::Created);
        // A Batch retry within the job can move to another host: the latest event's host wins,
        // whatever order they fold in.
        let stamp = e.log_order();
        if let Some(instance) = &e.instance_id {
            keep_latest(
                &mut ap.instance_id,
                &mut ap.instance_at,
                instance,
                stamp,
                InstanceId::as_str,
            );
        }
        keep_min(&mut ap.queue, e.queue.as_deref());
        keep_min_by(&mut ap.requested_vcpus, e.requested_vcpus, f64::min);
        keep_min_by(&mut ap.requested_memory_mib, e.requested_memory_mib, i64::min);
        match &e.data {
            EventData::TaskQueued {} => ap.advance_to(AttemptState::Queued),
            EventData::TaskStarted { started_at, log_stream } => {
                ap.advance_to(AttemptState::Running);
                if let Some(stream) = log_stream {
                    keep_latest(
                        &mut ap.live_log_stream,
                        &mut ap.log_stream_at,
                        stream,
                        stamp,
                        String::as_str,
                    );
                }
                // Latch the EARLIEST start (min), not last-write: AWS STARTING and RUNNING both
                // arrive as TaskStarted, so an attempt sees two, and an unconditional overwrite
                // made this depend on fold order (incremental diverged from replay). Min over the
                // observed set is order-independent. Prefer the explicit started_at; else the
                // event's own source-time.
                let started =
                    started_at.as_ref().map_or_else(|| e.ts.as_micros(), |t| t.as_micros());
                keep_min_micros(&mut ap.started_at_micros, started);
            }
            EventData::TaskSucceeded { exit_code, log_stream } => {
                // Success is the winning terminal (see `rank`); record its detail and clear any
                // failure category a Failed terminal folded earlier had set. There is at most one
                // Succeeded per attempt, so this is idempotent and order-independent.
                ap.advance_to(AttemptState::Succeeded);
                ap.exit_code = Some(*exit_code);
                ap.termination_category = None;
                ap.status_reason = None;
                ap.finished_at_micros = Some(e.ts.as_micros());
                keep_min(&mut ap.succeeded_log_stream, log_stream.as_deref());
            }
            EventData::TaskFailed {
                exit_code,
                termination_category,
                status_reason,
                log_stream,
            } => {
                ap.advance_to(AttemptState::Failed);
                // Kept whatever wins; shown only while failure is the winning terminal.
                keep_min(&mut ap.failed_log_stream, log_stream.as_deref());
                // Record failure detail only while failure is still the winning terminal. If a
                // success terminal has folded (or folds later), it outranks this and owns the
                // detail — so the terminal detail is a pure function of the event set, never a
                // mix of a success state with failure detail (or vice versa), regardless of the
                // order the two terminals arrive.
                if ap.state == AttemptState::Failed {
                    // `-1` is the translator's "none was reported" sentinel, never a real exit.
                    ap.exit_code = (*exit_code >= 0).then_some(*exit_code);
                    if termination_category.is_some() {
                        ap.termination_category = termination_category.clone();
                    }
                    if status_reason.is_some() {
                        ap.status_reason = status_reason.clone();
                    }
                    ap.finished_at_micros = Some(e.ts.as_micros());
                }
            }
            _ => {}
        }
        ap.refresh_log_stream();
    }

    /// Apply a run-scoped event (engine run_*, coordinator_*, control-plane, or an
    /// engine task_* that already carries run_id + task_key).
    ///
    /// A single-job run's engine facts are scoped to its coordinator generation (a Batch retry
    /// after losing the host): see [`generation_scoped`].
    fn apply_run_scoped(&mut self, run: &RunId, e: &RunEvent) {
        let engine = e.source.is_engine() && !matches!(e.data, EventData::Unknown(_));
        if engine {
            let rp = self.run_mut(run);
            let anchored = rp.anchor_order.is_some() || rp.preparing_seen;
            if !anchored {
                // Applied eagerly below (right for a tasks-mode run) and kept until the anchor
                // says whether the run is single-job, which replays it by generation.
                self.unanchored_engine
                    .entry(run.as_str().into())
                    .or_default()
                    .insert(e.dedup_key.clone(), e.clone());
            } else if rp.single_job.is_some() {
                let generation = e.coordinator_generation();
                if generation > MAX_COORDINATOR_GENERATION {
                    return;
                }
                if generation_scoped(e) {
                    if generation < rp.coordinator_attempt {
                        return;
                    }
                    if generation > rp.coordinator_attempt {
                        rp.reset_engine_generation(generation, keeps_finished_tasks(&e.source));
                    }
                }
            }
        }
        self.apply_run_fact(run, e);
        if matches!(e.data, EventData::RunSubmitted(_) | EventData::RunPreparing(_)) {
            if let Some(buffered) = self.unanchored_engine.remove(run.as_str()) {
                if self.run_mut(run).single_job.is_some() {
                    let buffered: Vec<&RunEvent> = buffered
                        .values()
                        .filter(|e| e.coordinator_generation() <= MAX_COORDINATOR_GENERATION)
                        .collect();
                    let generation = buffered
                        .iter()
                        .filter(|e| generation_scoped(e))
                        .map(|e| e.coordinator_generation())
                        .max()
                        .unwrap_or(1);
                    let keep = buffered.iter().any(|e| keeps_finished_tasks(&e.source));
                    self.run_mut(run).reset_engine_generation(generation, keep);
                    // The facts a reset kept (a Snakemake run's tasks) were applied eagerly
                    // already; the rest are the newest generation's.
                    for fact in buffered.into_iter().filter(|e| {
                        generation_scoped(e) && e.coordinator_generation() == generation
                    }) {
                        self.apply_run_fact(run, fact);
                    }
                }
            }
        }
    }

    fn apply_run_fact(&mut self, run: &RunId, e: &RunEvent) {
        // An execution arrives here only for a coordinator-bound job, and folds into that job's
        // executions and nowhere else. It stops here, before everything below: an execution is
        // not coordinator lifecycle, so it neither launches the run nor names its coordinator
        // job, log stream or host, and its end is not the coordinator's (Batch may run the job
        // again), so the run's state does not move.
        if is_batch_execution(&e.data) {
            if let Some(job) = &e.batch_job_id {
                let executions = &mut self.run_mut(run).coordinator_executions;
                merge_execution(get_or_insert_with(executions, job.as_str(), Executions::new), e);
            }
            return;
        }
        // Task-scoped engine events that carry their own task_key.
        if let Some(task) = e.task_key.clone() {
            match &e.data {
                EventData::TaskCreated { group, deps, image } => {
                    let tp = self.ensure_task(run, &task);
                    if let Some(g) = group {
                        tp.set_group(g);
                    }
                    if let Some(i) = image {
                        tp.set_created_image(i);
                    }
                    // A task never depends on itself (a plugin mapping slip), so drop a self-edge.
                    tp.deps.extend(deps.iter().filter(|d| d.as_str() != task.as_str()).cloned());
                    return;
                }
                EventData::TaskRetrying { next_attempt, .. } => {
                    let tp = self.ensure_task(run, &task);
                    tp.record_retry(*next_attempt, e.ts.as_micros());
                    return;
                }
                // The engine failed an attempt: recorded on the task and weighed against its
                // attempts when the task is read (`TaskProjection::counted_state`). A verdict this
                // build does not know folds to nothing.
                EventData::EngineTaskVerdict { verdict, category, reason } => {
                    if *verdict == TaskVerdict::Failed {
                        self.ensure_task(run, &task).record_verdict(Verdict {
                            attempt: e.attempt,
                            at_micros: e.ts.as_micros(),
                            category: category.clone(),
                            reason: reason.clone(),
                        });
                    }
                    return;
                }
                // A task-lifecycle event that already carries (run_id, task_key, batch_job_id)
                // is attributed directly rather than silently dropped (the two-stream join).
                EventData::TaskQueued {}
                | EventData::TaskStarted { .. }
                | EventData::TaskSucceeded { .. }
                | EventData::TaskFailed { .. }
                    if e.batch_job_id.is_some() =>
                {
                    self.apply_attributed(run, &task, e);
                    return;
                }
                // A task terminal with NO batch_job_id is an engine-reported terminal for a local
                // rule (never submitted to Batch, so no attempt exists). Record it at the task
                // level; `state()` honors it only when there are zero attempts, keeping AWS
                // authoritative for Batch tasks.
                EventData::TaskSucceeded { .. } => {
                    self.ensure_task(run, &task).record_engine_terminal(TaskState::Succeeded);
                    return;
                }
                // A cache-hit: a succeeded terminal (it counts as done) flagged cached, so it is
                // not counted as work run this time. Cache-hits never became a Batch job, so this
                // is always the attempt-less engine-terminal path.
                EventData::TaskCached {} => {
                    let tp = self.ensure_task(run, &task);
                    tp.record_engine_terminal(TaskState::Succeeded);
                    tp.mark_cached();
                    return;
                }
                EventData::TaskFailed { .. } => {
                    let tp = self.ensure_task(run, &task);
                    tp.record_engine_terminal(TaskState::Failed);
                    tp.record_engine_failure(e.attempt, e.ts.as_micros());
                    return;
                }
                _ => {}
            }
        }

        // Run/coordinator/control-plane level. Each arm sets a monotonic flag (or advances a
        // monotonic field) and re-derives `state`; the CANCELLED-vs-FAILED disambiguation is a
        // property of the flag set, not of arrival order.
        let rp = self.run_mut(run);
        // Any coordinator lifecycle event carries the coordinator's Batch job id (bound via
        // coordinator_binding). Record it so the run detail can link to the coordinator job/logs —
        // the only visible artifact when a run fails before scheduling any child task.
        // Latched like the stream and host below: a conflicting id folds the same in any order.
        if is_coordinator_lifecycle(&e.data) {
            keep_min(&mut rp.coordinator_job_id, e.batch_job_id.as_ref().map(BatchJobId::as_str));
            rp.launched_seen = true;
            keep_min_micros(&mut rp.launched_at_micros, e.ts.as_micros());
            keep_min(&mut rp.coordinator_log_stream, coordinator_log_stream(&e.data));
            if e.source == EventSource::AwsBatch {
                keep_min(
                    &mut rp.coordinator_host_id,
                    e.instance_id.as_ref().map(InstanceId::as_str),
                );
            }
        }
        match &e.data {
            // Outputs fold as a set-union keyed on the object (task-scoped or run-level alike).
            EventData::TaskOutputs { role, reused, outputs } => {
                let role = role.clone().unwrap_or(OutputRole::Output);
                let task_key = e.task_key.as_ref().map(TaskKey::as_str);
                for r in outputs {
                    rp.merge_output(task_key, e.attempt, &role, *reused, r);
                }
                return;
            }
            // The group-level dependency graph: fold nodes and edges as set-unions so re-emission
            // or out-of-order delivery converges to the same graph.
            EventData::RunDag { dag } => {
                for node in &dag.nodes {
                    rp.dag_nodes.insert(node.id.clone());
                }
                for edge in &dag.edges {
                    rp.dag_edges.insert((edge.from.clone(), edge.to.clone()));
                }
                return;
            }
            EventData::RunSubmitted(anchor) => {
                let RunSubmitted {
                    workflow_version_id,
                    submitter,
                    submitter_label,
                    input_uris,
                    params,
                    output_prefix,
                    engine_session_id,
                    resumed_from,
                    run_name,
                    input_tables,
                    reference_bundle,
                    sample_count,
                    samples,
                    retention,
                    experiment_id,
                    single_job,
                    ..
                } = &**anchor;
                // The anchor carries provenance/timing for the monitor's Runs columns. There is one
                // anchor per run (see `anchor_order`); the earliest in log order supplies them all.
                rp.launched_seen = true;
                keep_min_micros(&mut rp.launched_at_micros, e.ts.as_micros());
                // The earliest of the anchor and a `run_preparing`: when the user submitted.
                keep_min_micros(&mut rp.submitted_at_micros, e.ts.as_micros());
                let order = e.log_order();
                if rp.anchor_order.is_none_or(|current| order < current) {
                    rp.anchor_order = Some(order);
                    rp.workflow_version_id = Some(workflow_version_id.clone());
                    rp.submitter = submitter.clone();
                    rp.submitter_label = submitter_label.clone();
                    rp.input_uris = input_uris.clone();
                    rp.output_prefix = output_prefix.clone();
                    rp.engine_session_id = engine_session_id.clone();
                    rp.resumed_from = resumed_from.clone();
                    rp.launched_run_name = run_name.clone();
                    rp.derive_run_name();
                    rp.input_tables = input_tables.clone();
                    rp.reference_bundle = reference_bundle.clone();
                    rp.sample_count = *sample_count;
                    rp.samples.clone_from(samples);
                    rp.retention.clone_from(retention);
                    rp.anchor_experiment_id.clone_from(experiment_id);
                    rp.single_job.clone_from(single_job);
                    // Resolved params, already secret-redacted at submit (the backend's
                    // runs/submit/redaction.rs runs before the event is built), so this is safe to
                    // project and surface.
                    rp.params = params.clone();
                }
            }
            EventData::RunPreparing(p) => {
                rp.preparing_seen = true;
                keep_min_micros(&mut rp.submitted_at_micros, e.ts.as_micros());
                // Kept whether or not the anchor folded first: [`RunProjection::experiment_id`]
                // falls back to it when the anchor names none. One `run_preparing` per run.
                rp.preparing_experiment_id.clone_from(&p.experiment_id);
                // Provisional facts until the anchor folds; the anchor's win whichever order they
                // arrive in (it overwrites these, and once it has folded these are skipped).
                if rp.anchor_order.is_none() {
                    let RunPreparing {
                        workflow_version_id,
                        params,
                        input_uris,
                        submitter,
                        submitter_label,
                        run_name,
                        output_prefix,
                        resumed_from,
                        images: _,
                        experiment_id: _,
                        single_job,
                    } = &**p;
                    rp.workflow_version_id = Some(workflow_version_id.clone());
                    rp.submitter = submitter.clone();
                    rp.submitter_label = submitter_label.clone();
                    rp.launched_run_name = run_name.clone();
                    rp.derive_run_name();
                    rp.input_uris = input_uris.clone();
                    rp.output_prefix = output_prefix.clone();
                    rp.resumed_from = resumed_from.clone();
                    rp.single_job.clone_from(single_job);
                    // Redacted at submit, like the anchor's.
                    rp.params = params.clone();
                }
            }
            EventData::RunStarted { session_id } => {
                rp.engine_running_seen = true;
                // Each coordinator generation starts the engine again, so the newest generation's
                // session is the one that ran last; conflicting sessions within one generation
                // settle on the least. Both are independent of arrival order.
                let generation = e.coordinator_generation();
                if generation > rp.reported_session_generation {
                    rp.reported_session_generation = generation;
                    rp.reported_session_id = None;
                }
                if generation == rp.reported_session_generation {
                    keep_min(&mut rp.reported_session_id, session_id.as_deref());
                }
            }
            EventData::CoordinatorRunning { .. } => {
                rp.running_seen = true;
            }
            EventData::RunProgress { completed, total } => {
                rp.engine_running_seen = true;
                // Order-independent monotonic progress: keep the highest `completed`
                // observed; on a tie in `completed`, keep the larger `total`. A DAG only grows (a
                // checkpoint can raise `total` while `completed` is unchanged), so this both stops
                // a stale event moving the bar backward AND makes the folded (completed, total) a
                // pure function of the observed set. The old `>=` with an unconditional `total`
                // overwrite was last-write-wins for `total` at equal `completed`, so it diverged
                // under reordering. (The translate-side dedup key now also carries `total`, so the
                // two facts reach the fold rather than colliding.)
                let advances = match rp.completed {
                    None => true,
                    Some(c) if *completed > c => true,
                    Some(c) if *completed == c => rp.total.is_none_or(|t| *total > t),
                    Some(_) => false,
                };
                if advances {
                    rp.completed = Some(*completed);
                    rp.total = Some(*total);
                }
            }
            EventData::CancelRequested { actor, actor_label } => {
                let order = e.log_order();
                if rp.cancel_order.is_none_or(|current| order < current) {
                    rp.cancel_order = Some(order);
                    rp.cancel_actor = Some(actor.clone());
                    rp.cancel_actor_label.clone_from(actor_label);
                }
            }
            // Repeatable: the latest `(ts, seq)` wins, whatever order the changes fold in.
            EventData::ExperimentSet { experiment_id, .. } => {
                keep_latest(
                    &mut rp.experiment_set,
                    &mut rp.experiment_set_at,
                    experiment_id,
                    e.log_order(),
                    |set| set.as_deref().unwrap_or(""),
                );
            }
            // Repeatable: the latest `(ts, seq)` wins, whatever order the renames fold in.
            EventData::RunRenamed { run_name, .. } => {
                keep_latest(
                    &mut rp.run_renamed,
                    &mut rp.run_renamed_at,
                    run_name,
                    e.log_order(),
                    |name| name.as_deref().unwrap_or(""),
                );
                rp.derive_run_name();
            }
            // One per run; the earliest wins so a replayed fold agrees with the incremental one.
            EventData::WorkDeleted {} => {
                keep_min_micros(&mut rp.work_deleted_at_micros, e.ts.as_micros());
            }
            EventData::RunSucceeded {} if e.source.is_engine() => {
                rp.engine_succeeded = true;
                keep_min_micros(&mut rp.engine_completed_at_micros, e.ts.as_micros());
            }
            EventData::RunFailed { .. } if e.source.is_engine() => {
                keep_min_micros(&mut rp.engine_failed_at_micros, e.ts.as_micros());
                keep_min_micros(&mut rp.engine_completed_at_micros, e.ts.as_micros());
            }
            EventData::RunSucceeded {} | EventData::CoordinatorSucceeded { .. } => {
                rp.succeeded = true;
                rp.latch_completed_at(e.ts.as_micros());
            }
            // A `run_cancelled` is an explicit cancellation: the run is CANCELLED unless it
            // succeeded, with or without a `cancel_requested` before it.
            EventData::RunCancelled {} => {
                rp.run_cancelled = true;
                rp.latch_completed_at(e.ts.as_micros());
            }
            // A failure is CANCELLED only when a cancel was requested before it (see
            // `RunProjection::recompute_state`), so its time is kept.
            EventData::RunFailed { .. } | EventData::CoordinatorFailed { .. } => {
                keep_min_micros(&mut rp.failed_at_micros, e.ts.as_micros());
                rp.latch_completed_at(e.ts.as_micros());
                // Batch's account of the coordinator's stop, for the failure summary when the
                // engine wrote no log. A negative exit code is no exit code (`-1` is "none").
                if let EventData::CoordinatorFailed {
                    exit_code,
                    status_reason,
                    termination_category,
                    ..
                } = &e.data
                {
                    keep_min_by(
                        &mut rp.coordinator_exit_code,
                        exit_code.filter(|c| *c >= 0),
                        i32::min,
                    );
                    keep_min(&mut rp.coordinator_status_reason, status_reason.as_deref());
                    keep_min(
                        &mut rp.coordinator_termination_category,
                        termination_category
                            .as_ref()
                            .map(crate::event::TerminationCategory::as_str),
                    );
                }
            }
            _ => {}
        }
        rp.recompute_state();
    }
}
