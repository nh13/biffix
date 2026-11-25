//! The one builder of a Batch job's execution events (`eventbridge::execution`), and the reader
//! that makes its snapshot of an EventBridge `detail`. Pure, no AWS.

use std::collections::HashSet;

use biffix_core::dedup::{aws_batch_execution_ended, aws_batch_execution_started};
use biffix_core::event::{EventData, RunEvent, TerminationCategory};
use biffix_core::eventbridge::decode::batch_snapshot;
use biffix_core::eventbridge::execution::{execution_events, BatchJobSnapshot, ExecutionFacts};
use biffix_core::ids::{BatchJobId, InstanceId};
use biffix_core::source::EventSource;
use biffix_core::testing::{
    execution_container_instance_arn, execution_log_stream, execution_task_arn, retried_job_detail,
    retried_job_snapshot, RETRIED_CASES, RETRIED_JOB_ID, RETRIED_LOST_REASON, RETRIED_QUEUE,
};
use biffix_core::time::Timestamp;
use biffix_core::wire::aws_batch::BatchStatus;

/// Execution `n` as an `attempts[]` entry with these times (AWS milliseconds): its own task ARN,
/// host and log stream, and nothing else.
fn facts(n: u32, started: Option<i64>, stopped: Option<i64>) -> ExecutionFacts {
    ExecutionFacts { started_at_millis: started, stopped_at_millis: stopped, ..container(n) }
}

/// Execution `n` as the job-level `container` block, which carries no times of its own.
fn container(n: u32) -> ExecutionFacts {
    ExecutionFacts {
        task_arn: Some(execution_task_arn(n)),
        container_instance_arn: Some(execution_container_instance_arn(n)),
        log_stream: Some(execution_log_stream(n)),
        ..ExecutionFacts::default()
    }
}

/// Execution 1 of the retried case as an `attempts[]` entry: lost with its host, no exit code.
fn lost() -> ExecutionFacts {
    ExecutionFacts {
        status_reason: Some(RETRIED_LOST_REASON.into()),
        ..facts(1, Some(3_000), Some(684_000))
    }
}

/// The retried job in `status` with these `attempts[]` entries, no job-level times and an empty
/// job-level `container`.
fn job(status: BatchStatus, attempts: Vec<ExecutionFacts>) -> BatchJobSnapshot {
    BatchJobSnapshot {
        batch_job_id: RETRIED_JOB_ID.into(),
        queue: Some(RETRIED_QUEUE.into()),
        status,
        started_at_millis: None,
        stopped_at_millis: None,
        status_reason: None,
        current: ExecutionFacts::default(),
        attempts,
    }
}

/// The events' dedup keys, in order.
fn keys(events: &[RunEvent]) -> Vec<&str> {
    events.iter().map(|event| event.dedup_key.as_str()).collect()
}

/// The dedup key of execution `n`'s start.
fn start_key(n: u32) -> String {
    aws_batch_execution_started(&execution_task_arn(n))
}

/// The dedup key of execution `n`'s end.
fn end_key(n: u32) -> String {
    aws_batch_execution_ended(&execution_task_arn(n))
}

/// `seconds` since the epoch.
fn at(seconds: f64) -> Timestamp {
    Timestamp::from_epoch_seconds(seconds)
}

/// The id of the host an event names.
fn host(event: &RunEvent) -> Option<&str> {
    event.instance_id.as_ref().map(InstanceId::as_str)
}

/// A running job whose retry is on execution 2 after execution 1 was lost, with this job-level
/// start.
fn running_retry(job_started_at_millis: i64) -> BatchJobSnapshot {
    BatchJobSnapshot {
        current: container(2),
        started_at_millis: Some(job_started_at_millis),
        ..job(BatchStatus::Running, vec![lost()])
    }
}

