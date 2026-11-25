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

/// Seed an ended execution of `job` (role `role`) on host `host` and queue `queue`, started at
/// `started` (epoch seconds, `None`: no start recorded) and stopped at `stopped`, directly as a
/// pricing fixture: after any append, since a refold replaces the run's execution rows.
#[allow(clippy::too_many_arguments)]
async fn add_execution(
    pool: &sqlx::PgPool,
    job: &str,
    execution: &str,
    role: &str,
    host: &str,
    queue: &str,
    started: Option<f64>,
    stopped: f64,
) {
    sqlx::query(
        "INSERT INTO execution_projection (run_id, execution_id, batch_job_id, role, ordinal, \
         is_final, ended, instance_id, queue, started_at, stopped_at) \
         VALUES ('r1', $1, $2, $3, 1, false, true, $4, $5, $6, $7)",
    )
    .bind(execution)
    .bind(job)
    .bind(role)
    .bind(host)
    .bind(queue)
    .bind(started.map(|s| (s * 1e6) as i64))
    .bind((stopped * 1e6) as i64)
    .execute(pool)
    .await
    .unwrap();
}

/// A job Batch ran twice, and a coordinator, are priced per execution, each on its own host and
/// queue over its own run time, once; the legacy queues no longer offer them, though their
/// attempt and coordinator facts would price them. An unpriceable execution leaves the queue too.
#[tokio::test]
#[ignore = "requires the LocalStack/Postgres harness"]
async fn each_execution_is_priced_on_its_own_host_once() {
    use biffix_ingest::store::pricing::{
        record_job_cost, record_unpriceable, unpriced_executions, JobRole,
    };
    let pool = common::isolated_pool("each_execution_is_priced_on_its_own_host_once").await;
    register_host(&pool, "ci-a", "c6a.4xlarge", Some("spot")).await;
    register_host(&pool, "ci-b", "m6i.large", Some("on_demand")).await;
    // A coordinator the legacy queue would price: bound, on a known host, with its Batch facts.
    let run = RunId::new("r1");
    let bind = biffix_core::event::coordinator_submitted(
        &run,
        Timestamp::from_epoch_seconds(1.0),
        &BatchJobId::new("coord-1"),
    );
    let running = ev(
        2.0,
        EventSource::AwsBatch,
        "aws-batch:coord-1:RUNNING",
        EventData::CoordinatorRunning { log_stream: None },
    )
    .with_job("coord-1")
    .with_instance("ci-b")
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
    // A finished attempt the legacy queue would price over its whole span on ci-b.
    sqlx::query(
        "INSERT INTO attempt_projection (run_id, batch_job_id, task_key, attempt, state, queue, \
         instance_id, started_at, finished_at) \
         VALUES ('r1', 'job-1', 'align', 1, 'succeeded', 'q-od', 'ci-b', 1000000, 3600000000)",
    )
    .execute(&pool)
    .await
    .unwrap();
    // job-1 was reclaimed on spot ci-a, then finished on ci-b; the coordinator ran once.
    add_execution(&pool, "job-1", "e1", "task", "ci-a", "q-spot", Some(1.0), 100.0).await;
    add_execution(&pool, "job-1", "e2", "task", "ci-b", "q-od", Some(200.0), 3600.0).await;
    add_execution(&pool, "coord-1", "e3", "coordinator", "ci-b", "coord-od", Some(2.0), 602.0)
        .await;
    // Another job's execution, which turns out to be unpriceable.
    add_execution(&pool, "job-2", "e4", "task", "ci-a", "q-spot", Some(700.0), 800.0).await;
    assert!(all_unpriced(&pool, 10).await.is_empty(), "the legacy queues are gated off");

    let mut queued = unpriced_executions(&pool, 0, None, 10).await.unwrap();
    queued.sort_by(|a, b| a.execution_id.cmp(&b.execution_id));
    // (execution, role, host type, lifecycle, queue, start, stop)
    let seen: Vec<_> = queued
        .iter()
        .map(|j| {
            (
                j.execution_id.as_deref().unwrap(),
                j.role,
                j.instance_type.as_str(),
                j.lifecycle.as_deref().unwrap(),
                j.queue.as_deref().unwrap(),
                j.started_micros,
                j.finished_micros,
            )
        })
        .collect();
    assert_eq!(
        seen,
        [
            ("e1", JobRole::Task, "c6a.4xlarge", "spot", "q-spot", 1_000_000, 100_000_000),
            ("e2", JobRole::Task, "m6i.large", "on_demand", "q-od", 200_000_000, 3_600_000_000),
            (
                "e3",
                JobRole::Coordinator,
                "m6i.large",
                "on_demand",
                "coord-od",
                2_000_000,
                602_000_000
            ),
            ("e4", JobRole::Task, "c6a.4xlarge", "spot", "q-spot", 700_000_000, 800_000_000),
        ]
    );

    let cost = biffix_ingest::pricing::JobCost {
        hours: 1.0,
        price_per_hour: 0.2,
        price_source: biffix_ingest::pricing::PriceSource::OnDemand,
        compute_usd: 0.2,
        ebs_usd: 0.01,
    };
    record_unpriceable(&pool, &queued[3], "no_run_time").await.unwrap();
    for job in &queued[..3] {
        record_job_cost(&pool, job, job.lifecycle.as_deref(), &cost).await.unwrap();
        // Recorded again in place: still one line.
        record_job_cost(&pool, job, job.lifecycle.as_deref(), &cost).await.unwrap();
    }
    assert!(unpriced_executions(&pool, 0, None, 10).await.unwrap().is_empty());
    let count = |sql: &'static str| {
        let pool = pool.clone();
        async move { sqlx::query_scalar::<_, i64>(sql).fetch_one(&pool).await.unwrap() }
    };
    assert_eq!(count("SELECT COUNT(*) FROM execution_cost WHERE batch_job_id = 'job-1'").await, 2);
    let coordinator =
        "SELECT COUNT(*) FROM execution_cost WHERE execution_id = 'e3' AND role = 'coordinator'";
    assert_eq!(count(coordinator).await, 1);
    let unpriceable = "SELECT COUNT(*) FROM unpriceable_execution WHERE execution_id = 'e4'";
    assert_eq!(count(unpriceable).await, 1);
    assert_eq!(count("SELECT COUNT(*) FROM attempt_cost").await, 0);
    assert_eq!(count("SELECT COUNT(*) FROM unpriceable_job").await, 0);
}

