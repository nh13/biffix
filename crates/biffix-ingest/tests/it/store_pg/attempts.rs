//! The attempt projection: reclaimed attempts, requested resources, and refolds that backfill them.

use super::*;

#[tokio::test]
#[ignore = "requires the LocalStack/Postgres harness"]
async fn attempt_rows_keep_a_reclaimed_attempt_after_its_retry_succeeds() {
    let pool = common::isolated_pool("attempt_rows_keep_a_reclaimed_attempt").await;
    let submit = |attempt: u32, job: &str, ts: f64| {
        task_submitted(ts, &format!("dk-sub-{job}"), "r1", "align:sample=A", job)
            .with_attempt(attempt)
    };
    let aws = |job: &str, ts: f64, data: EventData| {
        ev(ts, EventSource::AwsBatch, format!("dk-aws-{job}-{ts}"), data)
            .with_job(job)
            .with_instance(&format!("ci-{job}"))
            .with_queue("biffix-acme-arch-c8g64")
    };
    let events = [
        submit(1, "job-1", 1.0),
        aws(
            "job-1",
            2.0,
            EventData::TaskStarted { started_at: None, log_stream: Some("ls/1".into()) },
        ),
        aws(
            "job-1",
            3.0,
            EventData::TaskFailed {
                exit_code: 137,
                termination_category: None,
                status_reason: Some("Host EC2 (instance i-0c62) terminated.".into()),
                log_stream: None,
            },
        ),
        submit(2, "job-2", 4.0),
        aws(
            "job-2",
            5.0,
            EventData::TaskStarted { started_at: None, log_stream: Some("ls/2".into()) },
        ),
        aws("job-2", 6.0, EventData::TaskSucceeded { exit_code: 0, log_stream: None }),
    ];
    for e in &events {
        append(&pool, e).await.unwrap();
    }

    let rows = sqlx::query(
        "SELECT batch_job_id, task_key, attempt, state, exit_code, status_reason, log_stream, \
                queue, instance_id, started_at, finished_at \
           FROM attempt_projection WHERE run_id = 'r1' ORDER BY attempt",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(rows.len(), 2, "both attempts persist, not just the display attempt");

    let first = &rows[0];
    assert_eq!(first.get::<String, _>("batch_job_id"), "job-1");
    assert_eq!(first.get::<String, _>("task_key"), "align:sample=A");
    assert_eq!(first.get::<Option<i32>, _>("attempt"), Some(1));
    assert_eq!(first.get::<String, _>("state"), "failed");
    assert_eq!(first.get::<Option<i32>, _>("exit_code"), Some(137));
    assert_eq!(
        first.get::<Option<String>, _>("status_reason").as_deref(),
        Some("Host EC2 (instance i-0c62) terminated.")
    );
    assert_eq!(first.get::<Option<String>, _>("log_stream").as_deref(), Some("ls/1"));
    assert_eq!(first.get::<Option<String>, _>("queue").as_deref(), Some("biffix-acme-arch-c8g64"));
    assert_eq!(first.get::<Option<String>, _>("instance_id").as_deref(), Some("ci-job-1"));
    assert_eq!(first.get::<Option<i64>, _>("started_at"), Some(2_000_000));
    assert_eq!(first.get::<Option<i64>, _>("finished_at"), Some(3_000_000));

    let second = &rows[1];
    assert_eq!(second.get::<String, _>("batch_job_id"), "job-2");
    assert_eq!(second.get::<String, _>("state"), "succeeded");
    assert_eq!(second.get::<Option<String>, _>("status_reason"), None);

    // task_projection still summarizes only the display (succeeded) attempt.
    let display: String = sqlx::query_scalar(
        "SELECT batch_job_id FROM task_projection WHERE run_id='r1' AND task_key='align:sample=A'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(display, "job-2");
}

#[tokio::test]
#[ignore = "requires the LocalStack/Postgres harness"]
async fn attempt_rows_carry_the_requested_resources_and_a_refold_reproduces_them() {
    let pool = common::isolated_pool("attempt_rows_carry_requested_resources").await;
    let submit = |attempt: u32, job: &str, ts: f64| {
        task_submitted(ts, &format!("dk-sub-{job}"), "r1", "align:sample=A", job)
            .with_attempt(attempt)
    };
    let aws = |job: &str, ts: f64, request: Option<(f64, i64)>, data: EventData| RunEvent {
        requested_vcpus: request.map(|(vcpus, _)| vcpus),
        requested_memory_mib: request.map(|(_, memory)| memory),
        ..ev(ts, EventSource::AwsBatch, format!("dk-aws-{job}-{ts}"), data).with_job(job)
    };
    // job-1's terminal arrives before its submit binds it (late attribution); only its RUNNABLE
    // names the request. job-2's events never name one (as for every event before the field).
    let events = [
        aws("job-1", 3.0, None, EventData::TaskSucceeded { exit_code: 0, log_stream: None }),
        aws("job-1", 2.0, Some((4.0, 30_000)), EventData::TaskQueued {}),
        submit(1, "job-1", 1.0),
        submit(1, "job-2", 4.0),
        aws("job-2", 5.0, None, EventData::TaskQueued {}),
    ];
    for e in &events {
        append(&pool, e).await.unwrap();
    }
    let rows = || async {
        sqlx::query_as::<_, (String, Option<f64>, Option<i64>)>(
            "SELECT batch_job_id, requested_vcpus, requested_memory_mib \
               FROM attempt_projection WHERE run_id = 'r1' ORDER BY batch_job_id",
        )
        .fetch_all(&pool)
        .await
        .unwrap()
    };
    // The common live order: the engine's submit lands first (the row is written with no request),
    // then the Batch RUNNABLE that names it, which must UPDATE the existing row.
    append(&pool, &submit(3, "job-3", 6.0)).await.unwrap();
    assert_eq!(rows().await.last().cloned(), Some(("job-3".to_string(), None, None)));
    append(&pool, &aws("job-3", 7.0, Some((0.5, 8_192)), EventData::TaskQueued {})).await.unwrap();
    let incremental = rows().await;
    assert_eq!(
        incremental,
        vec![
            ("job-1".to_string(), Some(4.0), Some(30_000)),
            ("job-2".to_string(), None, None),
            ("job-3".to_string(), Some(0.5), Some(8_192)),
        ]
    );
    // A replay from the log (what a deploy's refold does) yields the same rows.
    sqlx::query("DELETE FROM attempt_projection").execute(&pool).await.unwrap();
    refold_runs(&pool, None, 10).await.unwrap();
    assert_eq!(rows().await, incremental);
}

#[tokio::test]
#[ignore = "requires the LocalStack/Postgres harness"]
async fn refold_runs_pages_through_every_run_and_backfills_attempts() {
    let pool = common::isolated_pool("refold_runs_pages_through_every_run").await;
    for run in ["r1", "r2", "r3"] {
        let sub = task_submitted(1.0, &format!("dk-sub-{run}"), run, "t", &format!("job-{run}"));
        append(&pool, &sub).await.unwrap();
    }
    // What the fold produced (attempt_projection has no volatile columns), so the backfill can be
    // checked row for row, not only by count.
    let folded = || async {
        sqlx::query_scalar::<_, serde_json::Value>(
            "SELECT to_jsonb(a) FROM attempt_projection a ORDER BY batch_job_id",
        )
        .fetch_all(&pool)
        .await
        .unwrap()
    };
    let before = folded().await;
    assert_eq!(before.len(), 3, "one attempt per run was folded");
    // Stand in for runs folded before attempt_projection existed: the append folded them, so drop
    // the rows the backfill must restore.
    sqlx::query("DELETE FROM attempt_projection").execute(&pool).await.unwrap();
    let count = || async {
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) AS n FROM attempt_projection")
            .fetch_one(&pool)
            .await
            .unwrap()
    };
    assert_eq!(count().await, 0);

    let first = refold_runs(&pool, None, 2).await.unwrap();
    assert_eq!(first.refolded, 2);
    assert_eq!(first.last_run_id.as_deref(), Some("r2"));
    let second = refold_runs(&pool, first.last_run_id.as_deref(), 2).await.unwrap();
    assert_eq!(second.refolded, 1);
    assert_eq!(second.last_run_id.as_deref(), Some("r3"));
    let done = refold_runs(&pool, second.last_run_id.as_deref(), 2).await.unwrap();
    assert_eq!(done.refolded, 0);
    assert_eq!(done.last_run_id, None);

    assert_eq!(count().await, 3, "every run's attempt was backfilled");
    assert_eq!(folded().await, before, "the backfill restores exactly the rows the fold produced");
}

/// A run that claims one Batch job under two of its tasks (a producer's task-key bug, or a holder
/// of the run's capability token) folds that job into both tasks, while `attempt_projection` holds
/// one row per `(run_id, batch_job_id)`. The fold still persists, with one row for the job (the
/// task its Batch facts reached), so the batch that carries the second claim is not refused.
#[tokio::test]
#[ignore = "requires the LocalStack/Postgres harness"]
async fn one_job_claimed_by_two_tasks_of_a_run_keeps_one_attempt_row() {
    let pool = common::isolated_pool("one_job_claimed_by_two_tasks_of_a_run").await;
    let claim =
        |task: &str, ts: f64| task_submitted(ts, &format!("dk-sub-{task}"), "r1", task, "job-1");
    let started = ev(
        3.0,
        EventSource::AwsBatch,
        "dk-aws-job-1-started",
        EventData::TaskStarted { started_at: None, log_stream: Some("ls/1".into()) },
    )
    .with_job("job-1");
    let other_run = submitted(2.5, "dk-other", "r2");
    append(&pool, &claim("b:sample=A", 1.0)).await.unwrap();
    // The second claim shares its batch with another run's event: neither is refused.
    let appended = ingest_batch(&pool, &[claim("a", 2.0), other_run], &[]).await.unwrap();
    assert_eq!(appended, 2);
    append(&pool, &started).await.unwrap();

    let rows = || async {
        sqlx::query_as::<_, (String, String, String)>(
            "SELECT batch_job_id, task_key, state FROM attempt_projection WHERE run_id = 'r1'",
        )
        .fetch_all(&pool)
        .await
        .unwrap()
    };
    // The first claim bound the job, so its Batch facts (RUNNING) reached that task.
    let folded = rows().await;
    assert_eq!(folded, [("job-1".into(), "b:sample=A".into(), "running".into())]);
    refold_runs(&pool, None, 10).await.unwrap();
    assert_eq!(rows().await, folded, "a refold persists the same row");
    let runs: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM run_projection").fetch_one(&pool).await.unwrap();
    assert_eq!(runs, 2);
}
