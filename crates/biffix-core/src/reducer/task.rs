//! The task level of the fold: a task ([`TaskProjection`]) reduced over its attempts, and the
//! engine's verdicts on them.

use std::collections::BTreeMap;

use crate::event::TerminationCategory;
use crate::ids::{Attempt, TaskKey};
use crate::reducer::keep::{keep_min, keep_min_micros};
use crate::reducer::AttemptProjection;
use crate::state::{AttemptState, TaskState};

// Named only by the doc link of `Verdict`.
#[cfg(doc)]
use crate::event::EventData;

#[derive(Clone, Debug, PartialEq)]
pub struct TaskProjection {
    pub task_key: TaskKey,
    /// Attempts keyed by `batch_job_id` (so iterated in job-id order, not submission order:
    /// [`TaskProjection::display_attempt`] orders by attempt number).
    pub attempts: BTreeMap<String, AttemptProjection>,
    /// The highest `next_attempt` announced by any `task_retrying` event.
    ///
    /// Stored as a monotonic max rather than an arrival-order toggle so that `retry_pending`
    /// is a pure function of the observed event *set*: incremental and replay folds must agree
    /// under any delivery order. A retry to attempt `N` is pending until an attempt
    /// answering it is actually observed (see [`TaskProjection::retry_pending`]).
    max_next_attempt: Option<Attempt>,
    /// Source-time (epoch micros) of the earliest `task_retrying` that announced
    /// `max_next_attempt`. A pure function of the event set (max attempt, then min time).
    retry_announced_micros: Option<i64>,
    /// Engine-reported terminal for a **local** rule (one run on the coordinator, never submitted
    /// to Batch, so it has no attempt). `None` for a normal Batch task, whose terminality is a
    /// reduction over its attempts. Honored by [`TaskProjection::state`] only when there are zero
    /// attempts. Latched monotonically (success wins, and never regresses) so the projection stays
    /// a pure function of the observed event set.
    engine_terminal: Option<TaskState>,
    /// The highest attempt number an engine-reported local failure carried, if any did.
    engine_failed_attempt: Option<Attempt>,
    /// Source-time (epoch micros) of the latest engine-reported local failure. With
    /// `engine_failed_attempt`, tells a failure *before* a retry announcement (the retry is still
    /// pending) from the retry's own failure. Max-latched, so order-independent.
    engine_failed_at_micros: Option<i64>,
    /// Whether this task was satisfied from the engine cache (a `task_cached` event). Orthogonal to
    /// [`TaskProjection::state`] (a cached task is SUCCEEDED); it flags that the task never became
    /// a Batch job, so cost/runtime views don't count it as work run this time. Latched
    /// monotonically (once cached, stays cached) so it is order-independent.
    pub cached: bool,
    /// The graph node (rule/process) this task belongs to, from `task_created.group`. Set-once; on
    /// a conflicting value the lexicographic min wins, so it is deterministic under any delivery
    /// order. Rolls task states up onto the DAG nodes.
    pub group: Option<String>,
    /// The in-run tasks this one depends on (produced its inputs), from `task_created.deps`. Folded
    /// as a set-union, so a redelivered or reordered `task_created` changes nothing.
    pub deps: std::collections::BTreeSet<String>,
    /// The engine's `failed` verdicts on this task's attempts (`engine_task_verdict`), as a set.
    /// Evaluated against the attempts when the task is read ([`TaskProjection::rejection`]), never
    /// applied to them as they fold, so the outcome is a pure function of the observed event set
    /// whichever of a verdict and its attempt's facts folds first.
    verdicts: Vec<Verdict>,
}

/// An engine's `failed` verdict on a task's attempt (see [`EventData::EngineTaskVerdict`]).
#[derive(Clone, Debug, PartialEq)]
pub struct Verdict {
    /// The attempt it names; `None` ⇒ every attempt. Either way only an execution submitted at or
    /// before `at_micros` (or with no submit seen yet): an engine can re-run a task under the same
    /// attempt number, and a verdict never reaches a resubmission recorded after it.
    pub attempt: Option<Attempt>,
    /// The verdict's source time (epoch micros).
    pub at_micros: i64,
    pub category: Option<TerminationCategory>,
    pub reason: Option<String>,
}

impl Verdict {
    /// Whether this verdict names attempt `a`.
    fn names(&self, a: &AttemptProjection) -> bool {
        match self.attempt {
            Some(n) => {
                a.attempt == Some(n) && a.submitted_at_micros.is_none_or(|s| s <= self.at_micros)
            }
            None => a.submitted_at_micros.is_some_and(|s| s <= self.at_micros),
        }
    }

    /// A total order over verdicts, so the one reported for an attempt named by several is
    /// deterministic.
    fn order_key(&self) -> (Option<&str>, Option<&str>, Option<Attempt>, i64) {
        (
            self.reason.as_deref(),
            self.category.as_ref().map(TerminationCategory::as_str),
            self.attempt,
            self.at_micros,
        )
    }
}

