//! Provenance due-ness and the status page's samples.

use super::*;
use biffix_backend::store::event_log::EventLogReads;
use biffix_backend::store::provenance::ProvenanceStore;
use biffix_backend::store::status::StatusStore;

/// The epoch seconds of the database's `now()`: what its time windows are measured from, which
/// can differ from the host's clock (a Docker VM's drifts).
async fn db_now_secs(pool: &sqlx::PgPool) -> f64 {
    sqlx::query_scalar("SELECT EXTRACT(EPOCH FROM now())::float8").fetch_one(pool).await.unwrap()
}

/// The provenance sweep's reads: the anchor as appended, which ended runs are due (quiet for the
/// settle window, newer than their last write), and that a write clears a run until a newer event.
#[tokio::test]
#[ignore = "requires --features pg + the Postgres harness"]
async fn provenance_due_tracks_ended_runs_and_their_newest_event() {
    use biffix_core::time::Timestamp;
    let pool = migrated_pool("provenance_due").await;
    let now_micros = biffix_core::clock::epoch_micros();
    for (run, state) in [("done", "succeeded"), ("live", "running")] {
        let ev = biffix_core::event::run_submitted(
            &RunId::new(run),
            Timestamp::from_micros(now_micros),
            biffix_core::event::RunSubmitted::new(
                "v1",
                "abc123",
                "sha256:feed",
                serde_json::json!({}),
                vec![],
            ),
        );
        // Through the single writer (which also folds the anchor into a `submitted` row), then pin
        // the run's state and end time, which the sweep selects on.
        biffix_db::log::append_and_advance(&pool, std::slice::from_ref(&ev)).await.unwrap();
        sqlx::query(
            "INSERT INTO run_projection (run_id, state, completed_at) VALUES ($1, $2, $3) \
             ON CONFLICT (run_id) DO UPDATE SET state = EXCLUDED.state, \
               completed_at = EXCLUDED.completed_at",
        )
        .bind(run)
        .bind(state)
        .bind(now_micros)
        .execute(&pool)
        .await
        .unwrap();
    }
    // Every event last arrived `secs` ago, on the database's clock (the one the window is on).
    let quiet_for = |pool: &sqlx::PgPool, secs: f64| {
        let pool = pool.clone();
        async move {
            sqlx::query("UPDATE run_events SET received_at = now() - make_interval(secs => $1)")
                .bind(secs)
                .execute(&pool)
                .await
                .unwrap();
        }
    };
    let quiet = |pool: &sqlx::PgPool| quiet_for(pool, 3600.0);
    let store = PgStore::new(pool.clone());

    let anchor = store.run_anchor(&RunId::new("done")).await.unwrap().unwrap();
    assert_eq!(
        biffix_backend::runs::provenance::AnchorFacts::from_event(anchor).unwrap().git_sha,
        "abc123"
    );
    assert!(store.run_anchor(&RunId::new("nope")).await.unwrap().is_none());

    // Not yet quiet: nothing is due, even 10 s short of the settle window.
    assert!(store.provenance_due(600, 10).await.unwrap().is_empty());
    quiet_for(&pool, 590.0).await;
    assert!(store.provenance_due(600, 10).await.unwrap().is_empty());
    // 10 s past it, the ended run is.
    quiet_for(&pool, 610.0).await;
    let due = store.provenance_due(600, 10).await.unwrap();
    assert_eq!(due.len(), 1, "only the ended run: {due:?}");
    let (run, seq) = due[0].clone();
    assert_eq!(run.as_str(), "done");

    store
        .mark_provenance(&run, seq, "s3://b/biffix/runs/done/provenance/x", now_micros)
        .await
        .unwrap();
    assert!(store.provenance_due(600, 10).await.unwrap().is_empty());

    // A late event makes it due again, once quiet.
    sqlx::query(
        "INSERT INTO run_events (dedup_key, ts_micros, source, kind, run_id, schema_version, payload) \
         VALUES ('late', $1, 'aws_batch', 'spot_reclaim_warning', 'done', 1, '{}')",
    )
    .bind(now_micros)
    .execute(&pool)
    .await
    .unwrap();
    quiet(&pool).await;
    let again = store.provenance_due(600, 10).await.unwrap();
    assert_eq!(again.len(), 1);
    assert!(again[0].1 > seq);
}

