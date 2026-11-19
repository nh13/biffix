//! A run folded by the ingest writer reads back as the fold says, and run ids order bytewise.

use super::*;
use biffix_backend::store::cost::CostReads;
use biffix_backend::store::outputs::OutputStore;
use biffix_backend::store::pending::PendingLaunchStore;
use biffix_backend::store::provenance::ProvenanceStore;
use biffix_backend::store::runs::RunReads;

/// A run folded by the ingest writer (not seeded by hand) reads back through the store as the fold
/// of its events says: the run's state and progress, and each task's state, attempts, group and
/// display attempt. So the reader and the writer agree on every column they share.
#[tokio::test]
#[ignore = "requires --features pg + the Postgres harness"]
async fn a_folded_run_reads_back_as_the_fold_says() {
    use biffix_core::event::EventData;
    use biffix_core::source::EventSource;
    use biffix_core::testing::{self, RunEventExt};
    use biffix_core::time::Timestamp;

    let pool = migrated_pool("folded_run_reads_back").await;
    let run = RunId::new("rf");
    // A run-level event of the run.
    let ev = |secs: f64, source: EventSource, dedup: &str, data: EventData| {
        testing::event(secs, source, dedup, data).with_run(run.as_str())
    };
    // An engine event about task `key`'s first attempt, Batch job `job`.
    let task = |secs: f64, dedup: &str, key: &str, job: &str, data: EventData| {
        ev(secs, EventSource::Snakemake, dedup, data).with_task(key).with_attempt(1).with_job(job)
    };
    // A Batch event about job `job` (no run or task: joined on the job id).
    let batch = |secs: f64, dedup: &str, job: &str, data: EventData| {
        testing::event(secs, EventSource::AwsBatch, dedup, data).with_job(job)
    };
    let created = |group: &str| EventData::TaskCreated {
        group: Some(group.into()),
        deps: vec![],
        image: None,
    };
    let started = || EventData::TaskStarted { started_at: None, log_stream: None };
    let events = vec![
        biffix_core::event::run_submitted(
            &run,
            Timestamp::from_epoch_seconds(1.0),
            biffix_core::event::RunSubmitted::new(
                "v1",
                "abc",
                "sha256:x",
                serde_json::json!({}),
                vec![],
            ),
        ),
        ev(2.0, EventSource::Snakemake, "started", EventData::RunStarted { session_id: None }),
        task(3.0, "a-created", "align:s=A", "job-a", created("align")),
        task(
            3.1,
            "a-sub",
            "align:s=A",
            "job-a",
            EventData::TaskSubmitted { image: Some("repo/a@sha256:1".into()) },
        ),
        batch(4.0, "a-start", "job-a", started()),
        batch(5.0, "a-ok", "job-a", EventData::TaskSucceeded { exit_code: 0, log_stream: None }),
        task(6.0, "s-created", "sort:s=A", "job-s", created("sort")),
        task(6.1, "s-sub", "sort:s=A", "job-s", EventData::TaskSubmitted { image: None }),
        batch(7.0, "s-start", "job-s", started()),
        // The engine failed `align` although its job succeeded.
        {
            let mut e = task(
                5.5,
                "a-verdict",
                "align:s=A",
                "job-a",
                EventData::EngineTaskVerdict {
                    verdict: biffix_core::event::TaskVerdict::Failed,
                    category: Some(biffix_core::event::TerminationCategory::MissingOutput),
                    reason: Some("missing A.bam".into()),
                },
            );
            e.batch_job_id = None;
            e
        },
        ev(
            8.0,
            EventSource::Snakemake,
            "progress",
            EventData::RunProgress { completed: 1, total: 2 },
        ),
    ];
    biffix_db::log::append_and_advance(&pool, &events).await.unwrap();

    let fold = biffix_core::reducer::Projection::replay(&events);
    let want = &fold.runs[run.as_str()];
    let got = PgStore::new(pool).get_run(&run).await.unwrap();
    assert_eq!(got.run.state, want.state().as_str());
    assert_eq!(
        (got.run.completed, got.run.total),
        (want.completed.map(|n| n as i64), want.total.map(|n| n as i64))
    );
    // The materialized task counts, read back through the reader's aliases.
    let counts = want.task_counts();
    assert_eq!(
        [
            got.run.tasks_observed,
            got.run.tasks_succeeded,
            got.run.tasks_running,
            got.run.tasks_queued,
            got.run.tasks_failed,
            got.run.tasks_cancelled,
            got.run.tasks_cached,
        ],
        [
            counts.observed,
            counts.succeeded,
            counts.running,
            counts.queued,
            counts.failed,
            counts.cancelled,
            counts.cached,
        ]
        .map(|n| n as i64)
    );
    let read = got
        .tasks
        .iter()
        .map(|t| {
            (
                t.task_key.to_string(),
                t.state.clone(),
                t.attempts,
                t.group.clone(),
                t.batch_job_id.as_ref().map(ToString::to_string),
                t.exit_code,
                t.termination_category.clone(),
                t.image.clone(),
                t.verdict_reason.clone(),
            )
        })
        .collect::<std::collections::BTreeSet<_>>();
    let folded = want
        .tasks
        .values()
        .map(|t| {
            let shown = t.display_attempt();
            (
                t.task_key.to_string(),
                want.effective_task_state(t).as_str().to_string(),
                t.attempts.len() as i64,
                t.group.clone(),
                shown.map(|a| a.batch_job_id.to_string()),
                shown.and_then(|a| a.exit_code).map(i64::from),
                shown
                    .and_then(|a| a.termination_category.as_ref())
                    .or_else(|| t.verdict().and_then(|v| v.category.as_ref()))
                    .map(|c| c.as_str().to_string()),
                t.image().map(str::to_string),
                t.verdict().and_then(|v| v.reason.clone()),
            )
        })
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(folded.len(), 2, "{folded:?}");
    assert_eq!(read, folded);
    let align = got.tasks.iter().find(|t| t.task_key.as_str() == "align:s=A").unwrap();
    assert_eq!(
        (align.state.as_str(), align.image.as_deref(), align.verdict_reason.as_deref()),
        ("failed", Some("repo/a@sha256:1"), Some("missing A.bam"))
    );
}

