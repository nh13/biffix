//! A run's experiment and name, folded from its anchor and latest change.

use super::*;

/// Each run's folded experiment, by run id.
async fn experiment_rows(pool: &sqlx::PgPool) -> Vec<(String, Option<String>)> {
    sqlx::query_as("SELECT run_id, experiment_id FROM run_projection ORDER BY run_id")
        .fetch_all(pool)
        .await
        .unwrap()
}

#[tokio::test]
#[ignore = "requires the LocalStack/Postgres harness"]
async fn a_run_s_experiment_folds_from_its_anchor_and_latest_set_and_a_refold_rebuilds_it() {
    let pool = common::isolated_pool("experiment_set_folds").await;
    let at = Timestamp::from_epoch_seconds;
    let launched = |run: &str, experiment: Option<&str>| {
        let a = biffix_core::event::RunSubmitted {
            experiment_id: experiment.map(Into::into),
            ..biffix_core::testing::anchor()
        };
        biffix_core::event::run_submitted(&RunId::new(run), at(1.0), a)
    };
    let set = |run: &str, s: f64, experiment: Option<&str>| {
        biffix_core::event::experiment_set(&RunId::new(run), at(s), experiment, "alice")
    };
    // `a` is launched into e1 and stays; `b` into e1 then moved to e2; `c` into none, added to e1
    // then taken out; `d` moved e1 → e2 → e1 (a later change back to an earlier experiment).
    ingest_batch(
        &pool,
        &[
            launched("a", Some("e1")),
            launched("b", Some("e1")),
            launched("c", None),
            launched("d", None),
            set("d", 2.0, Some("e1")),
        ],
        &[],
    )
    .await
    .unwrap();
    // Later batches: each change advances the run row (out of order within the batch, too).
    ingest_batch(
        &pool,
        &[
            set("c", 3.0, None),
            set("b", 2.0, Some("e2")),
            set("c", 2.0, Some("e1")),
            set("d", 3.0, Some("e2")),
        ],
        &[],
    )
    .await
    .unwrap();
    ingest_batch(&pool, &[set("d", 4.0, Some("e1"))], &[]).await.unwrap();
    let expected = vec![
        ("a".to_string(), Some("e1".to_string())),
        ("b".into(), Some("e2".into())),
        ("c".into(), None),
        ("d".into(), Some("e1".into())),
    ];
    assert_eq!(experiment_rows(&pool).await, expected);
    // A re-sent change is the same fact: nothing new is appended and nothing moves.
    assert_eq!(ingest_batch(&pool, &[set("d", 4.0, Some("e1"))], &[]).await.unwrap(), 0);
    // Lose the column: a refold rebuilds it from the log.
    sqlx::query("UPDATE run_projection SET experiment_id = 'stale'").execute(&pool).await.unwrap();
    refold_runs(&pool, None, 100).await.unwrap();
    assert_eq!(experiment_rows(&pool).await, expected);
}

/// Each run's folded name, by run id.
async fn name_rows(pool: &sqlx::PgPool) -> Vec<(String, Option<String>)> {
    sqlx::query_as("SELECT run_id, run_name FROM run_projection ORDER BY run_id")
        .fetch_all(pool)
        .await
        .unwrap()
}

#[tokio::test]
#[ignore = "requires the LocalStack/Postgres harness"]
async fn a_run_s_name_folds_from_its_anchor_and_latest_rename_and_a_refold_rebuilds_it() {
    let pool = common::isolated_pool("run_renamed_folds").await;
    let at = Timestamp::from_epoch_seconds;
    let launched = |run: &str, name: Option<&str>| {
        let a = biffix_core::event::RunSubmitted {
            run_name: name.map(Into::into),
            ..biffix_core::testing::anchor()
        };
        biffix_core::event::run_submitted(&RunId::new(run), at(1.0), a)
    };
    let rename = |run: &str, s: f64, name: Option<&str>| {
        biffix_core::event::run_renamed(&RunId::new(run), at(s), name, "alice")
    };
    // `a` keeps its launch name; `b` has none and is named; `c` is renamed then cleared; `d` is
    // renamed A → B → A (out of order within a batch).
    ingest_batch(
        &pool,
        &[
            launched("a", Some("launch a")),
            launched("b", None),
            launched("c", Some("launch c")),
            launched("d", None),
        ],
        &[],
    )
    .await
    .unwrap();
    ingest_batch(
        &pool,
        &[
            rename("b", 2.0, Some("Liver batch 2")),
            rename("c", 3.0, None),
            rename("c", 2.0, Some("named c")),
            rename("d", 4.0, Some("A")),
            rename("d", 2.0, Some("A")),
            rename("d", 3.0, Some("B")),
        ],
        &[],
    )
    .await
    .unwrap();
    let expected = vec![
        ("a".to_string(), Some("launch a".to_string())),
        ("b".into(), Some("Liver batch 2".into())),
        ("c".into(), None),
        ("d".into(), Some("A".into())),
    ];
    assert_eq!(name_rows(&pool).await, expected);
    // A re-sent rename is the same fact.
    assert_eq!(ingest_batch(&pool, &[rename("d", 4.0, Some("A"))], &[]).await.unwrap(), 0);
    // Lose the column: a refold rebuilds it from the log.
    sqlx::query("UPDATE run_projection SET run_name = 'stale'").execute(&pool).await.unwrap();
    refold_runs(&pool, None, 100).await.unwrap();
    assert_eq!(name_rows(&pool).await, expected);
}
