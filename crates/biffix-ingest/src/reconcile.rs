//! AWS Batch reconciliation backstop: which events a described job yields, and what the reaper
//! does with a run's still-live children, decided from DescribeJobs' answer.
//!
//! A DescribeJobs start or terminal becomes a `RunEvent` through
//! `biffix_core::eventbridge::describe` (`started_run_event`, `terminal_run_event`), and the job's
//! executions through `biffix_core::eventbridge::execution` (`execution_events`), each the same
//! shape and dedup key as the live EventBridge one, so a job whose live events never arrived still
//! gets its start (and host) and every execution as well as its end. This module picks which of
//! them to enqueue (a start or an execution event only when the log lacks it, on the stale path
//! and the reaper's alike, and a terminal only once every such fact of its job was accepted),
//! plans the reap around them, synthesizes the FAILED of a child Batch has forgotten, and decides
//! what a finished job still lacks and when its one describe is done. The
//! caller enqueues every event on the ordinary ingest queue; it never writes the event store
//! directly.

use std::collections::{BTreeSet, HashMap, HashSet};

use biffix_core::event::{RunEvent, BATCH_JOB_PRE_TERMINAL_KINDS};
use biffix_core::eventbridge::describe::{
    started_run_event, terminal_run_event, DescribedJobFacts, DescribedStartedJob,
    DescribedTerminalJob, TerminalStatus,
};
use biffix_core::eventbridge::execution::{execution_events, BatchJobSnapshot};
use biffix_core::ids::BatchJobId;

/// Maximum number of job IDs accepted by one AWS Batch `DescribeJobs` request.
pub const DESCRIBE_JOBS_BATCH_SIZE: usize = 100;

/// Synthesize the terminal RunEvent for a child of an ended run that AWS Batch no longer knows.
///
/// The reaper describes every still-"live" child before acting: a child Batch reports as finished
/// gets its **real** terminal ([`terminal_run_event`]), and a child still running is terminated
/// and left for its real FAILED (EventBridge, or the next poll's describe). Only a child that
/// DescribeJobs has omitted for a while gets this event: Batch has forgotten it, so no real
/// terminal can ever arrive, and without one its task row would stay `running` forever. Appending
/// this FAILED closes that loop *inside the log*: the fold attributes it via the existing
/// `batch_job_id → (run, task)` binding and marks the task terminal. Its dedup key is the real
/// `(job, FAILED)` key; since it is only written once Batch has no record of the job,
/// there is no real terminal left for it to shadow.
pub fn reap_terminal_event(
    child_job_id: &str,
    coordinator_job_id: &str,
    stopped_at_millis: i64,
) -> RunEvent {
    terminal_run_event(&DescribedTerminalJob {
        batch_job_id: child_job_id.to_owned(),
        status: TerminalStatus::Failed,
        stopped_at_millis,
        exit_code: None,
        status_reason: Some(forgotten_reason(coordinator_job_id)),
        // Batch no longer knows the job, so nothing about it is known but its id: no stream, host,
        // queue or request, and a reaped child is a task.
        facts: DescribedJobFacts::default(),
    })
}

/// The events the backstop enqueues for one described job, in order: its starts (one per
/// execution that started, `crate::decode::described_starts`), each of its executions' events
/// ([`execution_events`] of `snapshot`), then its terminal.
///
/// A start or an execution event is enqueued only when the log does not hold it: its dedup key is
/// not among `known_fact_keys`, the keys of the job's AWS Batch starts and execution events in the
/// log. So a job that runs for hours adds nothing to the queue on each rotation, while a fact the
/// log lacks (a Batch retry's start, a later execution) is still backfilled, since the test is on
/// the fact rather than on "any start". The terminal is never suppressed: logging it is what ends
/// the job's candidacy.
pub fn described_events(
    starts: &[DescribedStartedJob],
    snapshot: Option<&BatchJobSnapshot>,
    terminal: Option<&DescribedTerminalJob>,
    known_fact_keys: &[String],
) -> Vec<RunEvent> {
    let executions = snapshot.map(execution_events).unwrap_or_default();
    starts
        .iter()
        .map(started_run_event)
        .chain(executions)
        .filter(|event| !known_fact_keys.contains(&event.dedup_key))
        .chain(terminal.map(terminal_run_event))
        .collect()
}

/// Whether `event` is a fact the backstop sends ahead of its job's terminal
/// (`BATCH_JOB_PRE_TERMINAL_KINDS`): a job's start or an execution's start or end, the kinds
/// whose keys `known_fact_keys` holds. [`described_events`] yields nothing else but terminals.
pub fn is_pre_terminal_fact(event: &RunEvent) -> bool {
    BATCH_JOB_PRE_TERMINAL_KINDS.contains(&event.kind())
}

