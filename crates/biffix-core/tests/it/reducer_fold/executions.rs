//! A Batch job's executions: they fold under their job's attempt (or, for a coordinator's job,
//! under the run) and move no fact the attempt, the task or the run already had.

use super::local_rules::single_job_anchor;
use super::*;

use biffix_core::ids::BatchJobId;
use biffix_core::reducer::{
    execution_durations, is_trusted_producer, ordered_executions, AttemptProjection,
    ExecutionDurations, RunProjection,
};

/// The start of execution `t{n}` of `job` at `at_s`, on host `ci{n}` and queue `q-spot`.
fn exec_started(job: &str, n: u32, at_s: f64) -> RunEvent {
    testing::execution_started(job, &format!("t{n}"), at_s)
        .with_instance(&format!("ci{n}"))
        .with_queue("q-spot")
}

/// The end of execution `t{n}` of `job` at `stopped_s`, on host `ci{n}` and queue `q-spot`.
fn exec_ended(
    job: &str,
    n: u32,
    started_s: Option<f64>,
    stopped_s: f64,
    category: Option<TerminationCategory>,
) -> RunEvent {
    testing::execution_ended(job, &format!("t{n}"), started_s, stopped_s, category)
        .with_instance(&format!("ci{n}"))
        .with_queue("q-spot")
}

/// One attempt (`job-1` of task `align` in `r1`) that AWS ran twice: execution 1 lost with its
/// host, execution 2 finishing. The job's own lifecycle names only the first host.
fn log() -> Vec<RunEvent> {
    vec![
        task_submitted(1.0, "r1", "align", 1, "job-1"),
        on_host(
            batch(
                2.0,
                "job-1",
                EventData::TaskStarted { started_at: None, log_stream: Some("ls".into()) },
            ),
            "ci1",
        ),
        exec_started("job-1", 1, 3.0),
        exec_ended("job-1", 1, Some(3.0), 684.0, Some(TerminationCategory::HostTerminated)),
        exec_ended("job-1", 2, Some(831.0), 2217.0, None),
        batch(2217.0, "job-1", EventData::TaskSucceeded { exit_code: 0, log_stream: None }),
    ]
}

/// [`log`] without its three execution events.
fn log_without_executions() -> Vec<RunEvent> {
    let log = log();
    vec![log[0].clone(), log[1].clone(), log[5].clone()]
}

/// The attempt of [`log`].
fn ap(p: &Projection) -> &AttemptProjection {
    &p.runs["r1"].tasks["align"].attempts["job-1"]
}

/// `r1`'s coordinator job `coord-1`, bound by the control plane at 1 s.
fn coordinator_bound() -> RunEvent {
    biffix_core::event::coordinator_submitted(
        &RunId::new("r1"),
        Timestamp::from_epoch_seconds(1.0),
        &BatchJobId::new("coord-1"),
    )
}

/// The two executions of the coordinator job `coord-1`: one lost, the next one running.
fn coordinator_executions() -> [RunEvent; 2] {
    [
        exec_ended("coord-1", 1, Some(3.0), 684.0, Some(TerminationCategory::HostTerminated)),
        exec_started("coord-1", 2, 831.0),
    ]
}

/// `r1` folded from `log`, after checking every arrival order agrees.
fn replayed_run(log: &[RunEvent]) -> RunProjection {
    assert_incremental_equals_replay(log);
    Projection::replay(log).runs["r1"].clone()
}

#[test]
fn two_executions_fold_onto_their_attempt_in_any_order() {
    let log = log();
    assert_incremental_equals_replay(&log);
    let p = Projection::replay(&log);
    let ap = ap(&p);
    let ordered = ordered_executions(&ap.executions);
    assert_eq!(ordered.iter().map(|x| x.execution_id.as_str()).collect::<Vec<_>>(), ["t1", "t2"]);
    assert_eq!(
        ordered.iter().map(|x| x.instance_id()).collect::<Vec<_>>(),
        [Some("ci1"), Some("ci2")]
    );
    assert_eq!(
        execution_durations(&ap.executions),
        Some(ExecutionDurations {
            run_micros: 2_067_000_000,
            lost_micros: 681_000_000,
            wait_micros: 147_000_000,
        })
    );
    // The attempt's own facts are its job's, as before: the whole span, and the host its
    // lifecycle named (the later execution on `ci2` did not take it).
    assert_eq!(ap.state, AttemptState::Succeeded);
    assert_eq!(ap.started_at_micros, Some(2_000_000));
    assert_eq!(ap.finished_at_micros, Some(2_217_000_000));
    assert_eq!(ap.instance_id.as_ref().map(biffix_core::ids::InstanceId::as_str), Some("ci1"));
    assert_eq!(ap.termination_category, None);
}

