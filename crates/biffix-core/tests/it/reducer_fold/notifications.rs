//! Notifications and who cancelled.

use super::*;

/// `run_anchor`, submitted by `submitter` (labelled `label`).
fn anchor_by(ts: f64, run: &str, submitter: &str, label: Option<&str>) -> RunEvent {
    let mut e = run_anchor(ts, run);
    if let EventData::RunSubmitted(a) = &mut e.data {
        a.submitter = Some(submitter.into());
        a.submitter_label = label.map(Into::into);
    }
    e
}

fn labelled_cancel(ts: f64, run: &str, actor: &str, label: Option<&str>) -> RunEvent {
    control(
        ts,
        run,
        EventData::CancelRequested { actor: actor.into(), actor_label: label.map(Into::into) },
    )
}

#[test]
fn each_terminal_state_notifies_the_submitting_user_once_at_when_it_ended() {
    let failed = || EventData::RunFailed { reason: None, stage: None };
    for (terminal, kind) in [
        (run_level(5.0, "r1", EventData::RunSucceeded {}), RunState::Succeeded),
        (run_level(5.0, "r1", failed()), RunState::Failed),
        (control(5.0, "r1", EventData::RunCancelled {}), RunState::Cancelled),
    ] {
        // A second, later terminal (the coordinator's) does not move the time it ended.
        let later = coord(
            9.0,
            "r1",
            EventData::CoordinatorFailed {
                log_stream: None,
                exit_code: None,
                status_reason: None,
                termination_category: None,
            },
        );
        let log = vec![anchor_by(1.0, "r1", "user:alice", None), terminal, later];
        let p = Projection::replay(&log);
        let n = p.runs["r1"].notification().expect("a terminal run notifies");
        assert_eq!(n.kind, kind);
        assert_eq!(n.user_sub, "alice");
        assert_eq!(n.id, format!("r1:{}", kind.as_str()));
        assert_eq!(n.at_micros, Timestamp::from_epoch_seconds(5.0).as_micros());
        assert_incremental_equals_replay(&log);
    }
}

#[test]
fn a_pre_launch_failure_notifies_from_the_run_preparing_submitter() {
    // A run that waited for its images and failed before launching has no anchor; its
    // `run_preparing` names the submitter.
    let preparing = biffix_core::event::RunPreparing {
        workflow_version_id: "wf1".into(),
        params: serde_json::json!({}),
        input_uris: vec![],
        submitter: Some("user:bob".into()),
        submitter_label: None,
        run_name: None,
        output_prefix: None,
        resumed_from: None,
        images: vec![],
        experiment_id: None,
    };
    let run = RunId::new("r1");
    let log = vec![
        biffix_core::event::run_preparing(&run, Timestamp::from_epoch_seconds(1.0), preparing),
        biffix_core::event::run_failed(
            &run,
            Timestamp::from_epoch_seconds(2.0),
            biffix_core::event::FAILURE_STAGE_IMAGE_PREPARATION,
            "the image did not build",
        ),
    ];
    let n = Projection::replay(&log).runs["r1"].notification().unwrap();
    assert_eq!((n.kind, n.user_sub.as_str()), (RunState::Failed, "bob"));
}

#[test]
fn no_notification_before_the_end_or_for_a_run_no_user_submitted() {
    let running = vec![
        anchor_by(1.0, "r1", "user:alice", None),
        run_level(2.0, "r1", EventData::RunStarted { session_id: None }),
    ];
    assert_eq!(Projection::replay(&running).runs["r1"].notification(), None);
    for submitter in ["watcher:w1", "user:"] {
        let log = vec![
            anchor_by(1.0, "r1", submitter, None),
            run_level(2.0, "r1", EventData::RunSucceeded {}),
        ];
        assert_eq!(Projection::replay(&log).runs["r1"].notification(), None);
    }
    // No anchor at all: nobody to tell.
    let log = vec![run_level(2.0, "r1", EventData::RunSucceeded {})];
    assert_eq!(Projection::replay(&log).runs["r1"].notification(), None);
}

#[test]
fn a_cancel_requested_before_a_failure_but_delivered_after_it_notifies_cancelled() {
    // The fold's terminal state decides the notification, so a cancel that reaches the fold after
    // the failure, but was requested before it, re-keys it: the writer replaces the failed one
    // (projection == fold), in every arrival order.
    let failed = run_level(3.0, "r1", EventData::RunFailed { reason: None, stage: None });
    let log = vec![
        anchor_by(1.0, "r1", "user:alice", None),
        failed,
        labelled_cancel(2.0, "r1", "alice", None),
    ];
    let n = Projection::replay(&log).runs["r1"].notification().unwrap();
    assert_eq!(n.id, "r1:cancelled");
    for order in permutations(&log) {
        assert_eq!(fold_in_order(&order).runs["r1"].notification(), Some(n.clone()));
    }
}