/// The terminals to enqueue once the pre-terminal facts described with them have been: each of
/// `terminals` except one whose job had a fact among `facts` (its start or an execution event,
/// [`is_pre_terminal_fact`]) that SQS rejected (`failed_facts`, indices into `facts`).
/// SendMessageBatch is not atomic, so a fact and its job's terminal sent together can split, and
/// once the terminal is logged the job is no longer a candidate on either path: the rejected start
/// would never be retried and the attempt never priced, and a rejected execution event would leave
/// that execution out of the log. Held back, the terminal keeps the job a candidate, so a later
/// poll describes it again and retries them all.
pub fn terminals_to_send(
    facts: &[RunEvent],
    failed_facts: &BTreeSet<usize>,
    terminals: Vec<RunEvent>,
) -> Vec<RunEvent> {
    let held: HashSet<&str> = failed_facts
        .iter()
        .filter_map(|&index| facts.get(index)?.batch_job_id.as_ref().map(BatchJobId::as_str))
        .collect();
    terminals
        .into_iter()
        .filter(|terminal| {
            terminal.batch_job_id.as_ref().is_none_or(|id| !held.contains(id.as_str()))
        })
        .collect()
}

/// What one DescribeJobs answer yields for the jobs asked about (see [`execution_backfill`]).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ExecutionBackfill {
    /// The events to enqueue: starts and execution events only.
    pub events: Vec<RunEvent>,
    /// Each described job with the range of `events` that is its own (empty when the log lacks
    /// nothing Batch reports), for [`execution_marks`].
    pub jobs: Vec<(String, std::ops::Range<usize>)>,
    /// The described jobs Batch does not report as finished (or reports with no status): their
    /// `attempts[]` may still grow, so this answer is not their last word.
    pub unfinished: Vec<String>,
}

/// The events to enqueue for finished jobs whose executions or start the log lacks
/// (`store::reconcile::execution_candidates`), from DescribeJobs' answer: for each `described`
/// job (its id, and the starts and snapshot read from it), the start and execution events the
/// log lacks ([`described_events`]). `candidates` maps every job asked about to the keys of the
/// facts the log holds for it; a described job that was not asked about, or returned twice, is
/// ignored. The reaper takes the same `events` for an ended run's live children (and ignores
/// `unfinished`): their terminals it plans itself ([`plan_reap`]), and the synthetic FAILED of a
/// forgotten child ([`reap_terminal_event`]) carries no execution, since Batch said nothing of one.
///
/// Never a terminal: the log already holds it (that is what makes the job a candidate), so
/// sending it again adds nothing, and a coordinator's must not be acted on a second time (the
/// stale path reaps the run's children on finding one). The start is included because nothing
/// else asks about a job whose terminal arrived while its start was lost.
///
/// A job whose snapshot is not in a terminal status (or that has none) is listed as
/// [`unfinished`](ExecutionBackfill::unfinished). The log holds Batch's terminal for every job
/// asked about, so Batch should agree; if it does not, what it reports is still enqueued (each is
/// a fact, and idempotent), but the job must not be taken for described once and for all.
pub fn execution_backfill<'a, I>(
    described: I,
    candidates: &HashMap<&str, &[String]>,
) -> ExecutionBackfill
where
    I: IntoIterator<Item = (&'a str, Vec<DescribedStartedJob>, Option<BatchJobSnapshot>)>,
{
    let mut backfill = ExecutionBackfill::default();
    let mut seen = HashSet::new();
    for (id, starts, snapshot) in described {
        let Some(&known_fact_keys) = candidates.get(id) else { continue };
        if !seen.insert(id) {
            continue;
        }
        let first = backfill.events.len();
        backfill.events.extend(described_events(&starts, snapshot.as_ref(), None, known_fact_keys));
        backfill.jobs.push((id.to_owned(), first..backfill.events.len()));
        if !snapshot.is_some_and(|snapshot| snapshot.status.is_terminal()) {
            backfill.unfinished.push(id.to_owned());
        }
    }
    backfill
}

/// Of `jobs` (each with the range of the enqueued events that is its own), the ids whose range
/// holds no index in `failed` (the events SQS did not accept). A job with nothing to send (an
/// empty range) is fully sent.
fn fully_sent_jobs(
    jobs: &[(String, std::ops::Range<usize>)],
    failed: &BTreeSet<usize>,
) -> Vec<String> {
    jobs.iter()
        .filter(|(_, range)| failed.range(range.clone()).next().is_none())
        .map(|(id, _)| id.clone())
        .collect()
}

/// What to record for one chunk of finished jobs after their describe (see [`execution_marks`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExecutionMarks {
    /// Jobs that have had their one describe (`store::reconcile::mark_executions_described`).
    pub described: Vec<String>,
    /// Jobs to ask about again on a later poll: left unmarked, in the rotation every job asked
    /// about joins before its events are sent (`store::reconcile::touch_described`).
    pub retry: Vec<String>,
}