#[test]
fn execution_events_move_no_attempt_fact() {
    let with = Projection::replay(&log());
    let without = Projection::replay(&log_without_executions());
    // The one attempt fact an execution may set: the queue, which the job's lifecycle here
    // never names.
    assert_eq!(ap(&with).queue.as_deref(), Some("q-spot"));
    assert_eq!(ap(&without).queue, None);
    // The whole run, less the executions themselves and that queue: nothing else an execution
    // event folds into may differ.
    let mut stripped = with.runs["r1"].clone();
    for (task_key, task) in &mut stripped.tasks {
        for (job, attempt) in &mut task.attempts {
            attempt.executions.clear();
            attempt.queue = without.runs["r1"].tasks[task_key].attempts[job].queue.clone();
        }
    }
    assert!(!ap(&with).executions.is_empty());
    assert_eq!(stripped, without.runs["r1"]);
}

#[test]
fn an_execution_latches_the_attempts_queue_as_its_lifecycle_does() {
    // The lifecycle names `q-z`, the executions `q-spot`: the lesser name wins, whichever event
    // names it and in every arrival order.
    let named = |queue: &str| {
        let mut log = log();
        log[1] = log[1].clone().with_queue(queue);
        log
    };
    for (lifecycle, expected) in [("q-z", "q-spot"), ("q-a", "q-a")] {
        let log = named(lifecycle);
        assert_incremental_equals_replay(&log);
        let p = Projection::replay(&log);
        assert_eq!(ap(&p).queue.as_deref(), Some(expected), "lifecycle on {lifecycle}");
    }
}

#[test]
fn a_lost_execution_does_not_fail_the_attempt_the_task_or_the_run() {
    // The lost execution's end is in the log; the job's own terminal is not (Batch is retrying).
    let full = log();
    let log = vec![run_anchor(0.5, "r1"), full[0].clone(), full[1].clone(), full[3].clone()];
    assert_incremental_equals_replay(&log);
    let p = Projection::replay(&log);
    let ap = ap(&p);
    assert_eq!(ap.state, AttemptState::Running);
    assert_eq!(ap.exit_code, None);
    assert_eq!(ap.finished_at_micros, None);
    assert_eq!(task_state(&p, "r1", "align"), TaskState::Running);
    assert!(!run_state(&p, "r1").is_terminal());
    assert_eq!(p.runs["r1"].completed_at_micros, None);
    assert_eq!(p.runs["r1"].tasks["align"].attempts.len(), 1);
}

#[test]
fn executions_before_the_binding_are_attributed_when_it_arrives() {
    let log = log();
    let late_binding = [log[2].clone(), log[3].clone(), log[4].clone(), log[0].clone()];
    let p = fold_in_order(&late_binding);
    assert_eq!(ap(&p).executions.len(), 2);
    assert_eq!(p.runs, Projection::replay(&late_binding).runs);
}

#[test]
fn a_coordinators_executions_fold_onto_the_run_not_a_task() {
    let [lost, running] = coordinator_executions();
    let bare = vec![run_anchor(0.5, "r1"), coordinator_bound()];
    let log = [bare.clone(), vec![lost, running]].concat();
    // Every arrival order, so also an execution that folds before its job's binding.
    let rp = replayed_run(&log);
    assert_eq!(rp.coordinator_executions["coord-1"].len(), 2);
    assert!(rp.tasks.is_empty());
    assert_eq!(rp.coordinator_host_id, None);
    assert_eq!(rp.completed_at_micros, None);
    // The whole run, less the executions themselves: they move no other coordinator fact.
    let mut stripped = rp.clone();
    stripped.coordinator_executions.clear();
    assert_eq!(stripped, replayed_run(&bare));
}

