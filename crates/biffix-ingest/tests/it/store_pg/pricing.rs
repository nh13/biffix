//! The pricing queue: finished jobs are priced once, and an unpriceable one leaves it.

use super::*;

/// Insert a registered host (the columns `unpriced_jobs` reads, plus the required ones).
async fn register_host(pool: &sqlx::PgPool, ci: &str, itype: &str, lifecycle: Option<&str>) {
    sqlx::query(
        "INSERT INTO host_registry (container_instance_id, ec2_instance_id, cluster_arn, \
         instance_type, availability_zone, detail_version, raw_detail, first_seen_at, lifecycle) \
         VALUES ($1, $1 || '-ec2', 'arn:cluster', $2, 'us-east-1a', 1, '{}', 0, $3)",
    )
    .bind(ci)
    .bind(itype)
    .bind(lifecycle)
    .execute(pool)
    .await
    .unwrap();
}

/// The first page of each role's unpriced jobs (tasks, then coordinators), over the whole window.
async fn all_unpriced(
    pool: &sqlx::PgPool,
    limit: i64,
) -> Vec<biffix_ingest::store::pricing::UnpricedJob> {
    use biffix_ingest::store::pricing::{unpriced_jobs, JobRole};
    let mut jobs = unpriced_jobs(pool, JobRole::Task, 0, None, limit).await.unwrap();
    jobs.extend(unpriced_jobs(pool, JobRole::Coordinator, 0, None, limit).await.unwrap());
    jobs
}

