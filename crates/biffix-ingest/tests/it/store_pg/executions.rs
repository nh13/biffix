//! The execution projection: a Batch job's executions as `execution_projection` rows, and the
//! run, lost and wait time they give their attempt.

use super::*;

use biffix_core::event::TerminationCategory;
use biffix_core::testing::{
    execution_ended, execution_log_stream, execution_started, execution_task_arn,
    RETRIED_LOST_REASON,
};
use sqlx::postgres::PgRow;

/// The start of execution `n` (its task ARN the id) of `job` at `at_s`, on host `ci{n}` and
/// queue `q-spot`.
fn exec_started(job: &str, n: u32, at_s: f64) -> RunEvent {
    execution_started(job, &execution_task_arn(n), at_s)
        .with_instance(&format!("ci{n}"))
        .with_queue("q-spot")
}

/// The end of execution `n` of `job` at `stopped_s`, on host `ci{n}` and queue `q-spot`, with its
/// own log stream. One categorized `host_terminated` was lost with its host: it carries Batch's
/// reason for that and no exit code. Any other exited 0.
fn exec_ended(
    job: &str,
    n: u32,
    started_s: Option<f64>,
    stopped_s: f64,
    category: Option<TerminationCategory>,
) -> RunEvent {
    let lost = category == Some(TerminationCategory::HostTerminated);
    let mut e = execution_ended(job, &execution_task_arn(n), started_s, stopped_s, category)
        .with_instance(&format!("ci{n}"))
        .with_queue("q-spot");
    if let EventData::BatchExecutionEnded { exit_code, status_reason, log_stream, .. } = &mut e.data
    {
        *exit_code = (!lost).then_some(0);
        *status_reason = lost.then(|| RETRIED_LOST_REASON.to_owned());
        *log_stream = Some(execution_log_stream(n));
    }
    e
}

/// The retried case as `r1`'s one attempt: `job-1` of task `align:sample=A`, which AWS ran twice
/// (execution 1 lost with its host, execution 2 finishing). The job's own lifecycle names only
/// the first host.
fn retried_log() -> Vec<RunEvent> {
    let aws = |ts: f64, dedup: &str, data: EventData| {
        ev(ts, EventSource::AwsBatch, dedup, data).with_job("job-1")
    };
    vec![
        task_submitted(1.0, "dk-sub", "r1", "align:sample=A", "job-1"),
        aws(2.0, "dk-start", EventData::TaskStarted { started_at: None, log_stream: None })
            .with_instance("ci1"),
        exec_ended("job-1", 1, Some(3.0), 684.0, Some(TerminationCategory::HostTerminated)),
        exec_ended("job-1", 2, Some(831.0), 2217.0, None),
        aws(2217.0, "dk-done", EventData::TaskSucceeded { exit_code: 0, log_stream: None }),
    ]
}

/// Append `events` one by one; each must be accepted (a fold the database refused would fail its
/// append).
async fn append_all(pool: &sqlx::PgPool, events: &[RunEvent]) {
    for e in events {
        append(pool, e).await.unwrap();
    }
}

/// `run`'s `execution_projection` rows, by `ordinal` then (two jobs share ordinals) job and
/// execution id.
async fn execution_rows(pool: &sqlx::PgPool, run: &str) -> Vec<PgRow> {
    let mut rows =
        sqlx::query("SELECT * FROM execution_projection WHERE run_id = $1 ORDER BY ordinal")
            .bind(run)
            .fetch_all(pool)
            .await
            .unwrap();
    // The text columns are sorted here, not by the database, whose collation is not fixed.
    rows.sort_by_key(|row| {
        (
            row.get::<i32, _>("ordinal"),
            row.get::<String, _>("batch_job_id"),
            row.get::<String, _>("execution_id"),
        )
    });
    rows
}