/// The execution queue's keyset pages past executions that stopped at the same time, of one job
/// and of another, each once; an execution that stopped before the window, or ran on a host the
/// registry does not know, is not in it.
#[tokio::test]
#[ignore = "requires the LocalStack/Postgres harness"]
async fn the_execution_queue_pages_past_ties_in_stop_and_job() {
    use biffix_ingest::store::pricing::unpriced_executions;
    let pool = common::isolated_pool("the_execution_queue_pages_past_ties").await;
    register_host(&pool, "ci-1", "c6a.4xlarge", Some("spot")).await;
    for (job, execution) in [("job-a", "ea1"), ("job-a", "ea2"), ("job-a", "ea3"), ("job-b", "eb1")]
    {
        add_execution(&pool, job, execution, "task", "ci-1", "q", Some(1.0), 500.0).await;
    }
    add_execution(&pool, "job-c", "e-old", "task", "ci-1", "q", Some(1.0), 50.0).await;
    add_execution(&pool, "job-c", "e-nohost", "task", "ci-unknown", "q", Some(1.0), 500.0).await;
    let mut seen = Vec::new();
    let mut after = None;
    loop {
        let page = unpriced_executions(&pool, 100_000_000, after.as_ref(), 1).await.unwrap();
        let Some(last) = page.into_iter().next() else { break };
        seen.push(last.execution_id.clone().unwrap());
        assert!(seen.len() <= 4, "paged past the end: {seen:?}");
        after = Some(last);
    }
    seen.sort();
    assert_eq!(seen, ["ea1", "ea2", "ea3", "eb1"]);
}

