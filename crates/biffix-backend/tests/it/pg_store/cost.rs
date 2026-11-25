//! Costs: launch estimates, usage, pricing and the list's run cost.

use super::*;
use biffix_backend::store::cost::CostReads;
use biffix_backend::store::runs::RunReads;

#[tokio::test]
#[ignore = "requires --features pg + the Postgres harness"]
async fn an_estimate_is_based_on_the_workflow_s_priced_successful_runs() {
    use biffix_backend::pg::registry::PgRegistryStore;
    use biffix_backend::store::registry::RegistryStore;

    let pool = migrated_pool("estimate_basis").await;
    let registry = PgRegistryStore::new(pool.clone());
    for (id, name) in [("v1", "rnaseq"), ("v2", "rnaseq"), ("other", "chipseq")] {
        let mut v = registered_version(id);
        v.name = name.into();
        registry.put_version(&v).await.unwrap();
    }
    // (run, version, state, samples, submitted s, completed s, usd)
    for (run, version, state, samples, sub, done, usd) in [
        ("a", "v1", "succeeded", Some(2), 0, 3600, Some(4.0)),
        ("b", "v2", "succeeded", Some(4), 100, 7300, Some(12.0)),
        ("failed", "v1", "failed", Some(2), 0, 50, Some(1.0)),
        ("unpriced", "v1", "succeeded", Some(2), 0, 60, None),
        ("no-samples", "v1", "succeeded", None, 0, 70, Some(1.0)),
        ("other-wf", "other", "succeeded", Some(2), 0, 80, Some(1.0)),
    ] {
        sqlx::query(
            "INSERT INTO run_projection (run_id, state, workflow_version, sample_count, \
             submitted_at, completed_at) VALUES ($1,$2,$3,$4,$5,$6)",
        )
        .bind(run)
        .bind(state)
        .bind(version)
        .bind(samples)
        .bind(sub as i64 * 1_000_000)
        .bind(done as i64 * 1_000_000)
        .execute(&pool)
        .await
        .unwrap();
        if let Some(usd) = usd {
            sqlx::query(
                "INSERT INTO attempt_cost (run_id, batch_job_id, role, instance_type, hours, \
                 price_per_hour, price_source, compute_usd, ebs_usd) \
                 VALUES ($1, 'j1', 'task', 'c7i.large', 1, 1, 'on_demand', $2, 0.5)",
            )
            .bind(run)
            .bind(usd - 0.5)
            .execute(&pool)
            .await
            .unwrap();
        }
    }
    // Each launched when it was submitted, but `b` waited 1000 s for its submit-time images: its
    // run time is from launch.
    exec_all(
        &pool,
        &[
            "UPDATE run_projection SET launched_at = submitted_at",
            "UPDATE run_projection SET launched_at = 1100000000 WHERE run_id = 'b'",
        ],
    )
    .await;
    // Successful runs no basis counts: a resume of `a` (it reused `a`'s work), one with no launch
    // time, and one priced but with a job not priced yet (an attempt the pricer can still price).
    for (run, resumed_from, launched) in [
        ("resumed", Some("a"), Some(0)),
        ("unlaunched", None, None),
        ("partly-priced", None, Some(0)),
    ] {
        sqlx::query(
            "INSERT INTO run_projection (run_id, state, workflow_version, sample_count, \
             submitted_at, launched_at, completed_at, resumed_from) \
             VALUES ($1, 'succeeded', 'v1', 2, 0, $2, 90000000, $3)",
        )
        .bind(run)
        .bind(launched)
        .bind(resumed_from)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO attempt_cost (run_id, batch_job_id, role, instance_type, hours, \
             price_per_hour, price_source, compute_usd, ebs_usd) \
             VALUES ($1, 'j1', 'task', 'c7i.large', 1, 1, 'on_demand', 0.5, 0.5)",
        )
        .bind(run)
        .execute(&pool)
        .await
        .unwrap();
    }
    // Its unpriced job finished just now on a host the registry knows: the pricer will price it.
    // (An attempt still open in an ended run is not pending: its terminal was lost.)
    exec_all(
        &pool,
        &["INSERT INTO host_registry (container_instance_id, ec2_instance_id, cluster_arn, \
             instance_type, detail_version, raw_detail, first_seen_at) VALUES \
             ('ci-1', 'i-1', 'c', 'c7i.large', 1, '{}'::jsonb, 1)"],
    )
    .await;
    let now = biffix_core::clock::epoch_micros();
    sqlx::query(
        "INSERT INTO attempt_projection (run_id, batch_job_id, task_key, attempt, state, \
         instance_id, started_at, finished_at) \
         VALUES ('partly-priced', 'j2', 'align:sample=A', 1, 'succeeded', 'ci-1', $1, $2)",
    )
    .bind(now - 60_000_000)
    .bind(now)
    .execute(&pool)
    .await
    .unwrap();
    let store = PgStore::new(pool);
    let basis = store.estimate_basis("p", "rnaseq", 20).await.unwrap();
    // Newest first; only priced, successful runs with a sample count, of this workflow (any version).
    assert_eq!(
        basis.iter().map(|r| (r.usd, r.samples, r.wall_secs)).collect::<Vec<_>>(),
        [(12.0, 4, 6200.0), (4.0, 2, 3600.0)]
    );
    // A limit keeps the newest: `b`, not `a`.
    let newest = store.estimate_basis("p", "rnaseq", 1).await.unwrap();
    assert_eq!(
        newest.iter().map(|r| (r.usd, r.samples, r.wall_secs)).collect::<Vec<_>>(),
        [(12.0, 4, 6200.0)]
    );
    // A typical run's basis: every fresh, launched successful run of the workflow, priced or not
    // (a partly priced one's cost is unknown), with a sample count or not; newest first
    // (completed 7300, 3600, 90, 70, 60).
    let typical = store.typical_basis("p", "rnaseq", 20).await.unwrap();
    let walls = |basis: &[biffix_backend::runs::estimate::PastWall]| {
        basis.iter().map(|r| (r.wall_secs, r.usd)).collect::<Vec<_>>()
    };
    let want =
        [(6200.0, Some(12.0)), (3600.0, Some(4.0)), (90.0, None), (70.0, Some(1.0)), (60.0, None)];
    assert_eq!(walls(&typical), want);
    // Every workflow's at once, by (project, name), the same runs.
    let all = store.typical_bases(20).await.unwrap();
    assert_eq!(walls(&all[&("p".to_string(), "rnaseq".to_string())]), want);
    assert_eq!(walls(&all[&("p".to_string(), "chipseq".to_string())]), [(80.0, Some(1.0))]);
    assert_eq!(all.len(), 2);
    // A limit keeps the newest run, not just one of them.
    assert_eq!(
        walls(&store.typical_bases(1).await.unwrap()[&("p".to_string(), "rnaseq".to_string())]),
        [want[0]]
    );
    assert_eq!(walls(&store.typical_basis("p", "rnaseq", 1).await.unwrap()), [want[0]]);
    assert!(store.typical_basis("p", "nope", 20).await.unwrap().is_empty());
}

