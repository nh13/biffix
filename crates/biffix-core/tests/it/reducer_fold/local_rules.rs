//! Local (engine-terminal) rules: no Batch job, so the engine is the terminal source, including the
//! ones the engine retries.

use super::*;

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

#[test]
fn local_rule_only_created_is_running() {
    // A local aggregation target (e.g. `all`) that has been created but not finished is Running —
    // it has no Batch attempt to reduce over, and no engine terminal yet.
    let p = Projection::replay(&[run_anchor(1.0, "r1"), task_created(2.0, "r1", "all")]);
    assert_eq!(task_state(&p, "r1", "all"), TaskState::Running);
    assert!(p.runs["r1"].tasks["all"].attempts.is_empty());
}

#[test]
fn local_rule_engine_success_is_succeeded() {
    // The regression: `all` runs locally on the coordinator (never a Batch job), so its terminal
    // comes only from the engine. With zero attempts, the engine terminal governs.
    let p = Projection::replay(&[
        run_anchor(1.0, "r1"),
        task_created(2.0, "r1", "all"),
        engine_succeeded(3.0, "r1", "all"),
    ]);
    assert_eq!(task_state(&p, "r1", "all"), TaskState::Succeeded);
    assert!(p.runs["r1"].tasks["all"].attempts.is_empty());
}

#[test]
fn cached_task_is_succeeded_and_flagged_cached() {
    // A cache-hit folds to a SUCCEEDED terminal (it counts as done) but is flagged `cached`, so
    // cost/runtime views can exclude it — it never became a Batch job. The flag latches
    // monotonically and does not depend on arrival order relative to task_created.
    let events = vec![
        run_anchor(1.0, "r1"),
        task_created(2.0, "r1", "step"),
        engine_cached(3.0, "r1", "step"),
    ];
    let replay = Projection::replay(&events);
    assert_eq!(task_state(&replay, "r1", "step"), TaskState::Succeeded);
    assert!(replay.runs["r1"].tasks["step"].cached);
    assert!(replay.runs["r1"].tasks["step"].attempts.is_empty());

    // Order-independence + idempotency: the cached flag survives any permutation + a duplicate.
    for perm in permutations(&events) {
        let mut inc = Projection::new();
        for e in &perm {
            inc.apply(e);
        }
        inc.apply(&engine_cached(3.0, "r1", "step")); // duplicate
        assert_eq!(task_state(&inc, "r1", "step"), TaskState::Succeeded);
        assert!(inc.runs["r1"].tasks["step"].cached);
    }
}

#[test]
fn run_dag_and_group_fold_as_sets() {
    use biffix_core::event::{Dag, DagEdge, DagNode};
    let dag = |ns: &[&str], es: &[(&str, &str)]| {
        run_level(
            2.0,
            "r1",
            EventData::RunDag {
                dag: Dag {
                    nodes: ns.iter().map(|n| DagNode { id: n.to_string() }).collect(),
                    edges: es
                        .iter()
                        .map(|(f, t)| DagEdge { from: f.to_string(), to: t.to_string() })
                        .collect(),
                },
            },
        )
    };
    let created = |task: &str, group: &str| {
        run_level(
            3.0,
            "r1",
            EventData::TaskCreated { group: Some(group.to_string()), deps: vec![] },
        )
        .with_task(task)
    };
    // Two DAG events (overlapping + out of order) union; a task carries its group.
    let events = vec![
        run_anchor(1.0, "r1"),
        dag(&["align", "sort"], &[("align", "sort")]),
        created("sort:sample=A", "sort"),
        dag(&["sort", "index"], &[("sort", "index")]),
    ];
    let p = Projection::replay(&events);
    let rp = &p.runs["r1"];
    let want_nodes: std::collections::BTreeSet<String> =
        ["align", "index", "sort"].iter().map(std::string::ToString::to_string).collect();
    assert_eq!(rp.dag_nodes, want_nodes);
    let want_edges: std::collections::BTreeSet<(String, String)> =
        [("align", "sort"), ("sort", "index")]
            .iter()
            .map(|(from, to)| (from.to_string(), to.to_string()))
            .collect();
    assert_eq!(rp.dag_edges, want_edges);
    assert_incremental_equals_replay(&events);
    assert_eq!(rp.tasks["sort:sample=A"].group.as_deref(), Some("sort"));
}

