//! Full-path drain tests (the SQS handler + Postgres). `#[ignore]`d — require the Postgres harness
//! AND --features aws:
//!
//!   docker compose -f dev/docker-compose.yml up -d postgres
//!   BIFFIX_TEST_PG=postgres://biffix:biffix@localhost:5433/biffix \
//!     cargo test -p biffix-ingest --features aws --test it -- --ignored drain_pg::
//!
//! Feed synthetic SQS batches through the real drain → append → projection path: a bound
//! coordinator terminal reaches the run's projection, bad records are dropped without poisoning the
//! batch, and a record whose own append fails is the only one handed back to SQS.

#![cfg(feature = "aws")]

use aws_lambda_events::sqs::{SqsBatchResponse, SqsEvent, SqsMessage};
use biffix_core::dedup::aws_batch_execution_ended;
use biffix_core::event::EventData;
use biffix_core::eventbridge::decode::QueueContext;
use biffix_core::eventbridge::NormalizedIngestEnvelope;
use biffix_core::source::EventSource;
use biffix_core::testing::{
    event, execution_log_stream, execution_task_arn, retried_job_detail, RunEventExt,
    RETRIED_JOB_ID,
};
use biffix_ingest::drain::handle_sqs_batch;

use crate::common;

/// The message ids a batch response hands back to SQS.
fn failed_ids(response: &SqsBatchResponse) -> Vec<&str> {
    response.batch_item_failures.iter().map(|failure| failure.item_identifier.as_str()).collect()
}

fn ctx() -> QueueContext {
    QueueContext { coordinator_queues: vec!["biffix-dev-coordinator".into()] }
}

// The SQS types are `#[non_exhaustive]`: built from their defaults, not struct literals.
fn sqs_msg(id: &str, body: Option<String>) -> SqsMessage {
    let mut message = SqsMessage::default();
    message.message_id = Some(id.into());
    message.body = body;
    message
}

fn sqs_event(records: Vec<SqsMessage>) -> SqsEvent {
    let mut event = SqsEvent::default();
    event.records = records;
    event
}

/// The control plane's coordinator binding for `run-drain`, as the reconciler-style envelope.
fn coordinator_submitted(dedup_key: &str) -> String {
    let event = common::coordinator_bound("run-drain", 1.0, "coord-1", dedup_key);
    serde_json::to_string(&NormalizedIngestEnvelope::new(event)).unwrap()
}

/// EventBridge's coordinator SUCCEEDED for `coord-1` (no run id: attributed via the binding).
fn coordinator_succeeded() -> String {
    serde_json::json!({
        "source": "aws.batch",
        "detail-type": "Batch Job State Change",
        "time": "2026-09-12T04:12:31Z",
        "detail": {
            "jobId": "coord-1",
            "jobQueue": "arn:aws:batch:us-east-1:1:job-queue/biffix-dev-coordinator",
            "status": "SUCCEEDED",
            "stoppedAt": 1789186351123_i64,
            "container": {"exitCode": 0}
        }
    })
    .to_string()
}

/// The dedup key of [`task_bound`]'s event.
const TASK_BOUND_KEY: &str = "engine:job-retried:submitted";

/// The engine's binding of the retried job to `run-drain`: attempt 1 of task `align`, as the
/// envelope the drain routes.
fn task_bound() -> String {
    let data = EventData::TaskSubmitted { image: None };
    let bound = event(1.0, EventSource::Snakemake, TASK_BOUND_KEY, data)
        .with_run("run-drain")
        .with_task("align")
        .with_attempt(1)
        .with_job(RETRIED_JOB_ID);
    serde_json::to_string(&NormalizedIngestEnvelope::new(bound)).unwrap()
}

/// EventBridge's SUCCEEDED for the retried job (the retried case: execution 1 lost with its
/// host, execution 2 finishing), with `stream_1` as execution 1's log stream.
fn retried_succeeded(stream_1: &str) -> String {
    let mut detail = retried_job_detail("SUCCEEDED", 681, 147, 1386);
    detail["attempts"][0]["container"]["logStreamName"] = stream_1.into();
    serde_json::json!({
        "source": "aws.batch",
        "detail-type": "Batch Job State Change",
        "time": "2026-09-12T04:12:31Z",
        "detail": detail,
    })
    .to_string()
}

/// The dedup keys the retried job's SUCCEEDED decodes to and [`task_bound`]'s, sorted.
fn retried_keys() -> Vec<String> {
    let mut keys = vec![
        TASK_BOUND_KEY.to_owned(),
        "aws-batch:job-retried:SUCCEEDED".to_owned(),
        aws_batch_execution_ended(&execution_task_arn(1)),
        aws_batch_execution_ended(&execution_task_arn(2)),
    ];
    keys.sort();
    keys
}

/// The log's dedup keys, sorted here (the database's collation is not fixed).
async fn sorted_dedup_keys(pool: &sqlx::PgPool) -> Vec<String> {
    let mut keys = dedup_keys(pool).await;
    keys.sort();
    keys
}