#[tokio::test]
#[ignore = "requires --features pg + the Postgres harness"]
async fn usage_groups_the_period_s_run_costs() {
    use biffix_backend::runs::usage::{parse_date, UsageGroup};
    let pool = migrated_pool("usage_groups_the_period_s_run_costs").await;
    for (id, name, project) in [("v1", "rnaseq", "p"), ("v2", "sarek", "q")] {
        sqlx::query(
            "INSERT INTO workflow_versions (registration_inputs, registration_inputs_hash, \
             runtime_key, runtime_recipe, \
             id, name, project, git_sha, source_content_hash, \
             image_digest, engine, engine_version, entrypoint, source, form_schema, queue_classes, \
             created_at) VALUES ('{}'::jsonb, '', '', '{}'::jsonb, \
             $1, $2, $3, 'abc', $1, '', 'nextflow', '25', 'main.nf', \
             '{}'::jsonb, '{}'::jsonb, '{}'::text[], 0)",
        )
        .bind(id)
        .bind(name)
        .bind(project)
        .execute(&pool)
        .await
        .unwrap();
    }
    let day = |d: &str| parse_date(d).unwrap();
    /// A priced job: (batch job id, instance type, compute USD, EBS USD).
    type Job = (&'static str, &'static str, f64, f64);
    // (run, version, submitter, submitted, jobs)
    let runs: [(&str, &str, &str, &str, &[Job]); 4] = [
        (
            "a",
            "v1",
            "alice@lab.example.org",
            "2026-08-10",
            &[("j1", "c6a.4xlarge", 1.0, 0.1), ("j2", "m6i.large", 0.5, 0.0)],
        ),
        ("b", "v1", "bob@lab.example.org", "2026-09-02", &[("j1", "c6a.4xlarge", 2.0, 0.2)]),
        ("c", "v2", "alice@lab.example.org", "2026-09-20", &[("j1", "c6a.4xlarge", 4.0, 0.4)]),
        // Outside the period.
        ("d", "v2", "alice@lab.example.org", "2026-10-01", &[("j1", "c6a.4xlarge", 100.0, 0.0)]),
    ];
    for (run, version, submitter, submitted, jobs) in runs {
        sqlx::query(
            "INSERT INTO run_projection (run_id, state, workflow_version, submitter_label, \
             submitted_at) VALUES ($1, 'succeeded', $2, $3, $4)",
        )
        .bind(run)
        .bind(version)
        .bind(submitter)
        .bind(day(submitted) + 3_600_000_000)
        .execute(&pool)
        .await
        .unwrap();
        for (job, itype, compute, ebs) in jobs {
            sqlx::query(
                "INSERT INTO attempt_cost (run_id, batch_job_id, role, instance_type, hours, \
                 price_per_hour, price_source, compute_usd, ebs_usd) \
                 VALUES ($1, $2, 'task', $3, 1, 1, 'on_demand', $4, $5)",
            )
            .bind(run)
            .bind(job)
            .bind(itype)
            .bind(compute)
            .bind(ebs)
            .execute(&pool)
            .await
            .unwrap();
        }
    }
    let store = PgStore::new(pool);
    let (from, to) = (day("2026-08-01"), day("2026-10-01"));
    let keys = |g| {
        let store = &store;
        async move {
            let r = store.usage(from, to, g).await.unwrap();
            (
                r.rows
                    .iter()
                    .map(|t| (t.key.clone(), t.runs, (t.compute_usd + t.ebs_usd) * 100.0))
                    .map(|(k, n, c)| (k, n, c.round() as i64))
                    .collect::<Vec<_>>(),
                r.total,
            )
        }
    };
    let (months, total) = keys(UsageGroup::Month).await;
    assert_eq!(months, vec![("2026-08".into(), 1, 160), ("2026-09".into(), 2, 660)]);
    assert_eq!(total.runs, 3, "the October run is outside the period");
    assert!((total.compute_usd - 7.5).abs() < 1e-9 && (total.ebs_usd - 0.7).abs() < 1e-9);
    // Largest first; a workflow is a name within its project.
    assert_eq!(
        keys(UsageGroup::Workflow).await.0,
        vec![("q/sarek".into(), 1, 440), ("p/rnaseq".into(), 2, 380)]
    );
    assert_eq!(
        keys(UsageGroup::Submitter).await.0,
        vec![("alice@lab.example.org".into(), 2, 600), ("bob@lab.example.org".into(), 1, 220)]
    );
    assert_eq!(
        keys(UsageGroup::InstanceType).await.0,
        vec![("c6a.4xlarge".into(), 3, 770), ("m6i.large".into(), 1, 50)]
    );
    assert_eq!(keys(UsageGroup::Project).await.0, vec![("q".into(), 1, 440), ("p".into(), 2, 380)]);
    // The period is `[from, to)`: a run submitted at exactly `from` (a) is in it, one at exactly
    // `to` (b) is not.
    let hour = 3_600_000_000;
    let edges = store
        .usage(day("2026-08-10") + hour, day("2026-09-02") + hour, UsageGroup::Month)
        .await
        .unwrap();
    assert_eq!(
        edges.rows.iter().map(|t| (t.key.as_str(), t.runs)).collect::<Vec<_>>(),
        [("2026-08", 1)]
    );
    assert_eq!(edges.total.runs, 1);
}

