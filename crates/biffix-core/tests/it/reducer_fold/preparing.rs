//! Submit-time images: `run_preparing`.

use super::*;

fn coordinator_binding(ts: f64, run: &str, job: &str) -> RunEvent {
    control(ts, run, EventData::CoordinatorSubmitted {}).with_job(job)
}

#[test]
fn preparing_run_launches_then_runs_order_independently() {
    let log = vec![
        run_preparing(1.0, "r1"),
        run_anchor(5.0, "r1"),
        coordinator_binding(5.0, "r1", "coord-1"),
        run_level(6.0, "r1", EventData::RunStarted { session_id: None }),
    ];
    assert_incremental_equals_replay(&log);
    let p = Projection::replay(&log);
    assert_eq!(run_state(&p, "r1"), RunState::Running);
    // Submitted at is when the user submitted; launched at when it launched.
    assert_eq!(p.runs["r1"].submitted_at_micros, Some(1_000_000));
    assert_eq!(p.runs["r1"].launched_at_micros, Some(5_000_000));
    // Whichever order the log arrives in (folded as it arrives: `replay` would re-sort it).
    let mut reversed = log.clone();
    reversed.reverse();
    let r = fold_in_order(&reversed);
    assert_eq!(r.runs["r1"].submitted_at_micros, Some(1_000_000));
    assert_eq!(r.runs["r1"].launched_at_micros, Some(5_000_000));
    // The anchor's facts win over the provisional ones.
    assert_eq!(p.runs["r1"].workflow_version_id.as_deref(), Some("wf1"));
    assert_eq!(p.runs["r1"].run_name, None);
}

#[test]
fn preparing_images_holds_only_until_launch() {
    let p = Projection::replay(&[run_preparing(1.0, "r1")]);
    assert_eq!(run_state(&p, "r1"), RunState::PreparingImages);
    let run = &p.runs["r1"];
    assert_eq!(run.submitted_at_micros, Some(1_000_000));
    assert_eq!(run.launched_at_micros, None);
    assert_eq!(run.workflow_version_id.as_deref(), Some("wf-prep"));
    assert_eq!(run.run_name.as_deref(), Some("prep"));
    assert_eq!(run.output_prefix.as_deref(), Some("s3://b/out/"));
    assert_eq!(run.params, serde_json::json!({"k": "v"}));

    // The anchor alone ends it, and so does a coordinator lifecycle event alone.
    let anchored = [run_preparing(1.0, "r1"), run_anchor(2.0, "r1")];
    assert_incremental_equals_replay(&anchored);
    assert_eq!(run_state(&Projection::replay(&anchored), "r1"), RunState::Submitted);
    let bound = [run_preparing(1.0, "r1"), coordinator_binding(2.0, "r1", "coord-1")];
    assert_incremental_equals_replay(&bound);
    let p = Projection::replay(&bound);
    assert_eq!(run_state(&p, "r1"), RunState::Submitted);
    assert_eq!(p.runs["r1"].launched_at_micros, Some(2_000_000));

    // A run without a run_preparing never shows it, and launched when it was submitted.
    let plain = Projection::replay(&[run_anchor(1.0, "r1")]);
    assert_eq!(run_state(&plain, "r1"), RunState::Submitted);
    assert_eq!(plain.runs["r1"].launched_at_micros, Some(1_000_000));
    assert_eq!(plain.runs["r1"].submitted_at_micros, Some(1_000_000));
}

#[test]
fn preparing_run_cancelled_or_failed_is_terminal() {
    let cancelled = vec![run_preparing(1.0, "r1"), control(2.0, "r1", EventData::RunCancelled {})];
    assert_incremental_equals_replay(&cancelled);
    assert_eq!(run_state(&Projection::replay(&cancelled), "r1"), RunState::Cancelled);

    let failed = vec![
        run_preparing(1.0, "r1"),
        biffix_core::event::run_failed(
            &RunId::new("r1"),
            Timestamp::from_epoch_seconds(2.0),
            biffix_core::event::FAILURE_STAGE_IMAGE_PREPARATION,
            "image `repo:tag` failed to import",
        ),
    ];
    assert_incremental_equals_replay(&failed);
    let p = Projection::replay(&failed);
    assert_eq!(run_state(&p, "r1"), RunState::Failed);
    assert_eq!(p.runs["r1"].completed_at_micros, Some(2_000_000));
}

#[test]
fn run_preparing_from_an_engine_does_not_fold() {
    let mut planted = run_preparing(1.0, "r1");
    planted.source = EventSource::Snakemake;
    assert!(!biffix_core::reducer::is_trusted_producer(&planted));
    assert!(biffix_core::reducer::is_trusted_producer(&run_preparing(1.0, "r1")));
    // And the fold honours it: alone, the engine's row creates no run ...
    assert!(!Projection::replay(std::slice::from_ref(&planted)).runs.contains_key("r1"));
    // ... and beside the anchor, none of its provisional facts land (an earlier submit time, its
    // name, its workflow version, or the preparing state).
    let log = [run_anchor(2.0, "r1"), planted];
    assert_incremental_equals_replay(&log);
    let p = Projection::replay(&log);
    let run = &p.runs["r1"];
    assert_eq!(run.submitted_at_micros, Some(2_000_000));
    assert_eq!(run.run_name, None);
    assert_eq!(run.workflow_version_id.as_deref(), Some("wf1"));
    assert_eq!(run.state(), RunState::Submitted);
}
