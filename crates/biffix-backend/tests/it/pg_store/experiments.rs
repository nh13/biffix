//! Experiments, releases and run renames.

use super::*;
use biffix_backend::store::cost::CostReads;
use biffix_backend::store::experiments::ExperimentReads;
use biffix_backend::store::notifications::NotificationStore;
use biffix_backend::store::runs::RunReads;

#[tokio::test]
#[ignore = "requires --features pg + the Postgres harness"]
async fn experiments_are_created_renamed_archived_and_restored() {
    use biffix_backend::pg::experiments::PgExperimentStore;
    use biffix_backend::store::experiments::{ExperimentChange, ExperimentRecord, ExperimentStore};
    use biffix_backend::store::StoreError;

    let store = PgExperimentStore::new(migrated_pool("experiments_crud").await);
    let record = |id: &str, project: &str, name: &str| ExperimentRecord {
        id: id.into(),
        project: project.into(),
        name: name.into(),
        description: Some("why".into()),
        created_by: Some("alice".into()),
        created_by_label: Some("alice@lab.example.org".into()),
        created_at_micros: 10,
        archived_at_micros: None,
    };
    store.create(&record("e1", "p", "Batch")).await.unwrap();
    assert_eq!(store.get("e1").await.unwrap(), record("e1", "p", "Batch"));
    // A live name is unique per project, ignoring case; the conflict names it.
    match store.create(&record("e2", "p", "BATCH")).await {
        Err(StoreError::Conflict(m)) => {
            assert_eq!(m, "project p already has an experiment named `BATCH`");
        }
        other => panic!("{other:?}"),
    }
    assert!(matches!(
        store.create(&record("e1", "q", "other")).await,
        Err(StoreError::Conflict(_))
    ));
    store.create(&record("e3", "q", "batch")).await.unwrap();

    // A partial update keeps what it does not name; `Some(None)` clears the description.
    let renamed = store
        .update(
            "e1",
            &ExperimentChange {
                name: Some("Batch A".into()),
                description: Some(None),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(
        (renamed.name.as_str(), renamed.description, renamed.created_by.as_deref()),
        ("Batch A", None, Some("alice"))
    );
    // Archived (twice: the first time stays), it leaves the live list and frees its name.
    let archive =
        |at| ExperimentChange { archived: Some(true), now_micros: at, ..Default::default() };
    assert_eq!(store.update("e1", &archive(50)).await.unwrap().archived_at_micros, Some(50));
    assert_eq!(store.update("e1", &archive(60)).await.unwrap().archived_at_micros, Some(50));
    let ids = |v: Vec<ExperimentRecord>| {
        let mut ids: Vec<String> = v.into_iter().map(|e| e.id).collect();
        ids.sort();
        ids
    };
    assert_eq!(ids(store.list(Some("p"), false).await.unwrap()), Vec::<String>::new());
    assert_eq!(ids(store.list(Some("p"), true).await.unwrap()), ["e1"]);
    assert_eq!(ids(store.list(None, false).await.unwrap()), ["e3"]);
    store.create(&record("e4", "p", "batch a")).await.unwrap();
    // Restoring onto a taken name clashes, naming it; renamed as it is restored, it comes back.
    let restore = ExperimentChange { archived: Some(false), ..Default::default() };
    match store.update("e1", &restore).await {
        Err(StoreError::Conflict(m)) => {
            assert_eq!(m, "project p already has an experiment named `Batch A`");
        }
        other => panic!("{other:?}"),
    }
    let back = store
        .update("e1", &ExperimentChange { name: Some("Batch A (1)".into()), ..restore.clone() })
        .await
        .unwrap();
    assert_eq!(back.archived_at_micros, None);
    assert!(matches!(store.update("nope", &restore).await, Err(StoreError::NotFound)));
    assert!(matches!(store.get("nope").await, Err(StoreError::NotFound)));
}

#[tokio::test]
#[ignore = "requires --features pg + the Postgres harness"]
async fn experiment_reads_filter_count_and_roll_up_runs() {
    let pool = migrated_pool("experiment_reads").await;
    exec_all(
        &pool,
        &[
            "INSERT INTO run_projection (run_id, state, submitted_at, experiment_id, samples, dag) \
             VALUES \
               ('r1','succeeded', 1000000, 'e1', '{\"ids\":[\"S1\",\"S2\"]}', \
                '{\"nodes\":[\"align\"],\"edges\":[]}'), \
               ('r2','failed', 2000000, 'e1', NULL, '{\"nodes\":[\"trim\",\"align\"],\"edges\":[[\"trim\",\"align\"]]}'), \
               ('r3','running', 3000000, 'e1', NULL, '{}'), \
               ('r4','running', 4000000, 'e2', NULL, '{}'), \
               ('r5','running', 5000000, NULL, NULL, '{}')",
            "INSERT INTO task_projection (run_id, task_key, state, attempts, sample, \"group\", cached) \
             VALUES ('r1','align:sample=S1','succeeded',1,'S1','align',false), \
                    ('r1','align:sample=S2','succeeded',2,'S2','align',true), \
                    ('r2','align:sample=S2','failed',1,'S2','align',false)",
            "INSERT INTO run_output (run_id, id, s3_uri, task_key, role) \
             VALUES ('r1','o1','s3://b/S1.bam','align:sample=S1','output')",
            "INSERT INTO attempt_cost (run_id, batch_job_id, role, instance_type, lifecycle, hours, \
               price_per_hour, price_source, compute_usd, ebs_usd) VALUES \
               ('r1','a1','task','c6a.4xlarge','spot',1,0.2,'spot_history',0.25,0.25), \
               ('r1','c1','coordinator','m6a.large','on_demand',1,0.1,'on_demand',0.1,0.0), \
               ('r3','a3','task','c6a.4xlarge','spot',1,0.2,'spot_history',1.0,0.5), \
               ('r4','a4','task','c6a.4xlarge','spot',1,0.2,'spot_history',9.0,0.0)",
        ],
    )
    .await;
    let store = PgStore::new(pool);
    let ids = |page: &biffix_backend::store::runs::RunPage| -> Vec<String> {
        page.runs.iter().map(|r| r.run_id.to_string()).collect()
    };

    // The list filter, with the true total, paging that keeps it, and the summary's experiment.
    let in_e1 = RunQuery { experiment: Some("e1".into()), limit: 2, ..Default::default() };
    let page = store.list_runs(&in_e1).await.unwrap();
    assert_eq!((ids(&page), page.total), (vec!["r3".into(), "r2".into()], 3));
    assert_eq!(page.runs[0].experiment_id.as_deref(), Some("e1"));
    let next = store
        .list_runs(&RunQuery { after: page.last_cursor.clone(), ..in_e1.clone() })
        .await
        .unwrap();
    assert_eq!(ids(&next), ["r1"]);
    let all = store.list_runs(&RunQuery::default()).await.unwrap();
    let r5 = all.runs.iter().find(|r| r.run_id.as_str() == "r5").unwrap();
    assert_eq!(r5.experiment_id, None);
    assert_eq!(
        store.get_run_light(&RunId::new("r4")).await.unwrap().run.experiment_id.as_deref(),
        Some("e2")
    );

    // Counts and newest submit per experiment, in one read; an experiment with no runs is absent.
    let stats = store.experiment_run_stats(&["e1".into(), "e2".into(), "e9".into()]).await.unwrap();
    assert_eq!(stats.len(), 2);
    assert_eq!((stats["e1"].count, stats["e1"].last_submitted_micros), (3, Some(3_000_000)));
    assert_eq!(stats["e2"].count, 1);
    assert!(store.experiment_run_stats(&[]).await.unwrap().is_empty());

    // The batched sample facts are the per-run reads', run by run.
    let runs: Vec<RunId> = ["r1", "r2", "r3", "nope"].map(RunId::new).to_vec();
    let batched = store.runs_sample_facts(&runs).await.unwrap();
    assert_eq!(batched.len(), 3);
    for run in &runs[..3] {
        let (spec, tasks) = store.sample_facts(run).await.unwrap();
        let light = store.get_run_light(run).await.unwrap();
        let got = &batched[run.as_str()];
        let sorted = |t: &[biffix_core::sample_progress::TaskFact]| {
            let mut v: Vec<String> = t.iter().map(|t| format!("{t:?}")).collect();
            v.sort();
            v
        };
        assert_eq!(got.spec, spec, "{run}");
        assert_eq!(sorted(&got.tasks), sorted(&tasks), "{run}");
        assert_eq!((&got.dag_nodes, &got.dag_edges), (&light.dag_nodes, &light.dag_edges));
    }
    assert!(batched["r1"].tasks.iter().any(|t| t.outputs));

    // The summed estimated cost of every priced job of every run in it; none when no run has one.
    let usd = store.experiment_priced_usd("e1").await.unwrap().unwrap();
    assert!((usd - 2.1).abs() < 1e-9, "{usd}");
    let usd = store.experiment_priced_usd("e2").await.unwrap().unwrap();
    assert!((usd - 9.0).abs() < 1e-9, "{usd}");
    assert_eq!(store.experiment_priced_usd("e9").await.unwrap(), None);

    // The run list's per-run costs, in one read: each exactly the run's detail cost as the list
    // shows it (every priced job, its coordinator's too); a run with no cost (r2, r5) is absent.
    let runs: Vec<RunId> = ["r1", "r2", "r3", "r4", "r5", "nope"].map(RunId::new).to_vec();
    let costs = store.run_costs(&runs).await.unwrap();
    let mut priced: Vec<&str> = costs.keys().map(String::as_str).collect();
    priced.sort();
    assert_eq!(priced, ["r1", "r3", "r4"]);
    for run in &runs[..5] {
        let light = store.get_run_light(run).await.unwrap();
        let page = ListCost::from_run_cost(light.cost.as_ref());
        assert_eq!(costs.get(run.as_str()).copied().unwrap_or_default(), page, "{run}");
    }
    // Σcompute + Σebs, as the detail adds them.
    assert_eq!(costs["r1"].usd, Some((0.25 + 0.1) + (0.25 + 0.0)));
    assert_eq!(costs["r3"].usd, Some(1.5));
    assert!(store.run_costs(&[]).await.unwrap().is_empty());

    // The detail's runs: the list's first page for the experiment, as the list builds it.
    let detail_runs = store.experiment_runs("e1", 2).await.unwrap();
    let full_page = store
        .list_runs(&RunQuery { experiment: Some("e1".into()), limit: 2, ..Default::default() })
        .await
        .unwrap();
    assert_eq!(detail_runs, full_page.runs);

    // Runs named by id: only those (an unknown id matches nothing), with the other filters and
    // paging as ever.
    let named =
        RunQuery { ids: Some(vec!["r4".into(), "r1".into(), "nope".into()]), ..Default::default() };
    let page = store.list_runs(&named).await.unwrap();
    assert_eq!((ids(&page), page.total), (vec!["r4".into(), "r1".into()], 2));
    let page = store
        .list_runs(&RunQuery { experiment: Some("e1".into()), ..named.clone() })
        .await
        .unwrap();
    assert_eq!((ids(&page), page.total), (vec!["r1".into()], 1));
    let first = store.list_runs(&RunQuery { limit: 1, ..named.clone() }).await.unwrap();
    assert_eq!(ids(&first), ["r4"]);
    let next = store
        .list_runs(&RunQuery { limit: 1, after: first.last_cursor.clone(), ..named.clone() })
        .await
        .unwrap();
    assert_eq!(ids(&next), ["r1"]);
}

#[tokio::test]
#[ignore = "requires --features pg + the Postgres harness"]
async fn an_experiment_s_detail_runs_are_its_newest_page_and_its_cost_is_over_all() {
    let pool = migrated_pool("experiment_detail_bound").await;
    // 201 runs in `big`, submitted 1..=201 s; each run's one priced job costs $1.
    exec_all(
        &pool,
        &[
            "INSERT INTO run_projection (run_id, state, submitted_at, experiment_id, dag) \
             SELECT 'r' || lpad(i::text, 3, '0'), 'succeeded', i * 1000000, 'big', '{}' \
               FROM generate_series(1, 201) AS i",
            "INSERT INTO attempt_cost (run_id, batch_job_id, role, instance_type, lifecycle, hours, \
               price_per_hour, price_source, compute_usd, ebs_usd) \
             SELECT 'r' || lpad(i::text, 3, '0'), 'a' || i, 'task', 'c6a.4xlarge', 'spot', 1, 1, \
                    'spot_history', 1.0, 0.0 \
               FROM generate_series(1, 201) AS i",
        ],
    )
    .await;
    let store = PgStore::new(pool);
    let max = biffix_backend::store::runs::MAX_RUN_PAGE;
    let runs = store.experiment_runs("big", max).await.unwrap();
    assert_eq!(runs.len() as i64, max);
    assert_eq!(runs[0].run_id.as_str(), "r201", "newest first");
    assert_eq!(runs.last().unwrap().run_id.as_str(), "r002");
    let stats = store.experiment_run_stats(&["big".into()]).await.unwrap();
    assert_eq!(stats["big"].count, 201);
    let usd = store.experiment_priced_usd("big").await.unwrap().unwrap();
    assert!((usd - 201.0).abs() < 1e-9, "{usd}");

    // Paging through them all (the samples CSV): pages without a count, every run once.
    let first = store.experiment_run_page("big", None).await.unwrap();
    assert_eq!((first.runs.len() as i64, first.total), (max, 0));
    let second = store.experiment_run_page("big", first.last_cursor.clone()).await.unwrap();
    assert_eq!(second.runs.len(), 1);
    assert_eq!(second.runs[0].run_id.as_str(), "r001");
    let all = store.all_experiment_runs("big").await.unwrap();
    assert_eq!(all.len(), 201);
    assert_eq!(all[0].run_id.as_str(), "r201");
    assert_eq!(all[200].run_id.as_str(), "r001");
}

#[tokio::test]
#[ignore = "requires --features pg + the Postgres harness"]
async fn a_version_is_released_and_unreleased_with_its_history_kept() {
    use biffix_backend::pg::registry::PgRegistryStore;
    use biffix_backend::store::registry::RegistryStore;
    use biffix_backend::store::StoreError;

    let registry = PgRegistryStore::new(migrated_pool("workflow_releases").await);
    let both = ["abc:h1".to_string(), "abc:h2".to_string()];
    assert!(registry.released_versions(&both).await.unwrap().is_empty());
    // No such version: nothing to release, and nothing recorded.
    assert!(matches!(
        registry.set_version_released("abc:h1", true, Some("u1"), None).await,
        Err(StoreError::NotFound)
    ));
    assert!(registry.release_history("abc:h1").await.unwrap().is_empty());

    registry.put_version(&registered_version("abc:h1")).await.unwrap();
    registry.put_version(&registered_version("abc:h2")).await.unwrap();
    // Un-releasing a version never released records nothing.
    assert!(registry
        .set_version_released("abc:h1", false, Some("u1"), None)
        .await
        .unwrap()
        .is_empty());

    let released = registry
        .set_version_released("abc:h1", true, Some("u1"), Some("u1@lab.example.org"))
        .await
        .unwrap();
    assert_eq!(released.len(), 1);
    assert!(released[0].released);
    assert_eq!(released[0].by.as_deref(), Some("u1"));
    assert_eq!(released[0].by_label.as_deref(), Some("u1@lab.example.org"));
    assert!(released[0].at_micros > 0);
    // Releasing it again records nothing.
    assert_eq!(
        registry.set_version_released("abc:h1", true, Some("u2"), None).await.unwrap(),
        released
    );
    let current = registry.released_versions(&both).await.unwrap();
    assert_eq!(current.len(), 1);
    assert_eq!(current["abc:h1"], released[0]);

    // Un-released: a developer version again, both changes kept (newest first).
    let history = registry.set_version_released("abc:h1", false, Some("u2"), None).await.unwrap();
    assert_eq!(history.len(), 2);
    assert!(!history[0].released);
    assert_eq!(history[0].by.as_deref(), Some("u2"));
    assert!(history[0].at_micros >= released[0].at_micros);
    assert_eq!(history[1], released[0]);
    assert!(registry.released_versions(&both).await.unwrap().is_empty());
    assert_eq!(registry.release_history("abc:h1").await.unwrap(), history);

    // Released again, and another version beside it: each read as its newest change.
    registry.set_version_released("abc:h1", true, None, None).await.unwrap();
    registry.set_version_released("abc:h2", true, Some("u3"), None).await.unwrap();
    let current = registry.released_versions(&both).await.unwrap();
    assert_eq!(current.len(), 2);
    assert_eq!(current["abc:h1"].by, None);
    assert_eq!(current["abc:h2"].by.as_deref(), Some("u3"));
    assert_eq!(registry.release_history("abc:h1").await.unwrap().len(), 3);
    // Only the versions asked about are answered.
    let one = registry.released_versions(&both[1..]).await.unwrap();
    assert_eq!(one.keys().collect::<Vec<_>>(), ["abc:h2"]);
    assert!(registry.released_versions(&[]).await.unwrap().is_empty());
}

#[tokio::test]
#[ignore = "requires --features pg + the Postgres harness"]
async fn a_renamed_run_and_its_pipeline_title_read_back_everywhere() {
    use biffix_core::time::Timestamp;
    let pool = migrated_pool("renamed_run_reads").await;
    // `titled` has a friendly title (padded, trimmed on read); `plain` a blank one.
    exec_all(
        &pool,
        &["INSERT INTO workflow_versions (registration_inputs, registration_inputs_hash, \
             runtime_key, runtime_recipe, \
             id, name, project, git_sha, source_content_hash, \
             image_digest, engine, engine_version, entrypoint, source, form_schema, queue_classes, \
             created_at) \
             VALUES ('{}'::jsonb, '', '', '{}'::jsonb, \
             'titled', 'nf-core/rnaseq', 'p', 'abc', 'h', 'sha256:x', 'nextflow', '24', \
             'main.nf', '{}'::jsonb, '{\"title\": \" Bulk RNA-seq \"}'::jsonb, '{}'::text[], 0), \
             ('{}'::jsonb, '', '', '{}'::jsonb, \
             'plain', 'lab-qc', 'p', 'abd', 'h', 'sha256:y', 'snakemake', '9', 'Snakefile', \
             '{}'::jsonb, '{\"title\": \"  \"}'::jsonb, '{}'::text[], 0)"],
    )
    .await;
    let at = Timestamp::from_epoch_seconds;
    let mut events = vec![];
    for (run, version) in [("r1", "titled"), ("r2", "plain")] {
        let rid = RunId::new(run);
        let mut anchor = biffix_core::event::RunSubmitted::new(
            version,
            "sha",
            "img",
            serde_json::json!({}),
            vec![],
        );
        anchor.submitter = Some("user:alice".into());
        anchor.run_name = Some("launch".into());
        events.push(biffix_core::event::run_submitted(&rid, at(10.0), anchor));
        events.push(biffix_core::event::run_failed(
            &rid,
            at(20.0),
            biffix_core::event::FAILURE_STAGE_LAUNCH,
            "no capacity",
        ));
    }
    events.push(biffix_core::event::run_renamed(
        &RunId::new("r1"),
        at(30.0),
        Some("Liver batch 2"),
        "alice",
    ));
    events.push(biffix_core::event::run_renamed(&RunId::new("r2"), at(30.0), None, "alice"));
    biffix_db::log::append_and_advance(&pool, &events).await.unwrap();
    let store = PgStore::new(pool);

    let r1 = store.get_run_light(&RunId::new("r1")).await.unwrap().run;
    assert_eq!(r1.run_name.as_deref(), Some("Liver batch 2"));
    assert_eq!(r1.workflow_title.as_deref(), Some("Bulk RNA-seq"));
    let r2 = store.get_run_light(&RunId::new("r2")).await.unwrap().run;
    assert_eq!((r2.run_name, r2.workflow_title), (None, None));

    let page = store.list_runs(&RunQuery::default()).await.unwrap();
    let listed: Vec<(&str, Option<&str>, Option<&str>)> = page
        .runs
        .iter()
        .map(|r| (r.run_id.as_str(), r.run_name.as_deref(), r.workflow_title.as_deref()))
        .collect();
    assert!(listed.contains(&("r1", Some("Liver batch 2"), Some("Bulk RNA-seq"))));
    assert!(listed.contains(&("r2", None, None)));

    let bell = store.list_notifications("alice", 10).await.unwrap();
    let n1 = bell.items.iter().find(|n| n.run_id.as_str() == "r1").unwrap();
    assert_eq!(
        (n1.run_name.as_deref(), n1.workflow_title.as_deref(), n1.submitted_at),
        (Some("Liver batch 2"), Some("Bulk RNA-seq"), Some(10.0))
    );
}