/// An ended execution with no start is recorded as such once, and only within the window; one
/// with a start, or not ended, is not. The record never keeps it from being priced: once the fold
/// fills its start in, it is in the execution queue, and the record is dropped.
#[tokio::test]
#[ignore = "requires the LocalStack/Postgres harness"]
async fn an_execution_with_no_start_is_recorded_until_its_start_is_known() {
    use biffix_ingest::store::pricing::{mark_start_unknown_executions, unpriced_executions};
    let pool = common::isolated_pool("an_execution_with_no_start_is_recorded").await;
    register_host(&pool, "ci-1", "c6a.4xlarge", Some("spot")).await;
    add_execution(&pool, "job-1", "x-thin", "task", "ci-1", "q", None, 100.0).await;
    add_execution(&pool, "job-1", "x-old", "task", "ci-1", "q", None, 10.0).await;
    add_execution(&pool, "job-1", "x-ran", "task", "ci-1", "q", Some(1.0), 100.0).await;
    sqlx::query(
        "INSERT INTO execution_projection (run_id, execution_id, batch_job_id, role, ordinal, \
         is_final, ended) VALUES ('r1', 'x-open', 'job-1', 'task', 1, true, false)",
    )
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(mark_start_unknown_executions(&pool, 50_000_000).await.unwrap(), 1);
    assert_eq!(mark_start_unknown_executions(&pool, 50_000_000).await.unwrap(), 0);
    let marked: Vec<(String, String)> =
        sqlx::query_as("SELECT execution_id, reason FROM unpriceable_execution")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(marked, [("x-thin".to_string(), "start_unknown".to_string())]);
    // The sorted execution ids in the execution queue.
    let queued = |pool: sqlx::PgPool| async move {
        let page = unpriced_executions(&pool, 50_000_000, None, 10).await.unwrap();
        let mut ids: Vec<String> = page.into_iter().filter_map(|j| j.execution_id).collect();
        ids.sort();
        ids
    };
    assert_eq!(queued(pool.clone()).await, ["x-ran"]);
    // Its start is folded in late: it is priced like any other, and no longer recorded.
    sqlx::query(
        "UPDATE execution_projection SET started_at = 60000000 WHERE execution_id = 'x-thin'",
    )
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(queued(pool.clone()).await, ["x-ran", "x-thin"]);
    assert_eq!(mark_start_unknown_executions(&pool, 50_000_000).await.unwrap(), 0);
    let left: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM unpriceable_execution")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(left, 0);
}

