//! The coordinator's log stream and host, and the engine session (Nextflow resume).

use super::*;

#[test]
fn coordinator_log_stream_and_host_latch_order_independently() {
    // The coordinator's stream and host keep the lexicographic min on a conflict. The min is on
    // the earlier, non-terminal event, so a terminal-wins or latest-wins rule would give the other.
    let events = vec![
        run_anchor(1.0, "r1"),
        coord(2.0, "r1", EventData::CoordinatorRunning { log_stream: None }),
        on_host(
            coord(3.0, "r1", EventData::CoordinatorRunning { log_stream: Some("coord/a".into()) }),
            "ci-a",
        ),
        on_host(
            coord(
                4.0,
                "r1",
                EventData::CoordinatorFailed {
                    log_stream: Some("coord/b".into()),
                    exit_code: None,
                    status_reason: None,
                    termination_category: None,
                },
            ),
            "ci-b",
        ),
    ];
    for order in permutations(&events) {
        let p = fold_in_order(&order);
        let rp = &p.runs["r1"];
        assert_eq!(rp.coordinator_log_stream.as_deref(), Some("coord/a"));
        assert_eq!(rp.coordinator_host_id.as_deref(), Some("ci-a"));
    }
}

#[test]
fn a_coordinator_event_without_a_stream_folds_to_no_stream() {
    // A payload with no log_stream (Batch names none before the job starts) deserializes with
    // None, and still folds.
    let bare: EventData =
        serde_json::from_value(serde_json::json!({"kind": "coordinator_running"})).unwrap();
    let p = Projection::replay(&[run_anchor(1.0, "r1"), coord(2.0, "r1", bare)]);
    assert_eq!(run_state(&p, "r1"), RunState::Running);
    assert_eq!(p.runs["r1"].coordinator_log_stream, None);
}

/// A Nextflow-style anchor carrying the session biffix launched with (and, optionally, the run it
/// resumes).
fn run_submitted_with_session(
    ts: f64,
    run: &str,
    session: &str,
    resumed_from: Option<&str>,
) -> RunEvent {
    let mut e = run_anchor(ts, run);
    if let EventData::RunSubmitted(anchor) = &mut e.data {
        anchor.engine_session_id = Some(session.into());
        anchor.resumed_from = resumed_from.map(Into::into);
    }
    e
}

fn run_started_with_session(ts: f64, run: &str, session: Option<&str>) -> RunEvent {
    run_level(ts, run, EventData::RunStarted { session_id: session.map(Into::into) })
}

#[test]
fn engine_session_id_and_resumed_from_latch_from_the_anchor() {
    let log = vec![
        run_submitted_with_session(1.0, "r1", "sess-a", Some("r0")),
        run_started_with_session(2.0, "r1", None),
    ];
    let p = Projection::replay(&log);
    let rp = p.runs.get("r1").unwrap();
    assert_eq!(rp.engine_session_id.as_deref(), Some("sess-a"));
    assert_eq!(rp.resumed_from.as_deref(), Some("r0"));
    // No engine report (Snakemake / older plugin) ⇒ the launched id is the effective one.
    assert_eq!(rp.reported_session_id, None);
    assert_eq!(rp.effective_session_id(), Some("sess-a"));
    assert!(!rp.session_mismatch());
    assert_incremental_equals_replay(&log);
}

#[test]
fn reported_session_id_wins_over_the_anchor_order_independently() {
    // The engine reported a different session than the one biffix launched it with (the NXF_UUID
    // pre-choice did not take): the engine's id is the truth, and the mismatch is flagged — in
    // every arrival order.
    let log = vec![
        run_submitted_with_session(1.0, "r1", "sess-launched", None),
        run_started_with_session(2.0, "r1", Some("sess-reported")),
        coord(3.0, "r1", EventData::CoordinatorSucceeded { log_stream: None }),
    ];
    assert_incremental_equals_replay(&log);
    let p = Projection::replay(&log);
    let rp = p.runs.get("r1").unwrap();
    assert_eq!(rp.effective_session_id(), Some("sess-reported"));
    assert!(rp.session_mismatch());
    assert_eq!(rp.resumed_from, None);
}

#[test]
fn matching_reported_session_is_not_a_mismatch() {
    let log = vec![
        run_submitted_with_session(1.0, "r1", "sess-a", None),
        run_started_with_session(2.0, "r1", Some("sess-a")),
    ];
    let p = Projection::replay(&log);
    let rp = p.runs.get("r1").unwrap();
    assert_eq!(rp.effective_session_id(), Some("sess-a"));
    assert!(!rp.session_mismatch());
}

#[test]
fn task_deps_union_across_events_in_any_order_and_drop_self_edges() {
    let created = |ts: f64, task: &str, deps: &[&str]| {
        run_level(
            ts,
            "r1",
            EventData::TaskCreated {
                group: Some("merge".into()),
                deps: deps.iter().map(std::string::ToString::to_string).collect(),
            },
        )
        .with_task(task)
    };
    // Two task_created events for one task (e.g. a re-emitted retry) with overlapping deps, plus a
    // self-edge a plugin could produce by mistake.
    let log = vec![
        run_anchor(1.0, "r1"),
        created(2.0, "merge", &["align:A", "align:B"]),
        created(3.0, "merge", &["align:B", "align:C", "merge"]),
    ];
    assert_incremental_equals_replay(&log);
    let p = Projection::replay(&log);
    let deps: Vec<&str> = p.runs["r1"].tasks["merge"].deps.iter().map(String::as_str).collect();
    assert_eq!(deps, vec!["align:A", "align:B", "align:C"]);
}