/// Which of the finished jobs `asked` about are done with, and which are asked about again: the
/// whole decision, so the tick only carries it out. `backfill` is what Batch's answer yielded
/// ([`execution_backfill`]) and `failed` the indices of its events SQS did not accept. Both lists
/// keep `asked`'s order, and every asked id is in exactly one.
///
/// Described, for good:
/// - a job Batch reports finished whose events were all accepted ([`fully_sent_jobs`]), including
///   one with none to send (a job that never got an execution): a finished job's `attempts[]` is
///   final, so there is nothing a second describe could add;
/// - a job Batch did not return: unlike a just-submitted job, a finished one Batch omits has been
///   forgotten, and no later describe can answer.
///
/// Retried instead: a job with a rejected event, so the event it lacks is described and sent
/// again, and a job Batch does not report as finished ([`ExecutionBackfill::unfinished`]), whose
/// answer is not final.
pub fn execution_marks(
    asked: &[String],
    backfill: &ExecutionBackfill,
    failed: &BTreeSet<usize>,
) -> ExecutionMarks {
    let returned: HashSet<&str> = backfill.jobs.iter().map(|(id, _)| id.as_str()).collect();
    let unfinished: HashSet<&str> = backfill.unfinished.iter().map(String::as_str).collect();
    let sent = fully_sent_jobs(&backfill.jobs, failed);
    let sent: HashSet<&str> = sent.iter().map(String::as_str).collect();
    let (described, retry) = asked.iter().cloned().partition(|id| {
        let id = id.as_str();
        !returned.contains(id) || (sent.contains(id) && !unfinished.contains(id))
    });
    ExecutionMarks { described, retry }
}

/// The reason biffix gives AWS for terminating a child job of an ended run.
pub fn reap_reason(coordinator_job_id: &str) -> String {
    format!("biffix reap: coordinator {coordinator_job_id} terminated")
}

/// The `status_reason` of [`reap_terminal_event`]: what happened, for an operator reading the task.
pub fn forgotten_reason(coordinator_job_id: &str) -> String {
    format!(
        "biffix reap: AWS Batch no longer knows this job; its run's coordinator \
         {coordinator_job_id} had ended"
    )
}

/// What the reaper does with one page of a run's still-"live" children, given DescribeJobs' answer.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ReapPlan {
    /// Children Batch reports as finished: enqueue their real terminal, never terminate them.
    pub terminals: Vec<DescribedTerminalJob>,
    /// Children Batch reports as still active: terminate them.
    pub terminate: Vec<String>,
    /// Children DescribeJobs left out (Batch no longer knows them).
    pub missing: Vec<String>,
}

/// One child's DescribeJobs answer: its id and, when Batch reports it finished, its terminal.
pub type DescribedChild = (String, Option<DescribedTerminalJob>);

/// Plan the reap of `children` from DescribeJobs' `described` answer (only the ids it returned).
pub fn plan_reap(children: &[String], described: Vec<DescribedChild>) -> ReapPlan {
    let mut plan = ReapPlan::default();
    let mut seen = std::collections::HashSet::new();
    for (id, terminal) in described {
        if !children.contains(&id) || !seen.insert(id.clone()) {
            continue;
        }
        match terminal {
            Some(terminal) => plan.terminals.push(terminal),
            None => plan.terminate.push(id),
        }
    }
    plan.missing = children.iter().filter(|id| !seen.contains(*id)).cloned().collect();
    plan
}

/// Of the `missing` children, those Batch has omitted since at or before `forgotten_before_micros`
/// (per `missing_since`, from `store::reconcile::record_describe_results`): long enough that the
/// omission is not a just-submitted job's visibility lag, so [`reap_terminal_event`] may close
/// them.
pub fn forgotten_children(
    missing: &[String],
    missing_since: &std::collections::HashMap<String, i64>,
    forgotten_before_micros: i64,
) -> Vec<String> {
    missing
        .iter()
        .filter(|id| missing_since.get(*id).is_some_and(|since| *since <= forgotten_before_micros))
        .cloned()
        .collect()
}