/// Only jobs the pricer can still price are pending: a job terminated before it got a host, one on
/// a host the registry never saw, and one that finished before the pricing window never will be.
#[tokio::test]
#[ignore = "requires --features pg + the Postgres harness"]
async fn only_jobs_the_pricer_can_still_price_are_pending() {
    let pool = migrated_pool("pending_jobs_priceable").await;
    let now = biffix_core::clock::epoch_micros();
    // The pricer's window (which the pending count mirrors), and an hour either side of its start.
    let window_start = now - biffix_db::rules::PRICING_WINDOW.as_micros() as i64;
    let hour = 3_600_000_000;
    let (inside, old) = (window_start + hour, window_start - hour);
    exec_all(
        &pool,
        &["INSERT INTO host_registry (container_instance_id, ec2_instance_id, cluster_arn, \
             instance_type, detail_version, raw_detail, first_seen_at) VALUES \
             ('ci-1', 'i-1', 'c', 'c6a.4xlarge', 1, '{}'::jsonb, 1)"],
    )
    .await;
    // A cancelled run whose coordinator never got a host and whose one attempt was terminated
    // while RUNNABLE (no host, never started).
    sqlx::query(
        "INSERT INTO run_projection (run_id, state, coordinator_job_id, completed_at) \
         VALUES ('gone', 'cancelled', 'coord-gone', $1)",
    )
    .bind(now)
    .execute(&pool)
    .await
    .unwrap();
    exec_all(
        &pool,
        &["INSERT INTO attempt_projection (run_id, batch_job_id, task_key, attempt, state) \
           VALUES ('gone', 'j-runnable', 't', 1, 'failed')"],
    )
    .await;
    // A live run: its coordinator (no host yet) and live attempt may still be priced, and so may
    // one that finished just now on a known host, and one that finished just inside the window;
    // one on an unknown host, and one that finished just before the window, may not.
    exec_all(
        &pool,
        &["INSERT INTO run_projection (run_id, state, coordinator_job_id) \
           VALUES ('live', 'running', 'coord-live')"],
    )
    .await;
    for (job, state, host, finished) in [
        ("j-live", "running", None, None),
        ("j-done", "succeeded", Some("ci-1"), Some(now)),
        ("j-unknown-host", "failed", Some("ci-never"), Some(now)),
        ("j-inside", "succeeded", Some("ci-1"), Some(inside)),
        ("j-old", "succeeded", Some("ci-1"), Some(old)),
    ] {
        sqlx::query(
            "INSERT INTO attempt_projection (run_id, batch_job_id, task_key, attempt, state, \
             instance_id, started_at, finished_at) VALUES ('live', $1, 't', 1, $2, $3, $4, $5)",
        )
        .bind(job)
        .bind(state)
        .bind(host)
        .bind(finished.map(|f: i64| f - 1_000_000).or(Some(now)))
        .bind(finished)
        .execute(&pool)
        .await
        .unwrap();
    }
    let store = PgStore::new(pool.clone());
    let gone = store.get_run(&RunId::new("gone")).await.unwrap();
    assert_eq!(gone.cost, None, "nothing of it can ever be priced");
    let live = store.get_run(&RunId::new("live")).await.unwrap();
    assert_eq!(
        live.cost.map(|c| c.pending_jobs),
        Some(4),
        "the coordinator, j-live, j-done and j-inside"
    );
    // The light load agrees.
    let light = store.get_run_light(&RunId::new("live")).await.unwrap();
    assert_eq!(light.cost.map(|c| c.pending_jobs), Some(4));
    // A job the pricer recorded as unpriceable is never pending.
    exec_all(
        &pool,
        &["INSERT INTO unpriceable_job (run_id, batch_job_id, reason) \
           VALUES ('live', 'j-done', 'unknown_instance_type')"],
    )
    .await;
    let live = store.get_run(&RunId::new("live")).await.unwrap();
    assert_eq!(live.cost.map(|c| c.pending_jobs), Some(3), "j-done is unpriceable");

    // An ended run's coordinator is pending while it can still be priced: it ran on a known host,
    // and its Batch facts give the pricer its run time. Without a Batch terminal (missed, and given
    // up on) the pricer can never price it, so it is not pending.
    sqlx::query(
        "INSERT INTO run_projection (run_id, state, coordinator_job_id, coordinator_host_id, \
         completed_at) VALUES ('ended', 'succeeded', 'coord-ended', 'ci-1', $1)",
    )
    .bind(now)
    .execute(&pool)
    .await
    .unwrap();
    let ended = store.get_run(&RunId::new("ended")).await.unwrap();
    assert_eq!(ended.cost.map(|c| c.pending_jobs), None, "no Batch terminal");
    // Its Batch facts, task-shaped (a coordinator queue the deploy does not list): still its own.
    for (kind, ts) in [("task_started", now - hour), ("task_succeeded", now)] {
        sqlx::query(
            "INSERT INTO run_events (dedup_key, ts_micros, source, kind, batch_job_id, \
             schema_version, payload) VALUES ($1, $2, 'aws-batch', $3, 'coord-ended', 1, '{}')",
        )
        .bind(format!("aws:coord-ended:{kind}"))
        .bind(ts)
        .bind(kind)
        .execute(&pool)
        .await
        .unwrap();
    }
    let ended = store.get_run(&RunId::new("ended")).await.unwrap();
    assert_eq!(ended.cost.map(|c| c.pending_jobs), Some(1));

    // The run list's batched read says "pending" exactly when the detail counts a pending job:
    // nothing priced yet, so no amount, for each.
    let ids = ["gone", "live", "ended"].map(RunId::new);
    let costs = store.run_costs(&ids).await.unwrap();
    for id in &ids {
        let light = store.get_run_light(id).await.unwrap();
        let page = ListCost::from_run_cost(light.cost.as_ref());
        assert_eq!(costs.get(id.as_str()).copied().unwrap_or_default(), page, "{id}");
    }
    assert!(!costs.contains_key("gone"));
    let pending_only = ListCost { usd: None, pending: true };
    assert_eq!((costs["live"], costs["ended"]), (pending_only, pending_only));
    // Once its coordinator is priced, the ended run's cost is final.
    exec_all(
        &pool,
        &[
            "INSERT INTO attempt_cost (run_id, batch_job_id, role, instance_type, lifecycle, \
               hours, price_per_hour, price_source, compute_usd, ebs_usd) VALUES \
               ('ended','coord-ended','coordinator','m6a.large','on_demand',1,0.1,'on_demand',0.1,0.0)",
        ],
    )
    .await;
    let costs = store.run_costs(&ids).await.unwrap();
    assert_eq!(costs["ended"], ListCost { usd: Some(0.1), pending: false });
    // An attempt of the ended run that never got its terminal (lost, and given up on) does not
    // keep it pending for good: an open attempt counts only while its run is going.
    exec_all(
        &pool,
        &["INSERT INTO attempt_projection (run_id, batch_job_id, task_key, attempt, state) \
           VALUES ('ended', 'j-lost', 't', 1, 'submitted')"],
    )
    .await;
    let ended = store.get_run(&RunId::new("ended")).await.unwrap();
    assert_eq!(ended.cost.map(|c| c.pending_jobs), Some(0));
    let costs = store.run_costs(&ids).await.unwrap();
    assert!(!costs["ended"].pending);
}