#[test]
fn local_rule_engine_failure_is_failed() {
    let p = Projection::replay(&[
        run_anchor(1.0, "r1"),
        task_created(2.0, "r1", "all"),
        engine_failed(3.0, "r1", "all"),
    ]);
    assert_eq!(task_state(&p, "r1", "all"), TaskState::Failed);
}

#[test]
fn engine_terminal_is_order_independent_and_success_latches() {
    // Monotonicity/idempotency: success outranks a stray failure regardless of arrival
    // order — even one reported later, so this is not a later-ts-wins rule — and incremental ==
    // replay under every permutation + duplication.
    let base = vec![
        run_anchor(1.0, "r1"),
        task_created(2.0, "r1", "all"),
        engine_succeeded(3.0, "r1", "all"),
        engine_failed(4.0, "r1", "all"),
    ];
    let replay = Projection::replay(&base);
    assert_eq!(task_state(&replay, "r1", "all"), TaskState::Succeeded);

    for perm in permutations(&base) {
        // incremental fold in this (possibly reordered) order, plus a duplicate of the success
        let mut inc = Projection::new();
        for e in &perm {
            inc.apply(e);
        }
        inc.apply(&engine_succeeded(3.0, "r1", "all")); // duplicate
        assert_eq!(task_state(&inc, "r1", "all"), TaskState::Succeeded);
        assert_eq!(inc.runs, replay.runs);
    }
}

#[test]
fn engine_terminal_does_not_override_a_batch_attempt() {
    // A task that DID run on Batch keeps AWS as authoritative: a stray engine terminal must not
    // flip a still-running Batch attempt (the engine terminal applies only with zero attempts).
    let p = Projection::replay(&[
        run_anchor(1.0, "r1"),
        task_submitted(2.0, "r1", "align:sample=A", 1, "job-1"),
        batch(3.0, "job-1", EventData::TaskStarted { started_at: None, log_stream: None }),
        engine_succeeded(4.0, "r1", "align:sample=A"), // stray engine terminal, no batch_job_id
    ]);
    // The Batch attempt is still Running; the engine terminal is ignored because attempts exist.
    assert_eq!(task_state(&p, "r1", "align:sample=A"), TaskState::Running);
}

#[test]
fn a_local_rule_being_retried_reads_running_not_failed() {
    let log = vec![
        run_anchor(1.0, "r1"),
        task_created(2.0, "r1", "align"),
        engine_failed(3.0, "r1", "align"),
        retrying(4.0, 2),
    ];
    let p = Projection::replay(&log);
    assert_eq!(task_state(&p, "r1", "align"), TaskState::Running);
    assert_eq!(p.runs["r1"].task_counts().failed, 0);
    assert_incremental_equals_replay(&log);
}

#[test]
fn a_local_rule_whose_retry_fails_reads_failed() {
    // The retry's own failure (reported after the announcement) ends the task. It carries no
    // attempt, which is the point: the timestamp alone must answer the retry. The key is set by
    // hand because the translator gives two attempt-less `task_failed`s of one task the same key,
    // so a real log keeps only the first; this probes the fold's rule should both reach it (an
    // engine that numbers its terminals is the next test).
    let mut retry_failed = engine_failed(5.0, "r1", "align");
    retry_failed.dedup_key = "local-failed-2".into();
    let log = vec![
        run_anchor(1.0, "r1"),
        task_created(2.0, "r1", "align"),
        engine_failed(3.0, "r1", "align"),
        retrying(4.0, 2),
        retry_failed,
    ];
    assert_eq!(task_state(&Projection::replay(&log), "r1", "align"), TaskState::Failed);
    assert_incremental_equals_replay(&log);
}

#[test]
fn a_local_failure_numbered_at_or_past_the_retry_reads_failed() {
    // An engine that numbers its local terminals: a failure numbered at (2) or past (3) the retry
    // answers it whatever its timestamp.
    for n in [2, 3] {
        let mut retry_failed = engine_failed(3.5, "r1", "align");
        retry_failed.attempt = Some(Attempt(n));
        retry_failed.dedup_key = format!("local-failed-{n}");
        let log = vec![
            task_created(2.0, "r1", "align"),
            engine_failed(3.0, "r1", "align"),
            retry_failed,
            retrying(4.0, 2),
        ];
        assert_eq!(
            task_state(&Projection::replay(&log), "r1", "align"),
            TaskState::Failed,
            "attempt {n}"
        );
        assert_incremental_equals_replay(&log);
    }
}
