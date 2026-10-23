//! The golden happy path, the core invariant (incremental == replay under every permutation), and
//! order-independent merges of conflicting values.

use super::*;

fn happy_log() -> Vec<RunEvent> {
    vec![
        run_anchor(1.0, "r1"),
        run_level(2.0, "r1", EventData::RunStarted { session_id: None }),
        task_submitted(3.0, "r1", "align:sample=A", 1, "job-1"),
        batch(4.0, "job-1", EventData::TaskQueued {}),
        batch(
            5.0,
            "job-1",
            EventData::TaskStarted { started_at: None, log_stream: Some("ls".into()) },
        ),
        batch(6.0, "job-1", EventData::TaskSucceeded { exit_code: 0, log_stream: None }),
        coord(7.0, "r1", EventData::CoordinatorSucceeded { log_stream: None }),
    ]
}

/// `replay` folds in `(ts, seq)` order, so an equal-`ts` tie breaks on the log sequence — not on
/// the order the caller happened to pass the slice. Probed through a fact whose
/// outcome depends on apply order: two runs claim one Batch job at the same `ts`, and the claim
/// that folds first owns the job's AWS facts. The lower-`seq` claim must win for either slice
/// order; a
/// `ts`-only sort would follow the slice order, and an inverted `seq` sort would pick the other.
#[test]
fn replay_breaks_ts_ties_by_seq_not_input_order() {
    let claim = |seq: i64, run: &str| task_submitted(2.0, run, "align", 1, "job-1").with_seq(seq);
    let done = batch(3.0, "job-1", EventData::TaskSucceeded { exit_code: 0, log_stream: None })
        .with_seq(3);
    let (lower, higher) = (claim(1, "r-low"), claim(2, "r-high"));

    for slice in [
        vec![lower.clone(), higher.clone(), done.clone()],
        vec![higher.clone(), lower.clone(), done.clone()],
    ] {
        let p = Projection::replay(&slice);
        let state = |run: &str| p.runs[run].tasks["align"].attempts["job-1"].state;
        assert_eq!(
            (state("r-low"), state("r-high")),
            (AttemptState::Succeeded, AttemptState::Submitted),
            "equal-ts tie must break on seq (lower seq folds first), independent of slice order",
        );
    }
}

#[test]
fn golden_happy_path() {
    let p = Projection::replay(&happy_log());
    assert_eq!(run_state(&p, "r1"), RunState::Succeeded);
    assert_eq!(task_state(&p, "r1", "align:sample=A"), TaskState::Succeeded);
    let ap = &p.runs["r1"].tasks["align:sample=A"].attempts["job-1"];
    assert_eq!(ap.exit_code, Some(0));
    assert_eq!(ap.log_stream.as_deref(), Some("ls"));
}

/// A task whose two attempts both fail with no further retry ⇒ Task FAILED, Run FAILED.
fn retry_to_failure_log() -> Vec<RunEvent> {
    let fail = || EventData::TaskFailed {
        exit_code: 1,
        termination_category: None,
        status_reason: None,
        log_stream: None,
    };
    vec![
        run_anchor(1.0, "r1"),
        task_submitted(2.0, "r1", "align:sample=A", 1, "job-1"),
        batch(3.0, "job-1", fail()),
        run_level(
            4.0,
            "r1",
            EventData::TaskRetrying { prev_attempt: Attempt(1), next_attempt: Attempt(2) },
        )
        .with_task("align:sample=A"),
        task_submitted(5.0, "r1", "align:sample=A", 2, "job-2"),
        batch(6.0, "job-2", fail()),
        coord(
            7.0,
            "r1",
            EventData::CoordinatorFailed {
                log_stream: None,
                exit_code: None,
                status_reason: None,
                termination_category: None,
            },
        ),
    ]
}

fn cancel_log() -> Vec<RunEvent> {
    vec![
        run_anchor(1.0, "r1"),
        run_level(2.0, "r1", EventData::RunStarted { session_id: None }),
        cancel(3.0, "r1", "operator"),
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
    ]
}

