//! Append is idempotent on `dedup_key`, and the projection folds to the expected run and task state
//! (projection == fold), whatever the chunking and however late a binding lands.

use super::*;

#[tokio::test]
#[ignore = "requires the LocalStack/Postgres harness"]
async fn append_is_idempotent_on_dedup_key() {
    let pool = common::isolated_pool("append_is_idempotent_on_dedup_key").await;
    let e = ev(
        1.0,
        EventSource::BiffixControlPlane,
        "dk-1",
        EventData::RunStarted { session_id: None },
    )
    .with_run("r1");
    assert!(append(&pool, &e).await.unwrap(), "first insert");
    assert!(!append(&pool, &e).await.unwrap(), "duplicate is a no-op");
    let n: i64 =
        sqlx::query_scalar("SELECT count(*) c FROM run_events").fetch_one(&pool).await.unwrap();
    assert_eq!(n, 1);
}

#[tokio::test]
#[ignore = "requires the LocalStack/Postgres harness"]
async fn projection_folds_a_happy_run_to_succeeded() {
    let pool = common::isolated_pool("projection_folds_a_happy_run_to_succeeded").await;

    let submitted = submitted(1.0, "dk-sub", "r1");

    let started =
        ev(2.0, EventSource::Snakemake, "dk-start", EventData::RunStarted { session_id: None })
            .with_run("r1");

    let task_sub = task_submitted(3.0, "dk-tsub", "r1", "align:sample=A", "job-1");

    let task_ok = ev(
        4.0,
        EventSource::AwsBatch,
        "dk-tok",
        EventData::TaskSucceeded { exit_code: 0, log_stream: None },
    )
    .with_job("job-1");

    let coord_ok = ev(
        5.0,
        EventSource::AwsBatch,
        "dk-cok",
        EventData::CoordinatorSucceeded { log_stream: None },
    )
    .with_run("r1");

    for e in [&submitted, &started, &task_sub, &task_ok, &coord_ok] {
        append(&pool, e).await.unwrap();
    }

    let run_state: String =
        sqlx::query_scalar("SELECT state FROM run_projection WHERE run_id='r1'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(run_state, "succeeded");

    let task_state: String = sqlx::query_scalar(
        "SELECT state FROM task_projection WHERE run_id='r1' AND task_key='align:sample=A'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(task_state, "succeeded");

    // The materialized per-outcome task-count columns must equal the fold's own task_counts:
    // one succeeded task, nothing else. Asserting the columns (not just state) catches a
    // bind-to-column transposition in upsert_run's INSERT, which is otherwise silently green.
    let counts = sqlx::query(
        "SELECT tasks_observed, tasks_succeeded, tasks_running, tasks_failed, tasks_cached \
         FROM run_projection WHERE run_id='r1'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(counts.get::<i64, _>("tasks_observed"), 1);
    assert_eq!(counts.get::<i64, _>("tasks_succeeded"), 1);
    assert_eq!(counts.get::<i64, _>("tasks_running"), 0);
    assert_eq!(counts.get::<i64, _>("tasks_failed"), 0);
    assert_eq!(counts.get::<i64, _>("tasks_cached"), 0);
}

#[tokio::test]
#[ignore = "requires the LocalStack/Postgres harness"]
async fn incremental_advance_equals_full_replay_across_chunks() {
    // The incremental fold must persist exactly what a full-log replay would:
    // each chunk is ONE `ingest_batch` (one append + fold transaction, as the SQS drain does),
    // and the second chunk carries run_id-less AWS terminals whose binding was folded in the first
    // (the cross-chunk arm of the affected-run query) or arrives later in the same chunk.
    let pool = common::isolated_pool("incremental_advance_equals_full_replay_across_chunks").await;

    // An AWS Batch event: a batch_job_id, no run_id (attributed via its binding).
    let batch = |ts: f64, job: &str, dk: &str, data: EventData| {
        ev(ts, EventSource::AwsBatch, dk, data).with_job(job)
    };
    let failed = |exit_code: i32| EventData::TaskFailed {
        exit_code,
        termination_category: None,
        status_reason: None,
        log_stream: None,
    };
    let succeeded = || EventData::TaskSucceeded { exit_code: 0, log_stream: None };

    // r1 succeeds (its coordinator bound, then ended by a run_id-less terminal); r2 has a task
    // still running until a coordinator failure terminates the run (the task is stranded); r3's
    // task is bound in chunk 1 and ended by a run_id-less terminal in chunk 2; r4's terminal
    // precedes its own binding within chunk 2; r5 is cancelled with its task still running, so
    // the task reads cancelled.
    let chunk1 = vec![
        submitted(1.0, "r1-sub", "r1"),
        biffix_core::event::coordinator_submitted(
            &RunId::new("r1"),
            Timestamp::from_epoch_seconds(2.0),
            &BatchJobId::new("coord-1"),
        ),
        task_submitted(3.0, "r1-tsub", "r1", "tA", "job-1"),
        batch(4.0, "job-1", "r1-tok", succeeded()),
        batch(5.0, "coord-1", "r1-cok", EventData::CoordinatorSucceeded { log_stream: None }),
        submitted(1.0, "r2-sub", "r2"),
        task_submitted(3.0, "r2-tsub", "r2", "tB", "job-2"),
        batch(
            4.0,
            "job-2",
            "r2-tstart",
            EventData::TaskStarted { started_at: None, log_stream: None },
        ),
        submitted(1.0, "r3-sub", "r3"),
        task_submitted(3.0, "r3-tsub", "r3", "tC", "job-3"),
        submitted(1.0, "r5-sub", "r5"),
        task_submitted(3.0, "r5-tsub", "r5", "tE", "job-5"),
        batch(
            4.0,
            "job-5",
            "r5-tstart",
            EventData::TaskStarted { started_at: None, log_stream: None },
        ),
    ];
    let chunk2 = vec![
        run_ev(
            6.0,
            EventSource::AwsBatch,
            "r2-cfail",
            "r2",
            EventData::CoordinatorFailed {
                log_stream: None,
                exit_code: Some(137),
                status_reason: Some("OutOfMemoryError: Container killed".into()),
                termination_category: Some(biffix_core::event::TerminationCategory::Oom),
            },
        ),
        batch(7.0, "job-3", "r3-tfail", failed(2)),
        run_ev(
            6.0,
            EventSource::BiffixControlPlane,
            "r5-cancel",
            "r5",
            EventData::CancelRequested { actor: "alice".into(), actor_label: None },
        ),
        run_ev(
            7.0,
            EventSource::AwsBatch,
            "r5-cfail",
            "r5",
            EventData::CoordinatorFailed {
                log_stream: None,
                exit_code: None,
                status_reason: None,
                termination_category: None,
            },
        ),
        batch(8.0, "job-4", "r4-tok", succeeded()),
        submitted(1.0, "r4-sub", "r4"),
        task_submitted(3.0, "r4-tsub", "r4", "tD", "job-4"),
    ];

    // Chunk 1 folds as one batch, then chunk 2 as another: the cursor advances between them.
    assert_eq!(ingest_batch(&pool, &chunk1, &[]).await.unwrap(), chunk1.len());
    assert_eq!(ingest_batch(&pool, &chunk2, &[]).await.unwrap(), chunk2.len());

    // Ground truth: a single replay of every event (it folds in ts order, so seq-less is fine).
    let all: Vec<RunEvent> = chunk1.into_iter().chain(chunk2).collect();
    let expected = Projection::replay(&all);

    // Every run's persisted columns that the reducer fills match replay, and the row counts match
    // (no extra/missing rows).
    let run_rows = sqlx::query(
        "SELECT run_id, state, completed_at, coordinator_job_id, tasks_observed, \
                tasks_succeeded, tasks_running, tasks_queued, tasks_failed, tasks_cancelled, \
                tasks_cached, coordinator_exit_code, coordinator_status_reason, \
                coordinator_termination_category \
           FROM run_projection",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(run_rows.len(), expected.runs.len(), "run row count == replay");
    for row in &run_rows {
        let run_id: String = row.get("run_id");
        let rp = expected.runs.get(&run_id).expect("run in replay");
        assert_eq!(
            row.get::<String, _>("state"),
            rp.state().as_str(),
            "run {run_id} state == replay"
        );
        assert_eq!(
            row.get::<Option<i64>, _>("completed_at"),
            rp.completed_at_micros,
            "run {run_id} completed_at == replay"
        );
        assert_eq!(
            row.get::<Option<String>, _>("coordinator_job_id"),
            rp.coordinator_job_id,
            "run {run_id} coordinator_job_id == replay"
        );
        assert_eq!(
            (
                row.get::<Option<i32>, _>("coordinator_exit_code"),
                row.get::<Option<String>, _>("coordinator_status_reason"),
                row.get::<Option<String>, _>("coordinator_termination_category"),
            ),
            (
                rp.coordinator_exit_code,
                rp.coordinator_status_reason.clone(),
                rp.coordinator_termination_category.clone(),
            ),
            "run {run_id} coordinator stop == replay"
        );
        let counts = rp.task_counts();
        let persisted: [i64; 7] = [
            "tasks_observed",
            "tasks_succeeded",
            "tasks_running",
            "tasks_queued",
            "tasks_failed",
            "tasks_cancelled",
            "tasks_cached",
        ]
        .map(|c| row.get::<i64, _>(c));
        assert_eq!(
            persisted,
            [
                counts.observed,
                counts.succeeded,
                counts.running,
                counts.queued,
                counts.failed,
                counts.cancelled,
                counts.cached,
            ]
            .map(|n| n as i64),
            "run {run_id} task counts == replay"
        );
    }

    assert_eq!(
        expected.runs["r2"].coordinator_status_reason.as_deref(),
        Some("OutOfMemoryError: Container killed"),
        "the fixture exercises the coordinator stop columns"
    );

    // Every task's persisted state matches the run's *effective* task state (a terminal run forces
    // an open task to Failed — r2/tB here), for every task, with matching counts.
    let task_rows = sqlx::query(
        "SELECT run_id, task_key, state, termination_category, attempts, exit_code \
           FROM task_projection",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    let expected_task_count: usize = expected.runs.values().map(|rp| rp.tasks.len()).sum();
    assert_eq!(task_rows.len(), expected_task_count, "task row count == replay");
    for row in &task_rows {
        let run_id: String = row.get("run_id");
        let task_key: String = row.get("task_key");
        let state: String = row.get("state");
        let rp = expected.runs.get(&run_id).expect("run in replay");
        let tp = rp.tasks.get(&task_key).expect("task in replay");
        assert_eq!(
            state,
            rp.effective_task_state(tp).as_str(),
            "task {run_id}/{task_key} state == replay effective state"
        );
        // A stranded task carries the display-only `run_ended` category.
        let category: Option<String> = row.get("termination_category");
        assert_eq!(
            category.as_deref() == Some(biffix_db::projection::RUN_ENDED_CATEGORY),
            rp.is_stranded(tp),
            "task {run_id}/{task_key} run_ended category iff stranded"
        );
        assert_eq!(
            row.get::<i32, _>("attempts") as usize,
            tp.attempts.len(),
            "task {run_id}/{task_key} attempts == replay"
        );
        assert_eq!(
            row.get::<Option<i32>, _>("exit_code"),
            tp.display_attempt().and_then(|a| a.exit_code),
            "task {run_id}/{task_key} exit_code == replay"
        );
    }

    // Concretely, so the comparison above cannot pass on a degenerate replay: r1 succeeded via its
    // bound coordinator, r2 failed with tB stranded, the run_id-less terminals reached r3/tC
    // (across chunks) and r4/tD (ahead of its binding in the same chunk), and r5's cancel ended
    // tE.
    let persisted_state = {
        let pool = &pool;
        move |sql: &'static str| async move {
            sqlx::query_scalar::<_, String>(sql).fetch_one(pool).await.unwrap()
        }
    };
    assert_eq!(
        persisted_state("SELECT state FROM run_projection WHERE run_id = 'r1'").await,
        "succeeded"
    );
    assert_eq!(
        persisted_state("SELECT state FROM run_projection WHERE run_id = 'r2'").await,
        "failed"
    );
    assert!(expected.runs["r2"].is_stranded(&expected.runs["r2"].tasks["tB"]));
    assert_eq!(
        persisted_state("SELECT state FROM task_projection WHERE task_key = 'tC'").await,
        "failed"
    );
    assert_eq!(
        persisted_state("SELECT state FROM task_projection WHERE task_key = 'tD'").await,
        "succeeded"
    );
    assert_eq!(
        persisted_state("SELECT state FROM run_projection WHERE run_id = 'r5'").await,
        "cancelled"
    );
    assert_eq!(
        persisted_state("SELECT state FROM task_projection WHERE task_key = 'tE'").await,
        "cancelled"
    );
    assert_eq!(expected.runs["r5"].task_counts().cancelled, 1);

    // Whole rows: the incrementally persisted projection equals a from-scratch refold of the log,
    // column for column (minus `updated_at`, the wall-clock write time), across every table the
    // run fold writes.
    let before = projection_snapshot(&pool).await;
    assert!(!before["attempt_projection"].is_empty());
    let page = refold_runs(&pool, None, 100).await.unwrap();
    assert_eq!(page.refolded, 5);
    assert_eq!(projection_snapshot(&pool).await, before, "incremental == full refold");
}

/// Every row of the tables the run fold writes, as JSON (minus the wall-clock `updated_at`), in a
/// deterministic order, keyed by table name.
async fn projection_snapshot(
    pool: &sqlx::PgPool,
) -> std::collections::BTreeMap<&'static str, Vec<serde_json::Value>> {
    let mut snapshot = std::collections::BTreeMap::new();
    for table in
        ["run_projection", "task_projection", "attempt_projection", "run_output", "notifications"]
    {
        let rows: Vec<serde_json::Value> = sqlx::query_scalar(AssertSqlSafe(format!(
            "SELECT to_jsonb(t) - 'updated_at' FROM {table} t ORDER BY (to_jsonb(t) - 'updated_at')::text"
        )))
        .fetch_all(pool)
        .await
        .unwrap();
        snapshot.insert(table, rows);
    }
    snapshot
}

/// Late attribution across transactions: an AWS terminal (batch_job_id, no run_id) committed BEFORE
/// its binding exists affects no run when it lands; the binding's own fold must pull it back in
/// (`fold_run`'s batch_job_id arm). The reducer tests cover this in memory; this is the persisted
/// path, where no in-memory pending map exists.
#[tokio::test]
#[ignore = "requires the LocalStack/Postgres harness"]
async fn a_terminal_logged_before_its_binding_folds_once_the_binding_lands() {
    let pool = common::isolated_pool("terminal_before_binding_across_txns").await;
    let mut log = vec![
        run_ev(
            1.0,
            EventSource::BiffixControlPlane,
            "late-anchor",
            "r-late",
            EventData::RunStarted { session_id: None },
        ),
        ev(
            5.0,
            EventSource::AwsBatch,
            "aws-batch:job-late:SUCCEEDED",
            EventData::TaskSucceeded { exit_code: 0, log_stream: None },
        )
        .with_job("job-late"),
    ];
    for e in &log {
        assert!(append(&pool, e).await.unwrap());
    }
    let tasks: i64 =
        sqlx::query_scalar("SELECT count(*) FROM task_projection WHERE run_id = 'r-late'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(tasks, 0, "an unbound terminal is attributed to no run yet");

    let binding = task_submitted(3.0, "late-tsub", "r-late", "align", "job-late");
    assert!(append(&pool, &binding).await.unwrap());
    log.push(binding);

    let expected = Projection::replay(&log);
    let rp = &expected.runs["r-late"];
    let (state, exit_code): (String, Option<i32>) = sqlx::query_as(
        "SELECT state, exit_code FROM task_projection WHERE run_id = 'r-late' AND task_key = 'align'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(state, "succeeded", "the earlier terminal was pulled in");
    assert_eq!(state, rp.effective_task_state(&rp.tasks["align"]).as_str());
    assert_eq!(exit_code, Some(0));
}

/// The coordinator shape of the case above: a run_id-less coordinator terminal logged before the
/// `coordinator_submitted` binding ends the run once the binding lands.
#[tokio::test]
#[ignore = "requires the LocalStack/Postgres harness"]
async fn a_coordinator_terminal_logged_before_its_binding_ends_the_run() {
    let pool = common::isolated_pool("coordinator_terminal_before_binding").await;
    let run = RunId::new("r-coord-late");
    let mut log = vec![
        biffix_core::event::run_submitted(
            &run,
            Timestamp::from_epoch_seconds(1.0),
            biffix_core::testing::anchor(),
        ),
        ev(
            9.0,
            EventSource::AwsBatch,
            "aws-batch:coord-late:SUCCEEDED",
            EventData::CoordinatorSucceeded { log_stream: None },
        )
        .with_job("coord-late"),
    ];
    for e in &log {
        assert!(append(&pool, e).await.unwrap());
    }
    let read = || async {
        sqlx::query_as::<_, (String, Option<i64>)>(
            "SELECT state, completed_at FROM run_projection WHERE run_id = 'r-coord-late'",
        )
        .fetch_one(&pool)
        .await
        .unwrap()
    };
    assert_eq!(
        read().await,
        ("submitted".to_string(), None),
        "an unbound coordinator terminal ends no run"
    );

    let binding = biffix_core::event::coordinator_submitted(
        &run,
        Timestamp::from_epoch_seconds(2.0),
        &BatchJobId::new("coord-late"),
    );
    assert!(append(&pool, &binding).await.unwrap());
    log.push(binding);

    let rp = &Projection::replay(&log).runs["r-coord-late"];
    assert_eq!(read().await, ("succeeded".to_string(), Some(9_000_000)));
    assert_eq!(rp.state().as_str(), "succeeded");
    assert_eq!(rp.completed_at_micros, Some(9_000_000));
}

/// A second run's later claim on a job another run already bound gets none of the job's AWS facts,
/// in the persisted fold as in the reducer: folding the claimant must still see the first claim
/// (`fold_run` reads other runs' rows for the jobs this run names), whatever transaction each lands
/// in.
#[tokio::test]
#[ignore = "requires the LocalStack/Postgres harness"]
async fn a_later_claim_on_a_bound_job_does_not_take_its_facts_when_persisted() {
    let pool = common::isolated_pool("later_claim_does_not_take_bound_job").await;
    let claim = |ts: f64, run: &str, task: &str| {
        task_submitted(ts, &format!("{run}-claim"), run, task, "job-v")
    };
    let log = vec![
        claim(2.0, "victim", "align"),
        claim(3.0, "attacker", "evil"),
        ev(
            4.0,
            EventSource::AwsBatch,
            "aws-batch:job-v:SUCCEEDED",
            EventData::TaskSucceeded { exit_code: 0, log_stream: None },
        )
        .with_job("job-v"),
    ];
    // Each fact in its own transaction.
    for e in &log {
        assert!(append(&pool, e).await.unwrap());
    }
    let expected = Projection::replay(&log);
    let victim = &expected.runs["victim"];
    let evil = &expected.runs["attacker"].tasks["evil"].attempts["job-v"];
    assert_eq!(evil.state.as_str(), "submitted", "replay: the claim gets no facts");

    let task_state: String = sqlx::query_scalar(
        "SELECT state FROM task_projection WHERE run_id = 'victim' AND task_key = 'align'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(task_state, "succeeded");
    assert_eq!(task_state, victim.effective_task_state(&victim.tasks["align"]).as_str());
    let attempt_state: String = sqlx::query_scalar(
        "SELECT state FROM attempt_projection WHERE run_id = 'attacker' AND batch_job_id = 'job-v'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(attempt_state, evil.state.as_str());
}

/// A run whose events no longer fold to a run (here, under a reducer that stops trusting a
/// producer: an engine planted the anchor) loses the rows an older fold wrote, in every table the
/// run fold owns, on the next advance and on a refold (`projection == fold`); its users' read state
/// is not the fold's and stays.
#[tokio::test]
#[ignore = "requires the LocalStack/Postgres harness"]
async fn a_run_the_fold_no_longer_produces_loses_its_rows() {
    let pool = common::isolated_pool("a_run_the_fold_no_longer_produces_loses_its_rows").await;
    for sql in [
        "INSERT INTO run_projection (run_id, state, submitter) VALUES ('ghost', 'failed', 'u')",
        "INSERT INTO task_projection (run_id, task_key, state) VALUES ('ghost', 't', 'failed')",
        "INSERT INTO attempt_projection (run_id, batch_job_id, task_key, state) \
         VALUES ('ghost', 'job-1', 't', 'failed')",
        "INSERT INTO run_output (run_id, s3_uri, id, role) \
         VALUES ('ghost', 's3://b/o', 'o1', 'output')",
        "INSERT INTO notifications (id, user_sub, run_id, kind, at) \
         VALUES ('n-ghost', 'u', 'ghost', 'run_failed', 1)",
        "INSERT INTO notification_reads (user_sub, notification_id) VALUES ('u', 'n-ghost')",
    ] {
        sqlx::query(sql).execute(&pool).await.unwrap();
    }
    let planted = run_ev(
        1.0,
        EventSource::Snakemake,
        "dk-planted",
        "ghost",
        EventData::RunSubmitted(Box::new(biffix_core::testing::anchor())),
    );
    append(&pool, &planted).await.unwrap();
    let count = {
        let pool = &pool;
        move |table: &'static str| async move {
            sqlx::query_scalar::<_, i64>(AssertSqlSafe(format!(
                "SELECT COUNT(*) FROM {table} WHERE run_id = 'ghost'"
            )))
            .fetch_one(pool)
            .await
            .unwrap()
        }
    };
    for table in
        ["run_projection", "task_projection", "attempt_projection", "run_output", "notifications"]
    {
        assert_eq!(count(table).await, 0, "{table}");
    }
    let reads: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM notification_reads")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(reads, 1, "read state is not the fold's");
    // A refold of the run agrees.
    sqlx::query("INSERT INTO run_projection (run_id, state) VALUES ('ghost', 'failed')")
        .execute(&pool)
        .await
        .unwrap();
    refold_runs(&pool, None, 10).await.unwrap();
    assert_eq!(count("run_projection").await, 0);
}
