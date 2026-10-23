//! Coordinator attribution.
//!
//! `from_aws_batch` emits `coordinator_*` with `run_id: None` by design, so without the control
//! plane's binding event those facts pend forever and a run never leaves SUBMITTED even after its
//! coordinator has exited. These tests pin the binding and its order-independence.

use crate::common;

use biffix_core::dedup;
use biffix_core::event::{
    coordinator_submitted, run_submitted, EventData, RunEvent, TerminationCategory,
};
use biffix_core::ids::{Attempt, BatchJobId, RunId, TaskKey};
use biffix_core::reducer::Projection;
use biffix_core::source::EventSource;
use biffix_core::state::{AttemptState, RunState};
use biffix_core::testing::{self, RunEventExt};
use biffix_core::time::Timestamp;
use biffix_core::translate;
use biffix_core::wire::aws_batch::BatchStatus;
use biffix_core::wire::engine::{EngineBody, EngineEvent};
use common::{fold_in_order, permutations};

const JOB: &str = "coord-job-1";

fn anchor(ts: f64, run: &str) -> RunEvent {
    run_submitted(
        &RunId::new(run),
        Timestamp::from_epoch_seconds(ts),
        biffix_core::event::RunSubmitted::new(
            "v1:hash",
            "abc",
            "sha256:x",
            serde_json::json!({}),
            vec![],
        ),
    )
}

fn binding(ts: f64, run: &str) -> RunEvent {
    coordinator_submitted(
        &RunId::new(run),
        Timestamp::from_epoch_seconds(ts),
        &BatchJobId::new(JOB),
    )
}

/// The Batch status the translator maps to `data`'s kind (for the event's dedup key).
fn status_of(data: &EventData) -> BatchStatus {
    match data {
        EventData::CoordinatorRunning { .. } | EventData::TaskStarted { .. } => {
            BatchStatus::Running
        }
        EventData::CoordinatorSucceeded { .. } | EventData::TaskSucceeded { .. } => {
            BatchStatus::Succeeded
        }
        EventData::CoordinatorFailed { .. } | EventData::TaskFailed { .. } => BatchStatus::Failed,
        other => panic!("not a Batch state change: {other:?}"),
    }
}

/// An AWS Batch event for `job` with the translator's correlation fields and key: no run_id,
/// keyed on `(job, status)` (a first attempt). Built by hand, not through
/// `translate::from_aws_batch`, so `data` can be any coordinator- or task-shaped kind the
/// translator would choose from the job's queue.
fn aws_job(ts: f64, job: &str, data: EventData) -> RunEvent {
    let key = dedup::aws_batch(&BatchJobId::new(job), &status_of(&data), 0);
    testing::event(ts, EventSource::AwsBatch, key, data).with_job(job)
}

/// An AWS event for the coordinator job [`JOB`].
fn aws_coordinator(ts: f64, data: EventData) -> RunEvent {
    aws_job(ts, JOB, data)
}

#[test]
fn coordinator_success_reaches_the_run_through_the_binding() {
    let log = vec![
        anchor(1.0, "r1"),
        binding(2.0, "r1"),
        aws_coordinator(3.0, EventData::CoordinatorRunning { log_stream: None }),
        aws_coordinator(4.0, EventData::CoordinatorSucceeded { log_stream: None }),
    ];
    let p = Projection::replay(&log);
    assert_eq!(p.runs["r1"].state(), RunState::Succeeded);
}

#[test]
fn a_run_reaches_terminal_state_from_the_aws_stream_alone() {
    // No engine events at all — this is the case that was broken: the AWS stream is sufficient for
    // run-level state, provided the coordinator binding exists.
    let log = vec![
        anchor(1.0, "r1"),
        binding(2.0, "r1"),
        aws_coordinator(9.0, EventData::CoordinatorSucceeded { log_stream: None }),
    ];
    assert_eq!(Projection::replay(&log).runs["r1"].state(), RunState::Succeeded);
}

#[test]
fn coordinator_events_arriving_before_the_binding_fold_identically() {
    // Late attribution: the AWS terminal fact can beat the binding (or the anchor). The same events
    // folded in every arrival order (incrementally — `replay` would re-sort them) must give the
    // same projection, every field the pending flush sets included.
    let anchor = anchor(1.0, "r1");
    let bind = binding(2.0, "r1");
    let terminal = aws_coordinator(
        3.0,
        EventData::CoordinatorSucceeded { log_stream: Some("coord/stream".into()) },
    );
    let log = [anchor, bind, terminal];
    let orders = every_order(&log);
    let rp = &orders[0].runs["r1"];
    assert_eq!(rp.state(), RunState::Succeeded);
    assert_eq!(rp.coordinator_job_id.as_deref(), Some(JOB));
    assert_eq!(rp.coordinator_log_stream.as_deref(), Some("coord/stream"));
    assert_eq!(rp.completed_at_micros, Some(3_000_000));
    for p in &orders {
        assert_eq!(p.runs, orders[0].runs);
    }
}