/// Real log dedup preserves image observations, and a host retry removes lost local SQL rows.
#[tokio::test]
#[ignore = "requires --features pg + the Postgres harness"]
async fn single_job_generation_replaces_lost_work_through_the_log() {
    use biffix_core::event::{RunSubmitted, SingleJobUse};
    use biffix_core::source::EventSource;
    use biffix_core::time::Timestamp;

    let pool = migrated_pool("single_job_generation").await;
    let run = RunId::new("single-job-generation");
    let mut submitted = RunSubmitted::new("v1", "abc", "sha256:x", serde_json::json!({}), vec![]);
    submitted.single_job = Some(SingleJobUse {
        queue_class: "whole-host".into(),
        instance_type: "r6id.4xlarge".into(),
        vcpus: 16,
        memory_mib: 120_000,
    });
    let anchor =
        biffix_core::event::run_submitted(&run, Timestamp::from_epoch_seconds(1.0), submitted);
    let wire = |mut value: serde_json::Value| {
        value["run_id"] = run.as_str().into();
        biffix_core::translate::from_engine_value(EventSource::Nextflow, value).unwrap()
    };
    let created = |image| {
        wire(serde_json::json!({"kind": "task_created", "ts": 2.0,
        "task_key": "align", "image": image}))
    };
    let initial = vec![
        anchor,
        created(None::<&str>),
        created(Some("repo/b")),
        created(Some("repo/a")),
        wire(serde_json::json!({"kind": "task_cached", "ts": 3.0, "task_key": "align"})),
        wire(serde_json::json!({"kind": "run_progress", "ts": 3.0, "completed": 1, "total": 2})),
        wire(serde_json::json!({"kind": "task_outputs", "ts": 3.0, "task_key": "align",
            "outputs": [{"s3_uri": "s3://b/lost"}]})),
    ];
    assert_eq!(biffix_db::log::append_and_advance(&pool, &initial).await.unwrap(), initial.len());
    assert_eq!(biffix_db::log::append_and_advance(&pool, &initial).await.unwrap(), 0);
    let got = PgStore::new(pool.clone()).get_run(&run).await.unwrap();
    assert_eq!(got.tasks[0].image.as_deref(), Some("repo/a"));
    assert_eq!(got.run.tasks_succeeded, 1);

    let boundary =
        wire(serde_json::json!({"kind": "run_started", "ts": 4.0, "coordinator_attempt": 2}));
    biffix_db::log::append_and_advance(&pool, &[boundary]).await.unwrap();
    let got = PgStore::new(pool.clone()).get_run(&run).await.unwrap();
    assert!(got.tasks.is_empty());
    assert_eq!((got.run.tasks_succeeded, got.run.tasks_cached), (0, 0));
    assert_eq!((got.run.completed, got.run.total), (None, None));
    let outputs: i64 = sqlx::query_scalar("SELECT count(*) FROM run_output WHERE run_id = $1")
        .bind(run.as_str())
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(outputs, 0);

    let failed = wire(serde_json::json!({"kind": "task_failed", "ts": 5.0,
        "task_key": "align", "coordinator_attempt": 2}));
    biffix_db::log::append_and_advance(&pool, &[failed]).await.unwrap();
    let got = PgStore::new(pool).get_run(&run).await.unwrap();
    assert_eq!(got.tasks[0].state, "failed");
    assert_eq!((got.run.tasks_failed, got.run.tasks_succeeded), (1, 0));
}