async fn dedup_keys(pool: &sqlx::PgPool) -> Vec<String> {
    sqlx::query_scalar("SELECT dedup_key FROM run_events ORDER BY dedup_key")
        .fetch_all(pool)
        .await
        .unwrap()
}

#[tokio::test]
#[ignore = "requires the LocalStack/Postgres harness + --features aws"]
async fn bound_coordinator_terminal_reaches_the_run_projection_and_bad_records_are_dropped() {
    let pool = common::isolated_pool("drain_bound_coordinator_terminal").await;

    let event = sqs_event(vec![
        sqs_msg("m-bind", Some(coordinator_submitted("cp:coord-1:submitted"))),
        sqs_msg("m-no-body", None),
        sqs_msg("m-not-json", Some("{not json".into())),
        // Tagged as biffix's own, but with no event: malformed, dropped.
        sqs_msg("m-bad-envelope", Some(r#"{"biffix_ingest":"run_event"}"#.into())),
        // Not an event biffix ingests: ignored.
        sqs_msg(
            "m-unrelated",
            Some(r#"{"source":"aws.s3","detail-type":"Object Created"}"#.into()),
        ),
        sqs_msg("m-terminal", Some(coordinator_succeeded())),
    ]);

    let outcome = handle_sqs_batch(&pool, &ctx(), &event).await.unwrap();
    assert_eq!(outcome.appended, 2, "only the two good records are appended");
    assert!(
        outcome.response.batch_item_failures.is_empty(),
        "dropped records are not retried: {:?}",
        outcome.response
    );
    assert_eq!(dedup_keys(&pool).await, ["aws-batch:coord-1:SUCCEEDED", "cp:coord-1:submitted"]);
    // The run-id-less AWS terminal folded into the run it is bound to.
    let (coordinator, completed): (Option<String>, Option<i64>) = sqlx::query_as(
        "SELECT coordinator_job_id, completed_at FROM run_projection WHERE run_id = 'run-drain'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(coordinator.as_deref(), Some("coord-1"));
    assert_eq!(completed, Some(1_789_186_351_123_000), "the coordinator terminal ended the run");
}

/// A redelivered batch appends nothing new, and the drain still runs the fold: an event an
/// earlier batch logged without folding it is folded then, rather than stranded.
#[tokio::test]
#[ignore = "requires the LocalStack/Postgres harness + --features aws"]
async fn a_redelivered_batch_still_folds_what_is_past_the_cursor() {
    let pool = common::isolated_pool("drain_redelivery_folds_the_unfolded").await;
    let event =
        sqs_event(vec![sqs_msg("m-bind", Some(coordinator_submitted("cp:coord-1:submitted")))]);
    assert_eq!(handle_sqs_batch(&pool, &ctx(), &event).await.unwrap().appended, 1);
    // Logged past the fold cursor, not folded.
    let late = common::coordinator_bound("run-late", 2.0, "coord-late", "cp:run-late:submitted");
    common::log_unfolded(&pool, &late, serde_json::to_value(&late).unwrap()).await;

    let again = handle_sqs_batch(&pool, &ctx(), &event).await.unwrap();
    assert_eq!(again.appended, 0);
    let coordinator: Option<String> = sqlx::query_scalar(
        "SELECT coordinator_job_id FROM run_projection WHERE run_id = 'run-late'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(coordinator.as_deref(), Some("coord-late"));
}

#[tokio::test]
#[ignore = "requires the LocalStack/Postgres harness + --features aws"]
async fn a_record_whose_append_fails_is_the_only_one_returned_to_sqs() {
    let pool = common::isolated_pool("drain_poison_record_partial_failure").await;

    // A NUL byte decodes fine but Postgres rejects it in text/jsonb: a hard DB error on append.
    let event = sqs_event(vec![
        sqs_msg("m-good", Some(coordinator_submitted("cp:coord-1:submitted"))),
        sqs_msg("m-poison", Some(coordinator_submitted("cp:poison\u{0}"))),
        sqs_msg("m-terminal", Some(coordinator_succeeded())),
    ]);

    let outcome = handle_sqs_batch(&pool, &ctx(), &event).await.unwrap();
    assert_eq!(
        failed_ids(&outcome.response),
        ["m-poison"],
        "only the poison record is redelivered"
    );
    assert_eq!(outcome.appended, 2);
    assert_eq!(
        dedup_keys(&pool).await,
        ["aws-batch:coord-1:SUCCEEDED", "cp:coord-1:submitted"],
        "the healthy records are committed"
    );

    // A redelivery of the whole batch appends nothing new and still isolates the poison record.
    let again = handle_sqs_batch(&pool, &ctx(), &event).await.unwrap();
    assert_eq!(again.appended, 0);
    assert_eq!(
        failed_ids(&again.response),
        ["m-poison"],
        "the redelivery hands back the poison record, and only it"
    );
}

/// A Batch state change decodes to several events (the job's terminal and its executions' ends).
/// The drain appends them all, and the fold records each execution under the job's attempt.
#[tokio::test]
#[ignore = "requires the LocalStack/Postgres harness + --features aws"]
async fn one_record_yielding_several_events_appends_them_together() {
    let pool = common::isolated_pool("drain_one_record_several_events").await;
    let event = sqs_event(vec![
        sqs_msg("m-bind", Some(task_bound())),
        sqs_msg("m-job", Some(retried_succeeded(&execution_log_stream(1)))),
    ]);

    let outcome = handle_sqs_batch(&pool, &ctx(), &event).await.unwrap();
    assert_eq!(outcome.appended, 4, "the binding, the terminal and two executions' ends");
    assert!(outcome.response.batch_item_failures.is_empty(), "{:?}", outcome.response);
    assert_eq!(sorted_dedup_keys(&pool).await, retried_keys());
    // Live JSON reached the execution projection through the drain's own fold.
    let executions: i64 =
        sqlx::query_scalar("SELECT count(*) FROM execution_projection WHERE run_id = 'run-drain'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(executions, 2);
}

/// When the batch's transaction fails on another record, a record with several events is
/// retried alone and all of its events are committed.
#[tokio::test]
#[ignore = "requires the LocalStack/Postgres harness + --features aws"]
async fn a_healthy_multi_event_record_survives_a_poison_neighbor() {
    let pool = common::isolated_pool("drain_multi_event_record_poison_neighbor").await;
    let event = sqs_event(vec![
        sqs_msg("m-bind", Some(task_bound())),
        sqs_msg("m-job", Some(retried_succeeded(&execution_log_stream(1)))),
        sqs_msg("m-poison", Some(coordinator_submitted("cp:poison\u{0}"))),
    ]);

    let outcome = handle_sqs_batch(&pool, &ctx(), &event).await.unwrap();
    assert_eq!(failed_ids(&outcome.response), ["m-poison"]);
    assert_eq!(outcome.appended, 4);
    assert_eq!(sorted_dedup_keys(&pool).await, retried_keys());
}

/// One record's events are appended in one transaction: when Postgres rejects one of them (a NUL
/// byte in execution 1's log stream), the record goes back to SQS whole and none of its events
/// is committed, not even the job's terminal, so its redelivery finds nothing half-recorded.
#[tokio::test]
#[ignore = "requires the LocalStack/Postgres harness + --features aws"]
async fn a_record_with_a_poison_execution_is_returned_whole() {
    let pool = common::isolated_pool("drain_poison_execution_returned_whole").await;
    let event = sqs_event(vec![
        sqs_msg("m-bind", Some(task_bound())),
        sqs_msg("m-bad", Some(retried_succeeded("ls\u{0}"))),
    ]);

    let outcome = handle_sqs_batch(&pool, &ctx(), &event).await.unwrap();
    assert_eq!(failed_ids(&outcome.response), ["m-bad"]);
    assert_eq!(outcome.appended, 1, "only the binding is committed");
    let keys = dedup_keys(&pool).await;
    assert_eq!(keys, [TASK_BOUND_KEY], "none of the bad record's events is in the log");
    assert!(!keys.iter().any(|key| key == "aws-batch:job-retried:SUCCEEDED"));
    assert!(!keys.iter().any(|key| key.starts_with("aws-batch-exec:")));
}

/// What the drain Lambda does at the end of every invocation (`biffix_db::pool::release_idle`)
/// leaves no connection open, server-side, into the frozen period between invocations; the pool
/// still serves the next invocation.
#[tokio::test]
#[ignore = "requires the LocalStack/Postgres harness + --features aws"]
async fn an_invocation_ends_with_no_connection_open() {
    let pool = common::isolated_pool("drain_releases_its_connections").await;
    // Several connections open at once, as a busy invocation's would be.
    let mut held = Vec::new();
    let mut pids = Vec::new();
    for _ in 0..3 {
        let mut connection = pool.acquire().await.unwrap();
        let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&mut *connection)
            .await
            .unwrap();
        pids.push(pid);
        held.push(connection);
    }
    drop(held);
    let event =
        sqs_event(vec![sqs_msg("m-bind", Some(coordinator_submitted("cp:coord-1:submitted")))]);
    assert_eq!(handle_sqs_batch(&pool, &ctx(), &event).await.unwrap().appended, 1);
    assert!(pool.size() >= 3);

    assert!(biffix_db::pool::release_idle(&pool).await >= 3);
    assert_eq!(pool.size(), 0, "the pool holds no connection");
    // Postgres ends a backend once its client says goodbye; wait for that, briefly.
    let url = std::env::var("BIFFIX_TEST_PG").unwrap();
    let observer = sqlx::PgPool::connect(&url).await.unwrap();
    let mut open = i64::MAX;
    for _ in 0..50 {
        open = sqlx::query_scalar("SELECT count(*) FROM pg_stat_activity WHERE pid = ANY($1)")
            .bind(&pids)
            .fetch_one(&observer)
            .await
            .unwrap();
        if open == 0 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    observer.close().await;
    assert_eq!(open, 0, "no server-side connection of the pool's is left");

    // The next invocation connects again.
    assert_eq!(dedup_keys(&pool).await, ["cp:coord-1:submitted"]);
}