/// The status page's reads: the newest sample per component, and the per-bucket rollup over a
/// window (buckets aligned to the epoch, window and bucket clamped to a minute or more).
#[tokio::test]
#[ignore = "requires --features pg + the Postgres harness"]
async fn status_reads_the_newest_sample_and_buckets_the_window() {
    use biffix_backend::store::status::{StatusBucketView, StatusComponentView};
    let pool = migrated_pool("status_reads").await;
    // The database's clock, which the window is measured on (`now()`), in whole seconds so every
    // sample time reads back exactly.
    let now = db_now_secs(&pool).await.floor();
    let hour = 3600.0;
    let day = 86400.0;
    let base = (now / hour).floor() * hour - 3.0 * hour;
    for (at, component, status, next) in [
        (base + 60.0, "api", "ok", Some(300)),
        (base + 120.0, "api", "down", Some(3600)),
        (base + hour + 10.0, "db", "degraded", Some(300)),
        (now - 10.0, "probe", "ok", None),
        // A minute inside a day's window, and a minute outside it.
        (now - day + 60.0, "edge", "ok", Some(300)),
        (now - day - 60.0, "api", "ok", Some(300)),
    ] {
        sqlx::query(
            "INSERT INTO status_samples (sampled_at, component, status, detail, next_within_secs) \
             VALUES (to_timestamp($1), $2, $3, $2 || ' ' || $3, $4)",
        )
        .bind(at)
        .bind(component)
        .bind(status)
        .bind(next.map(i64::from))
        .execute(&pool)
        .await
        .unwrap();
    }
    let store = PgStore::new(pool);
    let latest = |name: &str, status: &str, at: f64, next: Option<i64>| StatusComponentView {
        name: name.into(),
        status: status.into(),
        detail: Some(format!("{name} {status}")),
        sampled_at: Some(at),
        next_within_secs: next,
    };
    assert_eq!(
        store.status_components().await.unwrap(),
        vec![
            latest("api", "down", base + 120.0, Some(3600)),
            latest("db", "degraded", base + hour + 10.0, Some(300)),
            latest("edge", "ok", now - day + 60.0, Some(300)),
            latest("probe", "ok", now - 10.0, None),
        ]
    );
    let bucket = |at: f64, component: &str, ok, degraded, down| StatusBucketView {
        bucket: at as i64,
        component: component.into(),
        ok,
        degraded,
        down,
    };
    let probe_hour = ((now - 10.0) / hour).floor() * hour;
    assert_eq!(
        store.status_history(86400, 3600).await.unwrap(),
        vec![
            bucket(((now - day + 60.0) / hour).floor() * hour, "edge", 1, 0, 0),
            bucket(base, "api", 1, 0, 1),
            bucket(base + hour, "db", 0, 1, 0),
            bucket(probe_hour, "probe", 1, 0, 0),
        ]
    );
    // Clamped to a minute's window in minute buckets: only the probe's sample.
    let probe_minute = ((now - 10.0) / 60.0).floor() * 60.0;
    assert_eq!(
        store.status_history(1, 1).await.unwrap(),
        vec![bucket(probe_minute, "probe", 1, 0, 0)]
    );
}