/// A cancelled run's task that was still running reads back `cancelled`, and the run's summary
/// counts it under `tasks_cancelled`, not `tasks_failed`.
#[tokio::test]
#[ignore = "requires --features pg + the Postgres harness"]
async fn a_cancelled_runs_open_task_reads_back_cancelled() {
    use biffix_core::event::EventData;
    use biffix_core::source::EventSource;
    use biffix_core::testing::{self, RunEventExt};
    use biffix_core::time::Timestamp;

    let pool = migrated_pool("cancelled_run_reads_back").await;
    let run = RunId::new("rc");
    let ev = |secs: f64, source: EventSource, dedup: &str, data: EventData| {
        testing::event(secs, source, dedup, data).with_run(run.as_str())
    };
    let events = vec![
        biffix_core::event::run_submitted(
            &run,
            Timestamp::from_epoch_seconds(1.0),
            biffix_core::event::RunSubmitted::new(
                "v1",
                "abc",
                "sha256:x",
                serde_json::json!({}),
                vec![],
            ),
        ),
        ev(3.0, EventSource::Snakemake, "sub", EventData::TaskSubmitted { image: None })
            .with_task("align:s=A")
            .with_attempt(1)
            .with_job("job-a"),
        testing::event(
            4.0,
            EventSource::AwsBatch,
            "start",
            EventData::TaskStarted { started_at: None, log_stream: None },
        )
        .with_job("job-a"),
        ev(
            5.0,
            EventSource::BiffixControlPlane,
            "cancel",
            EventData::CancelRequested { actor: "alice".into(), actor_label: None },
        ),
        ev(
            6.0,
            EventSource::AwsBatch,
            "coord-failed",
            EventData::CoordinatorFailed {
                log_stream: None,
                exit_code: None,
                status_reason: None,
                termination_category: None,
            },
        ),
    ];
    biffix_db::log::append_and_advance(&pool, &events).await.unwrap();

    let got = PgStore::new(pool).get_run(&run).await.unwrap();
    assert_eq!(got.run.state, "cancelled");
    assert_eq!((got.run.tasks_observed, got.run.tasks_failed, got.run.tasks_cancelled), (1, 0, 1));
    let align = &got.tasks[0];
    assert_eq!(
        (align.state.as_str(), align.termination_category.as_deref()),
        ("cancelled", None),
        "a cancelled task is not reported as one the run's end left without a result"
    );
}