/// A job that failed before its container started, which `attempts[]` does not list.
fn failed_to_start() -> BatchJobSnapshot {
    BatchJobSnapshot {
        current: ExecutionFacts {
            container_reason: Some("CannotPullContainerError: pull access denied".into()),
            ..container(1)
        },
        stopped_at_millis: Some(684_000),
        status_reason: Some("Task failed to start".into()),
        ..job(BatchStatus::Failed, vec![])
    }
}

/// A finished retried job whose `attempts[]` lists only the lost execution.
fn finished_with_only_the_lost_entry() -> BatchJobSnapshot {
    BatchJobSnapshot {
        current: ExecutionFacts { exit_code: Some(0), ..container(2) },
        started_at_millis: Some(831_000),
        stopped_at_millis: Some(2_217_000),
        ..job(BatchStatus::Succeeded, vec![lost()])
    }
}

/// `attempts[]` entries whose times are zero or negative in every position.
fn zero_times() -> BatchJobSnapshot {
    job(
        BatchStatus::Failed,
        vec![
            facts(1, Some(0), Some(5_000)),
            facts(2, Some(1_000), Some(0)),
            facts(3, Some(0), Some(-1)),
        ],
    )
}

/// `attempts[]` out of time order, with one execution listed twice.
fn unordered_with_a_repeat() -> BatchJobSnapshot {
    let first = || facts(1, Some(3_000), Some(684_000));
    job(BatchStatus::Succeeded, vec![facts(2, Some(831_000), Some(2_217_000)), first(), first()])
}

/// A running job on its first execution, started at 3 s, with these `attempts[]` entries.
fn running_first(attempts: Vec<ExecutionFacts>) -> BatchJobSnapshot {
    BatchJobSnapshot {
        current: container(1),
        started_at_millis: Some(3_000),
        ..job(BatchStatus::Running, attempts)
    }
}

#[test]
fn e1_a_stopped_entry_yields_its_end_on_its_own_host() {
    let events = execution_events(&retried_job_snapshot());
    assert_eq!(keys(&events), [end_key(1), end_key(2)]);

    let lost = &events[0];
    assert_eq!(lost.source, EventSource::AwsBatch);
    assert_eq!(lost.batch_job_id.as_ref().map(BatchJobId::as_str), Some(RETRIED_JOB_ID));
    assert_eq!(host(lost), Some("ci1"));
    assert_eq!(lost.queue.as_deref(), Some(RETRIED_QUEUE));
    assert_eq!((&lost.run_id, &lost.task_key, lost.attempt), (&None, &None, None));
    assert_eq!(lost.ts.as_micros(), 684_000_000);
    assert_eq!(
        lost.data,
        EventData::BatchExecutionEnded {
            execution_id: execution_task_arn(1),
            started_at: Some(at(3.0)),
            stopped_at: at(684.0),
            exit_code: None,
            status_reason: Some(RETRIED_LOST_REASON.into()),
            termination_category: Some(TerminationCategory::HostTerminated),
            log_stream: Some("job-def/default/t1".into()),
        }
    );

    let finished = &events[1];
    assert_eq!(host(finished), Some("ci2"));
    assert_eq!(finished.ts.as_micros(), 2_217_000_000);
    assert_eq!(
        finished.data,
        EventData::BatchExecutionEnded {
            execution_id: execution_task_arn(2),
            started_at: Some(at(831.0)),
            stopped_at: at(2217.0),
            exit_code: Some(0),
            status_reason: Some("Essential container in task exited".into()),
            termination_category: None,
            log_stream: Some("job-def/default/t2".into()),
        }
    );
}

#[test]
fn e2_a_started_unstopped_entry_yields_one_start() {
    // The job-level `container` names the same execution: the entry's start is the only event.
    let events = execution_events(&running_first(vec![facts(1, Some(3_000), None)]));
    assert_eq!(keys(&events), [start_key(1)]);
    assert_eq!(
        events[0].data,
        EventData::BatchExecutionStarted {
            execution_id: execution_task_arn(1),
            started_at: at(3.0),
            log_stream: Some("job-def/default/t1".into()),
        }
    );
}