/// `run`'s engine binding `job` to its `task` (attempt 1), through the engine translator.
fn task_submitted(ts: f64, run: &str, task: &str, job: &str) -> RunEvent {
    translate::from_engine(
        EventSource::Snakemake,
        &EngineEvent {
            schema_version: 1,
            ts: Timestamp::from_epoch_seconds(ts),
            run_id: RunId::new(run),
            body: EngineBody::TaskSubmitted {
                task_key: TaskKey::new(task),
                attempt: Attempt(1),
                batch_job_id: BatchJobId::new(job),
                image: None,
            },
        },
    )
}

/// Every arrival order of `events`, each folded incrementally in that order.
fn every_order(events: &[RunEvent]) -> Vec<Projection> {
    permutations(events).iter().map(|order| fold_in_order(order)).collect()
}

#[test]
fn another_run_s_task_claim_cannot_take_over_the_coordinator_job() {
    // run "attacker"'s engine claims the victim's coordinator job as one of its tasks, stamping the
    // claim before or after the binding. The coordinator binding is authoritative: the victim's
    // coordinator facts still reach the victim, and the claim never turns them into a task.
    for claim_ts in [0.5, 2.5] {
        let log = vec![
            anchor(1.0, "victim"),
            binding(2.0, "victim"),
            task_submitted(claim_ts, "attacker", "evil", JOB),
            aws_coordinator(
                3.0,
                EventData::CoordinatorRunning { log_stream: Some("coord/stream".into()) },
            ),
            aws_coordinator(4.0, EventData::CoordinatorSucceeded { log_stream: None }),
        ];
        let p = Projection::replay(&log);
        assert_eq!(p.runs["victim"].state(), RunState::Succeeded, "claim at {claim_ts}");
        assert_eq!(p.runs["victim"].coordinator_job_id.as_deref(), Some(JOB));
        assert_eq!(p.runs["victim"].coordinator_log_stream.as_deref(), Some("coord/stream"));
        let evil = &p.runs["attacker"].tasks["evil"].attempts[JOB];
        assert_eq!(
            evil.state,
            AttemptState::Submitted,
            "the claim got none of the job's facts, not even RUNNING"
        );
        assert_eq!(evil.log_stream, None);
        assert_eq!(evil.started_at_micros, None);
        assert_eq!(evil.finished_at_micros, None);
        // The binding is published at submit, before the job can run; whenever the forged claim
        // arrives relative to the AWS facts after that, the victim's projection is the same.
        for tail in permutations(&log[2..]) {
            let mut inc = Projection::new();
            for e in log[..2].iter().chain(&tail) {
                inc.apply(e);
            }
            assert_eq!(inc.runs["victim"], p.runs["victim"]);
        }
    }
}

#[test]
fn the_binding_not_the_queue_name_makes_a_job_the_coordinator() {
    // A coordinator launched on a queue the ingest does not list as a coordinator queue: the
    // translator shapes its Batch events as task events. The control plane bound the job as the
    // run's coordinator, so they still drive the run (here: to FAILED, with its log stream).
    let log = vec![
        anchor(1.0, "r1"),
        binding(2.0, "r1"),
        aws_coordinator(
            3.0,
            EventData::TaskStarted { started_at: None, log_stream: Some("coord/stream".into()) },
        ),
        aws_coordinator(
            4.0,
            EventData::TaskFailed {
                exit_code: 1,
                termination_category: Some(TerminationCategory::HostTerminated),
                status_reason: Some("Host EC2 (instance i-1) terminated.".into()),
                log_stream: None,
            },
        ),
    ];
    let replayed = Projection::replay(&log);
    let rp = &replayed.runs["r1"];
    assert_eq!(rp.state(), RunState::Failed);
    assert_eq!(rp.coordinator_log_stream.as_deref(), Some("coord/stream"));
    // The task-shaped failure keeps Batch's account of the stop for the run.
    assert_eq!(rp.coordinator_exit_code, Some(1));
    assert_eq!(
        rp.coordinator_status_reason.as_deref(),
        Some("Host EC2 (instance i-1) terminated.")
    );
    assert_eq!(rp.coordinator_termination_category.as_deref(), Some("host_terminated"));
    assert_eq!(rp.coordinator_job_id.as_deref(), Some(JOB));
    assert!(rp.tasks.is_empty(), "a coordinator is never a task");
    for p in every_order(&log) {
        assert_eq!(p.runs, replayed.runs);
    }
}

