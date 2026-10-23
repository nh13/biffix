//! Run outputs, refolds that delete stale rows, and work due for deletion by retention.

use super::*;

#[tokio::test]
#[ignore = "requires the LocalStack/Postgres harness"]
async fn run_outputs_are_persisted_and_only_rewritten_when_the_set_changes() {
    use biffix_core::event::{OutputRef, OutputRole};
    let pool =
        common::isolated_pool("run_outputs_are_persisted_and_only_rewritten_when_the_set_changes")
            .await;
    let outputs = |dedup: &str, ts: f64, role: OutputRole, uris: &[&str]| {
        ev(
            ts,
            EventSource::Snakemake,
            dedup,
            EventData::TaskOutputs {
                role: Some(role),
                reused: false,
                outputs: uris
                    .iter()
                    .map(|u| OutputRef {
                        s3_uri: u.to_string(),
                        path: None,
                        etag: None,
                        size: None,
                    })
                    .collect(),
            },
        )
        .with_run("r1")
        .with_task("align")
        .with_attempt(1)
    };
    append(&pool, &outputs("dk-o1", 1.0, OutputRole::Output, &["s3://b/a.bam", "s3://b/a.bai"]))
        .await
        .unwrap();
    append(&pool, &outputs("dk-o2", 1.5, OutputRole::Intermediate, &["s3://b/work/x"]))
        .await
        .unwrap();

    let rows = sqlx::query(
        "SELECT s3_uri, id, task_key, attempt, role FROM run_output WHERE run_id='r1' \
         ORDER BY s3_uri",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(rows.len(), 3);
    let first = &rows[0];
    assert_eq!(first.get::<String, _>("s3_uri"), "s3://b/a.bai");
    assert_eq!(first.get::<String, _>("id"), biffix_core::reducer::output_id("r1", "s3://b/a.bai"));
    assert_eq!(first.get::<Option<String>, _>("task_key").as_deref(), Some("align"));
    assert_eq!(first.get::<Option<i32>, _>("attempt"), Some(1));
    assert_eq!(first.get::<String, _>("role"), "output");
    let (total, reused): (i64, i64) = sqlx::query_as(
        "SELECT outputs_total, outputs_reused FROM run_projection WHERE run_id='r1'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!((total, reused), (2, 0), "the intermediate is not counted");

    // Tamper with a row: an advance that does not change the output set must not rewrite it (the
    // digest gate), while one that does change it rewrites the set.
    sqlx::query("UPDATE run_output SET role='tampered' WHERE s3_uri='s3://b/a.bam'")
        .execute(&pool)
        .await
        .unwrap();
    let progress =
        ev(2.0, EventSource::Snakemake, "dk-p", EventData::RunProgress { completed: 1, total: 2 })
            .with_run("r1");
    append(&pool, &progress).await.unwrap();
    let role = || async {
        sqlx::query_scalar::<_, String>("SELECT role FROM run_output WHERE s3_uri='s3://b/a.bam'")
            .fetch_one(&pool)
            .await
            .unwrap()
    };
    assert_eq!(role().await, "tampered", "unchanged set ⇒ no rewrite");

    append(&pool, &outputs("dk-o3", 3.0, OutputRole::Log, &["s3://b/a.log"])).await.unwrap();
    assert_eq!(role().await, "output", "changed set ⇒ rewritten");
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM run_output WHERE run_id='r1'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(n, 4);
}

/// Persisted projection == fold, not a superset of every fold ever run: rows a
/// re-fold no longer produces — as after a reducer fix drops or re-keys a task, attempt or output —
/// are deleted by the refold, including when the fold produces none of that kind at all.
#[tokio::test]
#[ignore = "requires the LocalStack/Postgres harness"]
async fn a_refold_deletes_projection_rows_the_fold_no_longer_produces() {
    use biffix_core::event::{OutputRef, OutputRole};
    let pool = common::isolated_pool("a_refold_deletes_projection_rows").await;

    // r1: one task with one attempt and one output. r2: a run with no tasks, attempts or outputs
    // at all (the empty-set case).
    let sub = task_submitted(1.0, "dk-sub", "r1", "align", "job-1");
    let out = ev(
        2.0,
        EventSource::Snakemake,
        "dk-out",
        EventData::TaskOutputs {
            role: Some(OutputRole::Output),
            reused: false,
            outputs: vec![OutputRef {
                s3_uri: "s3://b/keep.bam".into(),
                path: None,
                etag: None,
                size: None,
            }],
        },
    )
    .with_run("r1")
    .with_task("align")
    .with_attempt(1);
    let started = ev(
        1.0,
        EventSource::BiffixControlPlane,
        "dk-r2",
        EventData::RunStarted { session_id: None },
    )
    .with_run("r2");
    for e in [&sub, &out, &started] {
        append(&pool, e).await.unwrap();
    }

    // Stale rows an older reducer produced, which today's fold does not. The run_output row leaves
    // the stored outputs digest untouched, as a pre-fix upsert-only refold would have.
    for run in ["r1", "r2"] {
        sqlx::query(
            "INSERT INTO task_projection (run_id, task_key, state, attempts) \
             VALUES ($1, 'stale-task', 'running', 1)",
        )
        .bind(run)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO attempt_projection (run_id, batch_job_id, task_key, attempt, state) \
             VALUES ($1, 'job-stale', 'stale-task', 1, 'running')",
        )
        .bind(run)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO run_output (run_id, s3_uri, id, role) VALUES ($1, '', $2, 'output')",
        )
        .bind(run)
        .bind(biffix_core::reducer::output_id(run, ""))
        .execute(&pool)
        .await
        .unwrap();
    }

    let page = refold_runs(&pool, None, 10).await.unwrap();
    assert_eq!(page.refolded, 2);

    let keys = {
        let pool = &pool;
        move |table: &'static str, col: &'static str, run: &'static str| async move {
            sqlx::query_scalar::<_, String>(AssertSqlSafe(format!(
                "SELECT {col} FROM {table} WHERE run_id = $1 ORDER BY {col}"
            )))
            .bind(run)
            .fetch_all(pool)
            .await
            .unwrap()
        }
    };
    assert_eq!(keys("task_projection", "task_key", "r1").await, ["align"]);
    assert_eq!(keys("attempt_projection", "batch_job_id", "r1").await, ["job-1"]);
    assert_eq!(keys("run_output", "s3_uri", "r1").await, ["s3://b/keep.bam"]);
    assert!(keys("task_projection", "task_key", "r2").await.is_empty());
    assert!(keys("attempt_projection", "batch_job_id", "r2").await.is_empty());
    assert!(keys("run_output", "s3_uri", "r2").await.is_empty());
}

