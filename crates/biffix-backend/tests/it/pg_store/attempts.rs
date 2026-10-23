//! Attempts and tasks joined to their hosts, requested resources, spot warnings and costs.

use super::*;
use biffix_backend::store::runs::RunReads;
use biffix_backend::store::tasks::TaskReads;

#[tokio::test]
#[ignore = "requires --features pg + the Postgres harness"]
async fn attempts_and_tasks_join_their_host() {
    let pool = seeded_pool("attempts_and_tasks_join_their_host").await;
    exec_all(
        &pool,
        &[
            // A reclaimed attempt on a host EC2 described (spot), and its retry on a host that
            // registered but whose lifecycle is not resolved yet.
            "INSERT INTO host_registry (container_instance_id, ec2_instance_id, cluster_arn, \
             instance_type, availability_zone, cpu_architecture, registered_vcpus, \
             registered_memory_mib, lifecycle, detail_version, raw_detail, first_seen_at) VALUES \
             ('ci-1', 'i-1', 'c', 'c8g.16xlarge', 'us-east-1b', 'arm64', 64, 126000, 'spot', 2, \
              '{}'::jsonb, 1), \
             ('ci-2', 'i-2', 'c', 'c8g.16xlarge', 'us-east-1a', 'arm64', 64, 126000, NULL, 2, \
              '{}'::jsonb, 2)",
            "INSERT INTO attempt_projection (run_id, batch_job_id, task_key, attempt, state, queue, \
             instance_id) VALUES \
             ('r1', 'job-1', 'align:sample=A', 1, 'failed', 'q-spot', 'ci-1'), \
             ('r1', 'job-2', 'align:sample=A', 2, 'running', 'q-spot', 'ci-2'), \
             ('r1', 'job-3', 'align:sample=A', 3, 'queued', 'q-spot', NULL)",
            "UPDATE task_projection SET batch_job_id = 'job-2', instance_id = 'ci-2' \
             WHERE run_id = 'r1' AND task_key = 'align:sample=A'",
        ],
    )
    .await;
    let store = PgStore::new(pool);

    let attempts =
        store.get_attempts(&RunId::new("r1"), &TaskKey::new("align:sample=A")).await.unwrap();
    assert_eq!(attempts.len(), 3);
    let first = attempts[0].host.as_ref().unwrap();
    assert_eq!(first.ec2_instance_id.as_deref(), Some("i-1"));
    assert_eq!(first.instance_type.as_deref(), Some("c8g.16xlarge"));
    assert_eq!(first.availability_zone.as_deref(), Some("us-east-1b"));
    assert_eq!(first.registered_vcpus, Some(64.0));
    assert_eq!(first.capacity.as_deref(), Some("spot"));
    assert_eq!(first.capacity_source.as_deref(), Some("instance"));
    // Unresolved lifecycle: host facts, no capacity yet (the handler falls back to the queue).
    let second = attempts[1].host.as_ref().unwrap();
    assert_eq!(second.availability_zone.as_deref(), Some("us-east-1a"));
    assert_eq!(second.capacity, None);
    assert_eq!(second.queue.as_deref(), Some("q-spot"));
    // No host yet: only the queue is known.
    let third = attempts[2].host.as_ref().unwrap();
    assert_eq!(third.ec2_instance_id, None);
    assert_eq!(third.queue.as_deref(), Some("q-spot"));

    let detail = store.get_run(&RunId::new("r1")).await.unwrap();
    let task = detail.tasks.iter().find(|t| t.task_key.as_str() == "align:sample=A").unwrap();
    let host = task.host.as_ref().unwrap();
    assert_eq!(host.ec2_instance_id.as_deref(), Some("i-2"), "the display attempt's host");
    assert_eq!(host.queue.as_deref(), Some("q-spot"));
}

#[tokio::test]
#[ignore = "requires --features pg + the Postgres harness"]
async fn attempts_and_tasks_carry_the_requested_resources() {
    let pool = seeded_pool("attempts_and_tasks_carry_requested_resources").await;
    exec_all(
        &pool,
        &[
            // A first attempt that asked for less memory, its retry (the display attempt) that
            // asked for more (a fractional vCPU count that a REAL column would read back as
            // 3.299999952316284), and an attempt folded before the request was recorded.
            "INSERT INTO host_registry (container_instance_id, ec2_instance_id, cluster_arn, \
             registered_vcpus, registered_memory_mib, detail_version, raw_detail, first_seen_at) \
             VALUES ('ci-1', 'i-1', 'c', 64, 126000, 2, '{}'::jsonb, 1)",
            "INSERT INTO attempt_projection (run_id, batch_job_id, task_key, attempt, state, \
             instance_id, requested_vcpus, requested_memory_mib) VALUES \
             ('r1', 'job-1', 'align:sample=A', 1, 'failed', 'ci-1', 4, 15000), \
             ('r1', 'job-2', 'align:sample=A', 2, 'succeeded', 'ci-1', 3.3, 30000), \
             ('r1', 'job-3', 'align:sample=A', 3, 'failed', NULL, NULL, NULL)",
            "UPDATE task_projection SET batch_job_id = 'job-2', instance_id = 'ci-1' \
             WHERE run_id = 'r1' AND task_key = 'align:sample=A'",
            // A task with no attempt yet.
            "INSERT INTO task_projection (run_id, task_key, state, attempts) \
             VALUES ('r1', 'call:sample=A', 'pending', 0)",
        ],
    )
    .await;
    let store = PgStore::new(pool);

    let attempts =
        store.get_attempts(&RunId::new("r1"), &TaskKey::new("align:sample=A")).await.unwrap();
    let requested: Vec<(Option<f64>, Option<i64>)> =
        attempts.iter().map(|a| (a.requested_vcpus, a.requested_memory_mib)).collect();
    assert_eq!(requested, vec![(Some(4.0), Some(15_000)), (Some(3.3), Some(30_000)), (None, None)]);
    // The request sits beside, not in place of, the host's capacity.
    let host = attempts[0].host.as_ref().unwrap();
    assert_eq!(host.registered_vcpus, Some(64.0));
    assert_eq!(host.registered_memory_mib, Some(126_000));

    let detail = store.get_run(&RunId::new("r1")).await.unwrap();
    let task = detail.tasks.iter().find(|t| t.task_key.as_str() == "align:sample=A").unwrap();
    assert_eq!(
        (task.requested_vcpus, task.requested_memory_mib),
        (Some(3.3), Some(30_000)),
        "the display attempt's request, read back exactly (not widened from a REAL)"
    );
    let unattempted =
        detail.tasks.iter().find(|t| t.task_key.as_str() != "align:sample=A").unwrap();
    assert_eq!((unattempted.requested_vcpus, unattempted.requested_memory_mib), (None, None));
}

