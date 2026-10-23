//! Per-task samples and the run's sample counts, including the samples a resumed run's origin finished.

use super::*;

#[tokio::test]
#[ignore = "requires the LocalStack/Postgres harness"]
async fn each_task_s_sample_and_the_run_s_sample_counts_are_persisted() {
    let pool =
        common::isolated_pool("each_task_s_sample_and_the_run_s_sample_counts_are_persisted").await;
    // A task folds before the anchor: its sample is still attributed from the anchor's spec.
    let first = ev(
        1.0,
        EventSource::Snakemake,
        "dk-trim-a",
        EventData::TaskCreated { group: Some("trim".into()), deps: vec![] },
    )
    .with_run("r1")
    .with_task("trim:lib=P07,read=R1");
    append(&pool, &first).await.unwrap();
    let submitted = ev(
        2.0,
        EventSource::BiffixControlPlane,
        "dk-sub",
        EventData::RunSubmitted(Box::new(biffix_core::event::RunSubmitted {
            samples: Some(biffix_core::event::SampleSpec {
                ids: vec!["P07".into(), "P08".into()],
                ..Default::default()
            }),
            ..biffix_core::testing::anchor()
        })),
    )
    .with_run("r1");
    append(&pool, &submitted).await.unwrap();
    let study = ev(
        3.0,
        EventSource::Snakemake,
        "dk-multiqc",
        EventData::TaskCreated { group: Some("multiqc".into()), deps: vec![] },
    )
    .with_run("r1")
    .with_task("multiqc");
    append(&pool, &study).await.unwrap();

    let rows: Vec<(String, Option<String>)> =
        sqlx::query_as("SELECT task_key, sample FROM task_projection WHERE run_id='r1' ORDER BY 1")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(
        rows,
        [
            ("multiqc".to_string(), None),
            ("trim:lib=P07,read=R1".to_string(), Some("P07".to_string()))
        ]
    );
    let run = sqlx::query(
        "SELECT samples, samples_total, samples_done, samples_failed, samples_declared \
         FROM run_projection WHERE run_id='r1'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        run.get::<Option<serde_json::Value>, _>("samples"),
        Some(serde_json::json!({"ids": ["P07", "P08"]}))
    );
    assert_eq!(run.get::<Option<i32>, _>("samples_total"), Some(2));
    assert_eq!(run.get::<Option<i32>, _>("samples_done"), Some(0));
    assert_eq!(run.get::<Option<i32>, _>("samples_failed"), Some(0));
    assert_eq!(run.get::<Option<bool>, _>("samples_declared"), Some(true));
}

