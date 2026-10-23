//! Output-prefix and launch claims, parked launches, and migrating again.

use super::*;
use biffix_backend::store::outputs::OutputStore;
use biffix_backend::store::pending::PendingLaunchStore;

/// One submit at a time holds an output prefix; dropping the claim releases it.
#[tokio::test]
#[ignore = "requires --features pg + the Postgres harness"]
async fn an_output_prefix_is_claimed_by_one_submit_at_a_time() {
    let store = PgStore::new(migrated_pool("prefix_claims").await);
    let prefix = "s3://b/runs/r0/";
    let held = store.claim_output_prefix(prefix).await.unwrap();
    assert!(held.is_some());
    assert!(store.claim_output_prefix(prefix).await.unwrap().is_none());
    // Another prefix is independent.
    assert!(store.claim_output_prefix("s3://b/runs/r1/").await.unwrap().is_some());
    drop(held);
    assert!(
        claim_again(|| store.claim_output_prefix(prefix)).await.is_some(),
        "a dropped claim releases the prefix"
    );
}

/// Parked launch requests list oldest first (then by run id), replace on a re-put, and delete.
#[tokio::test]
#[ignore = "requires --features pg + the Postgres harness"]
async fn pending_launches_round_trip_oldest_first() {
    use biffix_backend::store::pending::PendingLaunch;
    let store = PgStore::new(migrated_pool("pending_launch").await);
    let pending = |run: &str, at: i64| PendingLaunch {
        run_id: RunId::new(run),
        created_at_micros: at,
        plan: serde_json::json!({"run": run, "secret": "kept verbatim"}),
    };
    assert!(store.list_pending_launches().await.unwrap().is_empty());
    for p in [pending("rc", 30), pending("rb", 10), pending("ra", 10)] {
        store.put_pending_launch(&p).await.unwrap();
    }
    let order = |rows: Vec<PendingLaunch>| {
        rows.into_iter().map(|p| p.run_id.to_string()).collect::<Vec<_>>()
    };
    let rows = store.list_pending_launches().await.unwrap();
    assert_eq!(rows[0], pending("ra", 10), "the row reads back whole");
    assert_eq!(order(rows), ["ra", "rb", "rc"]);

    // A re-put replaces the run's row.
    store.put_pending_launch(&pending("rc", 5)).await.unwrap();
    assert_eq!(order(store.list_pending_launches().await.unwrap()), ["rc", "ra", "rb"]);

    store.delete_pending_launch(&RunId::new("ra")).await.unwrap();
    // Deleting an absent row is not an error.
    store.delete_pending_launch(&RunId::new("ra")).await.unwrap();
    assert_eq!(order(store.list_pending_launches().await.unwrap()), ["rc", "rb"]);

    // An unbound coordinator is recorded beside the plan, which reads back otherwise unchanged; a
    // run with no parked launch is a no-op.
    store.mark_pending_launch_orphan(&RunId::new("rb"), "coord-9").await.unwrap();
    store.mark_pending_launch_orphan(&RunId::new("absent"), "coord-0").await.unwrap();
    let rows = store.list_pending_launches().await.unwrap();
    let rb = rows.iter().find(|p| p.run_id.as_str() == "rb").unwrap();
    assert_eq!(rb.orphan_job_id(), Some("coord-9"));
    assert_eq!(
        rb.plan,
        serde_json::json!({"run": "rb", "secret": "kept verbatim",
                           "orphan_coordinator_job_id": "coord-9"})
    );
    // One run's row reads back as the listing has it; an absent one is none.
    assert_eq!(store.get_pending_launch(&RunId::new("rb")).await.unwrap().as_ref(), Some(rb));
    assert_eq!(store.get_pending_launch(&RunId::new("ra")).await.unwrap(), None);
    assert_eq!(order(rows), ["rc", "rb"]);
}

/// A dropped claim is released when its pooled connection is next used: claim again, retrying
/// briefly, until it is granted (or give up).
async fn claim_again<F, Fut>(mut claim: F) -> Option<biffix_backend::store::Claim>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<
        Output = Result<Option<biffix_backend::store::Claim>, biffix_backend::store::StoreError>,
    >,
{
    for _ in 0..50 {
        if let Some(claim) = claim().await.unwrap() {
            return Some(claim);
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    None
}

/// One holder at a time launches a run; dropping the claim releases it. The launch and output
/// prefix claims are separate namespaces.
#[tokio::test]
#[ignore = "requires --features pg + the Postgres harness"]
async fn a_run_launch_is_claimed_by_one_holder_at_a_time() {
    let store = PgStore::new(migrated_pool("launch_claims").await);
    let run = RunId::new("r0");
    let held = store.claim_run_launch(&run).await.unwrap();
    assert!(held.is_some());
    assert!(store.claim_run_launch(&run).await.unwrap().is_none());
    assert!(store.claim_run_launch(&RunId::new("r1")).await.unwrap().is_some());
    assert!(store.claim_output_prefix("r0").await.unwrap().is_some());
    drop(held);
    assert!(
        claim_again(|| store.claim_run_launch(&run)).await.is_some(),
        "a dropped claim releases the run"
    );
}

/// `migrate` runs on every cold start against an already-migrated database, and cold starts race:
/// a re-run, and two at once, succeed and apply nothing twice.
#[tokio::test]
#[ignore = "requires --features pg + the Postgres harness"]
async fn migrating_an_already_migrated_database_again_and_concurrently_is_a_no_op() {
    let pool = migrated_pool("migrate_rerun").await;
    let applied = |pool: sqlx::PgPool| async move {
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM _sqlx_migrations")
            .fetch_one(&pool)
            .await
            .unwrap()
    };
    let before = applied(pool.clone()).await;
    assert!(before > 0, "the fixture applied the migrations");
    biffix_db::migrate::run(&pool).await.unwrap();
    let (a, b) = tokio::join!(biffix_db::migrate::run(&pool), biffix_db::migrate::run(&pool));
    a.unwrap();
    b.unwrap();
    assert_eq!(applied(pool).await, before);
}