/// A job priced per execution is pending while one of its ended, started executions on a known
/// host is not priced, or, while its run goes on, one has not ended; it is counted once however
/// many reasons it has, and its legacy job-level line is no longer owed. A job whose only
/// execution ended with no start, or ran on a host the registry does not know, is still priced the
/// legacy way, so it stays pending until that line is written.
#[tokio::test]
#[ignore = "requires --features pg + the Postgres harness"]
async fn executions_still_to_price_are_pending_once_per_job() {
    let pool = migrated_pool("pending_executions").await;
    let now = biffix_core::clock::epoch_micros();
    let hour = 3_600_000_000;
    exec_all(
        &pool,
        &[
            "INSERT INTO host_registry (container_instance_id, ec2_instance_id, cluster_arn, \
               instance_type, detail_version, raw_detail, first_seen_at) VALUES \
               ('ci-1', 'i-1', 'c', 'c6a.4xlarge', 1, '{}'::jsonb, 1)",
            "INSERT INTO run_projection (run_id, state) VALUES ('live', 'running')",
        ],
    )
    .await;
    // (job, state): all but j-open finished on ci-1 (j-retry by the engine's word), j-open is
    // still running.
    for (job, state) in [
        ("j-exec", "succeeded"),
        ("j-open", "running"),
        ("j-thin", "succeeded"),
        ("j-legacy", "succeeded"),
        ("j-retry", "succeeded"),
        ("j-nohost", "succeeded"),
    ] {
        let finished = (state == "succeeded").then_some(now);
        sqlx::query(
            "INSERT INTO attempt_projection (run_id, batch_job_id, task_key, attempt, state, \
             instance_id, started_at, finished_at) VALUES ('live', $1, $1, 1, $2, 'ci-1', $3, $4)",
        )
        .bind(job)
        .bind(state)
        .bind(now - hour)
        .bind(finished)
        .execute(&pool)
        .await
        .unwrap();
    }
    // (run, execution, job, role, host, start, ended). j-exec ran twice; j-open has an execution
    // that ended (Batch is running it again); j-thin's only one ended with no start; j-retry's
    // second is still running though the engine reported the job done; j-nohost's ran on a host
    // the registry never saw; the ended run's coordinator ran once.
    for (run, execution, job, role, host, started, ended) in [
        ("live", "e1", "j-exec", "task", "ci-1", Some(now - hour), true),
        ("live", "e2", "j-exec", "task", "ci-1", Some(now - hour / 2), true),
        ("live", "e3", "j-open", "task", "ci-1", Some(now - hour), true),
        ("live", "e4", "j-thin", "task", "ci-1", None, true),
        ("live", "e6", "j-retry", "task", "ci-1", Some(now - hour), true),
        ("live", "e7", "j-retry", "task", "ci-1", Some(now - hour / 2), false),
        ("live", "e8", "j-nohost", "task", "ci-gone", Some(now - hour), true),
        ("ended", "e5", "coord-x", "coordinator", "ci-1", Some(now - hour), true),
    ] {
        sqlx::query(
            "INSERT INTO execution_projection (run_id, execution_id, batch_job_id, role, ordinal, \
             is_final, ended, instance_id, started_at, stopped_at) \
             VALUES ($1, $2, $3, $4, 1, false, $5, $6, $7, $8)",
        )
        .bind(run)
        .bind(execution)
        .bind(job)
        .bind(role)
        .bind(ended)
        .bind(host)
        .bind(started)
        .bind(ended.then_some(now - hour / 4))
        .execute(&pool)
        .await
        .unwrap();
    }
    // An ended run whose coordinator the pricer could price the legacy way (a known host, its Batch
    // facts), but whose execution is priced: the legacy line is not owed, so nothing is pending.
    sqlx::query(
        "INSERT INTO run_projection (run_id, state, coordinator_job_id, coordinator_host_id, \
         completed_at) VALUES ('ended', 'succeeded', 'coord-x', 'ci-1', $1)",
    )
    .bind(now)
    .execute(&pool)
    .await
    .unwrap();
    for (kind, ts) in [("task_started", now - hour), ("task_succeeded", now)] {
        sqlx::query(
            "INSERT INTO run_events (dedup_key, ts_micros, source, kind, batch_job_id, \
             schema_version, payload) VALUES ($1, $2, 'aws-batch', $3, 'coord-x', 1, '{}')",
        )
        .bind(format!("aws:coord-x:{kind}"))
        .bind(ts)
        .bind(kind)
        .execute(&pool)
        .await
        .unwrap();
    }
    let price =
        |run: &'static str, execution: &'static str, job: &'static str, role: &'static str| {
            let pool = pool.clone();
            async move {
                // Priced from the execution's current inputs.
                sqlx::query(
                    "INSERT INTO execution_cost (run_id, execution_id, batch_job_id, role, \
                   instance_type, hours, price_per_hour, price_source, compute_usd, ebs_usd, \
                   instance_id, queue, started_at, stopped_at) \
                 SELECT $1, $2, $3, $4, 'c6a.4xlarge', 1, 1, 'on_demand', 1, 0, x.instance_id, \
                   x.queue, x.started_at, x.stopped_at \
                   FROM execution_projection x WHERE x.run_id = $1 AND x.execution_id = $2",
                )
                .bind(run)
                .bind(execution)
                .bind(job)
                .bind(role)
                .execute(&pool)
                .await
                .unwrap();
            }
        };
    price("live", "e1", "j-exec", "task").await;
    price("live", "e6", "j-retry", "task").await;

    let store = PgStore::new(pool.clone());
    let pending = |run: &'static str| {
        let store = &store;
        async move {
            let id = RunId::new(run);
            let detail = store.get_run(&id).await.unwrap().cost.map(|c| c.pending_jobs);
            let listed = store.run_costs(std::slice::from_ref(&id)).await.unwrap()[run].pending;
            assert_eq!(listed, detail.unwrap_or(0) > 0, "{run}: the list agrees");
            detail
        }
    };
    // j-exec (e2), j-open (open, and e3: once), j-thin, j-legacy and j-nohost (legacy), and j-retry
    // (e7 still running).
    assert_eq!(pending("live").await, Some(6));
    // The ended run owes only its coordinator's execution.
    assert_eq!(pending("ended").await, Some(1));
    price("ended", "e5", "coord-x", "coordinator").await;
    assert_eq!(pending("ended").await, Some(0));
    // A replay moves e5's stop: its line was priced from the old one, so it is owed again.
    let move_stop = |by: i64| {
        let pool = pool.clone();
        async move {
            sqlx::query(
                "UPDATE execution_projection SET stopped_at = stopped_at + $1 \
                 WHERE execution_id = 'e5'",
            )
            .bind(by)
            .execute(&pool)
            .await
            .unwrap();
        }
    };
    move_stop(1).await;
    assert_eq!(pending("ended").await, Some(1));
    move_stop(-1).await;
    assert_eq!(pending("ended").await, Some(0));
    // Once e2 is priced j-exec owes nothing, though it has no legacy line.
    price("live", "e2", "j-exec", "task").await;
    assert_eq!(pending("live").await, Some(5));
    // j-open is still running, so priced e3 does not settle it.
    price("live", "e3", "j-open", "task").await;
    exec_all(
        &pool,
        &["INSERT INTO attempt_cost (run_id, batch_job_id, role, instance_type, hours, \
             price_per_hour, price_source, compute_usd, ebs_usd) VALUES \
             ('live', 'j-thin', 'task', 'c6a.4xlarge', 1, 1, 'on_demand', 1, 0), \
             ('live', 'j-legacy', 'task', 'c6a.4xlarge', 1, 1, 'on_demand', 1, 0), \
             ('live', 'j-nohost', 'task', 'c6a.4xlarge', 1, 1, 'on_demand', 1, 0)"],
    )
    .await;
    // j-open and j-retry are still running.
    assert_eq!(pending("live").await, Some(2));
}