#[tokio::test]
#[ignore = "requires the LocalStack/Postgres harness"]
async fn a_failed_run_counts_done_and_failed_samples_apart_and_stops_the_rest() {
    use biffix_core::event::SampleSpec;
    use biffix_core::sample_progress::SampleState;
    let pool = common::isolated_pool(
        "a_failed_run_counts_done_and_failed_samples_apart_and_stops_the_rest",
    )
    .await;
    let data = |v: serde_json::Value| -> EventData { serde_json::from_value(v).unwrap() };
    let mut log = vec![run_ev(
        1.0,
        EventSource::BiffixControlPlane,
        "dk-sub",
        "r1",
        EventData::RunSubmitted(Box::new(biffix_core::event::RunSubmitted {
            samples: Some(SampleSpec {
                ids: vec!["A".into(), "B".into(), "C".into(), "D".into()],
                ..Default::default()
            }),
            ..biffix_core::testing::anchor()
        })),
    )];
    log.push(run_ev(
        2.0,
        EventSource::Snakemake,
        "dk-dag",
        "r1",
        data(serde_json::json!({"kind": "run_dag", "dag": {
            "nodes": [{"id": "trim"}, {"id": "align"}],
            "edges": [{"from": "trim", "to": "align"}]
        }})),
    ));
    // A finishes both steps, B fails its first, C finishes only its first; D never starts.
    let tasks = [
        ("trim:sample=A", true),
        ("align:sample=A", true),
        ("trim:sample=B", false),
        ("trim:sample=C", true),
    ];
    for (i, (key, ok)) in tasks.iter().enumerate() {
        let ts = 3.0 + i as f64;
        let job = format!("job-{i}");
        let group = key.split(':').next().unwrap();
        let created = run_ev(
            ts,
            EventSource::Snakemake,
            &format!("dk-c{i}"),
            "r1",
            EventData::TaskCreated { group: Some(group.into()), deps: vec![] },
        )
        .with_task(key);
        let sub = task_submitted(ts + 0.1, &format!("dk-s{i}"), "r1", key, &job);
        let outcome = if *ok {
            EventData::TaskSucceeded { exit_code: 0, log_stream: None }
        } else {
            EventData::TaskFailed {
                exit_code: 1,
                termination_category: None,
                status_reason: None,
                log_stream: None,
            }
        };
        let end = ev(ts + 0.2, EventSource::AwsBatch, format!("dk-e{i}"), outcome).with_job(&job);
        log.extend([created, sub, end]);
    }
    log.push(run_ev(
        9.0,
        EventSource::Snakemake,
        "dk-failed",
        "r1",
        data(serde_json::json!({"kind": "run_failed"})),
    ));
    // A second run whose only task names no sample.
    let study = run_ev(
        1.0,
        EventSource::Snakemake,
        "dk-r2",
        "r2",
        EventData::TaskCreated { group: Some("multiqc".into()), deps: vec![] },
    )
    .with_task("multiqc");
    log.push(study);
    for e in &log {
        append(&pool, e).await.unwrap();
    }

    // The fold: A done, B failed, C and D stopped by the run's end.
    let grid = Projection::replay(&log).runs["r1"].sample_grid();
    let states: Vec<_> = grid.samples.iter().map(|r| (r.id.as_str(), r.state)).collect();
    assert_eq!(
        states,
        [
            ("A", SampleState::Done),
            ("B", SampleState::Failed),
            ("C", SampleState::Stopped),
            ("D", SampleState::Stopped)
        ]
    );
    // The persisted counts are the fold's, each in its own column.
    let counts = {
        let pool = &pool;
        move |run: &'static str| async move {
            sqlx::query_as::<_, (Option<i32>, Option<i32>, Option<i32>, Option<bool>)>(
                "SELECT samples_total, samples_done, samples_failed, samples_declared \
                 FROM run_projection WHERE run_id = $1",
            )
            .bind(run)
            .fetch_one(pool)
            .await
            .unwrap()
        }
    };
    assert_eq!(counts("r1").await, (Some(4), Some(1), Some(1), Some(true)));
    // No task names a sample: no counts at all.
    assert_eq!(counts("r2").await, (None, None, None, None));
}

#[tokio::test]
#[ignore = "requires the LocalStack/Postgres harness"]
async fn a_resumed_run_counts_the_samples_its_origin_finished() {
    use biffix_core::event::{ReusedSample, SampleSpec};
    let pool = common::isolated_pool("a_resumed_run_counts_reused_samples").await;
    // The resume ran nothing for A (finished before) and nothing at all for B.
    let log = vec![
        run_ev(
            1.0,
            EventSource::BiffixControlPlane,
            "dk-sub",
            "r1",
            EventData::RunSubmitted(Box::new(biffix_core::event::RunSubmitted {
                samples: Some(SampleSpec {
                    ids: vec!["A".into(), "B".into()],
                    reused: vec![ReusedSample {
                        id: "A".into(),
                        done: true,
                        steps: [("trim".to_string(), 1)].into(),
                    }],
                    ..Default::default()
                }),
                resumed_from: Some("r0".into()),
                ..biffix_core::testing::anchor()
            })),
        ),
        run_ev(2.0, EventSource::Snakemake, "dk-ok", "r1", EventData::RunSucceeded {}),
    ];
    ingest_batch(&pool, &log, &[]).await.unwrap();
    let counts = || async {
        sqlx::query_as::<_, (Option<i32>, Option<i32>)>(
            "SELECT samples_total, samples_done FROM run_projection WHERE run_id = 'r1'",
        )
        .fetch_one(&pool)
        .await
        .unwrap()
    };
    assert_eq!(counts().await, (Some(2), Some(1)));
    refold_runs(&pool, None, 100).await.unwrap();
    assert_eq!(counts().await, (Some(2), Some(1)));
}