/// Whether a failed `TerminateJob` is the expected outcome for a job that finished or vanished in
/// the moment since it was described (Batch answers `ClientException` naming the job as not found
/// or already finished). Every other failure — authorization, throttling, a service error — is
/// real and must surface, or a denied reap would look like a successful one.
pub fn terminate_error_is_benign(code: Option<&str>, message: Option<&str>) -> bool {
    if code != Some("ClientException") {
        return false;
    }
    let message = message.unwrap_or_default().to_ascii_lowercase();
    ["not found", "does not exist", "already", "terminal", "finished"]
        .iter()
        .any(|needle| message.contains(needle))
}

#[cfg(test)]
mod tests {
    use super::*;
    use biffix_core::event::EventData;
    use biffix_core::event::Kind;
    use biffix_core::source::EventSource;
    use biffix_core::testing::{
        execution_ended, execution_started, execution_task_arn, retried_job_snapshot,
    };

    /// `map` borrowed, as the backfill and the reaper hand their candidates' keys over.
    fn known(map: &HashMap<String, Vec<String>>) -> HashMap<&str, &[String]> {
        map.iter().map(|(id, keys)| (id.as_str(), keys.as_slice())).collect()
    }

    #[test]
    fn reap_terminal_event_is_a_child_failed_with_a_timestamp_independent_key() {
        let ev = reap_terminal_event("job-child", "coord-9", 1_789_186_351_123);
        // A task-level FAILED (not coordinator), attributed via the batch_job_id binding in the
        // fold.
        assert_eq!(ev.kind(), Kind::TaskFailed);
        assert_eq!(ev.source, EventSource::AwsBatch);
        assert_eq!(ev.batch_job_id.as_ref().unwrap().as_str(), "job-child");
        // The key is (job, FAILED) — timestamp-independent — so it collapses with any real
        // EventBridge FAILED for the same job.
        assert_eq!(ev.dedup_key, "aws-batch:job-child:FAILED");
        match ev.data {
            EventData::TaskFailed { status_reason, .. } => {
                assert_eq!(status_reason, Some(forgotten_reason("coord-9")));
            }
            other => panic!("expected task_failed, got {other:?}"),
        }
    }

    fn terminal(id: &str, status: TerminalStatus) -> DescribedTerminalJob {
        DescribedTerminalJob {
            batch_job_id: id.into(),
            status,
            stopped_at_millis: 1_000,
            exit_code: None,
            status_reason: None,
            facts: DescribedJobFacts {
                log_stream: None,
                instance_id: None,
                queue: None,
                is_coordinator: false,
                requested: Default::default(),
            },
        }
    }

    #[test]
    fn the_reaper_never_terminates_or_fabricates_for_a_child_batch_reports_finished() {
        let children: Vec<String> = ["a", "b", "c", "d"].map(String::from).to_vec();
        let plan = plan_reap(
            &children,
            vec![
                // A child whose SUCCEEDED the log missed: its real terminal, not a FAILED.
                ("a".into(), Some(terminal("a", TerminalStatus::Succeeded))),
                ("b".into(), None),
                ("c".into(), Some(terminal("c", TerminalStatus::Failed))),
                // Not one of the page's children, or a duplicate: ignored.
                ("zz".into(), None),
                ("b".into(), None),
            ],
        );
        assert_eq!(
            plan.terminals,
            vec![terminal("a", TerminalStatus::Succeeded), terminal("c", TerminalStatus::Failed)]
        );
        assert_eq!(plan.terminate, vec!["b".to_string()]);
        assert_eq!(plan.missing, vec!["d".to_string()]);
    }

    fn start(id: &str) -> DescribedStartedJob {
        DescribedStartedJob {
            batch_job_id: id.into(),
            started_at_millis: 500,
            prior_attempts: 0,
            facts: DescribedJobFacts {
                log_stream: None,
                instance_id: Some("ci-1".into()),
                queue: None,
                is_coordinator: false,
                requested: Default::default(),
            },
        }
    }

