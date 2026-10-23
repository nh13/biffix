//! Attempts: queued vs running, per-attempt detail, claims, retries that skip or reuse numbers, per-task images and the log stream across two terminals.

use super::*;

#[test]
fn task_with_only_queued_attempt_reads_queued() {
    // Snakemake submitted the job and Batch has it SUBMITTED/PENDING/RUNNABLE (TaskQueued), but it
    // has not started executing — the task is QUEUED, not running.
    let log = vec![
        run_anchor(1.0, "r1"),
        run_level(2.0, "r1", EventData::RunStarted { session_id: None }),
        task_submitted(3.0, "r1", "align:sample=A", 1, "job-1"),
        batch(4.0, "job-1", EventData::TaskQueued {}),
    ];
    let p = Projection::replay(&log);
    assert_eq!(task_state(&p, "r1", "align:sample=A"), TaskState::Queued);
    let counts = p.runs["r1"].task_counts();
    assert_eq!(counts.queued, 1);
    assert_eq!(counts.running, 0);
    assert_eq!(counts.observed, 1);
}

#[test]
fn task_submitted_but_not_yet_queued_reads_queued() {
    // Even before Batch reports RUNNABLE, an attempt only `Submitted` is not running.
    let log = vec![
        run_anchor(1.0, "r1"),
        run_level(2.0, "r1", EventData::RunStarted { session_id: None }),
        task_submitted(3.0, "r1", "t", 1, "job-1"),
    ];
    let p = Projection::replay(&log);
    assert_eq!(task_state(&p, "r1", "t"), TaskState::Queued);
}

#[test]
fn task_reads_running_once_started() {
    let log = vec![
        run_anchor(1.0, "r1"),
        run_level(2.0, "r1", EventData::RunStarted { session_id: None }),
        task_submitted(3.0, "r1", "t", 1, "job-1"),
        batch(4.0, "job-1", EventData::TaskQueued {}),
        batch(5.0, "job-1", EventData::TaskStarted { started_at: None, log_stream: None }),
    ];
    let p = Projection::replay(&log);
    assert_eq!(task_state(&p, "r1", "t"), TaskState::Running);
    let counts = p.runs["r1"].task_counts();
    assert_eq!(counts.running, 1);
    assert_eq!(counts.queued, 0);
}

#[test]
fn queued_task_on_terminal_run_surfaces_failed() {
    // A task still queued when its run terminates never ran — it is orphaned, so the effective
    // (persisted/counted) state is FAILED, exactly as an orphaned `running` task already was.
    let log = vec![
        run_anchor(1.0, "r1"),
        run_level(2.0, "r1", EventData::RunStarted { session_id: None }),
        task_submitted(3.0, "r1", "t", 1, "job-1"),
        batch(4.0, "job-1", EventData::TaskQueued {}),
        coord(
            9.0,
            "r1",
            EventData::CoordinatorFailed {
                log_stream: None,
                exit_code: None,
                status_reason: None,
                termination_category: None,
            },
        ),
    ];
    let p = Projection::replay(&log);
    assert_eq!(run_state(&p, "r1"), RunState::Failed);
    let rp = &p.runs["r1"];
    let tp = rp.tasks.get("t").unwrap();
    assert_eq!(tp.state(), TaskState::Queued);
    assert_eq!(rp.effective_task_state(tp), TaskState::Failed);
    let counts = rp.task_counts();
    assert_eq!(counts.failed, 1);
    assert_eq!(counts.queued, 0);
}

/// A Batch event for `job` that also names the queue the job ran on.
fn batch_on(ts: f64, job: &str, queue: &str, data: EventData) -> RunEvent {
    batch(ts, job, data).with_queue(queue)
}