#[tokio::test]
#[ignore = "requires --features pg + the Postgres harness"]
async fn attempts_are_matched_to_spot_warnings_on_their_host_while_running() {
    let pool = seeded_pool("attempts_matched_to_spot_warnings").await;
    exec_all(
        &pool,
        &[
            "INSERT INTO host_registry (container_instance_id, ec2_instance_id, cluster_arn, \
             detail_version, raw_detail, first_seen_at) VALUES ('ci-1', 'i-1', 'c', 1, '{}'::jsonb, 1)",
            // job-1: host known via the registry, warned while running.
            // job-2: host predates the registry; Batch's reason names it; warned while running.
            // job-3: its host was warned only after the attempt had finished.
            "INSERT INTO attempt_projection (run_id, batch_job_id, task_key, attempt, state, \
             status_reason, instance_id, started_at, finished_at) VALUES \
             ('r1', 'job-1', 't', 1, 'failed', NULL, 'ci-1', 100, 200), \
             ('r1', 'job-2', 't', 2, 'failed', 'Host EC2 (instance i-2) terminated.', 'ci-x', 300, 400), \
             ('r1', 'job-3', 't', 3, 'failed', 'Host EC2 (instance i-3) terminated.', 'ci-y', 500, 600)",
            "INSERT INTO spot_interruption (ec2_instance_id, warned_at) VALUES \
             ('i-1', 150), ('i-2', 350), ('i-3', 900)",
        ],
    )
    .await;
    let store = PgStore::new(pool);
    let attempts = store.get_attempts(&RunId::new("r1"), &TaskKey::new("t")).await.unwrap();
    let warned: Vec<Option<f64>> = attempts.iter().map(|a| a.spot_warned_at).collect();
    assert_eq!(warned, vec![Some(150e-6), Some(350e-6), None]);
}

#[tokio::test]
#[ignore = "requires --features pg + the Postgres harness"]
async fn run_detail_rolls_up_attempt_costs() {
    // align ran twice (a spot-lost first attempt, then the final one); the coordinator is priced;
    // a third attempt is not priced yet.
    let pool = seeded_pool("run_detail_rolls_up_attempt_costs").await;
    exec_all(
        &pool,
        &[
            "UPDATE run_projection SET coordinator_job_id = 'coord' WHERE run_id = 'r1'",
            "UPDATE task_projection SET batch_job_id = 'a2' WHERE run_id = 'r1'",
            "INSERT INTO attempt_projection (run_id, batch_job_id, task_key, attempt, state) VALUES \
               ('r1','a1','align:sample=A',1,'failed'), ('r1','a2','align:sample=A',2,'succeeded'), \
               ('r1','a3','align:sample=A',3,'running')",
            "INSERT INTO attempt_cost (run_id, batch_job_id, role, instance_type, lifecycle, hours, \
               price_per_hour, price_source, compute_usd, ebs_usd) VALUES \
               ('r1','a1','task','c6a.4xlarge','spot',1,0.2,'spot_history',0.2,0.01), \
               ('r1','a2','task','c6a.4xlarge','spot',1,0.3,'spot_history',0.3,0.01), \
               ('r1','coord','coordinator','m6i.large','on_demand',1,0.096,'on_demand',0.096,0.004)",
        ],
    )
    .await;
    let store = PgStore::new(pool);
    let d = store.get_run(&RunId::new("r1")).await.unwrap();
    let cost = d.cost.expect("a run with priced jobs has a cost");
    let close = |a: f64, b: f64| (a - b).abs() < 1e-9;
    assert!(close(cost.estimated_usd, 0.21 + 0.31 + 0.1), "{cost:?}");
    assert!(close(cost.winning_usd, 0.31 + 0.1), "{cost:?}");
    assert!(close(cost.coordinator_usd, 0.1));
    assert_eq!((cost.priced_jobs, cost.pending_jobs), (3, 1));
    // The task's cost is the gross of its attempts.
    assert!(close(d.tasks[0].cost_usd.unwrap(), 0.52));
    // And each attempt carries its own.
    let attempts = store
        .get_attempts(&RunId::new("r1"), &biffix_core::ids::TaskKey::new("align:sample=A"))
        .await
        .unwrap();
    let a1 = attempts.iter().find(|a| a.batch_job_id.as_str() == "a1").unwrap();
    assert_eq!(a1.cost.as_ref().unwrap().price_source, "spot_history");
    assert!(attempts.iter().find(|a| a.batch_job_id.as_str() == "a3").unwrap().cost.is_none());
}