/// The list's cost is the run page's to the bit: both add a run's cost lines as Σcompute + Σebs in
/// job id then execution id order, which these values tell apart from Σ(compute + ebs) and from
/// the other order of j3's two execution lines.
#[tokio::test]
#[ignore = "requires --features pg + the Postgres harness"]
async fn the_list_s_run_cost_is_the_detail_s_to_the_bit() {
    let pool = migrated_pool("run_cost_bit_identical").await;
    // (job, execution, compute, ebs): j3 is priced per execution (Batch ran it twice), the rest by
    // their legacy job-level line.
    let lines = [
        ("j1", None, 2.059452, 0.48452),
        ("j2", None, 2.177558, 0.263815),
        ("j3", Some("j3-e1"), 0.965989, 0.117177),
        ("j3", Some("j3-e2"), 0.983835, 0.120438),
        ("j4", None, 1.658579, 0.17285),
        ("j5", None, 2.030546, 0.380474),
    ];
    let split = |lines: &[(&str, Option<&str>, f64, f64)]| {
        lines.iter().map(|l| l.2).sum::<f64>() + lines.iter().map(|l| l.3).sum::<f64>()
    };
    // Preconditions: the ways of adding differ for these values.
    let per_line: f64 = lines.iter().map(|l| l.2 + l.3).sum();
    let mut swapped = lines;
    swapped.swap(2, 3);
    let split_of_lines = split(&lines);
    assert_ne!(per_line, split_of_lines, "the values must tell the two sums apart");
    assert_ne!(split(&swapped), split_of_lines, "the values must tell the two orders apart");
    let split = split_of_lines;
    exec_all(
        &pool,
        &["INSERT INTO run_projection (run_id, state, submitted_at, dag) \
           VALUES ('r1', 'succeeded', 1000000, '{}')"],
    )
    .await;
    // Inserted newest line first, so a scan-order sum is not the id-order one by accident.
    for (job, execution, compute, ebs) in lines.iter().rev() {
        let sql = match execution {
            None => {
                "INSERT INTO attempt_cost (run_id, batch_job_id, role, instance_type, lifecycle, \
                   hours, price_per_hour, price_source, compute_usd, ebs_usd) \
                 VALUES ('r1', $1, 'task', 'c6a.4xlarge', 'spot', 1, 1, 'spot_history', $3, $4)"
            }
            Some(_) => {
                "INSERT INTO execution_cost (run_id, batch_job_id, execution_id, role, \
                   instance_type, lifecycle, hours, price_per_hour, price_source, compute_usd, \
                   ebs_usd, instance_id, started_at, stopped_at) \
                 VALUES ('r1', $1, $2, 'task', 'c6a.4xlarge', 'spot', 1, 1, 'spot_history', $3, \
                   $4, 'ci-1', 0, 3600000000)"
            }
        };
        sqlx::query(sql)
            .bind(job)
            .bind(execution)
            .bind(compute)
            .bind(ebs)
            .execute(&pool)
            .await
            .unwrap();
    }
    let store = PgStore::new(pool);
    let run = RunId::new("r1");
    let detail = store.get_run(&run).await.unwrap().cost.unwrap();
    let light = store.get_run_light(&run).await.unwrap().cost.unwrap();
    let listed = store.run_costs(std::slice::from_ref(&run)).await.unwrap()["r1"];
    assert_eq!(detail.estimated_usd, split);
    assert_eq!(light.estimated_usd, split);
    assert_eq!(listed.usd, Some(split));
}