#[test]
fn attempt_status_reason_follows_the_winning_terminal_in_any_order() {
    // The failure is the later terminal, so "success outranks failure" decides, not "the later
    // terminal wins".
    let failed = batch(
        6.0,
        "job-1",
        EventData::TaskFailed {
            exit_code: 137,
            termination_category: Some(TerminationCategory::Spot),
            status_reason: Some("Host EC2 (instance i-1) terminated.".into()),
            log_stream: None,
        },
    );
    let succeeded =
        batch(5.0, "job-1", EventData::TaskSucceeded { exit_code: 0, log_stream: None });
    let bind = task_submitted(3.0, "r1", "align:sample=A", 1, "job-1");

    // Failure alone: the reason is kept.
    let p = Projection::replay(&[bind.clone(), failed.clone()]);
    let ap = &p.runs["r1"].tasks["align:sample=A"].attempts["job-1"];
    assert_eq!(ap.status_reason.as_deref(), Some("Host EC2 (instance i-1) terminated."));

    // A success outranks it and owns the detail, whichever terminal folds first.
    for order in permutations(&[bind, failed, succeeded]) {
        let p = fold_in_order(&order);
        let ap = &p.runs["r1"].tasks["align:sample=A"].attempts["job-1"];
        assert_eq!(ap.status_reason, None);
        assert_eq!(ap.termination_category, None);
    }
}

#[test]
fn attempt_queue_is_order_independent() {
    let events = vec![
        task_submitted(3.0, "r1", "align:sample=A", 1, "job-1"),
        batch_on(4.0, "job-1", "q-b", EventData::TaskQueued {}),
        batch_on(
            5.0,
            "job-1",
            "q-a",
            EventData::TaskStarted { started_at: None, log_stream: None },
        ),
        batch(6.0, "job-1", EventData::TaskSucceeded { exit_code: 0, log_stream: None }),
    ];
    for order in permutations(&events) {
        let p = fold_in_order(&order);
        let ap = &p.runs["r1"].tasks["align:sample=A"].attempts["job-1"];
        assert_eq!(ap.queue.as_deref(), Some("q-a"));
    }
}

/// A Batch event for `job` that names what the job requested.
fn batch_requesting(
    ts: f64,
    job: &str,
    vcpus: Option<f64>,
    memory_mib: Option<i64>,
    data: EventData,
) -> RunEvent {
    let mut e = batch(ts, job, data);
    e.requested_vcpus = vcpus;
    e.requested_memory_mib = memory_mib;
    e
}

#[test]
fn attempt_requested_resources_fold_from_any_batch_event_in_any_order() {
    // Only some events carry the request (an older producer, a terminal from reconciliation that
    // lacks it): any one that names it fills the attempt, and the result does not depend on order.
    let events = vec![
        task_submitted(3.0, "r1", "align:sample=A", 1, "job-1"),
        batch(4.0, "job-1", EventData::TaskQueued {}),
        batch_requesting(
            5.0,
            "job-1",
            Some(4.0),
            Some(30_000),
            EventData::TaskStarted { started_at: None, log_stream: None },
        ),
        batch(6.0, "job-1", EventData::TaskSucceeded { exit_code: 0, log_stream: None }),
    ];
    assert_incremental_equals_replay(&events);
    let ap = &Projection::replay(&events).runs["r1"].tasks["align:sample=A"].attempts["job-1"];
    assert_eq!(ap.requested_vcpus, Some(4.0));
    assert_eq!(ap.requested_memory_mib, Some(30_000));
}

#[test]
fn attempt_requested_resources_are_per_attempt_and_absent_when_never_reported() {
    // A retry is a new Batch job with its own request (the engine may raise memory on retry), and
    // an attempt whose events never named one stays unknown — the fold never invents a value.
    let events = vec![
        task_submitted(1.0, "r1", "align:sample=A", 1, "job-1"),
        batch_requesting(2.0, "job-1", Some(2.0), Some(4_096), EventData::TaskQueued {}),
        task_submitted(3.0, "r1", "align:sample=A", 2, "job-2"),
        batch_requesting(4.0, "job-2", Some(2.0), Some(8_192), EventData::TaskQueued {}),
        task_submitted(5.0, "r1", "call:sample=A", 1, "job-3"),
        batch(6.0, "job-3", EventData::TaskQueued {}),
    ];
    assert_incremental_equals_replay(&events);
    let tasks = &Projection::replay(&events).runs["r1"].tasks;
    let attempts = &tasks["align:sample=A"].attempts;
    assert_eq!(attempts["job-1"].requested_memory_mib, Some(4_096));
    assert_eq!(attempts["job-2"].requested_memory_mib, Some(8_192));
    let unknown = &tasks["call:sample=A"].attempts["job-3"];
    assert_eq!(unknown.requested_vcpus, None);
    assert_eq!(unknown.requested_memory_mib, None);
}