#[test]
fn e3_a_running_job_yields_its_current_execution() {
    let events = execution_events(&running_first(vec![]));
    assert_eq!(keys(&events), [start_key(1)]);
    assert_eq!(host(&events[0]), Some("ci1"));
    assert_eq!(events[0].ts.as_micros(), 3_000_000);
}

#[test]
fn e3_a_retrys_running_yields_the_lost_end_and_the_new_start() {
    let events = execution_events(&running_retry(831_000));
    assert_eq!(keys(&events), [end_key(1), start_key(2)]);
    assert_eq!(host(&events[1]), Some("ci2"));
    assert_eq!(events[1].ts.as_micros(), 831_000_000);
}

#[test]
fn e3_a_stale_job_level_start_is_not_the_current_executions() {
    // A job-level start that is execution 1's (3 s), or no later than its stop (684 s), is not
    // the retry's: the start has to be strictly later than every stop `attempts[]` lists.
    for stale in [3_000, 684_000] {
        assert_eq!(
            keys(&execution_events(&running_retry(stale))),
            [end_key(1)],
            "job start {stale}"
        );
    }
}

#[test]
fn e3_does_not_start_a_current_execution_its_entry_already_ended() {
    // Still RUNNING, but `attempts[]` already lists execution 2 as stopped: its end is the only
    // event for it, even though the job-level start (831 s) would otherwise be its own.
    let snapshot = BatchJobSnapshot {
        attempts: vec![lost(), facts(2, Some(831_000), Some(2_217_000))],
        ..running_retry(831_000)
    };
    assert_eq!(keys(&execution_events(&snapshot)), [end_key(1), end_key(2)]);
}

#[test]
fn an_entry_without_a_task_arn_still_counts_in_the_stale_start_guard() {
    // An unnamed entry yields no event of its own, but its stop still says the job-level start
    // before it belongs to an earlier execution, not to the one `container` names.
    let unnamed = ExecutionFacts { task_arn: None, ..lost() };
    let running = |job_started_at_millis| BatchJobSnapshot {
        attempts: vec![unnamed.clone()],
        ..running_retry(job_started_at_millis)
    };
    assert_eq!(execution_events(&running(684_000)), vec![]);
    assert_eq!(keys(&execution_events(&running(831_000))), [start_key(2)]);
}

#[test]
fn e3_a_job_level_start_no_later_than_another_executions_start_is_not_the_current_executions() {
    // Execution 1 is listed started but not yet stopped, and the job-level start is still its
    // own (3 s): no stop says so, but its start does, so execution 2 gets no start from it.
    let unstopped = |job_started_at_millis| BatchJobSnapshot {
        attempts: vec![facts(1, Some(3_000), None)],
        ..running_retry(job_started_at_millis)
    };
    for stale in [2_000, 3_000] {
        assert_eq!(keys(&execution_events(&unstopped(stale))), [start_key(1)], "job start {stale}");
    }
    assert_eq!(keys(&execution_events(&unstopped(3_001))), [start_key(1), start_key(2)]);
}

#[test]
fn e3_only_a_running_job_has_a_current_start() {
    for status in [BatchStatus::Starting, BatchStatus::Runnable] {
        let snapshot = BatchJobSnapshot { status, ..running_first(vec![]) };
        assert_eq!(execution_events(&snapshot), vec![]);
    }
}

#[test]
fn e4_a_terminal_without_its_entry_ends_from_the_job_level() {
    let events = execution_events(&failed_to_start());
    assert_eq!(keys(&events), [end_key(1)]);
    assert_eq!(host(&events[0]), Some("ci1"));
    assert_eq!(events[0].ts.as_micros(), 684_000_000);
    assert_eq!(
        events[0].data,
        EventData::BatchExecutionEnded {
            execution_id: execution_task_arn(1),
            started_at: None,
            stopped_at: at(684.0),
            exit_code: None,
            status_reason: Some(
                "Task failed to start: CannotPullContainerError: pull access denied".into()
            ),
            termination_category: None,
            log_stream: Some("job-def/default/t1".into()),
        }
    );
}