/// A job priced per execution counts its execution lines, never its legacy job-level line too
/// (written before its executions were priced, or by an older pricer mid-deploy): in the run's
/// cost, the list's, and its attempt's, which adds up the lines.
#[tokio::test]
#[ignore = "requires --features pg + the Postgres harness"]
async fn execution_lines_replace_their_job_s_legacy_line() {
    use biffix_backend::store::tasks::TaskReads;
    let pool = migrated_pool("execution_lines_replace_legacy").await;
    exec_all(
        &pool,
        &[
            "INSERT INTO run_projection (run_id, state, submitted_at, dag) \
             VALUES ('rx', 'succeeded', 1000000, '{}')",
            "INSERT INTO attempt_projection (run_id, batch_job_id, task_key, attempt, state) \
             VALUES ('rx', 'j1', 't1', 1, 'succeeded'), ('rx', 'j2', 't2', 1, 'succeeded')",
            "INSERT INTO attempt_cost (run_id, batch_job_id, role, instance_type, lifecycle, \
               hours, price_per_hour, price_source, compute_usd, ebs_usd) VALUES \
             ('rx', 'j1', 'task', 'c6a.4xlarge', 'spot', 3, 2.5, 'spot_history', 7.5, 2.5), \
             ('rx', 'j2', 'task', 'c6a.4xlarge', 'spot', 1, 3, 'spot_history', 3, 1)",
            // j1 ran twice: reclaimed on a spot c6a, then finished on an on-demand c7i.
            "INSERT INTO execution_cost (run_id, batch_job_id, execution_id, role, instance_type, \
               lifecycle, hours, price_per_hour, price_source, compute_usd, ebs_usd, instance_id, \
               started_at, stopped_at) VALUES \
             ('rx', 'j1', 'j1-e1', 'task', 'c6a.4xlarge', 'spot', 1, 0.25, 'spot_history', \
              0.25, 0.25, 'ci-a', 0, 3600000000), \
             ('rx', 'j1', 'j1-e2', 'task', 'c7i.4xlarge', 'on_demand', 3, 1, 'on_demand', \
              3, 0.5, 'ci-b', 3600000000, 14400000000)",
        ],
    )
    .await;
    let store = PgStore::new(pool);
    let run = RunId::new("rx");
    let cost = store.get_run(&run).await.unwrap().cost.unwrap();
    assert_eq!((cost.estimated_usd, cost.priced_jobs, cost.pending_jobs), (8.0, 2, 0));
    let listed = store.run_costs(std::slice::from_ref(&run)).await.unwrap()["rx"];
    assert_eq!(listed, ListCost { usd: Some(8.0), pending: false });

    // j1's attempt adds up its two lines, its price their average over its run time (neither
    // line's); j2's keeps its one line, its price as it is.
    let attempt_cost = |key: &'static str| {
        let store = &store;
        let run = &run;
        async move {
            let attempts = store.get_attempts(run, &TaskKey::new(key)).await.unwrap();
            assert_eq!(attempts.len(), 1);
            attempts.into_iter().next().unwrap().cost.unwrap()
        }
    };
    let j1 = attempt_cost("t1").await;
    assert_eq!((j1.usd, j1.compute_usd, j1.hours), (4.0, 3.25, 4.0));
    assert_eq!((j1.price_per_hour, j1.price_source.as_str()), (0.8125, "mixed"));
    let j2 = attempt_cost("t2").await;
    assert_eq!((j2.usd, j2.hours, j2.price_per_hour), (4.0, 1.0, 3.0));
    assert_eq!(j2.price_source, "spot_history");
}

