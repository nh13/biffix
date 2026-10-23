//! Two-stream reconciliation fixtures: the engine stream joined to the AWS stream.

use super::*;

#[test]
fn aws_terminal_before_engine_binding_stitches() {
    // The AWS SUCCEEDED for job-1 arrives BEFORE task_submitted binds it (late attribution).
    let log = vec![
        run_anchor(1.0, "r1"),
        batch(2.0, "job-1", EventData::TaskSucceeded { exit_code: 0, log_stream: None }), // unattributed, pends
        task_submitted(3.0, "r1", "align:sample=A", 1, "job-1"), // binds → flush
        coord(4.0, "r1", EventData::CoordinatorSucceeded { log_stream: None }),
    ];
    // Apply in arrival order (NOT ts-sorted) to exercise incremental pending/stitch.
    let mut p = Projection::new();
    for e in &log {
        p.apply(e);
    }
    assert_eq!(task_state(&p, "r1", "align:sample=A"), TaskState::Succeeded);
    assert_eq!(run_state(&p, "r1"), RunState::Succeeded);
    assert_eq!(p.runs, Projection::replay(&log).runs, "the pended-then-flushed fold equals replay");
}

#[test]
fn spot_reclaim_then_retry_keeps_task_running_until_success() {
    let log = vec![
        run_anchor(1.0, "r1"),
        run_level(2.0, "r1", EventData::RunStarted { session_id: None }),
        task_submitted(3.0, "r1", "align:sample=A", 1, "job-1"),
        batch(4.0, "job-1", EventData::TaskStarted { started_at: None, log_stream: None }),
        batch(
            5.0,
            "job-1",
            EventData::TaskFailed {
                exit_code: 137,
                termination_category: Some(TerminationCategory::Spot),
                status_reason: Some("Host EC2 instance terminated (spot)".into()),
                log_stream: None,
            },
        ),
        // engine retries as a NEW attempt/job under the same task_key
        run_level(
            6.0,
            "r1",
            EventData::TaskRetrying { prev_attempt: Attempt(1), next_attempt: Attempt(2) },
        )
        .with_task("align:sample=A"),
        task_submitted(7.0, "r1", "align:sample=A", 2, "job-2"),
        batch(8.0, "job-2", EventData::TaskSucceeded { exit_code: 0, log_stream: None }),
        coord(9.0, "r1", EventData::CoordinatorSucceeded { log_stream: None }),
    ];
    // Right after the failed first attempt, with no retry announced, the task has failed.
    let failed = fold_in_order(&log[..5]);
    assert_eq!(task_state(&failed, "r1", "align:sample=A"), TaskState::Failed);
    // Once the engine announces the retry, the task must be RUNNING (retry pending), not FAILED —
    // the whole point of the two-level model.
    let mid = fold_in_order(&log[..6]);
    assert_eq!(task_state(&mid, "r1", "align:sample=A"), TaskState::Running);

    let p = Projection::replay(&log);
    assert_eq!(task_state(&p, "r1", "align:sample=A"), TaskState::Succeeded);
    assert_eq!(run_state(&p, "r1"), RunState::Succeeded);
}

#[test]
fn cancel_then_coordinator_fail_is_cancelled_not_failed() {
    let mut cancel = ev(
        3.0,
        EventSource::BiffixControlPlane,
        EventData::CancelRequested { actor: "u".into(), actor_label: None },
    );
    cancel.run_id = Some(RunId::new("r1"));
    let log = vec![
        run_anchor(1.0, "r1"),
        run_level(2.0, "r1", EventData::RunStarted { session_id: None }),
        cancel,
        coord(
            4.0,
            "r1",
            EventData::CoordinatorFailed {
                log_stream: None,
                exit_code: None,
                status_reason: None,
                termination_category: None,
            },
        ),
    ];
    assert_incremental_equals_replay(&log);
    let p = Projection::replay(&log);
    assert_eq!(run_state(&p, "r1"), RunState::Cancelled);
    assert_eq!(p.runs["r1"].cancelled_by(), None, "no label recorded");
}

