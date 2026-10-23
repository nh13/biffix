//! Experiments and run renames.

use super::*;

/// The anchor of `run`, launched into `experiment`.
fn anchor_in(ts: f64, run: &str, experiment: Option<&str>) -> RunEvent {
    let mut e = run_anchor(ts, run);
    if let EventData::RunSubmitted(a) = &mut e.data {
        a.experiment_id = experiment.map(Into::into);
    }
    e
}

/// `run` put in `experiment` (`None`: taken out) at `ts`.
fn experiment_set(ts: f64, run: &str, experiment: Option<&str>) -> RunEvent {
    control(
        ts,
        run,
        EventData::ExperimentSet {
            experiment_id: experiment.map(Into::into),
            actor: Some("alice".into()),
        },
    )
}

#[test]
fn a_run_is_in_the_experiment_it_was_launched_into_until_a_set_moves_it() {
    let launched = vec![anchor_in(1.0, "r1", Some("exp-a"))];
    assert_eq!(Projection::replay(&launched).runs["r1"].experiment_id().as_deref(), Some("exp-a"));
    // Moved to B, taken out, then back into A: the latest change wins whatever order they fold in.
    let cases: [(Vec<RunEvent>, Option<&str>); 4] = [
        (
            vec![anchor_in(1.0, "r1", Some("exp-a")), experiment_set(2.0, "r1", Some("exp-b"))],
            Some("exp-b"),
        ),
        (
            vec![
                anchor_in(1.0, "r1", Some("exp-a")),
                experiment_set(2.0, "r1", Some("exp-b")),
                experiment_set(3.0, "r1", None),
            ],
            None,
        ),
        (
            vec![
                anchor_in(1.0, "r1", Some("exp-a")),
                experiment_set(2.0, "r1", Some("exp-b")),
                experiment_set(3.0, "r1", None),
                experiment_set(4.0, "r1", Some("exp-a")),
            ],
            Some("exp-a"),
        ),
        // A run launched into none, then added.
        (vec![anchor_in(1.0, "r1", None), experiment_set(2.0, "r1", Some("exp-c"))], Some("exp-c")),
    ];
    for (log, want) in cases {
        for order in permutations(&log) {
            assert_eq!(
                fold_in_order(&order).runs["r1"].experiment_id().as_deref(),
                want,
                "{order:?}"
            );
        }
        assert_incremental_equals_replay(&log);
    }
}

#[test]
fn an_experiment_set_at_the_same_time_is_ordered_by_log_sequence() {
    // Two different changes stamped with the same microsecond, built as the API builds them: two
    // facts (distinct dedup keys), so both land and the log's sequence orders them.
    let at = Timestamp::from_epoch_seconds(2.0);
    let mut first = biffix_core::event::experiment_set(&RunId::new("r1"), at, Some("exp-z"), "a");
    first.seq = Some(LogSeq(10));
    let mut second = biffix_core::event::experiment_set(&RunId::new("r1"), at, Some("exp-a"), "b");
    second.seq = Some(LogSeq(11));
    assert_ne!(first.dedup_key, second.dedup_key);
    let log = vec![anchor_in(1.0, "r1", None), first, second];
    for order in permutations(&log) {
        assert_eq!(fold_in_order(&order).runs["r1"].experiment_id().as_deref(), Some("exp-a"));
    }
}

#[test]
fn a_waiting_run_is_in_its_experiment_from_its_run_preparing() {
    let mut preparing = run_preparing(1.0, "r1");
    if let EventData::RunPreparing(p) = &mut preparing.data {
        p.experiment_id = Some("exp-a".into());
    }
    assert_eq!(
        Projection::replay(std::slice::from_ref(&preparing)).runs["r1"].experiment_id().as_deref(),
        Some("exp-a")
    );
    // Moved while it waited: the anchor recorded at launch (later) does not move it back.
    let log = vec![
        preparing,
        experiment_set(2.0, "r1", Some("exp-b")),
        anchor_in(3.0, "r1", Some("exp-a")),
    ];
    for order in permutations(&log) {
        assert_eq!(fold_in_order(&order).runs["r1"].experiment_id().as_deref(), Some("exp-b"));
    }
}