/// Fold one run: its anchor (version, output prefix, retention) and, when `ended` is given, its
/// terminal `(epoch seconds, succeeded?)`.
async fn seed_run(
    pool: &sqlx::PgPool,
    run: &str,
    version: &str,
    output_prefix: &str,
    retention: Option<biffix_core::event::Retention>,
    ended: Option<(f64, bool)>,
) {
    let submitted = biffix_core::event::run_submitted(
        &RunId::new(run),
        Timestamp::from_epoch_seconds(1.0),
        biffix_core::event::RunSubmitted {
            workflow_version_id: version.into(),
            output_prefix: Some(output_prefix.into()),
            retention,
            ..biffix_core::testing::anchor()
        },
    );
    append(pool, &submitted).await.unwrap();
    if let Some((ts, succeeded)) = ended {
        let (dedup, data) = if succeeded {
            ("succeeded", EventData::RunSucceeded {})
        } else {
            ("failed", EventData::RunFailed { reason: None, stage: None })
        };
        let end =
            ev(ts, EventSource::BiffixControlPlane, format!("{run}-{dedup}"), data).with_run(run);
        append(pool, &end).await.unwrap();
    }
}

#[tokio::test]
#[ignore = "requires the LocalStack/Postgres harness"]
async fn work_is_due_for_deletion_by_retention_and_shared_prefix() {
    use biffix_core::event::Retention::{DeleteOnSuccess, Keep};
    let pool =
        common::isolated_pool("work_is_due_for_deletion_by_retention_and_shared_prefix").await;
    for (id, engine) in [("wf-nf", "nextflow"), ("wf-sm", "snakemake")] {
        sqlx::query(
            "INSERT INTO workflow_versions (registration_inputs, registration_inputs_hash, \
             runtime_key, runtime_recipe, \
             id, name, project, git_sha, source_content_hash, \
             image_digest, engine, engine_version, entrypoint, source, form_schema, queue_classes, \
             created_at) VALUES ('{}'::jsonb, '', '', '{}'::jsonb, \
             $1, 'w', 'p', 'abc', $1, 'sha256:x', $2, '1', 'main', \
             '{}'::jsonb, '{}'::jsonb, '{}'::text[], 0)",
        )
        // Each its own source (the id as its content hash): two versions may not share a
        // (source, build).
        .bind(id)
        .bind(engine)
        .execute(&pool)
        .await
        .unwrap();
    }
    let day = 86_400.0;
    let own = |run: &str| format!("s3://bkt/biffix/runs/{run}/");
    // Default retention, ended on day 10.
    seed_run(&pool, "a", "wf-nf", &own("a"), None, Some((10.0 * day, true))).await;
    // Delete on success: succeeded (due now) / failed (waits for the period).
    seed_run(&pool, "b", "wf-nf", &own("b"), Some(DeleteOnSuccess), Some((50.0 * day, true))).await;
    seed_run(&pool, "c", "wf-nf", &own("c"), Some(DeleteOnSuccess), Some((20.0 * day, false)))
        .await;
    // Keep: never.
    seed_run(&pool, "d", "wf-nf", &own("d"), Some(Keep), Some((1.0 * day, true))).await;
    // `f` resumed `e` (so shares its prefix) and is still running: never while it runs.
    seed_run(&pool, "e", "wf-nf", &own("e"), None, Some((1.0 * day, false))).await;
    seed_run(&pool, "f", "wf-nf", &own("e"), None, None).await;
    // A custom prefix, and a Snakemake run: never.
    seed_run(
        &pool,
        "g",
        "wf-nf",
        "s3://bkt/results/g/",
        Some(DeleteOnSuccess),
        Some((2.0 * day, true)),
    )
    .await;
    seed_run(&pool, "h", "wf-sm", &own("h"), Some(DeleteOnSuccess), Some((2.0 * day, true))).await;
    // A nested prefix that merely ends in `/biffix/runs/<id>/` is not the run's own prefix: never
    // admitted (the sweep could not locate it, so it would hold a slot of every tick's page).
    seed_run(
        &pool,
        "i",
        "wf-nf",
        "s3://bkt/team/biffix/runs/i/",
        Some(DeleteOnSuccess),
        Some((2.0 * day, true)),
    )
    .await;

    let due = {
        let pool = &pool;
        move |cutoff_days: Option<f64>| async move {
            let cutoff = cutoff_days.map(|d| (d * day * 1e6) as i64);
            work_due_for_deletion(pool, cutoff, 100, None)
                .await
                .unwrap()
                .into_iter()
                .map(|w| (w.output_prefix, w.run_ids))
                .collect::<Vec<_>>()
        }
    };
    // No retention period: only a succeeded delete-on-success run.
    assert_eq!(due(None).await, vec![(own("b"), vec!["b".to_string()])]);
    // A cutoff past day 20: the default and failed delete-on-success runs too, by prefix.
    assert_eq!(
        due(Some(25.0)).await,
        vec![
            (own("a"), vec!["a".to_string()]),
            (own("b"), vec!["b".to_string()]),
            (own("c"), vec!["c".to_string()]),
        ]
    );
    // Paged after a prefix, so a sweep can rotate through them: one that fails every tick cannot
    // hold the head of every page.
    let cutoff25 = Some((25.0 * day * 1e6) as i64);
    let page = {
        let pool = &pool;
        move |after: Option<&'static str>| async move {
            work_due_for_deletion(pool, cutoff25, 2, after)
                .await
                .unwrap()
                .into_iter()
                .map(|w| w.output_prefix)
                .collect::<Vec<_>>()
        }
    };
    assert_eq!(page(None).await, [own("a"), own("b")]);
    assert_eq!(page(Some("s3://bkt/biffix/runs/b/")).await, [own("c")]);

    // The sweep's re-check before each delete batch agrees: `e`'s dir is not due while `f` (which
    // resumed it) runs, and `b`'s is.
    let cutoff31 = Some((31.0 * day * 1e6) as i64);
    assert!(!work_still_due(&pool, &own("e"), cutoff31).await.unwrap());
    assert!(work_still_due(&pool, &own("b"), None).await.unwrap());
    assert!(!work_still_due(&pool, "s3://bkt/team/biffix/runs/i/", None).await.unwrap());

    // Once `f` ends, `e`'s shared work dir is due (for both runs) when the newest ended long enough
    // ago.
    let f_end =
        ev(30.0 * day, EventSource::BiffixControlPlane, "f-succeeded", EventData::RunSucceeded {})
            .with_run("f");
    append(&pool, &f_end).await.unwrap();
    assert!(!due(Some(25.0)).await.iter().any(|(p, _)| *p == own("e")));
    assert!(due(Some(31.0)).await.contains(&(own("e"), vec!["e".to_string(), "f".to_string()])));
    assert!(work_still_due(&pool, &own("e"), cutoff31).await.unwrap());

    // Recorded deletion: no longer due, and folded to work_deleted_at.
    let deleted = biffix_core::event::work_deleted(
        &RunId::new("b"),
        Timestamp::from_epoch_seconds(60.0 * day),
    );
    append(&pool, &deleted).await.unwrap();
    assert!(due(None).await.is_empty());
    let at: Option<i64> =
        sqlx::query_scalar("SELECT work_deleted_at FROM run_projection WHERE run_id='b'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(at, Some((60.0 * day * 1e6) as i64));
}
