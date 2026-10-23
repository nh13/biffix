//! The event-log writer: every batch advances the fold, including one that appends nothing new, and
//! a stored row that no longer deserializes never stops it.

use super::*;
use common::log_unfolded;

/// The fold cursor (`projection_cursors.last_seq`).
async fn folded_through(pool: &sqlx::PgPool) -> i64 {
    sqlx::query_scalar("SELECT last_seq FROM projection_cursors WHERE projection = 'run_task'")
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn run_state(pool: &sqlx::PgPool, run: &str) -> Option<String> {
    sqlx::query_scalar("SELECT state FROM run_projection WHERE run_id = $1")
        .bind(run)
        .fetch_optional(pool)
        .await
        .unwrap()
}

/// A redelivered batch appends nothing new, and must still fold what an earlier batch logged:
/// gating the advance on new appends would strand those events unfolded for good.
#[tokio::test]
#[ignore = "requires the LocalStack/Postgres harness"]
async fn an_advance_with_nothing_new_folds_what_is_past_the_cursor() {
    let pool = common::isolated_pool("an_advance_with_nothing_new_folds").await;
    let late = submitted(1.0, "dk-late", "r1");
    let seq = log_unfolded(&pool, &late, serde_json::to_value(&late).unwrap()).await;
    assert_eq!(run_state(&pool, "r1").await, None);

    assert_eq!(ingest_batch(&pool, &[], &[]).await.unwrap(), 0);
    assert_eq!(run_state(&pool, "r1").await.as_deref(), Some("submitted"));
    assert!(folded_through(&pool).await >= seq);
}

/// A stored row that no longer deserializes is skipped, so one poison row never stops the fold
/// for every run: later events still fold, and the cursor moves past it.
#[tokio::test]
#[ignore = "requires the LocalStack/Postgres harness"]
async fn a_row_that_no_longer_deserializes_never_stops_the_fold() {
    let pool = common::isolated_pool("a_row_that_no_longer_deserializes").await;
    append(&pool, &submitted(1.0, "dk-r1", "r1")).await.unwrap();
    append(&pool, &submitted(1.0, "dk-r2", "r2")).await.unwrap();
    let poison = run_ev(
        2.0,
        EventSource::Snakemake,
        "dk-poison",
        "r1",
        EventData::RunStarted { session_id: None },
    );
    let poison_seq = log_unfolded(&pool, &poison, serde_json::json!({})).await;

    let started = run_ev(
        3.0,
        EventSource::Snakemake,
        "dk-r2-started",
        "r2",
        EventData::RunStarted { session_id: None },
    );
    assert!(append(&pool, &started).await.unwrap());
    assert_eq!(run_state(&pool, "r2").await.as_deref(), Some("running"));
    // r1 is folded without the row it can no longer read.
    assert_eq!(run_state(&pool, "r1").await.as_deref(), Some("submitted"));
    assert!(folded_through(&pool).await > poison_seq);
}