fn progress_log() -> Vec<RunEvent> {
    vec![
        run_anchor(1.0, "r1"),
        run_level(2.0, "r1", EventData::RunStarted { session_id: None }),
        run_level(3.0, "r1", EventData::RunProgress { completed: 2, total: 10 }),
        run_level(4.0, "r1", EventData::RunProgress { completed: 5, total: 10 }),
        run_level(5.0, "r1", EventData::RunProgress { completed: 8, total: 10 }),
    ]
}

#[test]
fn incremental_equals_replay_happy_path() {
    assert_incremental_equals_replay(&happy_log());
}

#[test]
fn incremental_equals_replay_retry_to_failure() {
    // Exercises the retry-pending path: under out-of-order delivery the old reducer showed this
    // task RUNNING forever; it must converge to FAILED regardless of ordering.
    assert_incremental_equals_replay(&retry_to_failure_log());
    let p = Projection::replay(&retry_to_failure_log());
    assert_eq!(task_state(&p, "r1", "align:sample=A"), TaskState::Failed);
    assert_eq!(run_state(&p, "r1"), RunState::Failed);
}

#[test]
fn incremental_equals_replay_cancel() {
    // The cancel-vs-fail classification must be a property of the event set, not arrival order:
    // a cancel delivered after coordinator_failed must still yield CANCELLED, not FAILED.
    assert_incremental_equals_replay(&cancel_log());
    let p = Projection::replay(&cancel_log());
    assert_eq!(run_state(&p, "r1"), RunState::Cancelled);
}

#[test]
fn incremental_equals_replay_progress_never_regresses() {
    // A stale/out-of-order progress event must not move the bar backward.
    assert_incremental_equals_replay(&progress_log());
    let p = Projection::replay(&progress_log());
    assert_eq!(p.runs["r1"].completed, Some(8));
    assert_eq!(p.runs["r1"].total, Some(10));
}

#[test]
fn duplicate_and_reordered_retry_is_idempotent() {
    // Duplicate a stale `task_submitted` for the first (failed) attempt AFTER the retry — the old
    // reducer flipped the task back to FAILED. It must stay consistent with replay.
    let mut log = retry_to_failure_log();
    log.push(task_submitted(2.5, "r1", "align:sample=A", 1, "job-1")); // stale dup of attempt 1
    let replayed = Projection::replay(&log);
    let mut inc = Projection::new();
    for e in &log {
        inc.apply(e);
    }
    assert_eq!(inc.runs, replayed.runs);
    assert_eq!(task_state(&inc, "r1", "align:sample=A"), TaskState::Failed);
}

#[test]
fn duplicates_are_idempotent() {
    let mut log = happy_log();
    let dup = log.clone();
    log.extend(dup); // every event twice
    let p = Projection::replay(&log);
    assert_eq!(run_state(&p, "r1"), RunState::Succeeded);
    // The whole projection, not a few fields: no duplicate inflates anything.
    assert_eq!(p.runs, Projection::replay(&happy_log()).runs);
}

mod property {
    use super::*;
    use proptest::prelude::*;

    /// A representative log exercising the hard paths in one fold: the run anchor + a monotonic
    /// progress progression, a task that succeeds on Batch after a Batch-internal retry (two
    /// starts with different streams and hosts; its terminals arrive with a `batch_job_id` but no
    /// `run_id`, so a late `task_submitted` must still stitch them), a task
    /// whose one job carries BOTH a Failed and a Succeeded terminal (the success-wins collision),
    /// and a coordinator terminal. Larger than the exhaustive-permutation logs.
    fn representative_log() -> Vec<RunEvent> {
        vec![
            run_anchor(1.0, "r1"),
            run_level(2.0, "r1", EventData::RunStarted { session_id: None }),
            run_level(2.4, "r1", EventData::RunProgress { completed: 1, total: 3 }),
            run_level(2.6, "r1", EventData::RunProgress { completed: 2, total: 3 }),
            task_submitted(3.0, "r1", "t0", 1, "j0"),
            batch(3.4, "j0", EventData::TaskQueued {}),
            on_host(
                batch(
                    3.6,
                    "j0",
                    EventData::TaskStarted { started_at: None, log_stream: Some("ls".into()) },
                ),
                "ci-1",
            ),
            // A Batch retry inside the job (RUNNING:1): a new container on another host.
            on_host(
                batch(
                    3.8,
                    "j0",
                    EventData::TaskStarted {
                        started_at: None,
                        log_stream: Some("ls-retry".into()),
                    },
                ),
                "ci-2",
            ),
            batch(4.0, "j0", EventData::TaskSucceeded { exit_code: 0, log_stream: None }),
            task_submitted(4.2, "r1", "t1", 1, "j1"),
            batch(
                4.6,
                "j1",
                EventData::TaskFailed {
                    exit_code: 137,
                    termination_category: Some(TerminationCategory::Spot),
                    status_reason: None,
                    log_stream: None,
                },
            ),
            batch(4.8, "j1", EventData::TaskSucceeded { exit_code: 0, log_stream: None }),
            // A task the engine failed although its job succeeded (an engine verdict), with the
            // job's image on its submit.
            {
                let mut e = task_submitted(5.0, "r1", "t2", 1, "j2");
                e.data = EventData::TaskSubmitted { image: Some("repo/x@sha256:2".into()) };
                e
            },
            aws_succeeded(5.2, "j2"),
            verdict(5.4, "r1", "t2", Some(1), TaskVerdict::Failed),
            coord(6.0, "r1", EventData::CoordinatorSucceeded { log_stream: None }),
        ]
    }