#[test]
fn a_coordinator_on_an_unlisted_queue_keeps_its_executions() {
    // The coordinator ran on a queue the ingest does not list, so its own lifecycle arrives
    // task-shaped; the binding still makes the job the run's coordinator.
    let [lost, running] = coordinator_executions();
    let started = batch(
        2.0,
        "coord-1",
        EventData::TaskStarted { started_at: None, log_stream: Some("coord/ls".into()) },
    );
    let bare = vec![run_anchor(0.5, "r1"), coordinator_bound(), started];
    let log = [bare.clone(), vec![lost, running]].concat();
    let rp = replayed_run(&log);
    assert_eq!(rp.coordinator_executions["coord-1"].len(), 2);
    assert!(rp.tasks.is_empty());
    assert_eq!(rp.state(), RunState::Running);
    assert_eq!(rp.state(), replayed_run(&bare).state());
}

#[test]
fn a_single_job_coordinators_executions_survive_a_generation_reset() {
    // Each execution of a single-job coordinator starts the engine again, as a new generation,
    // which drops the generation before it; the executions are AWS's facts and stay.
    let run_started = |ts: f64, generation: u32| {
        biffix_core::translate::from_engine_value(
            EventSource::Nextflow,
            serde_json::json!({"kind": "run_started", "ts": ts, "run_id": "r1",
                "coordinator_attempt": generation}),
        )
        .unwrap()
    };
    let [lost, running] = coordinator_executions();
    let bare = vec![
        single_job_anchor(0.5, "r1"),
        coordinator_bound(),
        run_started(4.0, 1),
        run_started(832.0, 2),
    ];
    let log = [bare.clone(), vec![lost, running]].concat();
    let rp = replayed_run(&log);
    assert_eq!(rp.coordinator_executions["coord-1"].len(), 2);
    assert_eq!(rp.state(), replayed_run(&bare).state());
}

#[test]
fn an_execution_event_from_another_source_is_ignored() {
    // What an engine could plant: an execution's end naming a job, a run and a task.
    let planted = testing::event(
        9.0,
        EventSource::Snakemake,
        "planted",
        EventData::BatchExecutionEnded {
            execution_id: "t1".into(),
            started_at: None,
            stopped_at: Timestamp::from_epoch_seconds(9.0),
            exit_code: Some(1),
            status_reason: None,
            termination_category: Some(TerminationCategory::HostTerminated),
            log_stream: None,
        },
    )
    .with_job("job-1")
    .with_run("r1")
    .with_task("align");
    assert!(!is_trusted_producer(&planted));
    assert!(is_trusted_producer(&exec_ended("job-1", 1, None, 9.0, None)));
    let p = fold_in_order(&[task_submitted(1.0, "r1", "align", 1, "job-1"), planted]);
    assert!(ap(&p).executions.is_empty());
}

#[test]
fn an_execution_event_naming_a_run_is_attributed_by_its_binding() {
    // The job is `r1`'s; an execution event that names `r2` still lands where its job is bound.
    let misnamed = exec_ended("job-1", 1, Some(3.0), 9.0, None).with_run("r2");
    let log = vec![task_submitted(1.0, "r1", "align", 1, "job-1"), misnamed.clone()];
    assert_incremental_equals_replay(&log);
    let p = Projection::replay(&log);
    assert_eq!(ap(&p).executions.len(), 1);
    assert!(!p.runs.contains_key("r2"));
    // With no job to bind it by, it folds to nothing, whatever run it names.
    let mut jobless = misnamed;
    jobless.batch_job_id = None;
    assert!(fold_in_order(&[jobless]).runs.is_empty());
}

#[test]
fn a_never_started_execution_is_kept_and_adds_no_time() {
    let log = vec![
        task_submitted(1.0, "r1", "align", 1, "job-1"),
        exec_ended("job-1", 1, None, 9.0, None),
    ];
    assert_incremental_equals_replay(&log);
    let p = Projection::replay(&log);
    let executions = &ap(&p).executions;
    assert_eq!(executions.len(), 1);
    assert_eq!(executions["t1"].started_at_micros(), None);
    assert_eq!(
        execution_durations(executions),
        Some(ExecutionDurations { run_micros: 0, lost_micros: 0, wait_micros: 0 })
    );
}
