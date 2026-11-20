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
            EventData::TaskCreated { group: Some(group.to_string()), deps: vec![], image: None },
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

fn created_with_image(ts: f64, task: &str, image: Option<&str>) -> RunEvent {
    let wire = serde_json::json!({
        "ts": ts, "run_id": "r1", "kind": "task_created", "task_key": task, "image": image,
    });
    biffix_core::translate::from_engine(
        EventSource::Snakemake,
        &serde_json::from_value(wire).unwrap(),
    )
}

#[test]
fn a_local_task_s_image_is_the_one_its_task_created_names_in_any_order() {
    // A single-job run's task: the engine runs it itself, so only `task_created` names its image.
    let log = vec![
        run_anchor(1.0, "r1"),
        created_with_image(2.0, "align", Some("repo/a@sha256:1")),
        engine_succeeded(3.0, "r1", "align"),
    ];
    assert_incremental_equals_replay(&log);
    let p = Projection::replay(&log);
    assert_eq!(p.runs["r1"].tasks["align"].image(), Some("repo/a@sha256:1"));
    // A task with no container names none.
    let bare = Projection::replay(&[
        created_with_image(2.0, "align", None),
        engine_succeeded(3.0, "r1", "align"),
    ]);
    assert_eq!(bare.runs["r1"].tasks["align"].image(), None);
    // Two that disagree (a plugin slip) settle on the least, whichever folds first.
    let conflict = vec![
        created_with_image(2.0, "align", Some("repo/b@sha256:2")),
        created_with_image(2.5, "align", Some("repo/a@sha256:1")),
    ];
    assert_incremental_equals_replay(&conflict);
    assert_eq!(
        Projection::replay(&conflict).runs["r1"].tasks["align"].image(),
        Some("repo/a@sha256:1")
    );
}

/// A single-job run's anchor.
fn single_job_anchor(ts: f64, run: &str) -> RunEvent {
    let mut anchor = run_anchor(ts, run);
    if let EventData::RunSubmitted(ref mut a) = anchor.data {
        a.single_job = Some(biffix_core::event::SingleJobUse {
            queue_class: "whole-host".into(),
            instance_type: "r6id.4xlarge".into(),
            vcpus: 16,
            memory_mib: 120_000,
        });
    }
    anchor
}

/// An engine fact from `source`, built from its wire JSON as the ingest route does.
fn engine_wire(source: EventSource, value: serde_json::Value) -> RunEvent {
    biffix_core::translate::from_engine(source, &serde_json::from_value(value).unwrap())
}

/// A Nextflow engine fact from its wire JSON.
fn nf(value: serde_json::Value) -> RunEvent {
    engine_wire(EventSource::Nextflow, value)
}

/// A Snakemake engine fact from its wire JSON.
fn sm(value: serde_json::Value) -> RunEvent {
    engine_wire(EventSource::Snakemake, value)
}

#[test]
fn single_job_host_retry_discards_lost_success_at_generation_start() {
    let anchor = single_job_anchor(1.0, "r1");
    let terminal = |ts, kind, generation| {
        nf(serde_json::json!({
            "ts": ts, "run_id": "r1", "kind": kind, "task_key": "align",
            "attempt": 1, "execution_id": format!("{generation}/1"),
            "coordinator_attempt": generation,
        }))
    };
    let started = nf(serde_json::json!({
        "ts": 4.0, "run_id": "r1", "kind": "run_started", "coordinator_attempt": 2,
    }));
    let log = vec![anchor, terminal(3.0, "task_succeeded", 1), started];
    let p = Projection::replay(&log);
    assert_eq!(p.runs["r1"].task_counts().succeeded, 0);
    let mut log = log;
    log.push(terminal(5.0, "task_failed", 2));
    let p = Projection::replay(&log);
    assert_eq!(task_state(&p, "r1", "align"), TaskState::Failed);
    assert_incremental_equals_replay(&log);
}

