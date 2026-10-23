//! Task pages and the light run and task-group loads, against the full load.

use super::*;
use biffix_backend::store::runs::RunReads;
use biffix_backend::store::tasks::TaskReads;

/// A seeded task: key, state, group, started, finished, cached, deps, batch job.
type SeedTask<'a> =
    (&'a str, &'a str, &'a str, Option<i64>, Option<i64>, bool, &'a [&'a str], Option<&'a str>);

/// A run `rp` with tasks across groups, states, start/finish times, dependencies, costs and a
/// coordinator, for the task-paging parity checks.
async fn seeded_task_pool(label: &str) -> sqlx::PgPool {
    let pool = seeded_pool(label).await;
    exec_all(
        &pool,
        &["INSERT INTO run_projection (run_id, state, coordinator_job_id) \
           VALUES ('rp','running','coord')"],
    )
    .await;
    let tasks: &[SeedTask] = &[
        ("fastqc:s=A", "succeeded", "fastqc", Some(0), Some(10_000_000), false, &[], Some("j1")),
        ("fastqc:s=B", "succeeded", "fastqc", Some(5_000_000), Some(8_000_000), true, &[], None),
        (
            "align:s=A",
            "failed",
            "align",
            Some(20_000_000),
            Some(90_000_000),
            false,
            &["fastqc:s=A"],
            Some("j2"),
        ),
        (
            "align:s=B",
            "running",
            "align",
            Some(30_000_000),
            None,
            false,
            &["fastqc:s=B"],
            Some("j3"),
        ),
        ("Align:s=C", "queued", "align", None, None, false, &[], None),
        ("markdup:s=A", "queued", "markdup", None, None, false, &["align:s=A", "align:s=B"], None),
        (
            "joint",
            "succeeded",
            "joint",
            Some(1_000_000),
            Some(2_000_000),
            false,
            &["markdup:s=A", "joint"],
            Some("j4"),
        ),
        ("orphan", "running", "", Some(40_000_000), None, false, &[], None),
    ];
    for (key, state, group, started, finished, cached, deps, job) in tasks {
        sqlx::query(
            "INSERT INTO task_projection (run_id, task_key, state, attempts, \"group\", started_at, \
             finished_at, cached, deps, batch_job_id) VALUES ('rp',$1,$2,1,NULLIF($3,''),$4,$5,$6,$7,$8)",
        )
        .bind(key)
        .bind(state)
        .bind(group)
        .bind(started)
        .bind(finished)
        .bind(cached)
        .bind(deps.iter().map(std::string::ToString::to_string).collect::<Vec<_>>())
        .bind(job)
        .execute(&pool)
        .await
        .unwrap();
    }
    exec_all(
        &pool,
        &[
            "INSERT INTO attempt_projection (run_id, batch_job_id, task_key, attempt, state) VALUES \
               ('rp','j0','align:s=A',1,'failed'), ('rp','j2','align:s=A',2,'failed'), \
               ('rp','j1','fastqc:s=A',1,'succeeded')",
            "INSERT INTO attempt_cost (run_id, batch_job_id, role, instance_type, lifecycle, hours, \
               price_per_hour, price_source, compute_usd, ebs_usd) VALUES \
               ('rp','j0','task','c6a.4xlarge','spot',1,0.2,'spot_history',0.2,0.01), \
               ('rp','j2','task','c6a.4xlarge','spot',1,0.3,'spot_history',0.3,0.02), \
               ('rp','j1','task','c6a.4xlarge','spot',1,0.1,'spot_history',0.1,0.0), \
               ('rp','coord','coordinator','m6i.large','on_demand',1,0.1,'on_demand',0.1,0.0)",
        ],
    )
    .await;
    pool
}

#[tokio::test]
#[ignore = "requires --features pg + the Postgres harness"]
async fn task_pages_match_the_reference_semantics() {
    use biffix_backend::runs::task_page::{page_tasks, TaskQuery, TaskSort, TaskStateFilter};
    let store = PgStore::new(seeded_task_pool("task_pages_match").await);
    let rp = RunId::new("rp");
    let all = store.get_run(&rp).await.unwrap().tasks;
    let now = 100.0; // epoch seconds: after every start
    let mut checked = 0;
    for state in [
        TaskStateFilter::All,
        TaskStateFilter::Active,
        TaskStateFilter::Succeeded,
        TaskStateFilter::Failed,
    ] {
        for sort in [TaskSort::Key, TaskSort::State, TaskSort::Duration, TaskSort::Started] {
            for reverse in [false, true] {
                for group in [None, Some("align".to_string())] {
                    for q in [None, Some("S=a".to_string())] {
                        for (task_key, (offset, limit)) in [
                            (None, (0, 100)),
                            (None, (1, 2)),
                            (Some("align:s=A".to_string()), (0, 100)),
                        ] {
                            let query = TaskQuery {
                                state,
                                group: group.clone(),
                                q: q.clone(),
                                task_key,
                                sort,
                                reverse,
                                offset,
                                limit,
                            };
                            let expected = page_tasks(&all, &query, now);
                            let got = store.list_tasks(&rp, &query, now).await.unwrap();
                            assert_eq!(got, expected, "{query:?}");
                            checked += 1;
                        }
                    }
                }
            }
        }
    }
    assert_eq!(checked, 4 * 4 * 2 * 2 * 2 * 3);
}

#[tokio::test]
#[ignore = "requires --features pg + the Postgres harness"]
async fn light_run_and_single_task_agree_with_the_full_load() {
    let store = PgStore::new(seeded_task_pool("light_run_agrees").await);
    let rp = RunId::new("rp");
    let full = store.get_run(&rp).await.unwrap();
    let light = store.get_run_light(&rp).await.unwrap();
    assert!(light.tasks.is_empty(), "the light load leaves the tasks out");
    assert_eq!(light.cost, full.cost);
    assert_eq!(light.run, full.run);
    // Its task groups: `light_task_groups_equal_the_full_load_s_in_order`.

    for t in &full.tasks {
        let one = store.get_task(&rp, t.task_key.as_str()).await.unwrap();
        assert_eq!(one.as_ref(), Some(t), "{}", t.task_key.as_str());
    }
    assert_eq!(store.get_task(&rp, "nope").await.unwrap(), None);
    assert!(matches!(
        store.get_run_light(&RunId::new("nope")).await,
        Err(biffix_backend::store::StoreError::NotFound)
    ));
}

/// The light load's per-step counts are the full load's, in the same order (not just the same
/// set): byte order on group and state, then cached.
#[tokio::test]
#[ignore = "requires --features pg + the Postgres harness"]
async fn light_task_groups_equal_the_full_load_s_in_order() {
    let store = PgStore::new(seeded_task_pool("light_groups_in_order").await);
    let rp = RunId::new("rp");
    let full = store.get_run(&rp).await.unwrap();
    let light = store.get_run_light(&rp).await.unwrap();
    assert!(full.task_groups.len() > 3, "{:?}", full.task_groups);
    assert_eq!(light.task_groups, full.task_groups);
}