/// Where rows tie on a sort's leading keys, the run id breaks the tie bytewise (`COLLATE "C"`), as
/// the in-memory store and clients order them, not by the database's collation: under `en_US`,
/// `x_b` sorts before `xB`; bytewise, `xB` (0x42) comes before `x_b` (0x5f).
#[tokio::test]
#[ignore = "requires --features pg + the Postgres harness"]
async fn run_id_ties_order_bytewise_whatever_the_database_collation() {
    use biffix_backend::pg::registry::PgRegistryStore;
    use biffix_backend::store::pending::PendingLaunch;
    use biffix_backend::store::registry::RegistryStore;
    let pool = migrated_pool("run_id_ties_bytewise").await;
    // The test means something only on a database whose collation disagrees with bytewise order.
    let collation_orders_otherwise: bool =
        sqlx::query_scalar("SELECT 'x_b' < 'xB'").fetch_one(&pool).await.unwrap();
    assert!(
        collation_orders_otherwise,
        "the harness database must use a non-C collation (en_US.utf8)"
    );
    let mut v = registered_version("v1");
    v.name = "rnaseq".into();
    PgRegistryStore::new(pool.clone()).put_version(&v).await.unwrap();
    // Two active runs in one prefix; two priced successes completed at the same instant; two
    // ended runs whose newest events arrived at the same instant.
    for (run, state, prefix, completed, usd) in [
        ("q_b", "running", Some("s3://b/tie/"), None, None),
        ("qB", "running", Some("s3://b/tie/"), None, None),
        ("e_b", "succeeded", None, Some(7_200_000_000_i64), Some(2.0)),
        ("eB", "succeeded", None, Some(7_200_000_000), Some(3.0)),
        ("d_b", "failed", None, Some(1), None),
        ("dB", "failed", None, Some(1), None),
    ] {
        sqlx::query(
            "INSERT INTO run_projection (run_id, state, output_prefix, workflow_version, \
             sample_count, submitted_at, launched_at, completed_at) \
             VALUES ($1, $2, $3, 'v1', 1, 0, 0, $4)",
        )
        .bind(run)
        .bind(state)
        .bind(prefix)
        .bind(completed)
        .execute(&pool)
        .await
        .unwrap();
        if let Some(usd) = usd {
            sqlx::query(
                "INSERT INTO attempt_cost (run_id, batch_job_id, role, instance_type, hours, \
                 price_per_hour, price_source, compute_usd, ebs_usd) \
                 VALUES ($1, 'j1', 'task', 'c7i.large', 1, 1, 'on_demand', $2, 0)",
            )
            .bind(run)
            .bind(usd)
            .execute(&pool)
            .await
            .unwrap();
        }
    }
    for run in ["d_b", "dB"] {
        sqlx::query(
            "INSERT INTO run_events \
               (dedup_key, ts_micros, source, kind, run_id, schema_version, payload, received_at) \
             VALUES ($1, 1, 'aws_batch', 'spot_reclaim_warning', $1, 1, '{}', \
                     timestamptz '2020-01-01 00:00:00+00')",
        )
        .bind(run)
        .execute(&pool)
        .await
        .unwrap();
    }
    let store = PgStore::new(pool.clone());
    for run in ["t_b", "tB"] {
        store
            .put_pending_launch(&PendingLaunch {
                run_id: RunId::new(run),
                created_at_micros: 10,
                plan: serde_json::json!({}),
            })
            .await
            .unwrap();
    }

    assert_eq!(store.active_run_in_prefix("s3://b/tie/").await.unwrap(), Some(RunId::new("qB")));
    let usd = |rows: Vec<biffix_backend::runs::estimate::PastRun>| {
        rows.iter().map(|r| r.usd).collect::<Vec<_>>()
    };
    assert_eq!(usd(store.estimate_basis("p", "rnaseq", 20).await.unwrap()), [3.0, 2.0]);
    assert_eq!(usd(store.estimate_basis("p", "rnaseq", 1).await.unwrap()), [3.0]);
    let pending: Vec<String> = store
        .list_pending_launches()
        .await
        .unwrap()
        .into_iter()
        .map(|p| p.run_id.to_string())
        .collect();
    assert_eq!(pending, ["tB", "t_b"]);
    let due: Vec<String> = store
        .provenance_due(600, 1)
        .await
        .unwrap()
        .into_iter()
        .map(|(run, _)| run.to_string())
        .collect();
    assert_eq!(due, ["dB"]);
}

/// A single-job run's anchor is folded onto its projection row and read back on its detail, and a
/// run one Batch job per task reads none.
#[tokio::test]
#[ignore = "requires --features pg + the Postgres harness"]
async fn a_single_job_anchor_reads_back_on_the_run_detail() {
    use biffix_core::event::{run_submitted, RunSubmitted, SingleJobUse};
    use biffix_core::time::Timestamp;

    let pool = migrated_pool("single_job_anchor_reads_back").await;
    let single = SingleJobUse {
        queue_class: "single".into(),
        instance_type: "c6id.8xlarge".into(),
        vcpus: 32,
        memory_mib: 61_184,
    };
    let anchor = |id: &str, single_job: Option<SingleJobUse>| {
        run_submitted(
            &RunId::new(id),
            Timestamp::from_epoch_seconds(1.0),
            RunSubmitted {
                single_job,
                ..RunSubmitted::new("v1", "abc", "sha256:x", serde_json::json!({}), vec![])
            },
        )
    };
    let events = vec![anchor("rs", Some(single.clone())), anchor("rt", None)];
    biffix_db::log::append_and_advance(&pool, &events).await.unwrap();
    let store = PgStore::new(pool);
    assert_eq!(store.get_run(&RunId::new("rs")).await.unwrap().single_job, Some(single));
    assert_eq!(store.get_run(&RunId::new("rt")).await.unwrap().single_job, None);
}
