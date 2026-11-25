//! The execution level of the fold: the executions of one Batch job ([`Executions`]).
//!
//! An **execution** is one ECS task Batch ran for a job, identified by its task ARN. A job Batch
//! retries itself (a reclaimed spot host) has several under one job id, which is still one engine
//! attempt. The log holds at most two rows for an execution, its `batch_execution_started` and
//! its `batch_execution_ended`, and an [`ExecutionProjection`] is those two rows read through
//! fixed getters: nothing is latched across them, so the order they fold in cannot matter.

use std::collections::BTreeMap;

use super::keep::keep_least_by;
use crate::event::{EventData, RunEvent, TerminationCategory};

/// What orders two events of one kind for one execution: `(ts micros, the event serialized)`.
/// The lesser one is kept. `seq` is not serialized, so the choice does not depend on log position
/// or on fold order. Not [`super::keep::Stamp`], which orders by log position.
type EventStamp = (i64, String);

/// What an execution's `batch_execution_started` says.
#[derive(Clone, Debug, PartialEq)]
struct StartFacts {
    stamp: EventStamp,
    started_at_micros: i64,
    log_stream: Option<String>,
    instance_id: Option<String>,
    queue: Option<String>,
}

/// What an execution's `batch_execution_ended` says: every field of the payload, and the host and
/// queue of its envelope.
#[derive(Clone, Debug, PartialEq)]
struct EndFacts {
    stamp: EventStamp,
    started_at_micros: Option<i64>,
    stopped_at_micros: i64,
    exit_code: Option<i32>,
    status_reason: Option<String>,
    termination_category: Option<TerminationCategory>,
    log_stream: Option<String>,
    instance_id: Option<String>,
    queue: Option<String>,
}

/// One execution of a Batch job, as its events tell it.
///
/// Each of its two events fills its own slot and neither overwrites the other. Every getter
/// answers from the end when the end has the value, else from the start: the end is AWS's final
/// account of the execution, and the start only fills what the end lacks (an end AWS reported
/// without a log stream, say).
#[derive(Clone, Debug, PartialEq)]
pub struct ExecutionProjection {
    /// The execution's identity: its ECS task ARN, verbatim.
    pub execution_id: String,
    from_started: Option<StartFacts>,
    from_ended: Option<EndFacts>,
}

impl ExecutionProjection {
    /// True once the execution's end has folded. Until then it is running, as far as the log
    /// knows.
    pub fn is_ended(&self) -> bool {
        self.from_ended.is_some()
    }

    /// When AWS says the execution's container started; `None` when it never started, or when no
    /// event that folded so far says.
    pub fn started_at_micros(&self) -> Option<i64> {
        let from_ended = self.from_ended.as_ref().and_then(|end| end.started_at_micros);
        from_ended.or(self.from_started.as_ref().map(|start| start.started_at_micros))
    }

    /// When AWS says the execution stopped; `None` while it has not ended.
    pub fn stopped_at_micros(&self) -> Option<i64> {
        self.from_ended.as_ref().map(|end| end.stopped_at_micros)
    }

    /// The container's exit code; `None` when AWS reported none (a lost host) or it has not ended.
    pub fn exit_code(&self) -> Option<i32> {
        self.from_ended.as_ref().and_then(|end| end.exit_code)
    }

    /// Batch's account of why the execution stopped.
    pub fn status_reason(&self) -> Option<&str> {
        self.from_ended.as_ref().and_then(|end| end.status_reason.as_deref())
    }

    /// The stop reason as a category, when one fits.
    pub fn termination_category(&self) -> Option<&TerminationCategory> {
        self.from_ended.as_ref().and_then(|end| end.termination_category.as_ref())
    }

    /// The execution's own CloudWatch log stream.
    pub fn log_stream(&self) -> Option<&str> {
        let from_ended = self.from_ended.as_ref().and_then(|end| end.log_stream.as_deref());
        from_ended.or(self.from_started.as_ref().and_then(|start| start.log_stream.as_deref()))
    }