#[test]
fn generation_start_clears_cache_outputs_progress_and_engine_terminals_not_aws() {
    let mut anchor = single_job_anchor(1.0, "r1");
    let engine = nf;
    let old_facts = [
        serde_json::json!({"kind": "task_cached", "task_key": "align"}),
        serde_json::json!({"kind": "task_outputs", "task_key": "align", "outputs": [{"s3_uri": "s3://b/lost"}]}),
        serde_json::json!({"kind": "run_progress", "completed": 7, "total": 8}),
        serde_json::json!({"kind": "run_succeeded"}),
    ];
    let mut log = vec![anchor.clone()];
    for mut fact in old_facts {
        fact["ts"] = 2.0.into();
        fact["run_id"] = "r1".into();
        log.push(engine(fact)); // absent generation is legacy generation 1
    }
    log.push(engine(serde_json::json!({"kind": "run_started", "ts": 3.0,
        "run_id": "r1", "coordinator_attempt": 2})));
    let p = Projection::replay(&log);
    let rp = &p.runs["r1"];
    assert!(rp.tasks.is_empty());
    assert!(rp.outputs.is_empty());
    assert_eq!((rp.completed, rp.total), (None, None));
    assert_eq!(rp.completed_at_micros, None);
    assert_eq!(rp.state(), RunState::Running);
    assert_incremental_equals_replay(&log);

    // Even an unknown engine kind with a very high generation is stored but cannot reset work.
    log.push(engine(serde_json::json!({"kind": "run_progress", "ts": 4.0,
        "run_id": "r1", "coordinator_attempt": 2, "completed": 1, "total": 8})));
    let unknown = biffix_core::translate::from_engine_value(EventSource::Nextflow,
        serde_json::json!({"kind": "engine_future", "ts": 5.0, "run_id": "r1", "coordinator_attempt": 999})).unwrap();
    let mut p = Projection::replay(&log);
    p.apply(&unknown);
    assert_eq!(p.runs["r1"].completed, Some(1));
    // AWS's final job result is independent of the engine's generation.
    let aws = coord(
        6.0,
        "r1",
        EventData::CoordinatorFailed {
            log_stream: None,
            exit_code: Some(1),
            status_reason: None,
            termination_category: None,
        },
    );
    log.push(aws);
    assert_eq!(Projection::replay(&log).runs["r1"].state(), RunState::Failed);
    let mut reverse = log.clone();
    reverse.reverse();
    assert_eq!(fold_in_order(&reverse).runs, Projection::replay(&log).runs);

    // Tasks mode preserves successful/cache/output facts across coordinator restarts.
    if let EventData::RunSubmitted(ref mut a) = anchor.data {
        a.single_job = None;
    }
    log[0] = anchor;
    let p = Projection::replay(&log);
    assert_eq!(p.runs["r1"].tasks["align"].state(), TaskState::Succeeded);
    assert!(p.runs["r1"].tasks["align"].cached);
    assert_eq!(p.runs["r1"].outputs.len(), 1);
    assert_eq!(p.runs["r1"].completed, Some(7));
    assert_eq!(fold_in_order(&log.into_iter().rev().collect::<Vec<_>>()).runs, p.runs);
}

#[test]
fn single_job_generation_keeps_batch_attempts_and_authoritative_control_state_in_any_order() {
    let anchor = single_job_anchor(1.0, "r1");
    let started = biffix_core::translate::from_engine_value(EventSource::Nextflow,
        serde_json::json!({"kind": "run_started", "ts": 4.0, "run_id": "r1", "coordinator_attempt": 2})).unwrap();
    let log = vec![
        anchor,
        task_submitted(2.0, "r1", "external", 1, "job-1"),
        aws_succeeded(3.0, "job-1"),
        // A Nextflow `-resume` cache hit: a retried Nextflow coordinator starts over.
        RunEvent { source: EventSource::Nextflow, ..engine_cached(3.0, "r1", "local") },
        started,
        control(5.0, "r1", EventData::RunCancelled {}),
        coord(
            6.0,
            "r1",
            EventData::CoordinatorFailed {
                log_stream: Some("final".into()),
                exit_code: Some(137),
                status_reason: Some("host terminated".into()),
                termination_category: Some(TerminationCategory::HostTerminated),
            },
        ),
    ];
    let p = Projection::replay(&log);
    assert_eq!(p.runs["r1"].state(), RunState::Cancelled);
    assert_eq!(p.runs["r1"].tasks["external"].state(), TaskState::Succeeded);
    assert!(!p.runs["r1"].tasks.contains_key("local"));
    assert_eq!(p.runs["r1"].coordinator_exit_code, Some(137));
    assert_eq!(p.runs["r1"].coordinator_log_stream.as_deref(), Some("final"));
    assert_incremental_equals_replay(&log);
}