#[test]
fn a_cancel_requested_after_the_run_failed_on_its_own_leaves_it_failed() {
    // The coordinator failed at t=5; its FAILED had not reached the projection when alice's cancel
    // was accepted at t=7. Her cancel caused nothing, so the run failed, in either arrival order.
    let mut late = cancel(7.0, "r1", "alice");
    late.data = EventData::CancelRequested {
        actor: "alice".into(),
        actor_label: Some("alice@example.org".into()),
    };
    let log = vec![
        run_anchor(1.0, "r1"),
        run_level(2.0, "r1", EventData::RunStarted { session_id: None }),
        coord(
            5.0,
            "r1",
            EventData::CoordinatorFailed {
                log_stream: None,
                exit_code: None,
                status_reason: None,
                termination_category: None,
            },
        ),
        late,
    ];
    assert_incremental_equals_replay(&log);
    let p = Projection::replay(&log);
    assert_eq!(run_state(&p, "r1"), RunState::Failed);
    assert_eq!(p.runs["r1"].cancelled_by(), None);
    // A cancel stamped before the failure (or within the clock-skew allowance after it) caused it.
    for at in [4.0, 5.0, 6.0] {
        let mut log = log.clone();
        log[3] = cancel(at, "r1", "alice");
        assert_incremental_equals_replay(&log);
        assert_eq!(
            run_state(&Projection::replay(&log), "r1"),
            RunState::Cancelled,
            "cancel at {at}"
        );
    }
}

#[test]
fn an_explicit_run_cancelled_wins_over_an_earlier_failure() {
    // A recorded `run_cancelled` says the run was cancelled, whenever its failure was stamped.
    let log = vec![
        run_anchor(1.0, "r1"),
        coord(
            2.0,
            "r1",
            EventData::CoordinatorFailed {
                log_stream: None,
                exit_code: None,
                status_reason: None,
                termination_category: None,
            },
        ),
        cancel(3.0, "r1", "alice"),
        control(3.0, "r1", EventData::RunCancelled {}),
    ];
    assert_incremental_equals_replay(&log);
    assert_eq!(run_state(&Projection::replay(&log), "r1"), RunState::Cancelled);
}

#[test]
fn explicit_run_cancelled_is_cancelled() {
    // The other route to CANCELLED: an explicit `run_cancelled` terminal with no prior
    // cancel_requested and no failure. It is CANCELLED (never FAILED), whatever the arrival order.
    let log = vec![
        run_anchor(1.0, "r1"),
        run_level(2.0, "r1", EventData::RunStarted { session_id: None }),
        control(3.0, "r1", EventData::RunCancelled {}),
    ];
    assert_incremental_equals_replay(&log);
    let p = Projection::replay(&log);
    assert_eq!(run_state(&p, "r1"), RunState::Cancelled);
}

#[test]
fn batch_job_lifecycle_naming_a_job_folds_only_from_aws_batch() {
    // A run-attributed Batch lifecycle fact names the job it writes: from any other producer it
    // could bind a job as the run's coordinator (taking a task's facts away) or overwrite another
    // job's attempt. No producer but the Batch translator emits these shapes, so the fold refuses
    // the rest, as it does the control plane's kinds.
    let log = vec![
        run_anchor(1.0, "r1"),
        task_submitted(2.0, "r1", "align", 1, "job-1"),
        batch(3.0, "job-1", EventData::TaskStarted { started_at: None, log_stream: None }),
        // A coordinator failure for the task's job, from an engine.
        ev(
            4.0,
            EventSource::Snakemake,
            EventData::CoordinatorFailed {
                log_stream: None,
                exit_code: Some(1),
                status_reason: None,
                termination_category: None,
            },
        )
        .with_run("r1")
        .with_job("job-1"),
        // A task terminal naming the job, from an engine.
        ev(
            5.0,
            EventSource::Nextflow,
            EventData::TaskFailed {
                exit_code: 1,
                termination_category: None,
                status_reason: None,
                log_stream: None,
            },
        )
        .with_run("r1")
        .with_task("align")
        .with_job("job-1"),
    ];
    assert_incremental_equals_replay(&log);
    let p = Projection::replay(&log);
    let rp = &p.runs["r1"];
    assert_eq!(rp.coordinator_job_id, None, "no coordinator was bound");
    assert_ne!(rp.state(), RunState::Failed, "no coordinator failed the run");
    assert_eq!(
        rp.tasks["align"].attempts["job-1"].state,
        AttemptState::Running,
        "the task keeps its Batch facts"
    );
    // An engine's own terminal for a local rule (no job) still folds.
    assert!(biffix_core::reducer::is_trusted_producer(&run_level(
        6.0,
        "r1",
        EventData::TaskFailed {
            exit_code: 1,
            termination_category: None,
            status_reason: None,
            log_stream: None,
        }
    )));
}