    /// The (container-instance) host the execution ran on. Its own, not the job's: each execution
    /// of a retried job ran on a different host.
    pub fn instance_id(&self) -> Option<&str> {
        let from_ended = self.from_ended.as_ref().and_then(|end| end.instance_id.as_deref());
        from_ended.or(self.from_started.as_ref().and_then(|start| start.instance_id.as_deref()))
    }

    /// The Batch queue (name) the execution's job ran on.
    pub fn queue(&self) -> Option<&str> {
        let from_ended = self.from_ended.as_ref().and_then(|end| end.queue.as_deref());
        from_ended.or(self.from_started.as_ref().and_then(|start| start.queue.as_deref()))
    }

    /// True when the execution ended because its host went away (category `spot` or
    /// `host_terminated`). A fact of the execution itself, so it holds whether or not the job's
    /// own terminal has arrived, and whatever that terminal says.
    pub fn is_host_lost(&self) -> bool {
        matches!(
            self.termination_category(),
            Some(TerminationCategory::Spot | TerminationCategory::HostTerminated)
        )
    }

    /// One execution from two partial projections of it (`other`'s id is `self`'s): each slot
    /// keeps the one with the lesser stamp, as folding both projections' events into one would.
    /// Order-independent: `a.merged(b) == b.merged(a)`.
    pub fn merged(&self, other: &ExecutionProjection) -> ExecutionProjection {
        let (mut from_started, mut from_ended) =
            (self.from_started.clone(), self.from_ended.clone());
        if let Some(facts) = other.from_started.clone() {
            keep_least_by(&mut from_started, facts, |facts| &facts.stamp);
        }
        if let Some(facts) = other.from_ended.clone() {
            keep_least_by(&mut from_ended, facts, |facts| &facts.stamp);
        }
        ExecutionProjection { execution_id: self.execution_id.clone(), from_started, from_ended }
    }

    /// How long the execution ran: `None` unless it ended and has a start. A stop before the
    /// start (a clock AWS reported wrong) counts as no time, not negative time.
    fn ran_micros(&self) -> Option<i64> {
        let (started, stopped) = (self.started_at_micros()?, self.stopped_at_micros()?);
        Some(stopped.saturating_sub(started).max(0))
    }
}

/// One Batch job's executions, by execution id.
pub type Executions = BTreeMap<String, ExecutionProjection>;

/// Fold `e` into `executions` when it is an execution's start or end; any other kind is not this
/// module's and changes nothing.
///
/// Each kind writes only its own slot of its execution. One dedup key per `(execution, kind)`
/// means a slot is written once; should a second event of one kind arrive all the same, the slot
/// keeps the one with the lesser [`EventStamp`], so the result is the same in every fold order.
pub(super) fn merge_execution(executions: &mut Executions, e: &RunEvent) {
    /// `executions[execution_id]`, made empty first when no event of the execution has folded yet.
    fn execution<'m>(
        executions: &'m mut Executions,
        execution_id: &str,
    ) -> &'m mut ExecutionProjection {
        super::get_or_insert_with(executions, execution_id, || ExecutionProjection {
            execution_id: execution_id.to_owned(),
            from_started: None,
            from_ended: None,
        })
    }

    // A `RunEvent` always serializes (string keys, no fallible `Serialize`). Were one not to, its
    // stamp would be `(ts, "")`: still the same in every fold order, and no panic in the fold.
    let stamp = || (e.ts.as_micros(), serde_json::to_string(e).unwrap_or_default());
    let instance_id = || e.instance_id.as_ref().map(|id| id.as_str().to_owned());
    match &e.data {
        EventData::BatchExecutionStarted { execution_id, started_at, log_stream } => {
            let facts = StartFacts {
                stamp: stamp(),
                started_at_micros: started_at.as_micros(),
                log_stream: log_stream.clone(),
                instance_id: instance_id(),
                queue: e.queue.clone(),
            };
            keep_least_by(&mut execution(executions, execution_id).from_started, facts, |facts| {
                &facts.stamp
            });
        }
        EventData::BatchExecutionEnded {
            execution_id,
            started_at,
            stopped_at,
            exit_code,
            status_reason,
            termination_category,
            log_stream,
        } => {
            let facts = EndFacts {
                stamp: stamp(),
                started_at_micros: started_at.map(crate::time::Timestamp::as_micros),
                stopped_at_micros: stopped_at.as_micros(),
                exit_code: *exit_code,
                status_reason: status_reason.clone(),
                termination_category: termination_category.clone(),
                log_stream: log_stream.clone(),
                instance_id: instance_id(),
                queue: e.queue.clone(),
            };
            keep_least_by(&mut execution(executions, execution_id).from_ended, facts, |facts| {
                &facts.stamp
            });
        }
        _ => {}
    }
}

