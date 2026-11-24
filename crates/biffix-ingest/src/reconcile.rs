//! AWS Batch reconciliation backstop: which events a described job yields, and what the reaper
//! does with a run's still-live children, decided from DescribeJobs' answer.
//!
//! A DescribeJobs start or terminal becomes a `RunEvent` through
//! `biffix_core::eventbridge::describe` (`started_run_event`, `terminal_run_event`), the same shape
//! and dedup key as the live EventBridge one, so a job whose live events never arrived still gets
//! its start (and host) as well as its end. This module picks which of them to enqueue (a start
//! only when the log lacks it, on the stale path and the reaper's alike, and a terminal only once
//! its job's start was accepted), plans the reap around them and synthesizes the FAILED of a child
//! Batch has forgotten. The caller enqueues
//! every event on the ordinary ingest queue; it never writes the event store directly.

use std::collections::{BTreeSet, HashMap, HashSet};

use biffix_core::event::RunEvent;
use biffix_core::eventbridge::describe::{
    started_run_event, terminal_run_event, DescribedJobFacts, DescribedStartedJob,
    DescribedTerminalJob, TerminalStatus,
};
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

/// The events the backstop enqueues for one described job: its start, when Batch reports one the
/// log does not hold (its dedup key is not among `log_start_keys`, the keys of the job's AWS Batch
/// starts in the log), then its terminal. Keyed on the fact rather than on "any start", so a Batch
/// retry's start is still backfilled when only the first attempt's reached the log.
pub fn described_events(
    start: Option<&DescribedStartedJob>,
    terminal: Option<&DescribedTerminalJob>,
    log_start_keys: &[String],
) -> Vec<RunEvent> {
    let start =
        start.map(started_run_event).filter(|event| !log_start_keys.contains(&event.dedup_key));
    start.into_iter().chain(terminal.map(terminal_run_event)).collect()
}

/// The starts the reaper enqueues for one page of an ended run's live children: each described
/// child's start ([`described_events`] with no terminal, since the reaper's terminals come from
/// [`plan_reap`]), only when the log lacks it. `children` maps every child of the page to the keys
/// of the AWS Batch starts the log holds for it (`store::reconcile::LiveChild::start_keys`); a
/// start for any other job is ignored, as [`plan_reap`] ignores one.
pub fn reap_start_events(
    starts: &[DescribedStartedJob],
    children: &HashMap<String, Vec<String>>,
) -> Vec<RunEvent> {
    starts
        .iter()
        .filter_map(|start| {
            let log_start_keys = children.get(&start.batch_job_id)?;
            Some(described_events(Some(start), None, log_start_keys))
        })
        .flatten()
        .collect()
}

/// The terminals to enqueue once the starts described with them have been: each of `terminals`
/// except one whose job had a start among `starts` that SQS rejected (`failed_starts`, indices into
/// `starts`). SendMessageBatch is not atomic, so a start and its job's terminal sent together can
/// split, and once the terminal is logged the job is no longer a candidate on either path: its
/// start would never be retried and the attempt never priced. Held back, the terminal keeps the
/// job a candidate, so a later poll describes it again and retries both.
pub fn terminals_to_send(
    starts: &[RunEvent],
    failed_starts: &BTreeSet<usize>,
    terminals: Vec<RunEvent>,
) -> Vec<RunEvent> {
    let held: HashSet<&str> = failed_starts
        .iter()
        .filter_map(|&index| starts.get(index)?.batch_job_id.as_ref().map(BatchJobId::as_str))
        .collect();
    terminals
        .into_iter()
        .filter(|terminal| {
            terminal.batch_job_id.as_ref().is_none_or(|id| !held.contains(id.as_str()))
        })
        .collect()
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
                     log_start_keys: &[&str]| {
            let keys: Vec<String> = log_start_keys.iter().map(|&key| key.to_owned()).collect();
            described_events(start, terminal, &keys).iter().map(RunEvent::kind).collect::<Vec<_>>()
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
            described_events(Some(&start), Some(&terminal), &[]),
            [started_run_event(&start), terminal_run_event(&terminal)]
        );
    }

    #[test]
    fn the_reaper_enqueues_a_childs_start_only_when_the_log_lacks_it() {
        let children = HashMap::from([
            ("a".to_string(), vec![]),
            ("b".to_string(), vec!["aws-batch:b:RUNNING".to_string()]),
            ("c".to_string(), vec!["aws-batch:c:RUNNING".to_string()]),
        ]);
        let retry = DescribedStartedJob { prior_attempts: 1, ..start("c") };
        let events = reap_start_events(
            &[
                // The log holds no start: enqueued.
                start("a"),
                // The log holds this one: nothing (an ended run's live child adds nothing each
                // poll).
                start("b"),
                // The log holds only the first attempt's: the retry's is a fact it lacks.
                retry.clone(),
                // Not one of the page's children: ignored.
                start("zz"),
            ],
            &children,
        );
        assert_eq!(events, [started_run_event(&start("a")), started_run_event(&retry)]);
        assert!(reap_start_events(&[], &children).is_empty());
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