#[test]
fn a_control_plane_kind_from_an_engine_does_not_fold() {
    // An engine row under a control-plane kind (stored unfolded by an older build that did not know
    // the kind) must not fold as the control plane's fact on replay: no cancel, no cancellation, no
    // anchor, no work deletion.
    for planted in [
        EventData::RunCancelled {},
        EventData::CancelRequested { actor: "engine".into(), actor_label: None },
        EventData::WorkDeleted {},
    ] {
        let log = vec![
            run_anchor(1.0, "r1"),
            run_level(2.0, "r1", EventData::RunStarted { session_id: None }),
            run_level(3.0, "r1", planted.clone()),
            coord(
                4.0,
                "r1",
                EventData::CoordinatorFailed {
                    log_stream: None,
                    exit_code: None,
                    status_reason: None,
                    termination_category: None,
                },
            ),
        ];
        assert_incremental_equals_replay(&log);
        let p = Projection::replay(&log);
        // A latched cancel (or a cancellation) would make the failed coordinator CANCELLED.
        assert_eq!(run_state(&p, "r1"), RunState::Failed, "{planted:?}");
        assert_eq!(p.runs["r1"].work_deleted_at_micros, None, "{planted:?}");
    }
    // Nor does an engine's run_submitted create (or re-anchor) a run.
    let mut anchor = run_anchor(1.0, "r2");
    anchor.source = EventSource::Snakemake;
    assert!(!Projection::replay(&[anchor]).runs.contains_key("r2"));
    // The control plane's own facts still fold.
    assert!(biffix_core::reducer::is_trusted_producer(&control(
        3.0,
        "r1",
        EventData::RunCancelled {}
    )));
    assert!(biffix_core::reducer::is_trusted_producer(&coord(
        3.0,
        "r1",
        EventData::CoordinatorSubmitted {}
    )));
    assert!(!biffix_core::reducer::is_trusted_producer(&run_level(
        3.0,
        "r1",
        EventData::CoordinatorSubmitted {}
    )));
}