impl TaskProjection {
    pub(super) fn new(task_key: TaskKey) -> Self {
        Self {
            task_key,
            attempts: BTreeMap::new(),
            max_next_attempt: None,
            retry_announced_micros: None,
            engine_terminal: None,
            engine_failed_attempt: None,
            engine_failed_at_micros: None,
            cached: false,
            group: None,
            deps: std::collections::BTreeSet::new(),
            verdicts: Vec::new(),
        }
    }

    /// Record an engine `failed` verdict: a set insert kept sorted by [`Verdict::order_key`], so a
    /// duplicate changes nothing and the set is the same whatever order verdicts fold in.
    pub(super) fn record_verdict(&mut self, verdict: Verdict) {
        let key = verdict.order_key();
        if let Err(at) = self.verdicts.binary_search_by(|v| v.order_key().cmp(&key)) {
            self.verdicts.insert(at, verdict);
        }
    }

    /// The engine's `failed` verdict on attempt `a`, if one names it (the least by
    /// `Verdict::order_key` when several do).
    pub fn rejection(&self, a: &AttemptProjection) -> Option<&Verdict> {
        self.verdicts.iter().filter(|v| v.names(a)).min_by(|x, y| x.order_key().cmp(&y.order_key()))
    }

    /// Attempt `a`'s state as the task counts it: a failed terminal when the engine rejected it,
    /// else its own (AWS) state. An engine verdict outranks AWS's `SUCCEEDED` (and ends a
    /// still-open attempt: the engine has concluded it) for the task, while the attempt keeps its
    /// AWS facts.
    pub fn counted_state(&self, a: &AttemptProjection) -> AttemptState {
        if a.state != AttemptState::Failed && self.rejection(a).is_some() {
            AttemptState::Failed
        } else {
            a.state
        }
    }

    /// The engine verdict that failed the task's display attempt, when that attempt's own (AWS)
    /// state is not a failure — i.e. when the verdict is what failed it, and so what explains it.
    pub fn verdict(&self) -> Option<&Verdict> {
        let da = self.display_attempt()?;
        (da.state != AttemptState::Failed).then(|| self.rejection(da)).flatten()
    }

    /// The container image the task ran in: its display attempt's, else the least any attempt
    /// recorded. `None` when no `task_submitted` named one.
    pub fn image(&self) -> Option<&str> {
        self.display_attempt()
            .and_then(|a| a.image.as_deref())
            .or_else(|| self.attempts.values().filter_map(|a| a.image.as_deref()).min())
    }

    /// Latch the cache-hit flag on (a `task_cached` event). Monotonic — never clears — so the
    /// projection is a pure function of the observed event set regardless of delivery order.
    pub(super) fn mark_cached(&mut self) {
        self.cached = true;
    }

    /// Set the task's graph group (from `task_created.group`). Deterministic under any order: keep
    /// the lexicographic min when a value is already set (a conflict is a plugin bug, not a state).
    pub(super) fn set_group(&mut self, group: &str) {
        keep_min(&mut self.group, Some(group));
    }

    /// Record an engine-reported terminal for a local rule. Monotonic: a success
    /// latches and outranks a later/duplicate failure, so the result is independent of arrival
    /// order.
    pub(super) fn record_engine_terminal(&mut self, s: TaskState) {
        self.engine_terminal = match (self.engine_terminal, s) {
            (Some(TaskState::Succeeded), _) | (_, TaskState::Succeeded) => {
                Some(TaskState::Succeeded)
            }
            (None, other) => Some(other),
            (current, _) => current,
        };
    }

    /// Record an engine-reported failure of a local rule at `ts_micros` (see
    /// `engine_failed_at_micros`). Max-latched on both fields, so order-independent.
    pub(super) fn record_engine_failure(&mut self, attempt: Option<Attempt>, ts_micros: i64) {
        self.engine_failed_attempt = self.engine_failed_attempt.max(attempt);
        self.engine_failed_at_micros = self.engine_failed_at_micros.max(Some(ts_micros));
    }

    /// Record a `task_retrying` linking `_prev → next`, announced at `ts_micros`.
    /// Monotonic: we keep the highest announced `next_attempt` and the earliest time it was
    /// announced, so the result is independent of arrival order.
    pub(super) fn record_retry(&mut self, next: Attempt, ts_micros: i64) {
        match self.max_next_attempt {
            Some(cur) if cur > next => {}
            Some(cur) if cur == next => {
                keep_min_micros(&mut self.retry_announced_micros, ts_micros);
            }
            _ => {
                self.max_next_attempt = Some(next);
                self.retry_announced_micros = Some(ts_micros);
            }
        }
    }

    /// Whether an observation (an attempt number, and when it was reported) answers the retry to
    /// `next` announced at `announced`: it is numbered `next` or later, or — for an engine that
    /// skips or reuses attempt numbers — it was reported strictly after the announcement. Both
    /// halves are functions of the event set, so the answer is order-independent.
    fn answers_retry(
        next: Attempt,
        announced: Option<i64>,
        observed: Option<Attempt>,
        at: Option<i64>,
    ) -> bool {
        observed.is_some_and(|n| n >= next)
            || matches!((at, announced), (Some(at), Some(announced)) if at > announced)
    }