    #[test]
    fn a_described_job_yields_its_start_only_when_the_log_lacks_that_fact() {
        let (start, terminal) = (start("a"), terminal("a", TerminalStatus::Succeeded));
        let kinds = |start: Option<&DescribedStartedJob>,
                     terminal: Option<&DescribedTerminalJob>,
                     known_fact_keys: &[&str]| {
            let keys: Vec<String> = known_fact_keys.iter().map(|&key| key.to_owned()).collect();
            described_events(
                start.map(std::slice::from_ref).unwrap_or_default(),
                None,
                terminal,
                &keys,
            )
            .iter()
            .map(RunEvent::kind)
            .collect::<Vec<_>>()
        };
        // The log has no start: Batch's start is enqueued, ahead of the terminal.
        assert_eq!(
            kinds(Some(&start), Some(&terminal), &[]),
            [Kind::TaskStarted, Kind::TaskSucceeded]
        );
        assert_eq!(kinds(Some(&start), None, &[]), [Kind::TaskStarted]);
        assert_eq!(kinds(None, Some(&terminal), &[]), [Kind::TaskSucceeded]);
        assert_eq!(kinds(None, None, &[]), []);
        // The log holds this start: only the terminal (a long-running job adds nothing each
        // rotation).
        let held = ["aws-batch:a:RUNNING"];
        assert_eq!(kinds(Some(&start), Some(&terminal), &held), [Kind::TaskSucceeded]);
        assert_eq!(kinds(Some(&start), None, &held), []);
        assert_eq!(kinds(None, Some(&terminal), &held), [Kind::TaskSucceeded]);
        assert_eq!(kinds(None, None, &held), []);
        // The log holds only the first attempt's start, and Batch reports its retry's: the retry's
        // start (its host and container) is a fact the log lacks.
        let retry = DescribedStartedJob { prior_attempts: 1, ..start.clone() };
        assert_eq!(kinds(Some(&retry), None, &held), [Kind::TaskStarted]);
        assert_eq!(kinds(Some(&retry), None, &["aws-batch:a:RUNNING:1"]), []);
        // The events are the shared translator's.
        assert_eq!(
            described_events(std::slice::from_ref(&start), None, Some(&terminal), &[]),
            [started_run_event(&start), terminal_run_event(&terminal)]
        );
    }

    /// The retried case (two executions, both ended) as job `id`.
    fn snapshot(id: &str) -> BatchJobSnapshot {
        BatchJobSnapshot { batch_job_id: id.into(), ..retried_job_snapshot() }
    }

    /// The dedup key of the end of the retried case's execution `n`.
    fn ended_key(n: u32) -> String {
        format!("aws-batch-exec:{}:ended", execution_task_arn(n))
    }

    fn keys(events: &[RunEvent]) -> Vec<&str> {
        events.iter().map(|event| event.dedup_key.as_str()).collect()
    }

    #[test]
    fn described_events_orders_the_start_then_executions_then_the_terminal() {
        let (start, terminal) = (start("job-1"), terminal("job-1", TerminalStatus::Succeeded));
        let snapshot = snapshot("job-1");
        let events =
            described_events(std::slice::from_ref(&start), Some(&snapshot), Some(&terminal), &[]);
        assert_eq!(
            events.iter().map(RunEvent::kind).collect::<Vec<_>>(),
            [
                Kind::TaskStarted,
                Kind::BatchExecutionEnded,
                Kind::BatchExecutionEnded,
                Kind::TaskSucceeded
            ]
        );
        // The executions are the shared builder's, in its order.
        assert_eq!(events[1..3], execution_events(&snapshot));
        // A job with nothing but executions to report yields only those.
        assert_eq!(described_events(&[], Some(&snapshot), None, &[]), execution_events(&snapshot));
    }

    #[test]
    fn known_keys_suppress_the_executions_the_log_already_holds() {
        let (start, terminal) = (start("job-1"), terminal("job-1", TerminalStatus::Succeeded));
        let snapshot = snapshot("job-1");
        let (start_key, terminal_key) = ("aws-batch:job-1:RUNNING", "aws-batch:job-1:SUCCEEDED");
        // The log holds the first execution's end: the second's is still a fact it lacks.
        let events = described_events(
            std::slice::from_ref(&start),
            Some(&snapshot),
            Some(&terminal),
            &[ended_key(1)],
        );
        assert_eq!(keys(&events), [start_key, ended_key(2).as_str(), terminal_key]);
        // The log holds every pre-terminal fact: only the terminal, which is never suppressed.
        let known = [start_key.to_owned(), ended_key(1), ended_key(2)];
        let events = described_events(
            std::slice::from_ref(&start),
            Some(&snapshot),
            Some(&terminal),
            &known,
        );
        assert_eq!(keys(&events), [terminal_key]);
        // And a job still running adds nothing to the queue on each rotation.
        assert!(described_events(std::slice::from_ref(&start), Some(&snapshot), None, &known)
            .is_empty());
    }

    #[test]
    fn a_terminal_is_held_back_when_its_jobs_execution_was_rejected() {
        let facts = [
            started_run_event(&start("job-1")),
            execution_events(&snapshot("job-1")).remove(0),
            started_run_event(&start("job-2")),
        ];
        assert_eq!(facts[1].dedup_key, ended_key(1));
        let terminals = vec![
            terminal_run_event(&terminal("job-1", TerminalStatus::Succeeded)),
            terminal_run_event(&terminal("job-2", TerminalStatus::Succeeded)),
        ];
        // job-1's start was accepted but its execution's end was not: logging the terminal would
        // end the job's candidacy with that execution never retried.
        let sent = terminals_to_send(&facts, &BTreeSet::from([1]), terminals.clone());
        assert_eq!(keys(&sent), ["aws-batch:job-2:SUCCEEDED"]);
        let sent = terminals_to_send(&facts, &BTreeSet::new(), terminals);
        assert_eq!(keys(&sent), ["aws-batch:job-1:SUCCEEDED", "aws-batch:job-2:SUCCEEDED"]);
    }