#[test]
fn terminal_run_forces_open_tasks_terminal_but_keeps_real_terminals() {
    // A run whose coordinator failed while a child task was still running (its authoritative
    // aws-batch terminal never arrived): the task's own state is Running, but the run is terminal,
    // so its *effective* state is FAILED — no task can be running once the run is over. A
    // task that reached its own terminal keeps it.
    let log = vec![
        run_anchor(1.0, "r1"),
        task_submitted(2.0, "r1", "still-running", 1, "job-open"),
        batch(3.0, "job-open", EventData::TaskStarted { started_at: None, log_stream: None }),
        task_submitted(2.5, "r1", "done", 1, "job-done"),
        batch(3.5, "job-done", EventData::TaskSucceeded { exit_code: 0, log_stream: None }),
        coord(
            4.0,
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
    let rp = &p.runs["r1"];
    assert_eq!(rp.state(), RunState::Failed);

    let open = &rp.tasks["still-running"];
    assert_eq!(open.state(), TaskState::Running, "the task's own state is still Running");
    assert_eq!(
        rp.effective_task_state(open),
        TaskState::Failed,
        "but a terminal run forces an open task's effective state to Failed"
    );
    assert!(rp.is_stranded(open), "an open task on an ended run is stranded");

    let done = &rp.tasks["done"];
    assert_eq!(
        rp.effective_task_state(done),
        TaskState::Succeeded,
        "a task with its own real terminal is untouched (success is never overwritten)"
    );
    assert!(!rp.is_stranded(done), "a task with its own terminal is not stranded");
}

#[test]
fn task_counts_use_effective_state_and_track_cached() {
    // A succeeded task, a cache-hit (also succeeded), and — on a terminal run — a task the log left
    // running (forced to Failed by effective_task_state). Counts must reflect the effective states:
    // observed 3, succeeded 2 (incl. the cache-hit), cached 1, failed 1, running 0.
    let log = vec![
        run_anchor(1.0, "r1"),
        task_submitted(2.0, "r1", "open", 1, "job-open"),
        batch(2.5, "job-open", EventData::TaskStarted { started_at: None, log_stream: None }),
        engine_succeeded(3.0, "r1", "done"),
        engine_cached(3.5, "r1", "cachehit"),
        coord(
            4.0,
            "r1",
            EventData::CoordinatorFailed {
                log_stream: None,
                exit_code: None,
                status_reason: None,
                termination_category: None,
            },
        ),
    ];
    let counts = Projection::replay(&log).runs["r1"].task_counts();
    assert_eq!(counts.observed, 3);
    assert_eq!(counts.succeeded, 2, "done + cachehit");
    assert_eq!(counts.cached, 1);
    assert_eq!(counts.failed, 1, "the open task, forced by the terminal run");
    assert_eq!(counts.running, 0);
}

#[test]
fn attempt_conflicting_terminals_let_success_win_deterministically() {
    // At most one Succeeded and one Failed can reach a single attempt (the AWS dedup key is
    // (job, status)). If both do, success wins and owns the terminal detail — never a success
    // state carrying failure detail — regardless of the order the two are folded.
    let succeeded =
        batch(5.0, "job-1", EventData::TaskSucceeded { exit_code: 0, log_stream: None });
    let failed = batch(
        6.0,
        "job-1",
        EventData::TaskFailed {
            exit_code: 137,
            termination_category: Some(TerminationCategory::Spot),
            status_reason: Some("Host EC2 instance terminated (spot)".into()),
            log_stream: None,
        },
    );
    // Every arrival order: `assert_incremental_equals_replay` permutes the log.
    let log = vec![
        run_anchor(1.0, "r1"),
        task_submitted(2.0, "r1", "align:sample=A", 1, "job-1"),
        succeeded,
        failed,
    ];
    assert_incremental_equals_replay(&log);
    let p = Projection::replay(&log);
    assert_eq!(task_state(&p, "r1", "align:sample=A"), TaskState::Succeeded);
    let attempt = &p.runs["r1"].tasks["align:sample=A"].attempts["job-1"];
    assert_eq!(attempt.state, AttemptState::Succeeded);
    assert_eq!(attempt.exit_code, Some(0), "success owns exit_code");
    assert_eq!(attempt.termination_category, None, "a success terminal has no failure category");
}

#[test]
fn unplanned_coordinator_fail_is_failed() {
    let log = vec![
        run_anchor(1.0, "r1"),
        run_level(2.0, "r1", EventData::RunStarted { session_id: None }),
        coord(
            3.0,
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
}

#[test]
fn attempt_started_at_is_earliest_observed_and_order_independent() {
    // AWS STARTING and RUNNING both translate to TaskStarted, so one attempt sees two. started_at
    // must be the EARLIEST (min), not last-write — else the incremental fold (arrival order) and
    // replay (ts order) disagree. Also value-asserts the attempt enrichment fields.
    let log = vec![
        run_anchor(1.0, "r1"),
        task_submitted(2.0, "r1", "align:sample=A", 1, "job-1"),
        batch(4.0, "job-1", EventData::TaskStarted { started_at: None, log_stream: None }), // STARTING
        batch(
            6.0,
            "job-1",
            EventData::TaskStarted { started_at: None, log_stream: Some("ls".into()) },
        ), // RUNNING
        batch(8.0, "job-1", EventData::TaskSucceeded { exit_code: 0, log_stream: None }),
        coord(9.0, "r1", EventData::CoordinatorSucceeded { log_stream: None }),
    ];
    assert_incremental_equals_replay(&log);
    let p = Projection::replay(&log);
    let ap = &p.runs["r1"].tasks["align:sample=A"].attempts["job-1"];
    assert_eq!(
        ap.started_at_micros,
        Some(4_000_000),
        "earliest TaskStarted (4s), not the later 6s one"
    );
    assert_eq!(ap.finished_at_micros, Some(8_000_000), "the winning terminal's time");
    assert_eq!(
        ap.log_stream.as_deref(),
        Some("ls"),
        "log_stream latched from the event that carried it"
    );
}

#[test]
fn a_batch_retry_s_latest_start_names_the_stream_and_host_in_any_order() {
    // Under a Batch retryStrategy one job runs RUNNING, RUNNING:1, RUNNING:2 (distinct dedup keys,
    // so all three reach the fold), each in a new container, possibly on another host. With no
    // terminal stream yet, the latest start names the attempt's stream and host in every arrival
    // order. The values sort so that the latest is neither the lexicographic min nor max.
    let start = |ts: f64, stream: &str, host: &str| {
        on_host(
            batch(
                ts,
                "job-1",
                EventData::TaskStarted { started_at: None, log_stream: Some(stream.into()) },
            ),
            host,
        )
    };
    let log = vec![
        task_submitted(2.0, "r1", "align", 1, "job-1"),
        start(3.0, "ls-a", "ci-a"),
        start(4.0, "ls-c", "ci-c"),
        start(5.0, "ls-b", "ci-b"),
    ];
    assert_incremental_equals_replay(&log);
    let p = Projection::replay(&log);
    let ap = &p.runs["r1"].tasks["align"].attempts["job-1"];
    assert_eq!(ap.log_stream.as_deref(), Some("ls-b"));
    assert_eq!(ap.instance_id.as_ref().map(biffix_core::ids::InstanceId::as_str), Some("ci-b"));
    assert_eq!(ap.started_at_micros, Some(3_000_000), "the first start");
}

#[test]
fn run_completed_at_is_earliest_terminal_and_order_independent() {
    // A successful run emits BOTH run_succeeded (engine) and coordinator_succeeded (the coordinator
    // Batch job), at different times, and both hit completed_at_micros. It must be the EARLIEST
    // (min), not first-write, so the incremental and replay folds agree.
    let log = vec![
        run_anchor(1.0, "r1"),
        run_level(2.0, "r1", EventData::RunStarted { session_id: None }),
        run_level(5.0, "r1", EventData::RunSucceeded {}), // engine terminal
        coord(9.0, "r1", EventData::CoordinatorSucceeded { log_stream: None }), // coordinator terminal
    ];
    assert_incremental_equals_replay(&log);
    let p = Projection::replay(&log);
    assert_eq!(run_state(&p, "r1"), RunState::Succeeded);
    assert_eq!(
        p.runs["r1"].completed_at_micros,
        Some(5_000_000),
        "earliest terminal (5s), independent of fold order"
    );
}

#[test]
fn progress_total_grows_at_equal_completed_order_independently() {
    // A checkpoint grows `total` while `completed` is unchanged. The fold keeps the larger `total`
    // at equal `completed`, deterministically under any arrival order — the old `>=` guard let
    // `total` be last-write-wins. Both facts now reach the reducer (the dedup key carries `total`).
    let log = vec![
        run_anchor(1.0, "r1"),
        run_level(2.0, "r1", EventData::RunProgress { completed: 5, total: 10 }),
        run_level(3.0, "r1", EventData::RunProgress { completed: 5, total: 20 }),
    ];
    assert_incremental_equals_replay(&log);
    let p = Projection::replay(&log);
    assert_eq!(p.runs["r1"].completed, Some(5));
    assert_eq!(p.runs["r1"].total, Some(20), "the larger total wins at equal completed");
}

#[test]
fn display_attempt_prefers_success_then_highest_attempt() {
    // display_attempt surfaces the attempt whose detail represents the task outcome: the succeeded
    // attempt if any, else the highest-numbered. It has zero coverage otherwise.
    let retry = |ts: f64| {
        run_level(
            ts,
            "r1",
            EventData::TaskRetrying { prev_attempt: Attempt(1), next_attempt: Attempt(2) },
        )
        .with_task("t")
    };
    let fail = || EventData::TaskFailed {
        exit_code: 1,
        termination_category: None,
        status_reason: None,
        log_stream: None,
    };

    // Attempt 1 failed, attempt 2 succeeded ⇒ the succeeded attempt is surfaced.
    let p = Projection::replay(&[
        run_anchor(1.0, "r1"),
        task_submitted(2.0, "r1", "t", 1, "job-1"),
        batch(3.0, "job-1", fail()),
        retry(4.0),
        task_submitted(5.0, "r1", "t", 2, "job-2"),
        batch(6.0, "job-2", EventData::TaskSucceeded { exit_code: 0, log_stream: None }),
    ]);
    let disp = p.runs["r1"].tasks["t"].display_attempt().expect("attempts");
    assert_eq!(disp.batch_job_id.as_str(), "job-2", "the succeeded attempt wins");
    assert_eq!(disp.exit_code, Some(0));

    // Success outranks a higher attempt: attempt 1 (on the lexicographically smaller job id)
    // succeeded, attempt 2 failed ⇒ attempt 1 is surfaced, which neither the attempt number nor the
    // job id would pick.
    let p1 = Projection::replay(&[
        run_anchor(1.0, "r1"),
        task_submitted(2.0, "r1", "t", 1, "job-1"),
        batch(3.0, "job-1", EventData::TaskSucceeded { exit_code: 0, log_stream: None }),
        retry(4.0),
        task_submitted(5.0, "r1", "t", 2, "job-9"),
        batch(6.0, "job-9", fail()),
    ]);
    let disp1 = p1.runs["r1"].tasks["t"].display_attempt().expect("attempts");
    assert_eq!(
        (disp1.batch_job_id.as_str(), disp1.attempt),
        ("job-1", Some(Attempt(1))),
        "the succeeded attempt wins over a higher-numbered failure"
    );

    // No success ⇒ the highest-numbered attempt is surfaced, here on the lexicographically smaller
    // job id, so only the attempt comparison picks it.
    let p2 = Projection::replay(&[
        run_anchor(1.0, "r1"),
        task_submitted(2.0, "r1", "t", 1, "job-z"),
        batch(3.0, "job-z", fail()),
        retry(4.0),
        task_submitted(5.0, "r1", "t", 2, "job-a"),
        batch(
            6.0,
            "job-a",
            EventData::TaskFailed {
                exit_code: 2,
                termination_category: None,
                status_reason: None,
                log_stream: None,
            },
        ),
    ]);
    let disp2 = p2.runs["r1"].tasks["t"].display_attempt().expect("attempts");
    assert_eq!(disp2.attempt, Some(Attempt(2)), "highest attempt when none succeeded");
    assert_eq!(disp2.exit_code, Some(2));

    // A local rule (no Batch attempts) has no display attempt.
    let p3 = Projection::replay(&[
        run_anchor(1.0, "r1"),
        task_created(2.0, "r1", "all"),
        engine_succeeded(3.0, "r1", "all"),
    ]);
    assert!(p3.runs["r1"].tasks["all"].display_attempt().is_none());
}