    /// Whether a retry is still pending: a `task_retrying` announced attempt `N`, but no attempt
    /// answering it (see [`Self::answers_retry`]) has been observed yet. Derived from state, never
    /// toggled — so a duplicate or out-of-order `task_submitted`/`task_retrying` cannot flip it.
    fn retry_pending(&self) -> bool {
        let Some(next) = self.max_next_attempt else {
            return false;
        };
        !self.attempts.values().any(|a| {
            Self::answers_retry(next, self.retry_announced_micros, a.attempt, a.submitted_at_micros)
        })
    }

    /// The local-rule analogue of [`Self::retry_pending`]: a retry was announced and no
    /// engine-reported local failure answers it, so the latched failure is the one the retry
    /// replaces. (The Snakemake plugin now puts the attempt on a local terminal, but older logs
    /// carry none, so this rests on the failure's time relative to the announcement, which holds
    /// either way.)
    fn local_retry_pending(&self) -> bool {
        let Some(next) = self.max_next_attempt else {
            return false;
        };
        !Self::answers_retry(
            next,
            self.retry_announced_micros,
            self.engine_failed_attempt,
            self.engine_failed_at_micros,
        )
    }

    /// Task state is a reduction over attempts:
    /// - SUCCEEDED if any attempt succeeded;
    /// - FAILED only when the last permitted attempt failed and no retry is pending;
    /// - QUEUED when the live attempt(s) exist but none has started executing (all still
    ///   `Created`/`Submitted`/`Queued` in Batch — e.g. `RUNNABLE`, waiting on capacity);
    /// - RUNNING once an attempt has started, or during a retry gap (a failed attempt with a retry
    ///   pending — the task is mid-execution between attempts).
    ///
    /// Each attempt counts with its [`Self::counted_state`], so an attempt the engine rejected is a
    /// failed attempt here whatever AWS reported.
    pub fn state(&self) -> TaskState {
        if self.attempts.values().any(|a| self.counted_state(a) == AttemptState::Succeeded) {
            return TaskState::Succeeded;
        }
        // A local rule has no Batch attempts; its terminal comes from the engine (or it is still
        // running). Batch attempts, when present, remain authoritative — the engine terminal is a
        // fallback only for the attempt-less case, so a Batch task is unaffected.
        // A local failure with a retry pending is a retry gap, like a Batch one: RUNNING.
        if self.attempts.is_empty() {
            return match self.engine_terminal {
                Some(TaskState::Failed) if self.local_retry_pending() => TaskState::Running,
                Some(terminal) => terminal,
                None => TaskState::Running,
            };
        }
        let retry_pending = self.retry_pending();
        let all_terminal = self.attempts.values().all(|a| self.counted_state(a).is_terminal());
        if all_terminal && !retry_pending {
            return TaskState::Failed;
        }
        // Non-terminal (or a retry pending). A retry gap counts as RUNNING — the task is in flight
        // between attempts. Otherwise RUNNING iff some attempt has actually started executing; if
        // every live attempt is still Created/Submitted/Queued, the task is QUEUED, not running.
        if retry_pending
            || self.attempts.values().any(|a| self.counted_state(a) == AttemptState::Running)
        {
            TaskState::Running
        } else {
            TaskState::Queued
        }
    }

    /// When the task's failure happened (epoch micros), for a task whose [`Self::state`] is
    /// FAILED: the latest of its failed attempts' ends (an attempt the engine rejected ends at its
    /// verdict), as the last attempt's failure is the one that left the task failed; for a local
    /// rule, its latest engine-reported failure. `None` when no failure time was recorded. A max
    /// over the observed event set, so order-independent.
    pub fn failed_at_micros(&self) -> Option<i64> {
        if self.attempts.is_empty() {
            return self.engine_failed_at_micros;
        }
        self.attempts
            .values()
            .filter(|a| self.counted_state(a) == AttemptState::Failed)
            .filter_map(|a| match a.state {
                AttemptState::Failed => a.finished_at_micros,
                _ => self.rejection(a).map(|v| v.at_micros),
            })
            .max()
    }

    /// The attempt whose per-attempt detail (exit_code, log_stream, ...) best represents the task's
    /// outcome — surfaced to the run-detail view. A pure function of the observed event set:
    /// the succeeded attempt if any (by [`Self::counted_state`], so not one the engine
    /// rejected), else the highest-numbered attempt, tie-broken by `batch_job_id` so the choice is
    /// deterministic under any delivery order. `None` for a task with no Batch attempts (a local
    /// rule, whose terminal comes from the engine).
    pub fn display_attempt(&self) -> Option<&AttemptProjection> {
        self.attempts.values().max_by(|a, b| {
            let a_succeeded = (self.counted_state(a) == AttemptState::Succeeded) as u8;
            let b_succeeded = (self.counted_state(b) == AttemptState::Succeeded) as u8;
            a_succeeded
                .cmp(&b_succeeded)
                .then_with(|| a.attempt.cmp(&b.attempt))
                .then_with(|| a.batch_job_id.as_str().cmp(b.batch_job_id.as_str()))
        })
    }
}
