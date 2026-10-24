//! What a cancelled run's tasks read: a task the cancel ended reads CANCELLED, whether or not the
//! engine's last events (refused once the cancel revoked the run's token) reached the log.

use super::*;

fn coordinator_failed(ts: f64, run: &str) -> RunEvent {
    coord(
        ts,
        run,
        EventData::CoordinatorFailed {
            log_stream: None,
            exit_code: None,
            status_reason: None,
            termination_category: None,
        },
    )
}

fn aws_failed(ts: f64, job: &str) -> RunEvent {
    batch(
        ts,
        job,
        EventData::TaskFailed {
            exit_code: 143,
            termination_category: None,
            status_reason: Some("Terminated by biffix: run cancelled".into()),
            log_stream: None,
        },
    )
}

fn started(ts: f64, job: &str) -> RunEvent {
    batch(ts, job, EventData::TaskStarted { started_at: None, log_stream: None })
}

fn engine_failed(ts: f64, run: &str, task: &str) -> RunEvent {
    run_level(
        ts,
        run,
        EventData::TaskFailed {
            exit_code: -1,
            termination_category: None,
            status_reason: None,
            log_stream: None,
        },
    )
    .with_task(task)
}

/// `task`'s effective state in `run`, as the projection persists it.
fn effective(p: &Projection, run: &str, task: &str) -> TaskState {
    let rp = &p.runs[run];
    rp.effective_task_state(&rp.tasks[task])
}

#[test]
fn a_cancelled_runs_open_tasks_read_cancelled_and_finished_ones_keep_their_outcome() {
    let log = vec![
        run_anchor(1.0, "r1"),
        // Running when the cancel came.
        task_submitted(2.0, "r1", "running", 1, "job-run"),
        started(2.5, "job-run"),
        // Still queued (never started) when the cancel came.
        task_submitted(2.1, "r1", "queued", 1, "job-q"),
        // Succeeded before the cancel.
        task_submitted(2.2, "r1", "done", 1, "job-done"),
        aws_succeeded(3.0, "job-done"),
        // Failed on its own, well before the cancel.
        task_submitted(2.3, "r1", "broke", 1, "job-broke"),
        aws_failed(3.5, "job-broke"),
        cancel(10.0, "r1", "alice"),
        coordinator_failed(11.0, "r1"),
    ];
    let p = Projection::replay(&log);
    assert_eq!(p.runs, fold_in_order(&log).runs, "incremental equals replay");
    let rp = &p.runs["r1"];
    assert_eq!(rp.state(), RunState::Cancelled);

    assert_eq!(task_state(&p, "r1", "running"), TaskState::Running, "its own state is untouched");
    assert_eq!(effective(&p, "r1", "running"), TaskState::Cancelled);
    assert_eq!(effective(&p, "r1", "queued"), TaskState::Cancelled);
    assert_eq!(effective(&p, "r1", "done"), TaskState::Succeeded, "a success is never overwritten");
    assert_eq!(effective(&p, "r1", "broke"), TaskState::Failed, "it failed before the cancel");
    // A cancelled task already says why it has no result, so none is "stranded".
    assert!(rp.tasks.values().all(|t| !rp.is_stranded(t)));

    let counts = rp.task_counts();
    assert_eq!(
        (counts.observed, counts.succeeded, counts.failed, counts.cancelled),
        (4, 1, 1, 2),
        "the two open tasks count as cancelled"
    );
    assert_eq!((counts.running, counts.queued), (0, 0));
}

#[test]
fn a_task_that_failed_once_the_cancel_was_requested_reads_cancelled() {
    // The cancel terminated the task's job: Batch stops it after the request (at 6.0), or within
    // the clock-skew allowance before it, as the request is stamped by the backend's clock. A
    // failure that came earlier was the task's own.
    for (failed_at, want) in [
        (6.0, TaskState::Cancelled),
        (5.0, TaskState::Cancelled),
        (4.5, TaskState::Cancelled),
        (3.0, TaskState::Failed),
    ] {
        let log = vec![
            run_anchor(1.0, "r1"),
            task_submitted(2.0, "r1", "align", 1, "job-1"),
            started(2.5, "job-1"),
            cancel(5.0, "r1", "alice"),
            aws_failed(failed_at, "job-1"),
            coordinator_failed(7.0, "r1"),
        ];
        assert_incremental_equals_replay(&log);
        let p = Projection::replay(&log);
        assert_eq!(run_state(&p, "r1"), RunState::Cancelled);
        assert_eq!(task_state(&p, "r1", "align"), TaskState::Failed);
        assert_eq!(effective(&p, "r1", "align"), want, "task failed at {failed_at}");
    }
}

#[test]
fn a_local_rule_that_failed_after_the_cancel_reads_cancelled() {
    for (failed_at, want) in [(6.0, TaskState::Cancelled), (3.0, TaskState::Failed)] {
        let log = vec![
            run_anchor(1.0, "r1"),
            task_created(2.0, "r1", "all"),
            engine_failed(failed_at, "r1", "all"),
            cancel(5.0, "r1", "alice"),
            coordinator_failed(7.0, "r1"),
        ];
        assert_incremental_equals_replay(&log);
        let p = Projection::replay(&log);
        assert_eq!(effective(&p, "r1", "all"), want, "local failure at {failed_at}");
    }
}