/// A task job's cost row whose attempt row is gone (a replay dropped it) counts in the gross, not
/// as the coordinator's cost.
#[tokio::test]
#[ignore = "requires --features pg + the Postgres harness"]
async fn a_task_cost_without_its_attempt_is_not_the_coordinator_s() {
    let pool = migrated_pool("orphan_task_cost").await;
    exec_all(
        &pool,
        &[
            "INSERT INTO run_projection (run_id, state, coordinator_job_id) \
             VALUES ('rc1', 'succeeded', 'coord')",
            "INSERT INTO attempt_cost (run_id, batch_job_id, role, instance_type, lifecycle, hours, \
             price_per_hour, price_source, compute_usd, ebs_usd) VALUES \
             ('rc1','coord','coordinator','m6i.large','on_demand',1,0.1,'on_demand',0.1,0), \
             ('rc1','job-orphan','task','c6a.4xlarge','spot',1,5,'spot_history',5,0)",
        ],
    )
    .await;
    let store = PgStore::new(pool);
    let cost = store.get_run(&RunId::new("rc1")).await.unwrap().cost.unwrap();
    let close = |a: f64, b: f64| (a - b).abs() < 1e-9;
    assert!(close(cost.estimated_usd, 5.1), "{cost:?}");
    assert!(close(cost.coordinator_usd, 0.1), "{cost:?}");
    assert!(close(cost.winning_usd, 0.1), "{cost:?}");
}