#[test]
fn e4_ends_the_final_execution_when_only_earlier_entries_are_listed() {
    let events = execution_events(&finished_with_only_the_lost_entry());
    assert_eq!(keys(&events), [end_key(1), end_key(2)]);
    assert_eq!(host(&events[1]), Some("ci2"));
    let EventData::BatchExecutionEnded { started_at, exit_code, .. } = &events[1].data else {
        panic!("not an execution's end: {:?}", events[1].data);
    };
    assert_eq!((*started_at, *exit_code), (Some(at(831.0)), Some(0)));
}

/// A finished retried job whose final execution's entry has a start but no stop yet, while the
/// job level already has the stop.
fn finished_with_an_unstopped_final_entry() -> BatchJobSnapshot {
    BatchJobSnapshot {
        attempts: vec![lost(), facts(2, Some(831_000), None)],
        ..finished_with_only_the_lost_entry()
    }
}

/// A finished retried job whose job-level start is still the lost execution's, and whose final
/// execution's entry carries no time.
fn finished_with_a_stale_job_level_start(job_started_at_millis: i64) -> BatchJobSnapshot {
    BatchJobSnapshot {
        started_at_millis: Some(job_started_at_millis),
        attempts: vec![lost(), facts(2, None, None)],
        ..finished_with_only_the_lost_entry()
    }
}

/// The start an execution's end carries.
fn start_of(event: &RunEvent) -> Option<Timestamp> {
    let EventData::BatchExecutionEnded { started_at, .. } = &event.data else {
        panic!("not an execution's end: {:?}", event.data);
    };
    *started_at
}

#[test]
fn e4_replaces_the_start_its_unstopped_entry_yielded() {
    // One event for execution 2, its end: never a start and an end of one execution at once.
    let events = execution_events(&finished_with_an_unstopped_final_entry());
    assert_eq!(keys(&events), [end_key(1), end_key(2)]);
    assert_eq!(events[1].ts.as_micros(), 2_217_000_000);
    assert_eq!(start_of(&events[1]), Some(at(831.0)));
}

#[test]
fn e4_takes_the_entrys_own_start_over_the_job_levels() {
    // The entry says when its execution started; a job-level start that disagrees is not used.
    let snapshot = BatchJobSnapshot {
        started_at_millis: Some(3_000),
        ..finished_with_an_unstopped_final_entry()
    };
    let events = execution_events(&snapshot);
    assert_eq!(keys(&events), [end_key(1), end_key(2)]);
    assert_eq!(start_of(&events[1]), Some(at(831.0)));
}

#[test]
fn e4_a_stale_job_level_start_is_not_the_final_executions() {
    // As for a running job: a job-level start no later than a stop `attempts[]` lists is an
    // earlier execution's, so the end carries no start rather than a wrong one.
    for stale in [3_000, 684_000] {
        let events = execution_events(&finished_with_a_stale_job_level_start(stale));
        assert_eq!(keys(&events), [end_key(1), end_key(2)], "job start {stale}");
        assert_eq!(start_of(&events[1]), None, "job start {stale}");
    }
    let events = execution_events(&finished_with_a_stale_job_level_start(684_001));
    assert_eq!(start_of(&events[1]), Some(at(684.001)));
}

#[test]
fn e4_does_not_end_an_execution_its_entry_already_ended() {
    // The retried case lists both executions, and its job level repeats execution 2.
    let mut snapshot = retried_job_snapshot();
    snapshot.stopped_at_millis = Some(9_999_000);
    let events = execution_events(&snapshot);
    assert_eq!(keys(&events), [end_key(1), end_key(2)]);
    assert_eq!(events[1].ts.as_micros(), 2_217_000_000);
}