    /// `(job, range)`s, owned, as the tick records them.
    fn ranges(jobs: &[(&str, std::ops::Range<usize>)]) -> Vec<(String, std::ops::Range<usize>)> {
        jobs.iter().map(|(id, range)| ((*id).to_owned(), range.clone())).collect()
    }

    fn owned(ids: &[&str]) -> Vec<String> {
        ids.iter().map(|&id| id.to_owned()).collect()
    }

    #[test]
    fn fully_sent_jobs_are_those_with_no_rejected_event() {
        // "c" had nothing to send: an empty range is fully sent.
        let jobs = ranges(&[("a", 0..2), ("b", 2..3), ("c", 3..3)]);
        assert_eq!(fully_sent_jobs(&jobs, &BTreeSet::from([1])), ["b", "c"]);
        assert_eq!(fully_sent_jobs(&jobs, &BTreeSet::new()), ["a", "b", "c"]);
        // Every event rejected (a request that failed outright): only the job that sent none.
        assert_eq!(fully_sent_jobs(&jobs, &BTreeSet::from([0, 1, 2])), ["c"]);
        assert!(fully_sent_jobs(&[], &BTreeSet::from([0])).is_empty());
    }

    /// A backfill of finished `jobs` with these event ranges (the events themselves do not
    /// matter to the marks).
    fn finished(jobs: &[(&str, std::ops::Range<usize>)]) -> ExecutionBackfill {
        ExecutionBackfill { jobs: ranges(jobs), ..ExecutionBackfill::default() }
    }

    #[test]
    fn a_finished_job_is_marked_once_its_events_were_all_accepted() {
        let asked = owned(&["a", "b", "c"]);
        let backfill = finished(&[("a", 0..2), ("b", 2..3), ("c", 3..3)]);
        // a's second event was rejected: it is retried, the others are done.
        let marks = execution_marks(&asked, &backfill, &BTreeSet::from([1]));
        assert_eq!(marks.described, ["b", "c"]);
        assert_eq!(marks.retry, ["a"]);
        // Nothing rejected: every job has had its one describe.
        let marks = execution_marks(&asked, &backfill, &BTreeSet::new());
        assert_eq!(marks.described, ["a", "b", "c"]);
        assert!(marks.retry.is_empty());
    }

    #[test]
    fn a_finished_job_batch_omits_is_marked() {
        // Batch returned only "b": it has forgotten "a" and "c", and will not remember them.
        let asked = owned(&["a", "b", "c"]);
        let marks = execution_marks(&asked, &finished(&[("b", 0..2)]), &BTreeSet::new());
        assert_eq!(marks.described, ["a", "b", "c"]);
        assert!(marks.retry.is_empty());
        // An omitted job is marked even when the returned one's send failed.
        let marks = execution_marks(&asked, &finished(&[("b", 0..2)]), &BTreeSet::from([0]));
        assert_eq!(marks.described, ["a", "c"]);
        assert_eq!(marks.retry, ["b"]);
        // Batch returned nothing at all.
        let marks = execution_marks(&asked, &finished(&[]), &BTreeSet::new());
        assert_eq!(marks.described, asked);
        // A job nobody asked about is never marked or retried.
        let marks = execution_marks(&owned(&["a"]), &finished(&[("zz", 0..0)]), &BTreeSet::new());
        assert_eq!((marks.described, marks.retry), (owned(&["a"]), vec![]));
        assert_eq!(
            execution_marks(&[], &finished(&[]), &BTreeSet::new()),
            ExecutionMarks::default()
        );
    }