/// An execution's line, or its unpriceable record, counts only while the execution's host, queue,
/// start and stop (and, for a line, its role) are the ones it was decided from: once the fold
/// changes one (a later fact, or a replay after a reducer fix), the execution is owed a line
/// again, and the new line or record replaces the old, so it keeps one, from its current inputs.
#[tokio::test]
#[ignore = "requires the LocalStack/Postgres harness"]
async fn an_execution_is_priced_again_when_the_fold_changes_its_inputs() {
    use biffix_ingest::store::pricing::{
        mark_start_unknown_executions, record_job_cost, record_unpriceable, unpriced_executions,
        JobRole,
    };
    let pool = common::isolated_pool("an_execution_is_priced_again").await;
    register_host(&pool, "ci-a", "c6a.4xlarge", Some("spot")).await;
    register_host(&pool, "ci-b", "m6i.large", Some("on_demand")).await;
    add_execution(&pool, "job-1", "e1", "task", "ci-a", "q", Some(1.0), 100.0).await;
    let cost = biffix_ingest::pricing::JobCost {
        hours: 1.0,
        price_per_hour: 0.2,
        price_source: biffix_ingest::pricing::PriceSource::OnDemand,
        compute_usd: 0.2,
        ebs_usd: 0.01,
    };
    let queued =
        |pool: sqlx::PgPool| async move { unpriced_executions(&pool, 0, None, 10).await.unwrap() };
    let fold = |sql: &'static str| {
        let pool = pool.clone();
        async move {
            sqlx::query(sql).execute(&pool).await.unwrap();
        }
    };
    // (instance, queue, start, stop) of the execution's line, and how many records it has.
    let recorded = |pool: sqlx::PgPool| async move {
        let lines: Vec<(String, Option<String>, i64, i64)> = sqlx::query_as(
            "SELECT instance_id, queue, started_at, stopped_at FROM execution_cost \
             WHERE execution_id = 'e1'",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        let records: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM unpriceable_execution WHERE execution_id = 'e1'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        (lines, records)
    };

    let job = queued(pool.clone()).await.pop().unwrap();
    record_job_cost(&pool, &job, job.lifecycle.as_deref(), &cost).await.unwrap();
    assert!(queued(pool.clone()).await.is_empty());

    // A later fact moves its stop: it is owed a line again, which replaces the old one.
    fold("UPDATE execution_projection SET stopped_at = 200000000 WHERE execution_id = 'e1'").await;
    let job = queued(pool.clone()).await.pop().expect("re-offered after its stop moved");
    assert_eq!((job.instance_id.as_str(), job.finished_micros), ("ci-a", 200_000_000));
    record_job_cost(&pool, &job, job.lifecycle.as_deref(), &cost).await.unwrap();
    assert!(queued(pool.clone()).await.is_empty());
    let ci_a = ("ci-a".to_string(), Some("q".to_string()), 1_000_000, 200_000_000);
    assert_eq!(recorded(pool.clone()).await, (vec![ci_a], 0));

    // A replay puts it on another host, where it cannot be priced: the record replaces the line.
    fold("UPDATE execution_projection SET instance_id = 'ci-b' WHERE execution_id = 'e1'").await;
    let job = queued(pool.clone()).await.pop().expect("re-offered after its host changed");
    assert_eq!((job.instance_id.as_str(), job.instance_type.as_str()), ("ci-b", "m6i.large"));
    record_unpriceable(&pool, &job, "no_run_time").await.unwrap();
    assert!(queued(pool.clone()).await.is_empty());
    assert_eq!(recorded(pool.clone()).await, (vec![], 1));

    // Its queue changes: the record no longer counts, and the tick drops it.
    fold("UPDATE execution_projection SET queue = 'q2' WHERE execution_id = 'e1'").await;
    let job = queued(pool.clone()).await.pop().expect("re-offered after its queue changed");
    assert_eq!(mark_start_unknown_executions(&pool, 0).await.unwrap(), 0);
    assert_eq!(recorded(pool.clone()).await, (vec![], 0));
    record_job_cost(&pool, &job, job.lifecycle.as_deref(), &cost).await.unwrap();
    assert!(queued(pool.clone()).await.is_empty());
    let ci_b = ("ci-b".to_string(), Some("q2".to_string()), 1_000_000, 200_000_000);
    assert_eq!(recorded(pool.clone()).await, (vec![ci_b], 0));

    // A replay binds its job as the run's coordinator: the line's role no longer matches, so it is
    // owed a line again, which records the new role.
    fold("UPDATE execution_projection SET role = 'coordinator' WHERE execution_id = 'e1'").await;
    let job = queued(pool.clone()).await.pop().expect("re-offered after its role changed");
    assert_eq!(job.role, JobRole::Coordinator);
    record_job_cost(&pool, &job, job.lifecycle.as_deref(), &cost).await.unwrap();
    assert!(queued(pool.clone()).await.is_empty());
    let role: String =
        sqlx::query_scalar("SELECT role FROM execution_cost WHERE execution_id = 'e1'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(role, "coordinator");

    // A replay drops its start: the line estimates a run time it no longer has, so the tick drops
    // it and records why it has no cost.
    fold("UPDATE execution_projection SET started_at = NULL WHERE execution_id = 'e1'").await;
    assert_eq!(mark_start_unknown_executions(&pool, 0).await.unwrap(), 1);
    assert_eq!(recorded(pool.clone()).await, (vec![], 1));
}