#[test]
fn a_snakemake_host_retry_keeps_the_finished_tasks_it_never_reports_again() {
    // Generation 1 finished `a` (its output is in S3); the retry skips `a`, runs `b`, and reports
    // only `b`, its own DAG, progress and session.
    let gen1 = |ts: f64, value: serde_json::Value| {
        let mut value = value;
        value["ts"] = ts.into();
        value["run_id"] = "r1".into();
        value["coordinator_attempt"] = 1.into();
        sm(value)
    };
    let gen2 = |ts: f64, value: serde_json::Value| {
        let mut value = value;
        value["ts"] = ts.into();
        value["run_id"] = "r1".into();
        value["coordinator_attempt"] = 2.into();
        sm(value)
    };
    let log = vec![
        single_job_anchor(1.0, "r1"),
        gen1(2.0, serde_json::json!({"kind": "run_started", "session_id": "s1"})),
        gen1(3.0, serde_json::json!({"kind": "task_succeeded", "task_key": "a"})),
        gen1(
            3.5,
            serde_json::json!({"kind": "task_outputs", "task_key": "a",
                "outputs": [{"s3_uri": "s3://b/a.txt"}]}),
        ),
        gen1(4.0, serde_json::json!({"kind": "run_progress", "completed": 1, "total": 3})),
        gen2(5.0, serde_json::json!({"kind": "run_started", "session_id": "s2"})),
        gen2(6.0, serde_json::json!({"kind": "task_succeeded", "task_key": "b"})),
    ];
    let p = Projection::replay(&log);
    let rp = &p.runs["r1"];
    assert_eq!(rp.tasks["a"].state(), TaskState::Succeeded);
    assert_eq!(rp.tasks["b"].state(), TaskState::Succeeded);
    assert_eq!(rp.outputs.len(), 1, "{:?}", rp.outputs);
    // The run-level facts are the retry's: the lost host's progress and session are gone.
    assert_eq!((rp.completed, rp.total), (None, None));
    assert_eq!(rp.reported_session_id.as_deref(), Some("s2"));
    assert_eq!(rp.state(), RunState::Running);
    assert_incremental_equals_replay(&log);
}

#[test]
fn a_generation_start_clears_the_lost_host_s_failure_dag_and_session() {
    let gen = |ts: f64, generation: u32, value: serde_json::Value| {
        let mut value = value;
        value["ts"] = ts.into();
        value["run_id"] = "r1".into();
        value["coordinator_attempt"] = generation.into();
        nf(value)
    };
    let log = vec![
        single_job_anchor(1.0, "r1"),
        // A session that sorts after none, so only the reset can clear it.
        gen(2.0, 1, serde_json::json!({"kind": "run_started", "session_id": "s1"})),
        gen(
            3.0,
            1,
            serde_json::json!({"kind": "run_dag",
                "dag": {"nodes": [{"id": "a"}, {"id": "b"}], "edges": [{"from": "a", "to": "b"}]}}),
        ),
        gen(4.0, 1, serde_json::json!({"kind": "run_failed", "error": "host lost"})),
        gen(5.0, 2, serde_json::json!({"kind": "run_started"})),
    ];
    let p = Projection::replay(&log);
    let rp = &p.runs["r1"];
    assert_eq!(rp.state(), RunState::Running);
    assert_eq!(rp.completed_at_micros, None);
    assert!(rp.dag_nodes.is_empty() && rp.dag_edges.is_empty());
    assert_eq!(rp.reported_session_id, None);
    assert_incremental_equals_replay(&log);
}