#[tokio::test]
#[ignore = "requires the LocalStack/Postgres harness"]
async fn finished_jobs_are_priced_once_tasks_and_coordinators_alike() {
    use biffix_ingest::store::pricing::{record_job_cost, JobRole};
    let pool = common::isolated_pool("finished_jobs_are_priced_once").await;
    register_host(&pool, "ci-task", "c6a.4xlarge", Some("spot")).await;
    register_host(&pool, "ci-coord", "m6i.large", None).await;
    // A coordinator that ran on the other host (its window comes from its own events). Its Batch
    // events carry no run id, as the translator emits them: the fold reaches it by the binding.
    let run = RunId::new("r1");
    let bind = biffix_core::event::coordinator_submitted(
        &run,
        Timestamp::from_epoch_seconds(1.0),
        &BatchJobId::new("coord-1"),
    );
    // The coordinator's own queue, from its Batch events (a deploy may have several).
    let running = ev(
        2.0,
        EventSource::AwsBatch,
        "aws-batch:coord-1:RUNNING",
        EventData::CoordinatorRunning { log_stream: None },
    )
    .with_job("coord-1")
    .with_instance("ci-coord")
    .with_queue("coord-od");
    let done = ev(
        602.0,
        EventSource::AwsBatch,
        "aws-batch:coord-1:SUCCEEDED",
        EventData::CoordinatorSucceeded { log_stream: None },
    )
    .with_job("coord-1");
    for e in [&bind, &running, &done] {
        append(&pool, e).await.unwrap();
    }
    // A finished task attempt on the spot host, seeded directly as a pricing fixture — after the
    // fold, since a refold replaces r1's attempt rows with exactly the ones its events produce.
    sqlx::query(
        "INSERT INTO attempt_projection (run_id, batch_job_id, task_key, attempt, state, queue, \
         instance_id, started_at, finished_at) \
         VALUES ('r1', 'job-1', 'align', 1, 'succeeded', 'q-spot', 'ci-task', 1000000, 3601000000)",
    )
    .execute(&pool)
    .await
    .unwrap();

    let mut jobs = all_unpriced(&pool, 10).await;
    jobs.sort_by(|a, b| a.batch_job_id.cmp(&b.batch_job_id));
    assert_eq!(jobs.len(), 2, "{jobs:?}");
    let (coord, task) = (&jobs[0], &jobs[1]);
    assert_eq!(
        (task.role, task.instance_type.as_str(), task.lifecycle.as_deref()),
        (JobRole::Task, "c6a.4xlarge", Some("spot"))
    );
    assert_eq!(task.queue.as_deref(), Some("q-spot"));
    assert_eq!((coord.role, coord.instance_type.as_str()), (JobRole::Coordinator, "m6i.large"));
    assert_eq!(coord.queue.as_deref(), Some("coord-od"));
    assert_eq!(coord.host_first_seen_micros, 0);
    assert_eq!((coord.started_micros, coord.finished_micros), (2_000_000, 602_000_000));

    let cost = biffix_ingest::pricing::JobCost {
        hours: 1.0,
        price_per_hour: 0.2,
        price_source: biffix_ingest::pricing::PriceSource::SpotHistory,
        compute_usd: 0.2,
        ebs_usd: 0.01,
    };
    for job in &jobs {
        record_job_cost(&pool, job, job.lifecycle.as_deref(), &cost).await.unwrap();
    }
    // Written once: nothing left to price, and a repeat record (even of another price) is a
    // no-op, not an error and not an update.
    assert!(all_unpriced(&pool, 10).await.is_empty());
    let repriced = biffix_ingest::pricing::JobCost {
        compute_usd: 9.9,
        price_source: biffix_ingest::pricing::PriceSource::OnDemand,
        ..cost.clone()
    };
    record_job_cost(&pool, task, Some("on_demand"), &repriced).await.unwrap();
    let n: i64 =
        sqlx::query_scalar("SELECT COUNT(*) c FROM attempt_cost").fetch_one(&pool).await.unwrap();
    assert_eq!(n, 2);
    let (usd, source, lifecycle): (f64, String, Option<String>) = sqlx::query_as(
        "SELECT compute_usd, price_source, lifecycle FROM attempt_cost \
         WHERE batch_job_id = 'job-1'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!((usd, source.as_str(), lifecycle.as_deref()), (0.2, "spot_history", Some("spot")));
}

/// A coordinator on a queue the deploy does not list (`BIFFIX_COORDINATOR_QUEUES`) has its Batch
/// facts stored task-shaped; the fold still reads them as the coordinator's through its binding,
/// and so does the pricer. One whose Batch terminal never reached the log is not priced (it has no
/// run time), nor is one that never started.
#[tokio::test]
#[ignore = "requires the LocalStack/Postgres harness"]
async fn a_coordinator_is_priced_from_its_batch_facts_of_either_shape() {
    use biffix_ingest::store::pricing::JobRole;
    let pool = common::isolated_pool("a_coordinator_is_priced_from_either_shape").await;
    register_host(&pool, "ci-coord", "m6i.large", Some("on_demand")).await;
    for (run, job) in [("r1", "coord-1"), ("r2", "coord-2"), ("r3", "coord-3")] {
        let bind = common::coordinator_bound(run, 1.0, job, &format!("cp:{job}"));
        append(&pool, &bind).await.unwrap();
    }
    // As the translator emits a Batch fact: no run, only the job (and its host and queue).
    let batch = |job: &str, ts: f64, data: EventData| {
        ev(ts, EventSource::AwsBatch, format!("aws:{job}:{ts}"), data)
            .with_job(job)
            .with_instance("ci-coord")
            .with_queue("unlisted-coordinator-queue")
    };
    let started = || EventData::TaskStarted { started_at: None, log_stream: None };
    let succeeded = || EventData::TaskSucceeded { exit_code: 0, log_stream: None };
    // coord-1 ran and ended (its newest queue counts); coord-2 ran, but its terminal was missed;
    // coord-3 never started.
    for e in [
        batch("coord-1", 1.5, EventData::TaskQueued {}).with_queue("an-older-queue"),
        batch("coord-1", 2.0, started()),
        batch("coord-1", 602.0, succeeded()),
        batch("coord-2", 2.0, started()),
        batch("coord-3", 602.0, succeeded()),
    ] {
        append(&pool, &e).await.unwrap();
    }
    let host: Option<String> =
        sqlx::query_scalar("SELECT coordinator_host_id FROM run_projection WHERE run_id = 'r1'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(host.as_deref(), Some("ci-coord"), "the fold latched the host");

    let jobs = all_unpriced(&pool, 10).await;
    let found: Vec<(&str, JobRole, i64, i64, Option<&str>)> = jobs
        .iter()
        .map(|j| {
            (
                j.batch_job_id.as_str(),
                j.role,
                j.started_micros,
                j.finished_micros,
                j.queue.as_deref(),
            )
        })
        .collect();
    assert_eq!(
        found,
        [(
            "coord-1",
            JobRole::Coordinator,
            2_000_000,
            602_000_000,
            Some("unlisted-coordinator-queue")
        )]
    );
}

/// A job that can never be priced is recorded as such and leaves the oldest-first pricing queue,
/// so a pile of them cannot hold its head; it writes no cost row.
#[tokio::test]
#[ignore = "requires the LocalStack/Postgres harness"]
async fn an_unpriceable_job_leaves_the_pricing_queue() {
    use biffix_ingest::store::pricing::record_unpriceable;
    let pool = common::isolated_pool("an_unpriceable_job_leaves_the_pricing_queue").await;
    register_host(&pool, "ci-new", "zz9.hyperlarge", Some("on_demand")).await;
    register_host(&pool, "ci-ok", "c6a.4xlarge", Some("on_demand")).await;
    sqlx::query(
        "INSERT INTO attempt_projection (run_id, batch_job_id, task_key, attempt, state, queue, \
         instance_id, started_at, finished_at) VALUES \
         ('r1', 'job-old', 'a', 1, 'succeeded', 'q', 'ci-new', 1000000, 2000000), \
         ('r1', 'job-new', 'b', 1, 'succeeded', 'q', 'ci-ok', 1000000, 3000000)",
    )
    .execute(&pool)
    .await
    .unwrap();
    // Limit 1: the oldest (unpriceable) job holds the only slot.
    let head = all_unpriced(&pool, 1).await;
    assert_eq!(head.len(), 1);
    assert_eq!(head[0].batch_job_id, "job-old");
    record_unpriceable(&pool, &head[0], "unknown_instance_type").await.unwrap();
    // Idempotent: a repeat record keeps the first reason.
    record_unpriceable(&pool, &head[0], "no_run_time").await.unwrap();
    let reason: String =
        sqlx::query_scalar("SELECT reason FROM unpriceable_job WHERE batch_job_id = 'job-old'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(reason, "unknown_instance_type");
    let next = all_unpriced(&pool, 1).await;
    assert_eq!(next.len(), 1);
    assert_eq!(next[0].batch_job_id, "job-new", "the queue moves past it");
    let costs: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM attempt_cost").fetch_one(&pool).await.unwrap();
    assert_eq!(costs, 0);
}

/// The queue is paged by a keyset, so the pricer can step past the jobs it leaves for a later tick
/// (a failed spot-history lookup) instead of fetching the same oldest page every time; the window
/// keeps out a job that finished before it, and a job that never started is never priceable.
#[tokio::test]
#[ignore = "requires the LocalStack/Postgres harness"]
async fn the_pricing_queue_pages_past_its_head_within_the_window() {
    use biffix_ingest::store::pricing::{unpriced_jobs, JobRole};
    let pool = common::isolated_pool("the_pricing_queue_pages_past_its_head").await;
    register_host(&pool, "ci-1", "c6a.4xlarge", Some("spot")).await;
    sqlx::query(
        "INSERT INTO attempt_projection (run_id, batch_job_id, task_key, attempt, state, queue, \
         instance_id, started_at, finished_at) VALUES \
         ('r1', 'job-old', 'a', 1, 'succeeded', 'q', 'ci-1', 1000000, 2000000), \
         ('r1', 'job-1', 'b', 1, 'succeeded', 'q', 'ci-1', 1000000, 5000000), \
         ('r2', 'job-1', 'b', 1, 'succeeded', 'q', 'ci-1', 1000000, 5000000), \
         ('r1', 'job-2', 'c', 1, 'succeeded', 'q', 'ci-1', 1000000, 6000000), \
         ('r1', 'job-unstarted', 'd', 1, 'failed', 'q', 'ci-1', NULL, 4000000)",
    )
    .execute(&pool)
    .await
    .unwrap();
    let key = |jobs: &[biffix_ingest::store::pricing::UnpricedJob]| {
        jobs.iter().map(|j| format!("{}/{}", j.run_id, j.batch_job_id)).collect::<Vec<_>>()
    };
    // Finished at 3 s or later: job-old is past the window.
    let first = unpriced_jobs(&pool, JobRole::Task, 3_000_000, None, 2).await.unwrap();
    assert_eq!(key(&first), ["r1/job-1", "r2/job-1"]);
    let second = unpriced_jobs(&pool, JobRole::Task, 3_000_000, first.last(), 2).await.unwrap();
    assert_eq!(key(&second), ["r1/job-2"]);
    let done = unpriced_jobs(&pool, JobRole::Task, 3_000_000, second.last(), 2).await.unwrap();
    assert!(done.is_empty(), "{done:?}");
}