/// Rolling-deploy safety: a build from before experiments launches a run a newer build parked, so
/// its anchor carries no `experiment_id`. That is "not known", not "none": the run stays in the
/// experiment its `run_preparing` names, whichever order they fold in; only an `experiment_set`
/// takes it out.
#[test]
fn an_anchor_without_an_experiment_keeps_the_run_preparing_s() {
    let mut preparing = run_preparing(1.0, "r1");
    if let EventData::RunPreparing(p) = &mut preparing.data {
        p.experiment_id = Some("exp-a".into());
    }
    let older_anchor = anchor_in(3.0, "r1", None);
    let cases: [(Vec<RunEvent>, Option<&str>); 2] = [
        (vec![preparing.clone(), older_anchor.clone()], Some("exp-a")),
        (vec![preparing, older_anchor, experiment_set(4.0, "r1", None)], None),
    ];
    for (log, want) in cases {
        for order in permutations(&log) {
            assert_eq!(
                fold_in_order(&order).runs["r1"].experiment_id().as_deref(),
                want,
                "{order:?}"
            );
        }
        assert_incremental_equals_replay(&log);
    }
}

#[test]
fn only_the_control_plane_sets_a_run_s_experiment() {
    let mut planted = experiment_set(2.0, "r1", Some("exp-b"));
    planted.source = EventSource::Snakemake;
    let log = vec![anchor_in(1.0, "r1", Some("exp-a")), planted];
    assert_eq!(Projection::replay(&log).runs["r1"].experiment_id().as_deref(), Some("exp-a"));
}

#[test]
fn the_experiment_set_wire_shape_round_trips_and_dedups_per_change() {
    let e = biffix_core::event::experiment_set(
        &RunId::new("r1"),
        Timestamp::from_epoch_seconds(2.0),
        None,
        "alice",
    );
    let json = serde_json::to_value(&e).unwrap();
    assert_eq!(json["kind"], "experiment_set");
    assert!(json["experiment_id"].is_null());
    let back: RunEvent = serde_json::from_value(json).unwrap();
    assert_eq!(back, e);
    // Two changes at different times are two facts; the same change re-sent is one.
    let later = biffix_core::event::experiment_set(
        &RunId::new("r1"),
        Timestamp::from_epoch_seconds(3.0),
        None,
        "alice",
    );
    assert_ne!(e.dedup_key, later.dedup_key);
    let again = biffix_core::event::experiment_set(
        &RunId::new("r1"),
        Timestamp::from_epoch_seconds(2.0),
        None,
        "alice",
    );
    assert_eq!(e.dedup_key, again.dedup_key);
    // A different change at the same time is another fact, a removal included.
    for target in [Some("exp-a"), Some("exp-b")] {
        let other = biffix_core::event::experiment_set(
            &RunId::new("r1"),
            Timestamp::from_epoch_seconds(2.0),
            target,
            "alice",
        );
        assert_ne!(e.dedup_key, other.dedup_key, "{target:?}");
    }
}

/// The anchor of `run`, launched with the name `name`.
fn anchor_named(ts: f64, run: &str, name: Option<&str>) -> RunEvent {
    let mut e = run_anchor(ts, run);
    if let EventData::RunSubmitted(a) = &mut e.data {
        a.run_name = name.map(Into::into);
    }
    e
}