/// A job's executions in the order they happened: each by its `started_at`, else (one that never
/// started) its `stopped_at`, then by execution id. An execution with neither time sorts last.
///
/// An execution that stopped without starting (its host was reclaimed while it was starting) so
/// sits where it happened, before the executions Batch ran after it, and not at the end.
///
/// The 1-based position is the ordinal a reader shows ("execution 2 of 3") and the last is the
/// job's final execution. A position is a display value only, never an identity: it can change
/// when an earlier execution's events arrive late.
pub fn ordered_executions(executions: &Executions) -> Vec<&ExecutionProjection> {
    let mut ordered: Vec<&ExecutionProjection> = executions.values().collect();
    ordered.sort_by_key(|x| {
        let happened_at = x.started_at_micros().or(x.stopped_at_micros());
        (happened_at.is_none(), happened_at, &x.execution_id)
    });
    ordered
}

/// Where the time of a job's executions went. The attempt's own span (start to finish) is
/// unchanged by executions and still covers all of it; these say what that span was made of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExecutionDurations {
    /// Time containers ran: the sum of every ended execution's start-to-stop interval.
    pub run_micros: i64,
    /// The part of `run_micros` spent in executions that were lost with their host
    /// ([`ExecutionProjection::is_host_lost`]): work Batch ran again.
    pub lost_micros: i64,
    /// Time between one execution stopping and the next one that started starting: the job
    /// waiting for a host. An execution that never started is passed over, since it held no host.
    pub wait_micros: i64,
}