#[test]
fn attempt_requested_resources_conflict_resolves_order_independently() {
    // A job's request is fixed at submit, so two values never happen in practice; if they did, the
    // fold must still agree with a replay (it keeps the smaller of each).
    let events = vec![
        task_submitted(1.0, "r1", "align:sample=A", 1, "job-1"),
        batch_requesting(2.0, "job-1", Some(8.0), Some(1_024), EventData::TaskQueued {}),
        batch_requesting(
            3.0,
            "job-1",
            Some(4.0),
            Some(2_048),
            EventData::TaskStarted { started_at: None, log_stream: None },
        ),
    ];
    assert_incremental_equals_replay(&events);
    let ap = &Projection::replay(&events).runs["r1"].tasks["align:sample=A"].attempts["job-1"];
    assert_eq!(ap.requested_vcpus, Some(4.0));
    assert_eq!(ap.requested_memory_mib, Some(1_024));
}

#[test]
fn another_run_s_later_claim_does_not_take_over_a_bound_worker_job() {
    // The victim's engine binds job-v; another run's engine then names the same job. The first
    // binding keeps the job's AWS facts, so the victim's task still reaches its terminal.
    let log = vec![
        run_anchor(1.0, "victim"),
        task_submitted(2.0, "victim", "align", 1, "job-v"),
        task_submitted(3.0, "attacker", "evil", 1, "job-v"),
        batch(4.0, "job-v", EventData::TaskSucceeded { exit_code: 0, log_stream: None }),
    ];
    let p = Projection::replay(&log);
    assert_eq!(task_state(&p, "victim", "align"), TaskState::Succeeded);
    let evil = &p.runs["attacker"].tasks["evil"].attempts["job-v"];
    assert_eq!(evil.state, AttemptState::Submitted, "the claim got no facts");
    // Whenever the AWS fact arrives, as long as the victim's claim folds first.
    for order in [[0, 1, 2, 3], [0, 1, 3, 2], [0, 3, 1, 2], [3, 0, 1, 2]] {
        let ordered: Vec<RunEvent> = order.iter().map(|&i| log[i].clone()).collect();
        assert_eq!(fold_in_order(&ordered).runs, p.runs, "order {order:?}");
    }
}

fn failed_batch(ts: f64, job: &str) -> RunEvent {
    batch(
        ts,
        job,
        EventData::TaskFailed {
            exit_code: 1,
            termination_category: None,
            status_reason: None,
            log_stream: None,
        },
    )
}

#[test]
fn a_retry_answered_by_a_skipped_attempt_number_still_fails() {
    // The engine announced attempt 2 but submitted the retry as attempt 3; when that fails too, the
    // task is FAILED rather than waiting forever for an attempt numbered exactly 2. Attempt 3 is
    // submitted no later than the announcement, so only its number answers the retry.
    let log = vec![
        task_submitted(1.0, "r1", "align", 1, "job-1"),
        failed_batch(2.0, "job-1"),
        retrying(3.0, 2),
        task_submitted(3.0, "r1", "align", 3, "job-3"),
        failed_batch(5.0, "job-3"),
    ];
    assert_eq!(task_state(&Projection::replay(&log), "r1", "align"), TaskState::Failed);
    assert_incremental_equals_replay(&log);
}

#[test]
fn a_retry_answered_by_a_reused_attempt_number_still_fails() {
    // The retry came back numbered 1 again (on a new job). It was submitted after the retry was
    // announced, so it answers the retry: once it fails, the task is FAILED.
    let log = vec![
        task_submitted(1.0, "r1", "align", 1, "job-1"),
        failed_batch(2.0, "job-1"),
        retrying(3.0, 2),
        task_submitted(4.0, "r1", "align", 1, "job-1b"),
        failed_batch(5.0, "job-1b"),
    ];
    assert_eq!(task_state(&Projection::replay(&log), "r1", "align"), TaskState::Failed);
    assert_incremental_equals_replay(&log);
}

#[test]
fn a_retry_is_still_pending_until_a_later_submission_answers_it() {
    // Only the first attempt (submitted before the announcement) exists: the retry gap is RUNNING.
    let log = vec![
        task_submitted(1.0, "r1", "align", 1, "job-1"),
        failed_batch(2.0, "job-1"),
        retrying(3.0, 2),
    ];
    assert_eq!(task_state(&Projection::replay(&log), "r1", "align"), TaskState::Running);
    assert_incremental_equals_replay(&log);
}