    /// Arbitrary arrival: a permutation of every base index (so every fact is delivered at least
    /// once) plus `0..=n` duplicate deliveries, all shuffled — arbitrary order AND redelivery in
    /// one strategy.
    fn arrivals(n: usize) -> impl Strategy<Value = Vec<usize>> {
        (Just((0..n).collect::<Vec<usize>>()), prop::collection::vec(0..n, 0..=n))
            .prop_map(|(base, dups)| {
                let mut all = base;
                all.extend(dups);
                all
            })
            .prop_shuffle()
    }

    proptest! {
        /// The fold invariant at larger N: for ANY arrival order of the representative log — with
        /// arbitrary duplicate redeliveries — the incremental fold equals `replay` of the base log.
        /// This is the safety net for the incremental fold.
        #[test]
        fn incremental_equals_replay_under_arbitrary_arrival(
            order in arrivals(representative_log().len())
        ) {
            let base = representative_log();
            let replayed = Projection::replay(&base);
            let mut inc = Projection::new();
            for &i in &order {
                inc.apply(&base[i]);
            }
            prop_assert_eq!(inc.runs, replayed.runs);
        }
    }
}

#[test]
fn conflicting_output_path_etag_and_size_merge_the_same_in_any_order() {
    // Three reports of one object that disagree on every field: whatever order they fold in, the
    // path is the min, the etag and the size the max. Each winner is on a report that is neither
    // the first nor the last, so no first- or latest-wins rule picks it.
    let log = vec![
        output_ref_event(2.0, "b/x.txt", "etag-2", 10),
        output_ref_event(3.0, "a/x.txt", "etag-3", 30),
        output_ref_event(4.0, "c/x.txt", "etag-1", 20),
    ];
    for order in permutations(&log) {
        let p = fold_in_order(&order);
        let o = &p.runs["r1"].outputs["s3://b/x.txt"];
        assert_eq!(o.path.as_deref(), Some("a/x.txt"));
        assert_eq!(o.etag.as_deref(), Some("etag-3"));
        assert_eq!(o.size, Some(30));
    }
    assert_incremental_equals_replay(&log);
}

#[test]
fn a_conflicting_task_group_keeps_the_lexicographic_min_in_any_order() {
    // An attempt-less `task_created` has one translator key per task, so a real log keeps only the
    // first; the hand-set keys probe the fold's merge rule should conflicting ones reach it.
    let created = |ts: f64, group: &str| {
        let mut e =
            run_level(ts, "r1", EventData::TaskCreated { group: Some(group.into()), deps: vec![] })
                .with_task("t");
        e.dedup_key = format!("created-{group}");
        e
    };
    let log = vec![created(2.0, "b"), created(3.0, "a"), created(4.0, "c")];
    for order in permutations(&log) {
        let p = fold_in_order(&order);
        assert_eq!(p.runs["r1"].tasks["t"].group.as_deref(), Some("a"));
    }
}