#[test]
fn a_task_the_engine_rejected_after_the_cancel_reads_cancelled() {
    // The engine's verdict is the failure (its job succeeded in Batch); its time is the task's.
    for (verdict_at, want) in [(6.0, TaskState::Cancelled), (3.5, TaskState::Failed)] {
        let log = vec![
            run_anchor(1.0, "r1"),
            task_submitted(2.0, "r1", "align", 1, "job-1"),
            aws_succeeded(3.0, "job-1"),
            verdict(verdict_at, "r1", "align", Some(1), TaskVerdict::Failed),
            cancel(5.0, "r1", "alice"),
            coordinator_failed(7.0, "r1"),
        ];
        assert_incremental_equals_replay(&log);
        let p = Projection::replay(&log);
        assert_eq!(task_state(&p, "r1", "align"), TaskState::Failed);
        assert_eq!(effective(&p, "r1", "align"), want, "verdict at {verdict_at}");
    }
}

#[test]
fn a_task_between_retries_when_the_cancel_came_reads_cancelled() {
    // Its first attempt failed on its own, and the engine announced a retry the cancel preempted:
    // the task was still in flight, so it did not fail, it was cancelled.
    let log = vec![
        run_anchor(1.0, "r1"),
        task_submitted(2.0, "r1", "align", 1, "job-1"),
        aws_failed(3.0, "job-1"),
        run_level(
            4.0,
            "r1",
            EventData::TaskRetrying { prev_attempt: Attempt(1), next_attempt: Attempt(2) },
        )
        .with_task("align"),
        cancel(5.0, "r1", "alice"),
        coordinator_failed(6.0, "r1"),
    ];
    assert_incremental_equals_replay(&log);
    let p = Projection::replay(&log);
    assert_eq!(task_state(&p, "r1", "align"), TaskState::Running, "a retry is pending");
    assert_eq!(effective(&p, "r1", "align"), TaskState::Cancelled);
}

#[test]
fn whether_the_engines_last_events_arrive_after_the_cancel_does_not_matter() {
    // The cancel revokes the run's token, so the engine's report of the task it was running when
    // the cancel came is usually refused. With or without that late report (and the task's Batch
    // stop), the task reads CANCELLED.
    let base = vec![
        run_anchor(1.0, "r1"),
        task_submitted(2.0, "r1", "align", 1, "job-1"),
        started(2.5, "job-1"),
        cancel(5.0, "r1", "alice"),
        coordinator_failed(6.0, "r1"),
    ];
    let late_engine = engine_failed(6.5, "r1", "align").with_job("job-1");
    let late_batch = aws_failed(6.2, "job-1");
    for extra in
        [vec![], vec![late_engine.clone()], vec![late_batch.clone()], vec![late_batch, late_engine]]
    {
        let mut log = base.clone();
        log.extend(extra);
        assert_incremental_equals_replay(&log);
        let p = Projection::replay(&log);
        assert_eq!(run_state(&p, "r1"), RunState::Cancelled);
        assert_eq!(effective(&p, "r1", "align"), TaskState::Cancelled, "{} events", log.len());
        assert_eq!(p.runs["r1"].task_counts().cancelled, 1);
    }
}

#[test]
fn a_bare_run_cancelled_cancels_open_tasks_but_not_failed_ones() {
    // With no recorded request there is no time to weigh a failure against, so only the tasks the
    // run left open read CANCELLED.
    let log = vec![
        run_anchor(1.0, "r1"),
        task_submitted(2.0, "r1", "open", 1, "job-open"),
        started(2.5, "job-open"),
        task_submitted(2.3, "r1", "broke", 1, "job-broke"),
        aws_failed(4.0, "job-broke"),
        control(5.0, "r1", EventData::RunCancelled {}),
    ];
    let p = Projection::replay(&log);
    assert_eq!(p.runs, fold_in_order(&log).runs, "incremental equals replay");
    assert_eq!(run_state(&p, "r1"), RunState::Cancelled);
    assert_eq!(effective(&p, "r1", "open"), TaskState::Cancelled);
    assert_eq!(effective(&p, "r1", "broke"), TaskState::Failed);
}

#[test]
fn a_cancel_the_run_outlived_cancels_nothing() {
    // The run failed on its own before the cancel was requested: it is FAILED, so its open task is
    // stranded (FAILED), and a task that failed after the request is the task's own failure.
    let log = vec![
        run_anchor(1.0, "r1"),
        task_submitted(2.0, "r1", "open", 1, "job-open"),
        started(2.5, "job-open"),
        coordinator_failed(4.0, "r1"),
        cancel(7.0, "r1", "alice"),
    ];
    assert_incremental_equals_replay(&log);
    let p = Projection::replay(&log);
    let rp = &p.runs["r1"];
    assert_eq!(rp.state(), RunState::Failed);
    assert_eq!(effective(&p, "r1", "open"), TaskState::Failed);
    assert!(rp.is_stranded(&rp.tasks["open"]));
    assert_eq!(rp.task_counts().cancelled, 0);
}