#[test]
fn a_rename_overrides_the_launch_name_and_the_latest_wins_in_any_order() {
    let cases: [(Vec<RunEvent>, Option<&str>); 5] = [
        (vec![anchor_named(1.0, "r1", Some("rnaseq · Oct 1"))], Some("rnaseq · Oct 1")),
        (
            vec![
                anchor_named(1.0, "r1", Some("rnaseq · Oct 1")),
                renamed(2.0, "r1", Some("Liver batch 2")),
            ],
            Some("Liver batch 2"),
        ),
        // Cleared: readers derive a name; the launch name does not come back.
        (
            vec![
                anchor_named(1.0, "r1", Some("rnaseq · Oct 1")),
                renamed(2.0, "r1", Some("Liver batch 2")),
                renamed(3.0, "r1", None),
            ],
            None,
        ),
        // Back to an earlier name: its own fact, and the latest.
        (
            vec![
                anchor_named(1.0, "r1", None),
                renamed(2.0, "r1", Some("A")),
                renamed(3.0, "r1", Some("B")),
                renamed(4.0, "r1", Some("A")),
            ],
            Some("A"),
        ),
        // A run with no name (CLI, watcher), named later.
        (vec![anchor_named(1.0, "r1", None), renamed(2.0, "r1", Some("Named"))], Some("Named")),
    ];
    for (log, want) in cases {
        for order in permutations(&log) {
            assert_eq!(fold_in_order(&order).runs["r1"].run_name.as_deref(), want, "{order:?}");
        }
        assert_incremental_equals_replay(&log);
    }
}

#[test]
fn a_rename_while_waiting_survives_the_later_anchor() {
    // Renamed while it waited for its images: the anchor recorded at launch (later) keeps it.
    let log = vec![
        run_preparing(1.0, "r1"),
        renamed(2.0, "r1", Some("Mine")),
        anchor_named(3.0, "r1", Some("launch name")),
    ];
    for order in permutations(&log) {
        assert_eq!(fold_in_order(&order).runs["r1"].run_name.as_deref(), Some("Mine"));
    }
    assert_incremental_equals_replay(&log);
}

#[test]
fn renames_at_the_same_time_are_ordered_by_log_sequence() {
    let at = Timestamp::from_epoch_seconds(2.0);
    let mut first = biffix_core::event::run_renamed(&RunId::new("r1"), at, Some("Zed"), "a");
    first.seq = Some(LogSeq(10));
    let mut second = biffix_core::event::run_renamed(&RunId::new("r1"), at, Some("Abe"), "b");
    second.seq = Some(LogSeq(11));
    assert_ne!(first.dedup_key, second.dedup_key);
    let log = vec![anchor_named(1.0, "r1", None), first, second];
    for order in permutations(&log) {
        assert_eq!(fold_in_order(&order).runs["r1"].run_name.as_deref(), Some("Abe"));
    }
}

#[test]
fn only_the_control_plane_renames_a_run() {
    let mut planted = renamed(2.0, "r1", Some("forged"));
    planted.source = EventSource::Nextflow;
    assert!(!biffix_core::reducer::is_trusted_producer(&planted));
    let log = vec![anchor_named(1.0, "r1", Some("real")), planted];
    assert_eq!(Projection::replay(&log).runs["r1"].run_name.as_deref(), Some("real"));
}

#[test]
fn the_run_renamed_wire_shape_round_trips_and_dedups_per_rename() {
    let at = Timestamp::from_epoch_seconds(2.0);
    let e = biffix_core::event::run_renamed(&RunId::new("r1"), at, Some("a:b"), "alice");
    let json = serde_json::to_value(&e).unwrap();
    assert_eq!(json["kind"], "run_renamed");
    assert_eq!(json["run_name"], "a:b");
    assert_eq!(json["actor"], "alice");
    let back: RunEvent = serde_json::from_value(json).unwrap();
    assert_eq!(back, e);
    // Unknown-field tolerant, and a bare event (no name, no actor) is a clear.
    let bare: EventData =
        serde_json::from_value(serde_json::json!({"kind": "run_renamed", "future": 1})).unwrap();
    assert_eq!(bare, EventData::RunRenamed { run_name: None, actor: None });
    // The same rename re-sent is one fact; another time or another name is another.
    let again = biffix_core::event::run_renamed(&RunId::new("r1"), at, Some("a:b"), "bob");
    assert_eq!(e.dedup_key, again.dedup_key);
    let later = biffix_core::event::run_renamed(
        &RunId::new("r1"),
        Timestamp::from_epoch_seconds(3.0),
        Some("a:b"),
        "alice",
    );
    assert_ne!(e.dedup_key, later.dedup_key);
    let cleared = biffix_core::event::run_renamed(&RunId::new("r1"), at, None, "alice");
    assert_ne!(e.dedup_key, cleared.dedup_key);
}
