//! Run and task rows: the coordinator's stream and host, engine sessions, sweep candidates, task deps, images and verdicts, many tasks, and image waits.

use super::*;

#[tokio::test]
#[ignore = "requires the LocalStack/Postgres harness"]
async fn coordinator_log_stream_and_host_are_persisted() {
    let pool = common::isolated_pool("coordinator_log_stream_and_host_are_persisted").await;
    // As in production: the control plane binds the coordinator job to its run, and the Batch
    // event carries no run id (the translator leaves it to the fold, through the binding).
    let bound = biffix_core::event::coordinator_submitted(
        &RunId::new("r1"),
        Timestamp::from_epoch_seconds(1.0),
        &BatchJobId::new("coord-1"),
    );
    append(&pool, &bound).await.unwrap();
    let running = ev(
        2.0,
        EventSource::AwsBatch,
        "aws-batch:coord-1:RUNNING",
        EventData::CoordinatorRunning { log_stream: Some("coord/stream".into()) },
    )
    .with_job("coord-1")
    .with_instance("ci-coord");
    assert!(running.run_id.is_none());
    append(&pool, &running).await.unwrap();

    let (log_stream, host): (Option<String>, Option<String>) = sqlx::query_as(
        "SELECT coordinator_log_stream, coordinator_host_id FROM run_projection WHERE run_id='r1'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(log_stream.as_deref(), Some("coord/stream"));
    assert_eq!(host.as_deref(), Some("ci-coord"));
}

#[tokio::test]
#[ignore = "requires the LocalStack/Postgres harness"]
async fn engine_session_mismatch_and_resumed_from_are_persisted() {
    let pool =
        common::isolated_pool("engine_session_mismatch_and_resumed_from_are_persisted").await;
    let submitted = ev(
        1.0,
        EventSource::BiffixControlPlane,
        "dk-sub",
        EventData::RunSubmitted(Box::new(biffix_core::event::RunSubmitted {
            engine_session_id: Some("sess-launched".into()),
            resumed_from: Some("r0".into()),
            reference_bundle: Some(biffix_core::event::ReferenceUse {
                id: "grch38".into(),
                uris: [("fasta".to_string(), "s3://refs/hg38.fa".to_string())].into(),
            }),
            sample_count: Some(3),
            retention: Some(biffix_core::event::Retention::DeleteOnSuccess),
            ..biffix_core::testing::anchor()
        })),
    )
    .with_run("r1");
    append(&pool, &submitted).await.unwrap();
    let bundle: Option<serde_json::Value> =
        sqlx::query_scalar("SELECT reference_bundle FROM run_projection WHERE run_id='r1'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        bundle,
        Some(serde_json::json!({"id": "grch38", "uris": {"fasta": "s3://refs/hg38.fa"}}))
    );
    let samples: Option<i32> =
        sqlx::query_scalar("SELECT sample_count FROM run_projection WHERE run_id='r1'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(samples, Some(3));
    let retention: Option<String> =
        sqlx::query_scalar("SELECT retention FROM run_projection WHERE run_id='r1'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(retention.as_deref(), Some("delete_on_success"));

    let read = || async {
        sqlx::query_as::<_, (Option<String>, bool, Option<String>)>(
            "SELECT engine_session_id, session_mismatch, resumed_from FROM run_projection \
             WHERE run_id='r1'",
        )
        .fetch_one(&pool)
        .await
        .unwrap()
    };
    // Only the anchor so far: the launched id, no mismatch.
    assert_eq!(read().await, (Some("sess-launched".into()), false, Some("r0".into())));

    // The engine reports a different session: it wins, and the mismatch is recorded.
    let started = ev(
        2.0,
        EventSource::Nextflow,
        "dk-start",
        EventData::RunStarted { session_id: Some("sess-reported".into()) },
    )
    .with_run("r1");
    append(&pool, &started).await.unwrap();
    assert_eq!(read().await, (Some("sess-reported".into()), true, Some("r0".into())));
}

#[tokio::test]
#[ignore = "requires the LocalStack/Postgres harness"]
async fn ended_run_with_a_live_child_is_a_sweep_candidate_until_the_child_ends() {
    let pool = common::isolated_pool("ended_run_with_a_live_child_is_a_sweep_candidate").await;
    let coord = biffix_core::event::coordinator_submitted(
        &RunId::new("r1"),
        Timestamp::from_epoch_seconds(1.0),
        &BatchJobId::new("coord-1"),
    );
    let child =
        ev(2.0, EventSource::Nextflow, "dk-child", EventData::TaskSubmitted { image: None })
            .with_run("r1")
            .with_task("SAY (1)")
            .with_attempt(1)
            .with_job("child-1");
    // The coordinator ended (e.g. terminated by a cancel) while the child is still live.
    let ended = ev(
        3.0,
        EventSource::AwsBatch,
        "aws-batch:coord-1:FAILED",
        EventData::CoordinatorFailed {
            log_stream: None,
            exit_code: None,
            status_reason: None,
            termination_category: None,
        },
    )
    .with_run("r1")
    .with_job("coord-1");
    for e in [&coord, &child, &ended] {
        append(&pool, e).await.unwrap();
    }

    let since = 0;
    assert_eq!(
        ended_runs_with_live_children(&pool, since, 10).await.unwrap(),
        vec![("r1".to_string(), "coord-1".to_string())]
    );
    // Outside the window (ended before `since`), it is not rescanned.
    assert!(ended_runs_with_live_children(&pool, 4_000_000, 10).await.unwrap().is_empty());

    // Once Batch reports the child's terminal, the run is no longer a candidate.
    let child_done = ev(
        4.0,
        EventSource::AwsBatch,
        "aws-batch:child-1:FAILED",
        EventData::TaskFailed {
            exit_code: 143,
            termination_category: None,
            status_reason: None,
            log_stream: None,
        },
    )
    .with_job("child-1");
    append(&pool, &child_done).await.unwrap();
    assert!(ended_runs_with_live_children(&pool, since, 10).await.unwrap().is_empty());
}

#[tokio::test]
#[ignore = "requires the LocalStack/Postgres harness"]
async fn task_deps_are_persisted_on_the_task_row() {
    let pool = common::isolated_pool("task_deps_are_persisted_on_the_task_row").await;
    let created = ev(
        1.0,
        EventSource::Snakemake,
        "dk-merge",
        EventData::TaskCreated {
            group: Some("merge".into()),
            deps: vec!["align:B".into(), "align:A".into()],
        },
    )
    .with_run("r1")
    .with_task("merge");
    append(&pool, &created).await.unwrap();
    let deps: Vec<String> = sqlx::query_scalar(
        "SELECT deps FROM task_projection WHERE run_id='r1' AND task_key='merge'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    // Stored sorted (the projection's set order), so reads are deterministic.
    assert_eq!(deps, vec!["align:A", "align:B"]);
}

#[tokio::test]
#[ignore = "requires the LocalStack/Postgres harness"]
async fn a_task_s_image_and_engine_verdict_are_persisted_on_the_task_row() {
    let pool =
        common::isolated_pool("a_task_s_image_and_engine_verdict_are_persisted_on_the_task_row")
            .await;
    let submitted = run_ev(
        1.0,
        EventSource::Nextflow,
        "dk-sub",
        "r1",
        EventData::TaskSubmitted { image: Some("repo/bwa@sha256:ab".into()) },
    )
    .with_task("ALIGN (A)")
    .with_attempt(1)
    .with_job("job-1");
    let succeeded = ev(
        2.0,
        EventSource::AwsBatch,
        "dk-ok",
        EventData::TaskSucceeded { exit_code: 0, log_stream: None },
    )
    .with_job("job-1");
    let verdict = run_ev(
        3.0,
        EventSource::Nextflow,
        "dk-verdict",
        "r1",
        EventData::EngineTaskVerdict {
            verdict: biffix_core::event::TaskVerdict::Failed,
            category: Some(biffix_core::event::TerminationCategory::MissingOutput),
            reason: Some("Missing output file(s) `A.bam`".into()),
        },
    )
    .with_task("ALIGN (A)")
    .with_attempt(1);
    for e in [&submitted, &succeeded, &verdict] {
        append(&pool, e).await.unwrap();
    }
    let task = sqlx::query(
        "SELECT state, exit_code, termination_category, image, verdict_reason \
           FROM task_projection WHERE run_id='r1' AND task_key='ALIGN (A)'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(task.get::<String, _>("state"), "failed");
    assert_eq!(task.get::<Option<i32>, _>("exit_code"), Some(0));
    assert_eq!(
        task.get::<Option<String>, _>("termination_category").as_deref(),
        Some("missing_output")
    );
    assert_eq!(task.get::<Option<String>, _>("image").as_deref(), Some("repo/bwa@sha256:ab"));
    assert_eq!(
        task.get::<Option<String>, _>("verdict_reason").as_deref(),
        Some("Missing output file(s) `A.bam`")
    );
    // The attempt keeps AWS's facts.
    let attempt: String = sqlx::query_scalar(
        "SELECT state FROM attempt_projection WHERE run_id='r1' AND batch_job_id='job-1'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(attempt, "succeeded");
}

#[tokio::test]
#[ignore = "requires the LocalStack/Postgres harness"]
async fn a_run_with_many_tasks_upserts_every_task_row_and_its_detail() {
    let pool = common::isolated_pool("many_tasks_upsert_in_one_statement").await;
    let mut events = Vec::new();
    for i in 0..300 {
        let created = ev(
            1.0,
            EventSource::Snakemake,
            format!("created-{i}"),
            EventData::TaskCreated {
                group: Some("align".into()),
                deps: if i == 0 { vec![] } else { vec![format!("t{}", i - 1)] },
            },
        )
        .with_run("r-many")
        .with_task(&format!("t{i}"));
        events.push(created);
    }
    ingest_batch(&pool, &events, &[]).await.unwrap();
    // Refold the run over its existing rows (the ON CONFLICT DO UPDATE arm of the one-statement
    // upsert). A redelivered event would fold nothing, so append a genuinely new fact: t0's
    // creation again under a fresh dedup key (the same content, so the fold's output is unchanged).
    // Tamper with a row first, so the upsert is seen to have rewritten it.
    sqlx::query(
        "UPDATE task_projection SET \"group\" = 'tampered' WHERE run_id = 'r-many' AND task_key = 't7'",
    )
    .execute(&pool)
    .await
    .unwrap();
    let refresh = RunEvent { dedup_key: "created-0-again".into(), ..events[0].clone() };
    assert_eq!(ingest_batch(&pool, &[refresh], &[]).await.unwrap(), 1);

    let n: i64 =
        sqlx::query_scalar("SELECT count(*) c FROM task_projection WHERE run_id = 'r-many'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(n, 300);
    let row = sqlx::query(
        "SELECT \"group\", deps, attempts, cached FROM task_projection \
         WHERE run_id = 'r-many' AND task_key = 't7'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(row.get::<Option<String>, _>("group").as_deref(), Some("align"));
    assert_eq!(row.get::<Vec<String>, _>("deps"), ["t6"]);
    assert_eq!(row.get::<i32, _>("attempts"), 0);
    assert!(!row.get::<bool, _>("cached"));
    let root_deps: Vec<String> = sqlx::query_scalar(
        "SELECT deps FROM task_projection WHERE run_id = 'r-many' AND task_key = 't0'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(root_deps.is_empty(), "no deps is an empty array, not NULL");
}

/// A run that waited for its submit-time images: `submitted_at` is when the user submitted (its
/// `run_preparing`), `launched_at` when it launched (its anchor), so the probe's "stuck in
/// submitted" and the wall-time estimate measure from the launch.
#[tokio::test]
#[ignore = "requires the LocalStack/Postgres harness"]
async fn a_run_that_waited_for_its_images_records_when_it_launched() {
    let pool =
        common::isolated_pool("a_run_that_waited_for_its_images_records_when_it_launched").await;
    append(&pool, &common::preparing("r-prep", 10.0)).await.unwrap();
    let times = || async {
        sqlx::query_as::<_, (String, Option<i64>, Option<i64>)>(
            "SELECT state, submitted_at, launched_at FROM run_projection WHERE run_id = 'r-prep'",
        )
        .fetch_one(&pool)
        .await
        .unwrap()
    };
    assert_eq!(times().await, ("preparing_images".into(), Some(10_000_000), None));
    let anchor = biffix_core::event::run_submitted(
        &RunId::new("r-prep"),
        Timestamp::from_epoch_seconds(2000.0),
        biffix_core::testing::anchor(),
    );
    append(&pool, &anchor).await.unwrap();
    assert_eq!(times().await, ("submitted".into(), Some(10_000_000), Some(2_000_000_000)));
}