#[test]
fn e4_needs_a_terminal_status() {
    // A job-level stop on a job that is not SUCCEEDED or FAILED, or whose status this build does
    // not know, ends nothing: only the listed entry's own end is recorded.
    for status in [
        BatchStatus::Submitted,
        BatchStatus::Pending,
        BatchStatus::Runnable,
        BatchStatus::Starting,
        BatchStatus::Running,
        BatchStatus::Unknown("FUTURE_STATE".into()),
    ] {
        let snapshot = BatchJobSnapshot {
            status: status.clone(),
            started_at_millis: None,
            ..finished_with_only_the_lost_entry()
        };
        assert_eq!(keys(&execution_events(&snapshot)), [end_key(1)], "status {status:?}");
    }
}

/// A job whose `attempts[]` entries name no task: one without the field, one with it empty.
fn entries_without_a_task() -> BatchJobSnapshot {
    let unnamed = |task_arn: Option<&str>| ExecutionFacts {
        task_arn: task_arn.map(String::from),
        ..facts(1, Some(3_000), Some(684_000))
    };
    job(BatchStatus::Succeeded, vec![unnamed(None), unnamed(Some(""))])
}

#[test]
fn an_entry_without_a_task_arn_yields_nothing() {
    let snapshot = entries_without_a_task();
    assert_eq!(execution_events(&snapshot), vec![]);
    assert_eq!(snapshot.entries_without_task_arn(), 2);
    assert_eq!(retried_job_snapshot().entries_without_task_arn(), 0);
}

#[test]
fn a_time_of_zero_or_less_is_absent() {
    let events = execution_events(&zero_times());
    // Execution 1 stopped with no start, execution 2 started with no stop, execution 3 has no time.
    assert_eq!(keys(&events), [start_key(2), end_key(1)]);
    let EventData::BatchExecutionEnded { started_at, .. } = &events[1].data else {
        panic!("not an execution's end: {:?}", events[1].data);
    };
    assert_eq!(*started_at, None);

    // The job-level times too: neither a current start nor a job-level end is made of a zero.
    let running = BatchJobSnapshot { started_at_millis: Some(0), ..running_first(vec![]) };
    assert_eq!(execution_events(&running), vec![]);
    let failed = BatchJobSnapshot { stopped_at_millis: Some(0), ..failed_to_start() };
    assert_eq!(execution_events(&failed), vec![]);
}

/// A failed job whose only listed execution never started its container.
fn never_started() -> BatchJobSnapshot {
    let entry = ExecutionFacts {
        status_reason: Some("Task failed to start".into()),
        container_reason: Some("CannotPullContainerError: x".into()),
        ..facts(1, None, Some(2_000))
    };
    job(BatchStatus::Failed, vec![entry])
}

#[test]
fn a_never_started_entry_ends_without_a_start() {
    let events = execution_events(&never_started());
    assert_eq!(keys(&events), [end_key(1)]);
    assert_eq!(
        events[0].data,
        EventData::BatchExecutionEnded {
            execution_id: execution_task_arn(1),
            started_at: None,
            stopped_at: at(2.0),
            exit_code: None,
            status_reason: Some("Task failed to start: CannotPullContainerError: x".into()),
            termination_category: None,
            log_stream: Some("job-def/default/t1".into()),
        }
    );
}

#[test]
fn events_are_sorted_with_one_event_per_key() {
    assert_eq!(keys(&execution_events(&unordered_with_a_repeat())), [end_key(1), end_key(2)]);
}