#[test]
fn a_single_job_fact_past_the_batch_attempt_limit_changes_nothing() {
    let log = vec![
        single_job_anchor(1.0, "r1"),
        nf(serde_json::json!({"ts": 2.0, "run_id": "r1", "kind": "task_succeeded",
            "task_key": "align", "attempt": 1, "execution_id": "1/1"})),
        nf(serde_json::json!({"ts": 3.0, "run_id": "r1", "kind": "run_started",
            "coordinator_attempt": biffix_core::event::MAX_COORDINATOR_GENERATION + 1})),
    ];
    // Ignored outright: no reset, and not even its `run_started`.
    let p = Projection::replay(&log);
    assert_eq!(p.runs["r1"].tasks["align"].state(), TaskState::Succeeded);
    assert_eq!(p.runs, Projection::replay(&log[..2]).runs);
    assert_incremental_equals_replay(&log);
}

#[test]
fn the_newest_generation_s_session_is_the_reported_one_in_any_order() {
    // Tasks mode: a retried coordinator starts the engine again; its session is the one that ran.
    let started = |ts: f64, generation: u32, session: &str| {
        nf(serde_json::json!({"ts": ts, "run_id": "r1", "kind": "run_started",
            "coordinator_attempt": generation, "session_id": session}))
    };
    let log = vec![run_anchor(1.0, "r1"), started(2.0, 1, "s-b"), started(3.0, 2, "s-c")];
    let p = Projection::replay(&log);
    assert_eq!(p.runs["r1"].reported_session_id.as_deref(), Some("s-c"));
    assert_incremental_equals_replay(&log);
}

#[test]
fn task_created_image_observations_survive_log_dedup_and_replay() {
    let events = vec![
        created_with_image(2.0, "align", None),
        created_with_image(3.0, "align", Some("repo/b@sha256:2")),
        created_with_image(4.0, "align", Some("repo/a@sha256:1")),
    ];
    assert_eq!(events[0].dedup_key, "snakemake:r1:align::task_created");
    for mut delivered in permutations(&events) {
        delivered.push(events[1].clone());
        let mut seen = std::collections::BTreeSet::new();
        // The log's ON CONFLICT (dedup_key) DO NOTHING retains the first fact per key.
        let retained: Vec<_> =
            delivered.into_iter().filter(|e| seen.insert(e.dedup_key.clone())).collect();
        assert_eq!(retained.len(), 3);
        let replay = Projection::replay(&retained);
        assert_eq!(replay.runs["r1"].tasks["align"].image(), Some("repo/a@sha256:1"));
        assert_incremental_equals_replay(&retained);
    }
    assert_eq!(
        events[1].dedup_key,
        created_with_image(5.0, "align", Some("repo/b@sha256:2")).dedup_key
    );
}

#[test]
fn a_batch_attempt_s_image_wins_over_the_one_task_created_names() {
    let mut submitted = task_submitted(3.0, "r1", "align", 1, "job-1");
    submitted.data = EventData::TaskSubmitted { image: Some("repo/batch@sha256:1".into()) };
    let log = vec![created_with_image(2.0, "align", Some("repo/created@sha256:2")), submitted];
    assert_incremental_equals_replay(&log);
    let p = Projection::replay(&log);
    assert_eq!(p.runs["r1"].tasks["align"].image(), Some("repo/batch@sha256:1"));
    // An attempt that names none falls back to it.
    let log = vec![
        created_with_image(2.0, "align", Some("repo/created@sha256:2")),
        task_submitted(3.0, "r1", "align", 1, "job-1"),
    ];
    assert_incremental_equals_replay(&log);
    let p = Projection::replay(&log);
    assert_eq!(p.runs["r1"].tasks["align"].image(), Some("repo/created@sha256:2"));
}