    #[test]
    fn a_job_batch_does_not_report_finished_is_retried_not_marked() {
        use biffix_core::wire::aws_batch::BatchStatus;
        let candidates = HashMap::from([
            ("done".to_string(), vec![]),
            ("running".to_string(), vec![]),
            ("statusless".to_string(), vec![]),
            ("failed".to_string(), vec![]),
        ]);
        let in_status = |id: &str, status: BatchStatus| BatchJobSnapshot { status, ..snapshot(id) };
        let backfill = execution_backfill(
            [
                ("done", vec![], Some(snapshot("done"))),
                // Batch says it is still going: its `attempts[]` is not final.
                ("running", vec![], Some(in_status("running", BatchStatus::Running))),
                // No status at all (so no snapshot): nothing says it finished.
                ("statusless", vec![start("statusless")], None),
                ("failed", vec![], Some(in_status("failed", BatchStatus::Failed))),
            ],
            &known(&candidates),
        );
        assert_eq!(backfill.unfinished, ["running", "statusless"]);
        // What Batch reports of them is still sent: each is a fact.
        assert_eq!(backfill.jobs.len(), 4);
        assert!(!backfill.events[backfill.jobs[1].1.clone()].is_empty());
        assert_eq!(
            keys(&backfill.events[backfill.jobs[2].1.clone()]),
            ["aws-batch:statusless:RUNNING"]
        );

        // Every event accepted, and still only the finished jobs are done with.
        let asked = owned(&["done", "running", "statusless", "failed"]);
        let marks = execution_marks(&asked, &backfill, &BTreeSet::new());
        assert_eq!(marks.described, ["done", "failed"]);
        assert_eq!(marks.retry, ["running", "statusless"]);
    }

    #[test]
    fn the_finished_job_backfill_sends_a_start_and_executions_but_never_a_terminal() {
        let candidates = HashMap::from([
            ("job-1".to_string(), vec![ended_key(1)]),
            ("job-2".to_string(), vec![]),
            ("job-3".to_string(), vec![]),
        ]);
        let backfill = execution_backfill(
            [
                // The log holds its first execution's end: its start and the second's end go.
                ("job-1", vec![start("job-1")], Some(snapshot("job-1"))),
                // It never reached a host, so Batch reports no start and no execution.
                (
                    "job-2",
                    vec![],
                    Some(BatchJobSnapshot {
                        attempts: vec![],
                        current: Default::default(),
                        ..snapshot("job-2")
                    }),
                ),
                ("job-3", vec![start("job-3")], Some(snapshot("job-3"))),
                // Not one of the jobs asked about: ignored.
                ("job-other", vec![start("job-other")], Some(snapshot("job-other"))),
                // Returned twice: the second is ignored.
                ("job-2", vec![start("job-2")], None),
            ],
            &known(&candidates),
        );
        assert_eq!(
            keys(&backfill.events),
            [
                "aws-batch:job-1:RUNNING",
                ended_key(2).as_str(),
                "aws-batch:job-3:RUNNING",
                ended_key(1).as_str(),
                ended_key(2).as_str(),
            ]
        );
        assert!(backfill.events.iter().all(is_pre_terminal_fact));
        assert_eq!(backfill.jobs, ranges(&[("job-1", 0..2), ("job-2", 2..2), ("job-3", 2..5)]));
        assert!(backfill.unfinished.is_empty());
        // What it sent and what it marks agree: job-1's rejected end leaves it to retry.
        let asked = owned(&["job-1", "job-2", "job-3", "job-gone"]);
        let marks = execution_marks(&asked, &backfill, &BTreeSet::from([1]));
        assert_eq!(marks.described, ["job-2", "job-3", "job-gone"]);
        assert_eq!(marks.retry, ["job-1"]);
    }

    #[test]
    fn the_reaper_takes_only_the_pages_children_and_unknown_facts() {
        let children = HashMap::from([("job-1".to_string(), vec![ended_key(1)])]);
        let events = execution_backfill(
            [
                ("job-1", vec![start("job-1")], Some(snapshot("job-1"))),
                ("job-other", vec![start("job-other")], Some(snapshot("job-other"))),
            ],
            &known(&children),
        )
        .events;
        assert_eq!(keys(&events), ["aws-batch:job-1:RUNNING", ended_key(2).as_str()]);
        assert_eq!(events[0], started_run_event(&start("job-1")));
        assert_eq!(events[1].batch_job_id.as_ref().map(BatchJobId::as_str), Some("job-1"));
    }

    #[test]
    fn only_starts_and_executions_are_pre_terminal_facts() {
        let coordinator =
            DescribedJobFacts { is_coordinator: true, ..DescribedJobFacts::default() };
        let coordinator_start =
            DescribedStartedJob { facts: coordinator.clone(), ..start("coord") };
        let coordinator_end = DescribedTerminalJob {
            facts: coordinator,
            ..terminal("coord", TerminalStatus::Failed)
        };
        let task_arn = execution_task_arn(1);
        let pre_terminal = [
            (started_run_event(&start("a")), Kind::TaskStarted),
            (started_run_event(&coordinator_start), Kind::CoordinatorRunning),
            (execution_started("a", &task_arn, 3.0), Kind::BatchExecutionStarted),
            (execution_ended("a", &task_arn, Some(3.0), 684.0, None), Kind::BatchExecutionEnded),
        ];
        for (event, kind) in pre_terminal {
            assert_eq!(event.kind(), kind);
            assert!(is_pre_terminal_fact(&event), "{kind:?}");
        }
        let terminals = [
            (terminal_run_event(&terminal("a", TerminalStatus::Succeeded)), Kind::TaskSucceeded),
            (terminal_run_event(&coordinator_end), Kind::CoordinatorFailed),
        ];
        for (event, kind) in terminals {
            assert_eq!(event.kind(), kind);
            assert!(!is_pre_terminal_fact(&event), "{kind:?}");
        }
    }

