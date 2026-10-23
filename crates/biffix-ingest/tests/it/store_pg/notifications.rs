//! Notifications: one per terminal run, rebuilt by a refold, re-keyed by a later batch.

use super::*;

/// The `notifications` rows, as `(id, user_sub, run_id, kind, at)`, by id.
async fn notification_rows(pool: &sqlx::PgPool) -> Vec<(String, String, String, String, i64)> {
    sqlx::query_as("SELECT id, user_sub, run_id, kind, at FROM notifications ORDER BY id")
        .fetch_all(pool)
        .await
        .unwrap()
}

/// Runs `a` (succeeded), `b` (failed before launch), `c` (cancelled on bob's request) submitted by
/// alice, and `w` (succeeded) triggered by a watcher: every terminal kind, and one nobody is told.
fn notified_runs_log() -> Vec<RunEvent> {
    let anchor = |run: &str, submitter: &str| {
        let a = biffix_core::event::RunSubmitted {
            submitter: Some(submitter.into()),
            submitter_label: Some("alice@lab.example.org".into()),
            ..biffix_core::testing::anchor()
        };
        biffix_core::event::run_submitted(&RunId::new(run), Timestamp::from_epoch_seconds(1.0), a)
    };
    vec![
        anchor("a", "user:alice"),
        run_ev(5.0, EventSource::Snakemake, "dk-a-ok", "a", EventData::RunSucceeded {}),
        anchor("b", "user:alice"),
        biffix_core::event::run_failed(
            &RunId::new("b"),
            Timestamp::from_epoch_seconds(6.0),
            biffix_core::event::FAILURE_STAGE_LAUNCH,
            "no capacity",
        ),
        anchor("c", "user:alice"),
        biffix_core::event::cancel_requested(
            &RunId::new("c"),
            Timestamp::from_epoch_seconds(2.0),
            "bob",
            Some("bob@lab.example.org"),
        ),
        biffix_core::event::run_cancelled(&RunId::new("c"), Timestamp::from_epoch_seconds(7.0)),
        anchor("w", "watcher:w1"),
        run_ev(8.0, EventSource::Snakemake, "dk-w-ok", "w", EventData::RunSucceeded {}),
    ]
}

#[tokio::test]
#[ignore = "requires the LocalStack/Postgres harness"]
async fn each_terminal_run_notifies_its_submitter_once_and_redelivery_is_a_no_op() {
    let pool = common::isolated_pool("notifications_written_on_each_terminal").await;
    let log = notified_runs_log();
    ingest_batch(&pool, &log, &[]).await.unwrap();
    let us = |s: f64| Timestamp::from_epoch_seconds(s).as_micros();
    let expected = vec![
        (
            "a:succeeded".to_string(),
            "alice".to_string(),
            "a".to_string(),
            "succeeded".to_string(),
            us(5.0),
        ),
        ("b:failed".into(), "alice".into(), "b".into(), "failed".into(), us(6.0)),
        ("c:cancelled".into(), "alice".into(), "c".into(), "cancelled".into(), us(7.0)),
    ];
    assert_eq!(notification_rows(&pool).await, expected);
    // A redelivery (and an advance with nothing new) leaves the same rows.
    assert_eq!(ingest_batch(&pool, &log, &[]).await.unwrap(), 0);
    assert_eq!(notification_rows(&pool).await, expected);

    // Who cancelled is folded onto the run row, and only the cancelled run has one.
    let cancelled_by: Vec<(String, Option<String>)> =
        sqlx::query_as("SELECT run_id, cancelled_by FROM run_projection ORDER BY run_id")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(
        cancelled_by,
        vec![
            ("a".to_string(), None),
            ("b".into(), None),
            ("c".into(), Some("bob@lab.example.org".into())),
            ("w".into(), None),
        ]
    );
}

#[tokio::test]
#[ignore = "requires the LocalStack/Postgres harness"]
async fn a_refold_rebuilds_notifications_and_leaves_reads_alone() {
    let pool = common::isolated_pool("notifications_rebuilt_by_refold").await;
    ingest_batch(&pool, &notified_runs_log(), &[]).await.unwrap();
    let folded = notification_rows(&pool).await;
    sqlx::query(
        "INSERT INTO notification_reads (user_sub, notification_id) VALUES ('alice', 'b:failed')",
    )
    .execute(&pool)
    .await
    .unwrap();
    // Lose the projection, and plant a row no fold produces (a stale kind of run `a`): the refold
    // makes the table equal the fold again.
    sqlx::query("DELETE FROM notifications").execute(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO notifications (id, user_sub, run_id, kind, at) \
         VALUES ('a:failed', 'alice', 'a', 'failed', 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    refold_runs(&pool, None, 100).await.unwrap();
    assert_eq!(notification_rows(&pool).await, folded);
    let reads: Vec<(String, String)> =
        sqlx::query_as("SELECT user_sub, notification_id FROM notification_reads")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(reads, vec![("alice".to_string(), "b:failed".to_string())]);
}

#[tokio::test]
#[ignore = "requires the LocalStack/Postgres harness"]
async fn a_later_batch_re_keys_a_failed_notification_to_cancelled() {
    let pool = common::isolated_pool("notifications_re_keyed_incrementally").await;
    let log = notified_runs_log();
    // Run `b` fails first (and `live` is still going: no row for it, before or after)...
    let live = biffix_core::event::RunSubmitted {
        submitter: Some("user:alice".into()),
        ..biffix_core::testing::anchor()
    };
    let first = vec![
        log[2].clone(),
        log[3].clone(),
        biffix_core::event::run_submitted(
            &RunId::new("live"),
            Timestamp::from_epoch_seconds(1.0),
            live,
        ),
    ];
    ingest_batch(&pool, &first, &[]).await.unwrap();
    let ids = |rows: &[(String, String, String, String, i64)]| {
        rows.iter().map(|r| r.0.clone()).collect::<Vec<_>>()
    };
    assert_eq!(ids(&notification_rows(&pool).await), ["b:failed"]);
    // ...then a later batch brings the cancel it was asked for: the fold's state is now cancelled,
    // and the incremental advance replaces the row, as a replay would.
    let cancel = biffix_core::event::cancel_requested(
        &RunId::new("b"),
        Timestamp::from_epoch_seconds(5.0),
        "alice",
        None,
    );
    ingest_batch(&pool, &[cancel], &[]).await.unwrap();
    let rows = notification_rows(&pool).await;
    assert_eq!(ids(&rows), ["b:cancelled"]);
    assert_eq!(rows[0].4, Timestamp::from_epoch_seconds(6.0).as_micros());
    refold_runs(&pool, None, 100).await.unwrap();
    assert_eq!(notification_rows(&pool).await, rows);
}