fn submitted_with_image(ts: f64, attempt: u32, job: &str, image: Option<&str>) -> RunEvent {
    let mut e = task_submitted(ts, "r1", "align", attempt, job);
    e.data = EventData::TaskSubmitted { image: image.map(str::to_string) };
    e
}

#[test]
fn a_task_s_image_is_its_display_attempt_s_in_any_order() {
    let log = vec![
        submitted_with_image(2.0, 1, "job-1", Some("repo/a@sha256:1")),
        batch(
            3.0,
            "job-1",
            EventData::TaskFailed {
                exit_code: 1,
                termination_category: None,
                status_reason: None,
                log_stream: None,
            },
        ),
        submitted_with_image(4.0, 2, "job-2", Some("repo/b@sha256:2")),
        aws_succeeded(5.0, "job-2"),
    ];
    assert_incremental_equals_replay(&log);
    let p = Projection::replay(&log);
    let task = &p.runs["r1"].tasks["align"];
    assert_eq!(task.image(), Some("repo/b@sha256:2"));
    assert_eq!(task.attempts["job-1"].image.as_deref(), Some("repo/a@sha256:1"));
    // A display attempt submitted without one falls back to another attempt's.
    let mut log = log;
    log[2] = submitted_with_image(4.0, 2, "job-2", None);
    let p = Projection::replay(&log);
    assert_eq!(p.runs["r1"].tasks["align"].image(), Some("repo/a@sha256:1"));
    // No image anywhere: none.
    let bare = Projection::replay(&[task_submitted(2.0, "r1", "align", 1, "job-1")]);
    assert_eq!(bare.runs["r1"].tasks["align"].image(), None);
}

#[test]
fn a_losing_failed_terminal_s_stream_never_lingers_in_any_order() {
    // One attempt carries both terminals (success wins). The Failed terminal names a stream, the
    // Succeeded one names none: the attempt's stream is the live one in every arrival order, never
    // the losing terminal's.
    let log = vec![
        task_submitted(2.0, "r1", "align", 1, "job-1"),
        batch(
            3.0,
            "job-1",
            EventData::TaskStarted { started_at: None, log_stream: Some("live".into()) },
        ),
        batch(
            4.0,
            "job-1",
            EventData::TaskFailed {
                exit_code: 1,
                termination_category: None,
                status_reason: None,
                log_stream: Some("failed-terminal".into()),
            },
        ),
        batch(5.0, "job-1", EventData::TaskSucceeded { exit_code: 0, log_stream: None }),
    ];
    for perm in permutations(&log) {
        let p = fold_in_order(&perm);
        let ap = &p.runs["r1"].tasks["align"].attempts["job-1"];
        assert_eq!(ap.state, AttemptState::Succeeded);
        assert_eq!(ap.log_stream.as_deref(), Some("live"), "{perm:?}");
    }
    assert_incremental_equals_replay(&log);
    // A Succeeded terminal that names a stream owns it; with only the Failed terminal, its stream.
    let mut named = log.clone();
    named[3] = aws_succeeded(5.0, "job-1");
    assert_incremental_equals_replay(&named);
    let p = Projection::replay(&named);
    assert_eq!(
        p.runs["r1"].tasks["align"].attempts["job-1"].log_stream.as_deref(),
        Some("job-1/final")
    );
    let p = Projection::replay(&log[..3]);
    assert_eq!(
        p.runs["r1"].tasks["align"].attempts["job-1"].log_stream.as_deref(),
        Some("failed-terminal")
    );
}

#[test]
fn a_failure_with_no_reported_exit_code_projects_none() {
    // The translator writes `-1` when AWS Batch reported no exit code; that is not a real exit.
    let log = vec![
        task_submitted(1.0, "r1", "align", 1, "job-1"),
        batch(
            2.0,
            "job-1",
            EventData::TaskFailed {
                exit_code: -1,
                termination_category: None,
                status_reason: Some("Terminated by user request".into()),
                log_stream: None,
            },
        ),
    ];
    let ap = &Projection::replay(&log).runs["r1"].tasks["align"].attempts["job-1"];
    assert_eq!(ap.state, AttemptState::Failed);
    assert_eq!(ap.exit_code, None);
    let known = Projection::replay(&[log[0].clone(), failed_batch(2.0, "job-1")]);
    assert_eq!(known.runs["r1"].tasks["align"].attempts["job-1"].exit_code, Some(1));
}