#[tokio::test]
#[ignore = "requires the LocalStack/Postgres harness"]
async fn execution_rows_match_the_fold() {
    let pool = common::isolated_pool("execution_rows_match_the_fold").await;
    append_all(&pool, &retried_log()).await;

    let rows = execution_rows(&pool, "r1").await;
    assert_eq!(rows.len(), 2);
    let text = |row: &PgRow, column: &str| row.get::<Option<String>, _>(column);

    let lost = &rows[0];
    assert_eq!(lost.get::<String, _>("execution_id"), execution_task_arn(1));
    assert_eq!(lost.get::<String, _>("batch_job_id"), "job-1");
    assert_eq!(lost.get::<String, _>("role"), "task");
    assert_eq!(text(lost, "task_key").as_deref(), Some("align:sample=A"));
    assert_eq!(lost.get::<i32, _>("ordinal"), 1);
    assert!(!lost.get::<bool, _>("is_final"));
    assert!(lost.get::<bool, _>("ended"));
    assert_eq!(text(lost, "instance_id").as_deref(), Some("ci1"));
    assert_eq!(text(lost, "queue").as_deref(), Some("q-spot"));
    assert_eq!(text(lost, "log_stream"), Some(execution_log_stream(1)));
    assert_eq!(lost.get::<Option<i64>, _>("started_at"), Some(3_000_000));
    assert_eq!(lost.get::<Option<i64>, _>("stopped_at"), Some(684_000_000));
    assert_eq!(lost.get::<Option<i32>, _>("exit_code"), None);
    assert_eq!(text(lost, "status_reason").as_deref(), Some(RETRIED_LOST_REASON));
    assert_eq!(text(lost, "termination_category").as_deref(), Some("host_terminated"));

    let last = &rows[1];
    assert_eq!(last.get::<String, _>("execution_id"), execution_task_arn(2));
    assert_eq!(last.get::<i32, _>("ordinal"), 2);
    assert!(last.get::<bool, _>("is_final"));
    assert!(last.get::<bool, _>("ended"));
    assert_eq!(text(last, "instance_id").as_deref(), Some("ci2"));
    assert_eq!(last.get::<Option<i64>, _>("started_at"), Some(831_000_000));
    assert_eq!(last.get::<Option<i64>, _>("stopped_at"), Some(2_217_000_000));
    assert_eq!(last.get::<Option<i32>, _>("exit_code"), Some(0));
    assert_eq!(text(last, "status_reason"), None);
    assert_eq!(text(last, "termination_category"), None);

    // The attempt gains what its span was made of, and keeps the span and host it had.
    let attempt = sqlx::query(
        "SELECT run_micros, lost_micros, wait_micros, started_at, finished_at, instance_id, state \
           FROM attempt_projection WHERE run_id = 'r1' AND batch_job_id = 'job-1'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(attempt.get::<Option<i64>, _>("run_micros"), Some(2_067_000_000));
    assert_eq!(attempt.get::<Option<i64>, _>("lost_micros"), Some(681_000_000));
    assert_eq!(attempt.get::<Option<i64>, _>("wait_micros"), Some(147_000_000));
    assert_eq!(attempt.get::<Option<i64>, _>("started_at"), Some(2_000_000));
    assert_eq!(attempt.get::<Option<i64>, _>("finished_at"), Some(2_217_000_000));
    assert_eq!(text(&attempt, "instance_id").as_deref(), Some("ci1"));
    assert_eq!(attempt.get::<String, _>("state"), "succeeded");
}

/// A job with no execution events (one from before they were recorded) has no durations, which
/// is not the same as zero.
#[tokio::test]
#[ignore = "requires the LocalStack/Postgres harness"]
async fn an_attempt_without_execution_facts_has_null_durations() {
    let pool = common::isolated_pool("an_attempt_without_execution_facts").await;
    let log = retried_log();
    append_all(&pool, &[log[0].clone(), log[1].clone(), log[4].clone()]).await;

    let attempt = sqlx::query(
        "SELECT run_micros, lost_micros, wait_micros, state FROM attempt_projection \
          WHERE run_id = 'r1' AND batch_job_id = 'job-1'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(attempt.get::<String, _>("state"), "succeeded");
    for column in ["run_micros", "lost_micros", "wait_micros"] {
        assert_eq!(attempt.get::<Option<i64>, _>(column), None, "{column}");
    }
    assert!(execution_rows(&pool, "r1").await.is_empty());
}

#[tokio::test]
#[ignore = "requires the LocalStack/Postgres harness"]
async fn a_coordinators_execution_is_a_row_with_no_task_key() {
    let pool = common::isolated_pool("a_coordinators_execution_is_a_row").await;
    append_all(
        &pool,
        &[
            common::coordinator_bound("r1", 1.0, "coord-1", "cp:coord-1"),
            exec_started("coord-1", 1, 3.0),
        ],
    )
    .await;

    let rows = execution_rows(&pool, "r1").await;
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(row.get::<String, _>("execution_id"), execution_task_arn(1));
    assert_eq!(row.get::<String, _>("role"), "coordinator");
    assert_eq!(row.get::<Option<String>, _>("task_key"), None);
    assert_eq!(row.get::<String, _>("batch_job_id"), "coord-1");
    assert!(!row.get::<bool, _>("ended"));
    assert!(row.get::<bool, _>("is_final"));
    assert_eq!(row.get::<i32, _>("ordinal"), 1);
    assert_eq!(row.get::<Option<i64>, _>("started_at"), Some(3_000_000));
    assert_eq!(row.get::<Option<i64>, _>("stopped_at"), None);
}

/// A refold makes the table equal the fold: a row no execution event accounts for goes, in a run
/// that has executions and in one that has none.
#[tokio::test]
#[ignore = "requires the LocalStack/Postgres harness"]
async fn a_refold_deletes_execution_rows_the_fold_no_longer_produces() {
    let pool = common::isolated_pool("a_refold_deletes_execution_rows").await;
    append_all(&pool, &retried_log()).await;
    append(&pool, &task_submitted(1.0, "dk-sub-r2", "r2", "align:sample=B", "job-2"))
        .await
        .unwrap();
    for (run, job) in [("r1", "job-1"), ("r2", "job-2")] {
        sqlx::query(
            "INSERT INTO execution_projection \
               (run_id, execution_id, batch_job_id, role, ordinal, is_final, ended) \
             VALUES ($1, 'stale', $2, 'task', 9, false, false)",
        )
        .bind(run)
        .bind(job)
        .execute(&pool)
        .await
        .unwrap();
    }
    // And stale durations on r2's attempt, whose fold has no executions.
    sqlx::query(
        "UPDATE attempt_projection SET run_micros = 1, lost_micros = 1, wait_micros = 1 \
          WHERE run_id = 'r2'",
    )
    .execute(&pool)
    .await
    .unwrap();

    refold_runs(&pool, None, 100).await.unwrap();

    let durations = |run: &'static str| {
        let pool = pool.clone();
        async move {
            let rows: Vec<(Option<i64>, Option<i64>, Option<i64>)> = sqlx::query_as(
                "SELECT run_micros, lost_micros, wait_micros FROM attempt_projection \
                  WHERE run_id = $1",
            )
            .bind(run)
            .fetch_all(&pool)
            .await
            .unwrap();
            rows
        }
    };
    assert_eq!(durations("r2").await, [(None, None, None)]);
    assert_eq!(
        durations("r1").await,
        [(Some(2_067_000_000), Some(681_000_000), Some(147_000_000))]
    );

    let ids = |rows: Vec<PgRow>| -> Vec<String> {
        rows.iter().map(|row| row.get::<String, _>("execution_id")).collect()
    };
    assert_eq!(
        ids(execution_rows(&pool, "r1").await),
        [execution_task_arn(1), execution_task_arn(2)]
    );
    assert!(execution_rows(&pool, "r2").await.is_empty());
}

/// One statement never carries two rows for one execution (SQLSTATE 21000 would refuse the run's
/// whole fold), however many claims or bindings its job has.
#[tokio::test]
#[ignore = "requires the LocalStack/Postgres harness"]
async fn a_job_bound_twice_writes_each_execution_once() {
    let pool = common::isolated_pool("a_job_bound_twice_writes_each_execution_once").await;
    // The end of execution `n` of `job`, lost at 9 s, as an event with no queue: it gives the
    // attempt no fact of its own (no state, queue, host or log stream), only an execution.
    let queueless_end = |job: &str, n: u32| {
        execution_ended(
            job,
            &execution_task_arn(n),
            Some(3.0),
            9.0,
            Some(TerminationCategory::HostTerminated),
        )
    };

    // (a) Two tasks of one run claim one job; the first claim binds it.
    append_all(
        &pool,
        &[
            task_submitted(1.0, "dk-a", "r1", "a", "job-1"),
            task_submitted(2.0, "dk-b", "r1", "b", "job-1"),
            queueless_end("job-1", 1),
        ],
    )
    .await;
    let rows = execution_rows(&pool, "r1").await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get::<Option<String>, _>("task_key").as_deref(), Some("a"));

    // The same, with the binding claim on the task whose key sorts last: the execution is the
    // job's only fact, and it still picks the attempt that holds it over the stand-in claim.
    append_all(
        &pool,
        &[
            task_submitted(1.0, "dk-z", "r3", "z", "job-3"),
            task_submitted(2.0, "dk-y", "r3", "a", "job-3"),
            queueless_end("job-3", 3),
        ],
    )
    .await;
    let rows = execution_rows(&pool, "r3").await;
    assert_eq!(rows.len(), 1, "the bound attempt's execution is kept");
    assert_eq!(rows[0].get::<Option<String>, _>("task_key").as_deref(), Some("z"));
    let (task_key, lost_micros): (String, Option<i64>) = sqlx::query_as(
        "SELECT task_key, lost_micros FROM attempt_projection \
          WHERE run_id = 'r3' AND batch_job_id = 'job-3'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!((task_key.as_str(), lost_micros), ("z", Some(6_000_000)));

    // (b) A job bound as a task's, then as the run's coordinator: its execution folds under
    // both, and the coordinator's row is the one written, with what each half saw: here only the
    // start named the log stream. (Each case has its own execution: an execution's events are
    // deduplicated by its id across the whole log.)
    let mut start_only_stream = exec_started("job-x", 2, 3.0);
    if let EventData::BatchExecutionStarted { log_stream, .. } = &mut start_only_stream.data {
        *log_stream = Some("only-the-start".into());
    }
    let mut streamless_end = exec_ended("job-x", 2, Some(3.0), 9.0, None);
    if let EventData::BatchExecutionEnded { log_stream, .. } = &mut streamless_end.data {
        *log_stream = None;
    }
    // Execution 4 ran and was lost under the task binding alone.
    append_all(
        &pool,
        &[
            task_submitted(1.0, "dk-x", "r2", "x", "job-x"),
            exec_ended("job-x", 4, Some(1.5), 2.5, Some(TerminationCategory::HostTerminated)),
            start_only_stream,
            common::coordinator_bound("r2", 5.0, "job-x", "cp:job-x"),
            streamless_end,
        ],
    )
    .await;
    let rows = execution_rows(&pool, "r2").await;
    assert_eq!(rows.len(), 2);
    let row = |n: u32| {
        rows.iter().find(|r| r.get::<String, _>("execution_id") == execution_task_arn(n)).unwrap()
    };
    assert_eq!(row(2).get::<String, _>("role"), "coordinator");
    assert_eq!(row(2).get::<Option<String>, _>("task_key"), None);
    assert!(row(2).get::<bool, _>("ended"));
    assert_eq!(row(2).get::<Option<String>, _>("log_stream").as_deref(), Some("only-the-start"));
    // Each binding numbers the rows it keeps, and has exactly one final row: the attempt's last
    // execution is execution 4 now that execution 2 is the coordinator's.
    let place = |r: &PgRow| (r.get::<i32, _>("ordinal"), r.get::<bool, _>("is_final"));
    assert_eq!(row(4).get::<String, _>("role"), "task");
    assert_eq!((place(row(4)), place(row(2))), ((1, true), (1, true)));
}

/// Another run's engine cannot take a coordinator's executions by claiming its job as a task:
/// the coordinator binding is authoritative, so the owner run gets the execution and a forger,
/// whether its claim is stamped before the binding or after everything else, gets none. Each
/// append folds every run the job's events reach, and so does a refold.
#[tokio::test]
#[ignore = "requires the LocalStack/Postgres harness"]
async fn a_forged_claim_on_a_coordinator_job_gets_none_of_its_executions() {
    let pool = common::isolated_pool("a_forged_claim_gets_no_executions").await;
    append_all(
        &pool,
        &[
            task_submitted(1.0, "dk-early", "forger-early", "evil", "coord-1"),
            common::coordinator_bound("owner", 2.0, "coord-1", "cp:coord-1"),
            exec_started("coord-1", 1, 3.0),
            exec_ended("coord-1", 1, Some(3.0), 9.0, None),
            task_submitted(10.0, "dk-late", "forger-late", "evil", "coord-1"),
        ],
    )
    .await;

    for pass in ["append", "refold"] {
        if pass == "refold" {
            refold_runs(&pool, None, 100).await.unwrap();
        }
        let rows = execution_rows(&pool, "owner").await;
        assert_eq!(rows.len(), 1, "{pass}");
        assert_eq!(rows[0].get::<String, _>("execution_id"), execution_task_arn(1), "{pass}");
        assert_eq!(rows[0].get::<String, _>("role"), "coordinator", "{pass}");
        assert!(rows[0].get::<bool, _>("ended"), "{pass}");
        for forger in ["forger-early", "forger-late"] {
            assert!(execution_rows(&pool, forger).await.is_empty(), "{pass}: {forger}");
            let durations: Vec<(Option<i64>, Option<i64>, Option<i64>)> = sqlx::query_as(
                "SELECT run_micros, lost_micros, wait_micros FROM attempt_projection \
                  WHERE run_id = $1",
            )
            .bind(forger)
            .fetch_all(&pool)
            .await
            .unwrap();
            assert_eq!(durations, [(None, None, None)], "{pass}: {forger}");
        }
    }
}