#[tokio::test]
#[ignore = "requires --features pg + the Postgres harness"]
async fn provenance_due_compares_log_positions_not_clocks() {
    let pool = migrated_pool("provenance_due_log_positions").await;
    // An ended run that completed long ago, so no recency window can rescue it.
    exec_all(
        &pool,
        &["INSERT INTO run_projection (run_id, state, completed_at) VALUES ('old', 'failed', 1)"],
    )
    .await;
    let event = |key: &'static str| {
        let pool = pool.clone();
        async move {
            sqlx::query_scalar::<_, i64>(
                "INSERT INTO run_events \
                   (dedup_key, ts_micros, source, kind, run_id, schema_version, payload, received_at) \
                 VALUES ($1, 1, 'aws_batch', 'spot_reclaim_warning', 'old', 1, '{}', \
                         now() - interval '1 hour') RETURNING seq",
            )
            .bind(key)
            .fetch_one(&pool)
            .await
            .unwrap()
        }
    };
    let first = event("e1").await;
    let store = PgStore::new(pool.clone());
    assert_eq!(store.provenance_due(600, 10).await.unwrap(), [(RunId::new("old"), first)]);
    // The writer's clock runs ahead of the database's (the crate was stamped after a slow write),
    // and an event lands after the crate's snapshot without touching the run row: still due.
    store.mark_provenance(&RunId::new("old"), first, "s3://b/p", i64::MAX / 2).await.unwrap();
    assert!(store.provenance_due(600, 10).await.unwrap().is_empty());
    let late = event("e2").await;
    assert_eq!(store.provenance_due(600, 10).await.unwrap(), [(RunId::new("old"), late)]);
    // Once written through the newest seq, a replay (every row rewritten, no new seq) makes
    // nothing due.
    store.mark_provenance(&RunId::new("old"), late, "s3://b/p", 0).await.unwrap();
    exec_all(&pool, &["UPDATE run_projection SET updated_at = now() + interval '1 day'"]).await;
    assert!(store.provenance_due(600, 10).await.unwrap().is_empty());
}

/// A run whose provenance keeps failing sorts after runs that have not failed, then waits between
/// tries; a written crate forgets its failures.
#[tokio::test]
#[ignore = "requires --features pg + the Postgres harness"]
async fn a_run_whose_provenance_keeps_failing_waits_behind_fresh_runs() {
    use biffix_core::time::Timestamp;
    let pool = migrated_pool("provenance_failures").await;
    let now_micros = biffix_core::clock::epoch_micros();
    // `older` ended first (its newest event is the older one), so it leads while neither failed.
    for (run, age) in [("older", 7200.0), ("newer", 3600.0)] {
        let ev = biffix_core::event::run_submitted(
            &RunId::new(run),
            Timestamp::from_micros(now_micros),
            biffix_core::event::RunSubmitted::new(
                "v1",
                "abc123",
                "sha256:feed",
                serde_json::json!({}),
                vec![],
            ),
        );
        biffix_db::log::append_and_advance(&pool, std::slice::from_ref(&ev)).await.unwrap();
        sqlx::query("UPDATE run_projection SET state = 'succeeded' WHERE run_id = $1")
            .bind(run)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "UPDATE run_events SET received_at = now() - make_interval(secs => $1) \
             WHERE run_id = $2",
        )
        .bind(age)
        .bind(run)
        .execute(&pool)
        .await
        .unwrap();
    }
    let store = PgStore::new(pool.clone());
    async fn due(store: &PgStore) -> Vec<String> {
        store
            .provenance_due(600, 10)
            .await
            .unwrap()
            .into_iter()
            .map(|(r, _)| r.to_string())
            .collect()
    }
    assert_eq!(due(&store).await, ["older", "newer"]);
    // One failure: due again at once, but after the run that has not failed.
    let older = RunId::new("older");
    store.mark_provenance_failed(&older, now_micros).await.unwrap();
    assert_eq!(due(&store).await, ["newer", "older"]);
    // A second: it waits (five minutes, doubling per failure).
    store.mark_provenance_failed(&older, now_micros).await.unwrap();
    assert_eq!(due(&store).await, ["newer"]);
    let (attempts, retry_after): (i32, i64) = sqlx::query_as(
        "SELECT attempts, retry_after FROM run_provenance_failure WHERE run_id = 'older'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        (attempts, retry_after),
        (2, biffix_backend::store::provenance::provenance_retry_after(2, now_micros))
    );
    store.mark_provenance_failed(&older, now_micros).await.unwrap();
    let (retry_after,): (i64,) =
        sqlx::query_as("SELECT retry_after FROM run_provenance_failure WHERE run_id = 'older'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        retry_after,
        biffix_backend::store::provenance::provenance_retry_after(3, now_micros)
    );
    // Its crate written at last: its failures are forgotten.
    store.mark_provenance(&older, i64::MAX, "s3://b/x", now_micros).await.unwrap();
    let (left,): (i64,) = sqlx::query_as("SELECT count(*) FROM run_provenance_failure")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(left, 0);
}