    #[test]
    fn the_reaper_enqueues_a_childs_start_only_when_the_log_lacks_it() {
        let children = HashMap::from([
            ("a".to_string(), vec![]),
            ("b".to_string(), vec!["aws-batch:b:RUNNING".to_string()]),
            ("c".to_string(), vec!["aws-batch:c:RUNNING".to_string()]),
        ]);
        let retry = DescribedStartedJob { prior_attempts: 1, ..start("c") };
        let events = execution_backfill(
            [
                // The log holds no start: enqueued.
                ("a", vec![start("a")], None),
                // The log holds this one: nothing (an ended run's live child adds nothing each
                // poll).
                ("b", vec![start("b")], None),
                // The log holds only the first attempt's: the retry's is a fact it lacks.
                ("c", vec![start("c"), retry.clone()], None),
                // Not one of the page's children: ignored.
                ("zz", vec![start("zz")], None),
            ],
            &known(&children),
        )
        .events;
        assert_eq!(events, [started_run_event(&start("a")), started_run_event(&retry)]);
        let nothing: [(&str, Vec<DescribedStartedJob>, Option<BatchJobSnapshot>); 0] = [];
        assert!(execution_backfill(nothing, &known(&children)).events.is_empty());
    }

    #[test]
    fn a_terminal_waits_while_its_jobs_start_was_rejected() {
        let starts = [started_run_event(&start("a")), started_run_event(&start("b"))];
        let terminals = vec![
            terminal_run_event(&terminal("a", TerminalStatus::Succeeded)),
            terminal_run_event(&terminal("b", TerminalStatus::Failed)),
            // A job with no start to send (the log holds it, or Batch reports none).
            terminal_run_event(&terminal("c", TerminalStatus::Succeeded)),
        ];
        let ids = |events: Vec<RunEvent>| {
            events
                .into_iter()
                .map(|event| event.batch_job_id.unwrap().as_str().to_owned())
                .collect::<Vec<_>>()
        };
        // Every start accepted: every terminal goes.
        assert_eq!(
            ids(terminals_to_send(&starts, &BTreeSet::new(), terminals.clone())),
            ["a", "b", "c"]
        );
        // b's start rejected: its terminal is held back, so the job stays a candidate and both are
        // retried next poll; the others still go.
        assert_eq!(
            ids(terminals_to_send(&starts, &BTreeSet::from([1]), terminals.clone())),
            ["a", "c"]
        );
        // Every start rejected: only the job that had none to send.
        assert_eq!(
            ids(terminals_to_send(&starts, &BTreeSet::from([0, 1]), terminals.clone())),
            ["c"]
        );
        // An index past the starts names no job.
        assert_eq!(
            ids(terminals_to_send(&starts, &BTreeSet::from([7]), terminals)),
            ["a", "b", "c"]
        );
    }

    #[test]
    fn only_a_child_batch_has_omitted_for_the_grace_period_is_closed_as_forgotten() {
        let missing: Vec<String> = ["old", "new", "unknown"].map(String::from).to_vec();
        let since =
            std::collections::HashMap::from([("old".to_string(), 100), ("new".to_string(), 500)]);
        assert_eq!(forgotten_children(&missing, &since, 200), vec!["old".to_string()]);
        assert!(forgotten_children(&missing, &since, 99).is_empty());
    }

    #[test]
    fn only_a_vanished_or_finished_job_makes_a_failed_terminate_benign() {
        assert!(terminate_error_is_benign(Some("ClientException"), Some("Job abc not found")));
        assert!(terminate_error_is_benign(
            Some("ClientException"),
            Some("Job abc is already in a terminal state")
        ));
        // A denied, throttled or rejected reap is a real failure.
        assert!(!terminate_error_is_benign(
            Some("AccessDeniedException"),
            Some("not authorized to perform batch:TerminateJob")
        ));
        assert!(!terminate_error_is_benign(Some("ClientException"), Some("Invalid job id")));
        assert!(!terminate_error_is_benign(Some("ServerException"), None));
        assert!(!terminate_error_is_benign(None, Some("dispatch failure")));
    }
}