#[test]
fn a_cancel_requested_after_the_run_failed_leaves_the_failed_notification() {
    // The run failed at t=3 on its own; the cancel accepted at t=9 (its FAILED had not reached the
    // projection yet) caused nothing: the notification stays the failure's, naming no one.
    let log = vec![
        anchor_by(1.0, "r1", "user:alice", None),
        run_level(3.0, "r1", EventData::RunFailed { reason: None, stage: None }),
        labelled_cancel(9.0, "r1", "bob", Some("bob@x")),
    ];
    assert_incremental_equals_replay(&log);
    let rp = &Projection::replay(&log).runs["r1"];
    assert_eq!(rp.state(), RunState::Failed);
    assert_eq!(rp.cancelled_by(), None);
    let n = rp.notification().unwrap();
    assert_eq!(n.id, "r1:failed");
    for order in permutations(&log) {
        assert_eq!(fold_in_order(&order).runs["r1"].notification(), Some(n.clone()));
    }
}

#[test]
fn cancelled_by_prefers_the_recorded_label_then_the_submitter_s_never_the_id() {
    let cancelled = |cancel: RunEvent, label: Option<&str>| {
        let log = vec![
            anchor_by(1.0, "r1", "user:alice", label),
            cancel,
            control(3.0, "r1", EventData::RunCancelled {}),
        ];
        assert_incremental_equals_replay(&log);
        Projection::replay(&log).runs["r1"].cancelled_by()
    };
    assert_eq!(
        cancelled(labelled_cancel(2.0, "r1", "bob", Some("bob@lab.example.org")), None).as_deref(),
        Some("bob@lab.example.org")
    );
    // The submitter cancelled their own run: their label from the anchor.
    assert_eq!(
        cancelled(labelled_cancel(2.0, "r1", "alice", None), Some("alice@lab.example.org"))
            .as_deref(),
        Some("alice@lab.example.org")
    );
    // Someone else, with no label recorded: nobody to name (their id is an opaque IdP sub).
    assert_eq!(
        cancelled(labelled_cancel(2.0, "r1", "bob", None), Some("alice@lab.example.org")),
        None
    );
    // The submitter cancelled their own run, but neither the request nor the anchor has a label.
    assert_eq!(cancelled(labelled_cancel(2.0, "r1", "alice", None), None), None);
}

#[test]
fn cancelled_by_is_the_earliest_request_in_any_fold_order() {
    // Two `cancel_requested` events (the log dedups a repeat, but the fold must not depend on it):
    // the earliest by (ts, seq) names who cancelled, whatever order they fold in.
    let anchor = anchor_by(1.0, "r1", "user:alice", None).with_seq(1);
    let ended = control(9.0, "r1", EventData::RunCancelled {}).with_seq(9);
    // The winner is the lexicographically larger request each time, so a smallest-label (or
    // smallest-actor) rule fails both halves.
    // Earlier ts wins, though its seq is the higher.
    let log = vec![
        anchor.clone(),
        labelled_cancel(2.0, "r1", "carol", Some("carol@x")).with_seq(3),
        labelled_cancel(3.0, "r1", "bob", Some("bob@x")).with_seq(2),
        ended.clone(),
    ];
    for order in permutations(&log) {
        assert_eq!(fold_in_order(&order).runs["r1"].cancelled_by().as_deref(), Some("carol@x"));
    }
    // Equal ts: the lower log seq wins.
    let log = vec![
        anchor,
        labelled_cancel(2.0, "r1", "carol", Some("carol@x")).with_seq(4),
        labelled_cancel(2.0, "r1", "bob", Some("bob@x")).with_seq(5),
        ended,
    ];
    for order in permutations(&log) {
        assert_eq!(fold_in_order(&order).runs["r1"].cancelled_by().as_deref(), Some("carol@x"));
    }
}

#[test]
fn cancelled_by_is_only_said_of_a_run_that_ended_cancelled() {
    let base = || {
        vec![
            anchor_by(1.0, "r1", "user:alice", None),
            labelled_cancel(2.0, "r1", "bob", Some("bob@lab.example.org")),
        ]
    };
    // Still running: not cancelled (yet).
    assert_eq!(Projection::replay(&base()).runs["r1"].cancelled_by(), None);
    // It succeeded anyway: the request cancelled nothing.
    let mut log = base();
    log.push(run_level(3.0, "r1", EventData::RunSucceeded {}));
    assert_eq!(Projection::replay(&log).runs["r1"].cancelled_by(), None);
    // An explicit cancellation with no recorded request: nobody to name.
    let log = vec![
        anchor_by(1.0, "r1", "user:alice", None),
        control(3.0, "r1", EventData::RunCancelled {}),
    ];
    assert_eq!(Projection::replay(&log).runs["r1"].cancelled_by(), None);
}