#[test]
fn an_empty_queue_host_or_log_stream_is_absent() {
    // Whatever a reader stored: an event never carries `queue: ""` or `instance_id: ""`.
    let blank = |facts: ExecutionFacts| ExecutionFacts {
        container_instance_arn: Some(String::new()),
        log_stream: Some(String::new()),
        ..facts
    };
    let ended = BatchJobSnapshot {
        queue: Some(String::new()),
        ..job(BatchStatus::Succeeded, vec![blank(facts(1, Some(3_000), Some(684_000)))])
    };
    let started = BatchJobSnapshot {
        // An ARN with an empty name is no queue either.
        queue: Some("arn:aws:batch:us-east-1:123456789012:job-queue/".into()),
        current: blank(container(1)),
        ..running_first(vec![])
    };
    for snapshot in [ended, started] {
        let events = execution_events(&snapshot);
        assert_eq!(events.len(), 1);
        assert_eq!((&events[0].queue, &events[0].instance_id), (&None, &None));
        let (EventData::BatchExecutionStarted { log_stream, .. }
        | EventData::BatchExecutionEnded { log_stream, .. }) = &events[0].data
        else {
            panic!("not an execution event: {:?}", events[0].data);
        };
        assert_eq!(*log_stream, None);
    }

    // An ARN that ends in `/` names no host either.
    let no_id = ExecutionFacts {
        container_instance_arn: Some(
            "arn:aws:ecs:us-east-1:123456789012:container-instance/".into(),
        ),
        ..facts(1, Some(3_000), Some(684_000))
    };
    let events = execution_events(&job(BatchStatus::Succeeded, vec![no_id]));
    assert_eq!(events[0].instance_id, None);
}

#[test]
fn no_execution_is_both_started_and_ended_in_one_snapshot() {
    let snapshots = [
        retried_job_snapshot(),
        running_first(vec![facts(1, Some(3_000), None)]),
        running_first(vec![]),
        running_retry(831_000),
        running_retry(3_000),
        failed_to_start(),
        finished_with_only_the_lost_entry(),
        finished_with_an_unstopped_final_entry(),
        finished_with_a_stale_job_level_start(3_000),
        entries_without_a_task(),
        zero_times(),
        never_started(),
        unordered_with_a_repeat(),
    ];
    for snapshot in &snapshots {
        let events = execution_events(snapshot);
        let executions = |started: bool| -> HashSet<String> {
            events
                .iter()
                .filter_map(|event| match &event.data {
                    EventData::BatchExecutionStarted { execution_id, .. } if started => {
                        Some(execution_id.clone())
                    }
                    EventData::BatchExecutionEnded { execution_id, .. } if !started => {
                        Some(execution_id.clone())
                    }
                    _ => None,
                })
                .collect()
        };
        let (started, ended) = (executions(true), executions(false));
        assert_eq!(started.len() + ended.len(), events.len(), "one event per key: {events:?}");
        assert!(started.is_disjoint(&ended), "started and ended at once: {events:?}");
        assert_eq!(execution_events(snapshot), events, "the builder is deterministic");
    }
}

#[test]
fn a_retried_jobs_detail_reads_into_the_snapshot() {
    let detail = retried_job_detail("SUCCEEDED", 681, 147, 1386);
    assert_eq!(batch_snapshot(&detail), Some(retried_job_snapshot()));
}

#[test]
fn every_retried_case_yields_two_ends_and_no_start() {
    for (lost_s, gap_s, run_s) in RETRIED_CASES {
        let detail = retried_job_detail("SUCCEEDED", lost_s, gap_s, run_s);
        let events = execution_events(&batch_snapshot(&detail).expect("a job's detail"));
        assert_eq!(keys(&events), [end_key(1), end_key(2)], "case {lost_s}/{gap_s}/{run_s}");
        assert_eq!(
            events[1].ts.as_micros() - events[0].ts.as_micros(),
            (gap_s + run_s) * 1_000_000,
            "case {lost_s}/{gap_s}/{run_s}"
        );
    }
}

#[test]
fn a_detail_with_odd_attempts_still_reads() {
    let detail = |attempts: serde_json::Value| serde_json::json!({"jobId": "job-odd", "status": "RUNNING", "attempts": attempts});
    let not_a_list = batch_snapshot(&detail(serde_json::json!("nope"))).expect("a job's detail");
    assert_eq!(not_a_list.attempts, vec![]);
    assert_eq!((not_a_list.status, not_a_list.queue), (BatchStatus::Running, None));

    let odd = batch_snapshot(&detail(serde_json::json!([1, null, {"container": 5}])))
        .expect("a job's detail");
    assert_eq!(odd.attempts, vec![ExecutionFacts::default(); 3]);
    assert_eq!(odd.entries_without_task_arn(), 3);
    assert_eq!(execution_events(&odd), vec![]);

    assert_eq!(batch_snapshot(&serde_json::json!({"status": "RUNNING"})), None);
    assert_eq!(batch_snapshot(&serde_json::json!({"jobId": "job-odd"})), None);
}