#[test]
fn a_task_bound_job_on_a_coordinator_queue_folds_as_its_task() {
    // The reverse: a worker job on a queue the ingest lists as a coordinator queue arrives
    // coordinator-shaped. It is bound to a task, so it folds as that task's attempt — and never
    // terminates the run.
    let log = vec![
        anchor(1.0, "r1"),
        task_submitted(2.0, "r1", "align", "worker-1"),
        aws_job(
            3.0,
            "worker-1",
            EventData::CoordinatorRunning { log_stream: Some("w/stream".into()) },
        ),
        aws_job(4.0, "worker-1", EventData::CoordinatorSucceeded { log_stream: None }),
    ];
    let replayed = Projection::replay(&log);
    let rp = &replayed.runs["r1"];
    let attempt = &rp.tasks["align"].attempts["worker-1"];
    assert_eq!(attempt.state, AttemptState::Succeeded);
    assert_eq!(attempt.log_stream.as_deref(), Some("w/stream"));
    assert_eq!(rp.state(), RunState::Submitted, "a task never ends the run");
    assert_eq!(rp.coordinator_job_id, None);
    for p in every_order(&log) {
        assert_eq!(p.runs, replayed.runs);
    }
}

#[test]
fn coordinator_failure_fails_the_run() {
    let log = vec![
        anchor(1.0, "r1"),
        binding(2.0, "r1"),
        aws_coordinator(
            3.0,
            EventData::CoordinatorFailed {
                log_stream: None,
                exit_code: None,
                status_reason: Some("Task failed to start".into()),
                termination_category: None,
            },
        ),
    ];
    let rp = &Projection::replay(&log).runs["r1"];
    assert_eq!(rp.state(), RunState::Failed);
    assert_eq!(rp.coordinator_exit_code, None);
    assert_eq!(rp.coordinator_status_reason.as_deref(), Some("Task failed to start"));
}

#[test]
fn a_coordinator_shaped_failure_of_a_task_bound_job_keeps_its_exit_code() {
    // The reverse of the above: the attempt gets the exit code and reason the coordinator kind
    // carried, not the "none reported" defaults.
    let log = vec![
        anchor(1.0, "r1"),
        task_submitted(2.0, "r1", "align", "worker-1"),
        aws_job(
            3.0,
            "worker-1",
            EventData::CoordinatorFailed {
                log_stream: None,
                exit_code: Some(137),
                status_reason: Some("OutOfMemoryError: Container killed".into()),
                termination_category: Some(TerminationCategory::Oom),
            },
        ),
    ];
    let p = Projection::replay(&log);
    let attempt = &p.runs["r1"].tasks["align"].attempts["worker-1"];
    assert_eq!(attempt.exit_code, Some(137));
    assert_eq!(attempt.status_reason.as_deref(), Some("OutOfMemoryError: Container killed"));
    assert_eq!(attempt.termination_category, Some(TerminationCategory::Oom));
    assert_eq!(p.runs["r1"].coordinator_status_reason, None);
}

#[test]
fn a_coordinator_event_never_becomes_a_task() {
    // The coordinator binding must not be conflated with the task binding: a coordinator job has no
    // task_key, and folding it as an attempt would invent a phantom task in the UI.
    let log = vec![
        anchor(1.0, "r1"),
        binding(2.0, "r1"),
        aws_coordinator(3.0, EventData::CoordinatorSucceeded { log_stream: None }),
    ];
    assert!(Projection::replay(&log).runs["r1"].tasks.is_empty());
}

#[test]
fn without_the_binding_the_run_is_stuck_at_submitted() {
    // Documents the bug this binding fixes: the coordinator succeeded, but with no binding the fact
    // is unattributable and the run cannot progress.
    let log = vec![
        anchor(1.0, "r1"),
        aws_coordinator(3.0, EventData::CoordinatorSucceeded { log_stream: None }),
    ];
    assert_eq!(Projection::replay(&log).runs["r1"].state(), RunState::Submitted);
}