/// The run, lost and wait time of a job's executions; `None` for a job with no execution facts
/// (one from before they were recorded), which is not the same as zero.
///
/// Only what AWS reported counts. An execution that has not ended, or never started, adds no run
/// time (a reader wanting "so far" adds `now - start` itself). A wait is counted between
/// neighbours among the executions that started, in [`ordered_executions`]' order, from the
/// earlier one's stop (when it has one) to the later one's start; an overlap is no wait, not a
/// negative one. An execution that never started is skipped: it used no host time, so the wait
/// runs through it, from the execution before it to the one after.
pub fn execution_durations(executions: &Executions) -> Option<ExecutionDurations> {
    if executions.is_empty() {
        return None;
    }
    let ordered = ordered_executions(executions);
    let mut durations = ExecutionDurations { run_micros: 0, lost_micros: 0, wait_micros: 0 };
    for x in &ordered {
        let ran = x.ran_micros().unwrap_or(0);
        durations.run_micros = durations.run_micros.saturating_add(ran);
        if x.is_host_lost() {
            durations.lost_micros = durations.lost_micros.saturating_add(ran);
        }
    }
    // The stop of the previous execution that started; `None` when it has not stopped.
    let mut previous_stop = None;
    for x in &ordered {
        let Some(started) = x.started_at_micros() else { continue };
        if let Some(stopped) = previous_stop {
            let waited = started.saturating_sub(stopped).max(0);
            durations.wait_micros = durations.wait_micros.saturating_add(waited);
        }
        previous_stop = x.stopped_at_micros();
    }
    Some(durations)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{EventData, RunEvent, TerminationCategory};
    use crate::source::EventSource;
    use crate::testing::{
        event, execution_ended, execution_started, execution_task_arn, RunEventExt, RETRIED_QUEUE,
    };

    const JOB: &str = "job-1";

    /// Execution `n`'s start at `at_s`, on host `ci{n}`.
    fn started(n: u32, at_s: f64) -> RunEvent {
        execution_started(JOB, &execution_task_arn(n), at_s)
            .with_instance(&format!("ci{n}"))
            .with_queue(RETRIED_QUEUE)
    }

    /// Execution `n`'s end at `stopped_s`, on host `ci{n}`.
    fn ended(
        n: u32,
        started_s: Option<f64>,
        stopped_s: f64,
        category: Option<TerminationCategory>,
    ) -> RunEvent {
        execution_ended(JOB, &execution_task_arn(n), started_s, stopped_s, category)
            .with_instance(&format!("ci{n}"))
            .with_queue(RETRIED_QUEUE)
    }

    /// `e` naming the log stream `stream`.
    fn with_stream(mut e: RunEvent, stream: &str) -> RunEvent {
        match &mut e.data {
            EventData::BatchExecutionStarted { log_stream, .. }
            | EventData::BatchExecutionEnded { log_stream, .. } => {
                *log_stream = Some(stream.to_owned());
            }
            other => panic!("not an execution event: {other:?}"),
        }
        e
    }

    /// The end `e` with exit code `code`, under the dedup key `key`.
    fn with_exit(mut e: RunEvent, code: i32, key: &str) -> RunEvent {
        match &mut e.data {
            EventData::BatchExecutionEnded { exit_code, .. } => *exit_code = Some(code),
            other => panic!("not an execution's end: {other:?}"),
        }
        e.dedup_key = key.to_owned();
        e
    }

    fn fold(events: &[RunEvent]) -> Executions {
        let mut executions = Executions::new();
        for e in events {
            merge_execution(&mut executions, e);
        }
        executions
    }

    /// The retried case: execution 1 runs 3 s to 684 s and ends as `lost`; execution 2 runs
    /// 831 s to 2217 s and exits by itself.
    fn retried(lost: TerminationCategory) -> Vec<RunEvent> {
        vec![ended(1, Some(3.0), 684.0, Some(lost)), ended(2, Some(831.0), 2217.0, None)]
    }

    fn only(executions: &Executions) -> &ExecutionProjection {
        assert_eq!(executions.len(), 1);
        executions.values().next().unwrap()
    }

    #[test]
    fn a_start_alone_is_a_running_execution() {
        let executions = fold(&[started(1, 3.0)]);
        let x = only(&executions);
        assert_eq!(x.execution_id, execution_task_arn(1));
        assert!(!x.is_ended());
        assert_eq!(x.started_at_micros(), Some(3_000_000));
        assert_eq!(x.stopped_at_micros(), None);
        assert_eq!(x.instance_id(), Some("ci1"));
        assert_eq!(x.queue(), Some(RETRIED_QUEUE));
    }

    #[test]
    fn the_end_wins_and_the_start_fills_what_it_lacks_in_either_order() {
        let start = with_stream(started(1, 3.0).with_instance("ci-a"), "ls-a");
        let end = ended(1, None, 684.0, None).with_instance("ci-b");
        let forward = fold(&[start.clone(), end.clone()]);
        let backward = fold(&[end, start]);
        assert_eq!(forward, backward);
        let x = only(&forward);
        assert!(x.is_ended());
        assert_eq!(x.instance_id(), Some("ci-b"));
        assert_eq!(x.log_stream(), Some("ls-a"));
        assert_eq!(x.started_at_micros(), Some(3_000_000));
        assert_eq!(x.stopped_at_micros(), Some(684_000_000));

        // Where both say: the end's start and queue win over the start's, in either order.
        let start = started(1, 3.0).with_queue("q-a");
        let end = ended(1, Some(4.0), 684.0, None).with_queue("q-b");
        for x in [fold(&[start.clone(), end.clone()]), fold(&[end, start])] {
            assert_eq!(only(&x).started_at_micros(), Some(4_000_000));
            assert_eq!(only(&x).queue(), Some("q-b"));
        }
    }

    #[test]
    fn two_ends_for_one_execution_settle_the_same_in_either_order() {
        let later = with_exit(ended(1, None, 5.0, None), 1, "k1");
        let earlier = with_exit(ended(1, None, 4.0, None), 2, "k2");
        let forward = fold(&[later.clone(), earlier.clone()]);
        let backward = fold(&[earlier, later]);
        assert_eq!(only(&forward).exit_code(), Some(2));
        assert_eq!(only(&backward).exit_code(), Some(2));
        assert_eq!(forward, backward);

        let one = with_exit(ended(1, None, 5.0, None), 1, "k1");
        let other = with_exit(ended(1, None, 5.0, None), 2, "k2");
        assert_eq!(fold(&[one.clone(), other.clone()]), fold(&[other, one]));
    }

    #[test]
    fn two_starts_for_one_execution_settle_the_same_in_either_order() {
        let rekeyed = |n: u32| {
            let mut e = with_stream(started(1, 3.0), &format!("ls-{n}"));
            e.dedup_key = format!("k{n}");
            e
        };
        let (one, other) = (rekeyed(1), rekeyed(2));
        let forward = fold(&[one.clone(), other.clone()]);
        let backward = fold(&[other, one]);
        assert_eq!(forward, backward);
        // Equal `ts`: the lesser serialized event, which is the one with the lesser key and stream.
        assert_eq!(only(&forward).log_stream(), Some("ls-1"));
    }

    #[test]
    fn a_start_an_end_and_a_second_end_fold_the_same_in_every_order() {
        let events = [
            with_stream(started(1, 3.0), "ls-a"),
            with_exit(ended(1, Some(3.0), 684.0, Some(TerminationCategory::Spot)), 1, "k1"),
            with_exit(ended(1, None, 600.0, None), 2, "k2"),
        ];
        let orders = [[0, 1, 2], [0, 2, 1], [1, 0, 2], [1, 2, 0], [2, 0, 1], [2, 1, 0]];
        let folded = |order: &[usize; 3]| fold(&order.map(|i| events[i].clone()));
        let first = folded(&orders[0]);
        for order in &orders[1..] {
            assert_eq!(folded(order), first, "order {order:?}");
        }
        // The earlier end is kept, and the start fills the start and the stream it lacks.
        let x = only(&first);
        assert_eq!(x.exit_code(), Some(2));
        assert_eq!(x.stopped_at_micros(), Some(600_000_000));
        assert_eq!(x.started_at_micros(), Some(3_000_000));
        assert_eq!(x.log_stream(), Some("ls-a"));
        assert!(!x.is_host_lost());
    }

    #[test]
    fn two_partial_projections_merge_as_one_fold_of_both() {
        use std::slice;

        let start = with_stream(started(1, 10.0), "s-1");
        let end = ended(1, Some(10.0), 20.0, None);
        let (start_only, end_only) = (fold(slice::from_ref(&start)), fold(slice::from_ref(&end)));
        let both = fold(&[start, end.clone()]);
        let (a, b) = (only(&start_only), only(&end_only));
        assert_eq!(a.merged(b), *only(&both));
        assert_eq!(b.merged(a), *only(&both));
        assert_eq!(a.merged(b).log_stream(), Some("s-1"));

        // Two ends of one execution, one on each side: the lesser stamp wins either way.
        let (one, two) = (with_exit(end.clone(), 1, "k-1"), with_exit(end, 2, "k-2"));
        let (left, right, all) =
            (fold(slice::from_ref(&one)), fold(slice::from_ref(&two)), fold(&[one, two]));
        assert_eq!(only(&left).merged(only(&right)), *only(&all));
        assert_eq!(only(&right).merged(only(&left)), *only(&all));
    }

    #[test]
    fn another_kind_merges_nothing() {
        let data = EventData::TaskStarted { started_at: None, log_stream: None };
        let e = event(1.0, EventSource::AwsBatch, "k", data).with_job(JOB);
        assert!(fold(&[e]).is_empty());
    }

    #[test]
    fn executions_order_by_start_else_stop_then_id() {
        let executions = fold(&[
            execution_started(JOB, "b", 10.0),
            execution_ended(JOB, "a", Some(10.0), 20.0, None),
            execution_ended(JOB, "c", None, 5.0, None),
            execution_started(JOB, "d", 2.0),
        ]);
        let order: Vec<&str> =
            ordered_executions(&executions).iter().map(|x| x.execution_id.as_str()).collect();
        assert_eq!(order, ["d", "c", "a", "b"]);
    }

    #[test]
    fn an_execution_that_never_started_sorts_where_it_stopped() {
        let order = |events: &[RunEvent]| -> Vec<String> {
            let executions = fold(events);
            ordered_executions(&executions).iter().map(|x| x.execution_id.clone()).collect()
        };
        // Lost while starting, between the two that ran: not the job's final execution.
        let between = [
            execution_ended(JOB, "B", Some(831.0), 2217.0, None),
            execution_ended(JOB, "X", None, 700.0, Some(TerminationCategory::Spot)),
            execution_ended(JOB, "A", Some(3.0), 684.0, Some(TerminationCategory::Spot)),
        ];
        assert_eq!(order(&between), ["A", "X", "B"]);
        // Lost while starting, then the one still running, which is the final one.
        let before_running = [
            execution_started(JOB, "B", 200.0),
            execution_ended(JOB, "X", None, 100.0, Some(TerminationCategory::Spot)),
        ];
        assert_eq!(order(&before_running), ["X", "B"]);
    }

    #[test]
    fn the_retried_case_splits_into_run_lost_and_wait() {
        let split = |lost_micros| {
            Some(ExecutionDurations {
                run_micros: 2_067_000_000,
                lost_micros,
                wait_micros: 147_000_000,
            })
        };
        let durations = |lost| execution_durations(&fold(&retried(lost)));
        assert_eq!(durations(TerminationCategory::HostTerminated), split(681_000_000));
        assert_eq!(durations(TerminationCategory::Spot), split(681_000_000));
        assert_eq!(durations(TerminationCategory::Oom), split(0));
    }

    #[test]
    fn a_running_or_never_started_execution_adds_no_time() {
        let mut events = retried(TerminationCategory::HostTerminated);
        events.push(started(3, 2300.0));
        assert_eq!(
            execution_durations(&fold(&events)),
            Some(ExecutionDurations {
                run_micros: 2_067_000_000,
                lost_micros: 681_000_000,
                wait_micros: 230_000_000,
            })
        );
        assert_eq!(
            execution_durations(&fold(&[ended(1, None, 9.0, None)])),
            Some(ExecutionDurations { run_micros: 0, lost_micros: 0, wait_micros: 0 })
        );
    }

    #[test]
    fn the_wait_runs_through_an_execution_that_never_started() {
        // Execution 3 was lost while starting, between the two that ran.
        let mut events = retried(TerminationCategory::HostTerminated);
        events.push(ended(3, None, 700.0, Some(TerminationCategory::HostTerminated)));
        assert_eq!(
            execution_durations(&fold(&events)),
            Some(ExecutionDurations {
                run_micros: 2_067_000_000,
                lost_micros: 681_000_000,
                wait_micros: 147_000_000,
            })
        );
    }

    #[test]
    fn no_executions_have_no_durations() {
        assert_eq!(execution_durations(&Executions::new()), None);
    }

    #[test]
    fn a_backwards_interval_or_an_overlap_counts_as_zero() {
        let backwards = fold(&[ended(1, Some(10.0), 5.0, None)]);
        assert_eq!(execution_durations(&backwards).unwrap().run_micros, 0);

        let overlapping = fold(&[ended(1, Some(1.0), 10.0, None), ended(2, Some(8.0), 20.0, None)]);
        let durations = execution_durations(&overlapping).unwrap();
        assert_eq!(durations.wait_micros, 0);
        assert_eq!(durations.run_micros, 21_000_000);
    }

    #[test]
    fn host_lost_is_a_fact_of_the_ended_execution() {
        let lost = |events: &[RunEvent]| only(&fold(events)).is_host_lost();
        assert!(lost(&[ended(1, Some(1.0), 2.0, Some(TerminationCategory::Spot))]));
        assert!(lost(&[ended(1, Some(1.0), 2.0, Some(TerminationCategory::HostTerminated))]));
        assert!(!lost(&[ended(1, Some(1.0), 2.0, Some(TerminationCategory::Oom))]));
        assert!(!lost(&[ended(1, Some(1.0), 2.0, None)]));
        assert!(!lost(&[started(1, 1.0)]));
    }
}