#[test]
fn a_detail_keeps_what_aws_sent_for_the_builder_to_judge() {
    // The reader stores an empty task ARN and a zero time as AWS sent them; only the builder
    // decides what is absent. An exit code that does not fit an `i32` is no exit code.
    let detail = serde_json::json!({
        "jobId": "job-odd",
        "jobQueue": "",
        "status": "SOMETHING_NEW",
        "startedAt": 0,
        "container": {"taskArn": "", "exitCode": 5_000_000_000_i64, "reason": "why"},
        "attempts": [{"startedAt": 0, "statusReason": "", "container": {"exitCode": 137}}],
    });
    let snapshot = batch_snapshot(&detail).expect("a job's detail");
    assert_eq!(snapshot.status, BatchStatus::Unknown("SOMETHING_NEW".into()));
    assert_eq!((snapshot.queue.as_deref(), snapshot.started_at_millis), (Some(""), Some(0)));
    assert_eq!(
        snapshot.current,
        ExecutionFacts {
            task_arn: Some(String::new()),
            container_reason: Some("why".into()),
            ..ExecutionFacts::default()
        }
    );
    assert_eq!(
        snapshot.attempts,
        vec![ExecutionFacts {
            started_at_millis: Some(0),
            status_reason: Some(String::new()),
            exit_code: Some(137),
            ..ExecutionFacts::default()
        }]
    );
}

#[test]
fn e4_keeps_what_the_entry_names_when_the_job_level_container_does_not() {
    // The entry for execution 2 names its host and log stream; the job-level `container` names
    // only its task. The end built for it loses neither.
    let snapshot = BatchJobSnapshot {
        current: ExecutionFacts {
            task_arn: Some(execution_task_arn(2)),
            exit_code: Some(0),
            ..ExecutionFacts::default()
        },
        ..finished_with_an_unstopped_final_entry()
    };
    let events = execution_events(&snapshot);
    assert_eq!(keys(&events), [end_key(1), end_key(2)]);
    assert_eq!(host(&events[1]), Some("ci2"));
    let EventData::BatchExecutionEnded { log_stream, exit_code, .. } = &events[1].data else {
        panic!("not an execution's end: {:?}", events[1].data);
    };
    assert_eq!((log_stream.as_deref(), *exit_code), (Some("job-def/default/t2"), Some(0)));
}

#[test]
fn e4_a_sole_entry_without_a_task_is_the_final_execution_when_its_times_are_the_jobs() {
    // One execution, whose entry omits its `container`: its start and stop are the job's, so it
    // is the execution the job-level `container` names, and the job-level start is that one's.
    let snapshot = BatchJobSnapshot {
        current: ExecutionFacts { exit_code: Some(0), ..container(1) },
        started_at_millis: Some(3_000),
        stopped_at_millis: Some(684_000),
        ..job(
            BatchStatus::Succeeded,
            vec![ExecutionFacts {
                started_at_millis: Some(3_000),
                stopped_at_millis: Some(684_000),
                ..ExecutionFacts::default()
            }],
        )
    };
    let events = execution_events(&snapshot);
    assert_eq!(keys(&events), [end_key(1)]);
    assert_eq!(start_of(&events[0]), Some(at(3.0)));
    // A RUNNING retry's entry is never taken for the current execution, however its times look.
    let running = BatchJobSnapshot {
        status: BatchStatus::Running,
        stopped_at_millis: None,
        attempts: vec![ExecutionFacts {
            started_at_millis: Some(3_000),
            ..ExecutionFacts::default()
        }],
        ..snapshot
    };
    assert_eq!(keys(&execution_events(&running)), Vec::<&str>::new());
}
