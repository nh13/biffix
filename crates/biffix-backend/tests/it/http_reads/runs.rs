//! Run and workflow reads: the run list and its costs, the run detail (prefix, engine session, lifecycle, DAG), paged tasks and the query bodies.

use super::*;

#[tokio::test]
async fn list_and_get_run() {
    let app = api::router(state_with_one_run());
    let (status, v) = get_json(app.clone(), "/runs").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["total"], 1, "{v}");
    assert_eq!(v["runs"][0]["run_id"], "r1", "{v}");
    let (status, v) = get_json(app, "/runs/r1").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["run_id"], "r1");
    assert_eq!(v["state"], "submitted");
}

fn run_cost(estimated_usd: f64, priced_jobs: i64, pending_jobs: i64) -> biffix_api::RunCost {
    biffix_api::RunCost {
        estimated_usd,
        compute_usd: estimated_usd,
        ebs_usd: 0.0,
        coordinator_usd: 0.0,
        winning_usd: estimated_usd,
        priced_jobs,
        pending_jobs,
        price_sources: vec![],
        by_instance_type: vec![],
    }
}

#[tokio::test]
async fn the_run_list_shows_each_run_s_cost_as_its_page_does_in_one_store_read() {
    // A finished run with priced jobs, a running one priced so far, one with only pending jobs,
    // and one with no cost at all.
    let store = Arc::new(MemStore::new());
    let mut done = testing::run_detail("r1", "succeeded");
    done.run.submitted_at = Some(4.0);
    done.cost = Some(run_cost(13.8, 5, 0));
    let mut live = testing::run_detail("r2", "running");
    live.run.submitted_at = Some(3.0);
    live.cost = Some(run_cost(1.25, 1, 2));
    let mut pending = testing::run_detail("r3", "running");
    pending.run.submitted_at = Some(2.0);
    pending.cost = Some(run_cost(0.0, 0, 1));
    let mut none = testing::run_detail("r4", "submitted");
    none.run.submitted_at = Some(1.0);
    for d in [done, live, pending, none] {
        store.insert(d);
    }
    let app = api::router(TestState::new().with_store(store.clone()).build());

    let (status, list) = get_json(app.clone(), "/runs").await;
    assert_eq!(status, StatusCode::OK, "{list}");
    assert_eq!(
        store.run_costs_calls.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "one cost read for the whole page, not one per run"
    );
    let runs = list["runs"].as_array().unwrap();
    assert_eq!(runs.len(), 4, "{list}");
    for run in runs {
        let id = run["run_id"].as_str().unwrap();
        let (_, detail) = get_json(app.clone(), &format!("/runs/{id}")).await;
        // The list's number is the run page's: its cost's estimate, once a job is priced; and it
        // is pending while the page counts a job not priced yet.
        let page = detail["cost"]["estimated_usd"]
            .as_f64()
            .filter(|_| detail["cost"]["priced_jobs"].as_i64() > Some(0));
        assert_eq!(run["cost_usd"].as_f64(), page, "{id}: {run} vs {detail}");
        let pending = detail["cost"]["pending_jobs"].as_i64() > Some(0);
        assert_eq!(run["cost_pending"], pending, "{id}: {run} vs {detail}");
        assert_eq!(detail["cost_usd"], run["cost_usd"], "{id}");
        assert_eq!(detail["cost_pending"], run["cost_pending"], "{id}");
    }
    let by_id = |id: &str| runs.iter().find(|r| r["run_id"] == id).unwrap().clone();
    let cost_of = |id: &str| {
        let r = by_id(id);
        (r.get("cost_usd").cloned(), r["cost_pending"].clone())
    };
    assert_eq!(cost_of("r1"), (Some(13.8.into()), false.into()));
    assert_eq!(cost_of("r2"), (Some(1.25.into()), true.into()));
    // Only pending jobs: no amount yet, but pending; no cost at all: neither.
    assert_eq!(cost_of("r3"), (None, true.into()));
    assert_eq!(cost_of("r4"), (None, false.into()));

    // The POST form carries it too.
    let (_, list) = post_json(app, "/runs/query", serde_json::json!({})).await;
    assert_eq!(list["runs"][0]["cost_usd"], 13.8, "{list}");
}

#[tokio::test]
async fn the_run_list_still_loads_without_costs_when_reading_them_fails() {
    // The cost column is a nicety: a failing cost read leaves it empty, not the list.
    let store = Arc::new(MemStore::new());
    let mut done = testing::run_detail("r1", "succeeded");
    done.cost = Some(run_cost(13.8, 5, 1));
    store.insert(done);
    store.fail_run_costs.store(true, std::sync::atomic::Ordering::SeqCst);
    let app = api::router(TestState::new().with_store(store.clone()).build());
    let (status, list) = get_json(app, "/runs").await;
    assert_eq!(status, StatusCode::OK, "{list}");
    let run = &list["runs"][0];
    assert_eq!(run["run_id"], "r1");
    assert!(run.get("cost_usd").is_none(), "{run}");
    assert_eq!(run["cost_pending"], false);
}

#[tokio::test]
async fn get_workflow_returns_full_version_with_form_schema() {
    let app = api::router(state_with_one_version().await);
    let (status, v) = get_json(app, "/workflows/abc:hash").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["id"], "abc:hash");
    assert_eq!(v["engine"], "snakemake");
    assert_eq!(v["source"]["kind"], "s3");
    // The detail carries the form (the list view leaves it out: see the next test).
    assert_eq!(v["form_schema"]["fields"][0]["name"], "reads");
}

#[tokio::test]
async fn the_workflow_list_leaves_the_form_out() {
    let app = api::router(state_with_one_version().await);
    let (status, v) = get_json(app, "/workflows").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let list = v.as_array().or_else(|| v["workflows"].as_array()).unwrap();
    assert_eq!(list.len(), 1, "{v}");
    assert!(list[0].get("form_schema").is_none(), "{v}");
}

#[tokio::test]
async fn get_missing_workflow_404() {
    let app = api::router(state_with_one_version().await);
    let (status, _) = get_json(app, "/workflows/nope:nope").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn get_missing_run_404() {
    let app = api::router(state_with_one_run());
    let (status, _) = get_json(app, "/runs/nope").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

fn state_with_run_prefix(recorded: Option<&str>) -> AppState {
    let mut detail = testing::run_detail("r1", "running");
    detail.output_prefix = recorded.map(String::from);
    state_with_run_detail(detail)
}

#[tokio::test]
async fn run_detail_returns_the_recorded_output_prefix() {
    // A `--output-prefix` run: the recorded prefix wins over the derived per-run default.
    let (status, v) =
        get_json(api::router(state_with_run_prefix(Some("s3://b/"))), "/runs/r1").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["output_prefix"], "s3://b/");
}

#[tokio::test]
async fn run_detail_exposes_engine_session_and_resumed_from() {
    let mut detail = testing::run_detail("r1", "succeeded");
    detail.engine_session_id = Some("3f6c2a1e-0000-4000-8000-000000000000".into());
    detail.session_mismatch = true;
    detail.resumed_from = Some(biffix_core::ids::RunId::new("r0"));
    let (status, v) = get_json(api::router(state_with_run_detail(detail)), "/runs/r1").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["engine_session_id"], "3f6c2a1e-0000-4000-8000-000000000000");
    assert_eq!(v["session_mismatch"], true);
    assert_eq!(v["resumed_from"], "r0");
}

#[tokio::test]
async fn a_resume_says_so_in_its_summary_in_the_list_and_the_detail() {
    let mut resumed = testing::run_detail("r1", "running");
    resumed.resumed_from = Some(biffix_core::ids::RunId::new("r0"));
    let store = MemStore::new();
    store.insert(resumed);
    store.insert(testing::run_detail("r0", "succeeded"));
    let state = TestState::new()
        .with_store(Arc::new(store))
        .with_projects(testing::bucket_project("b"))
        .build();
    let (status, list) = get_json(api::router(state.clone()), "/runs").await;
    assert_eq!(status, StatusCode::OK, "{list}");
    let resumed_of = |id: &str| {
        list["runs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["run_id"] == id)
            .map(|r| r.get("resumed").cloned())
            .unwrap()
    };
    assert_eq!(resumed_of("r1"), Some(serde_json::json!(true)));
    // Omitted when false.
    assert_eq!(resumed_of("r0"), None);
    let (_, v) = get_json(api::router(state), "/runs/r1").await;
    assert_eq!(v["resumed"], true);
    assert_eq!(v["resumed_from"], "r0");
}

#[tokio::test]
async fn run_detail_names_when_its_version_was_registered() {
    // Read with the run's row (the store joins `workflow_versions`), not from the registry.
    let mut detail = testing::run_detail("r1", "succeeded");
    detail.run.workflow = Some(biffix_core::ids::WorkflowVersionId::new("abc:hash"));
    detail.workflow_registered_at_micros = Some(1_759_500_000_000_000);
    let (status, v) =
        get_json(api::router(state_with_run_detail(detail.clone())), "/runs/r1").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["workflow_registered_at_micros"], serde_json::json!(1_759_500_000_000_000_i64));
    // A version the registry no longer has: left out, the page still loads.
    detail.workflow_registered_at_micros = None;
    let (status, v) = get_json(api::router(state_with_run_detail(detail)), "/runs/r1").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert!(v.get("workflow_registered_at_micros").is_none(), "{v}");
}

#[tokio::test]
async fn run_detail_omits_engine_session_fields_when_unset() {
    let detail = testing::run_detail("r1", "succeeded");
    let (status, v) = get_json(api::router(state_with_run_detail(detail)), "/runs/r1").await;
    assert_eq!(status, StatusCode::OK);
    for absent in ["engine_session_id", "session_mismatch", "resumed_from"] {
        assert!(v.get(absent).is_none(), "{absent} is omitted when unset");
    }
}

#[tokio::test]
async fn run_detail_serves_the_lifecycle_and_resume_facts() {
    // A finished Nextflow run that resumed r0 (so it shares r0's prefix), with its session.
    let mut detail = testing::run_detail("r1", "failed");
    detail.run.engine = Some("nextflow".into());
    detail.engine_session_id = Some("s1".into());
    detail.resumed_from = Some(biffix_core::ids::RunId::new("r0"));
    detail.output_prefix = Some("s3://b/biffix/runs/r0/".into());
    let (status, v) =
        get_json(api::router(state_with_run_detail(detail.clone())), "/runs/r1").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["terminal"], true);
    assert_eq!(v["resumable"], true);
    assert_eq!(v["prefix_kind"], "resumed");
    assert_eq!(v["resume_cache_uri"], "s3://b/biffix/runs/r0/.nextflow/cache/s1/");

    // The submit path's resume rules decide `resumable`: deleted work, no session, still running.
    type Perturb = fn(&mut biffix_backend::store::RunDetailView);
    let blockers: [Perturb; 3] = [
        |d| d.work_deleted_at_micros = Some(1),
        |d| d.engine_session_id = None,
        |d| d.run.state = "running".into(),
    ];
    for perturb in blockers {
        let mut d = detail.clone();
        perturb(&mut d);
        let (_, v) = get_json(api::router(state_with_run_detail(d)), "/runs/r1").await;
        assert_eq!(v["resumable"], false, "{v}");
    }

    // The list carries `terminal` too.
    let (_, list) = get_json(api::router(state_with_run_detail(detail)), "/runs").await;
    assert_eq!(list["runs"][0]["terminal"], true, "{list}");

    // A live Snakemake run on its own prefix: not terminal, not resumable yet, no Nextflow cache.
    let mut live = testing::run_detail("r2", "running");
    live.run.engine = Some("snakemake".into());
    live.output_prefix = Some("s3://b/biffix/runs/r2/".into());
    let (_, v) = get_json(api::router(state_with_run_detail(live)), "/runs/r2").await;
    assert_eq!(v["terminal"], false);
    assert_eq!(v["resumable"], false);
    assert_eq!(v["prefix_kind"], "isolated");
    assert!(v.get("resume_cache_uri").is_none());
}

#[tokio::test]
async fn tasks_page_filters_sorts_and_counts() {
    let app = || api::router(state_with_tasks());
    let (status, v) = post_json(app(), "/runs/r1/tasks/query", serde_json::json!({})).await;
    assert_eq!(status, StatusCode::OK);
    let keys = |v: &serde_json::Value| {
        v["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["task_key"].as_str().unwrap().to_string())
            .collect::<Vec<_>>()
    };
    assert_eq!(keys(&v), ["gone", "started", "waiting"]);
    assert_eq!(v["total"], 3);
    assert_eq!(v["counts"]["active"], 2);
    assert_eq!(v["counts"]["failed"], 1);

    let (_, v) = post_json(
        app(),
        "/runs/r1/tasks/query",
        serde_json::json!({"state": "active", "sort": "key", "order": "desc"}),
    )
    .await;
    assert_eq!(keys(&v), ["waiting", "started"]);
    assert_eq!(v["counts"]["all"], 3, "chip counts ignore the state filter");

    let (_, v) =
        post_json(app(), "/runs/r1/tasks/query", serde_json::json!({"limit": 1, "offset": 1}))
            .await;
    assert_eq!(keys(&v), ["started"]);
    assert_eq!((v["total"].clone(), v["offset"].clone()), (3.into(), 1.into()));
}

#[tokio::test]
async fn tasks_query_filters_on_an_exact_task_key_and_ignores_empty_filters() {
    let app = || api::router(state_with_tasks());
    let (_, exact) =
        post_json(app(), "/runs/r1/tasks/query", serde_json::json!({"task_key": "started"})).await;
    assert_eq!(exact["total"], 1);
    assert_eq!(exact["tasks"][0]["task_key"], "started");
    let (_, none) =
        post_json(app(), "/runs/r1/tasks/query", serde_json::json!({"task_key": "art"})).await;
    assert_eq!(none["total"], 0, "exact, not a substring");
    // An empty key (a cleared form field) is no filter, as an empty group is.
    let (_, all) = post_json(app(), "/runs/r1/tasks/query", serde_json::json!({})).await;
    for body in [serde_json::json!({"task_key": ""}), serde_json::json!({"group": ""})] {
        let (_, got) = post_json(app(), "/runs/r1/tasks/query", body.clone()).await;
        assert_eq!(got, all, "{body}");
    }
    let (status, _) =
        post_json(app(), "/runs/r1/tasks/query", serde_json::json!({"state": "done"})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = post_json(app(), "/runs/nope/tasks/query", serde_json::json!({})).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn runs_query_takes_the_list_params_in_a_body_and_answers_as_the_get_does() {
    let app = || api::router(state_with_tasks());
    for (query, body) in [
        ("", serde_json::json!({})),
        ("?filter=active", serde_json::json!({"filter": "active"})),
        ("?q=r1", serde_json::json!({"q": "r1"})),
        ("?q=nothing-matches", serde_json::json!({"q": "nothing-matches"})),
        ("?limit=1", serde_json::json!({"limit": 1})),
    ] {
        let (_, want) = get_json(app(), &format!("/runs{query}")).await;
        let (status, got) = post_json(app(), "/runs/query", body).await;
        assert_eq!(status, StatusCode::OK, "{query}");
        assert_eq!(got, want, "{query}");
    }
    let (_, hit) = post_json(app(), "/runs/query", serde_json::json!({"q": "r1"})).await;
    assert_eq!(hit["total"], 1);
    let (_, miss) = post_json(app(), "/runs/query", serde_json::json!({"q": "zzz"})).await;
    assert_eq!(miss["total"], 0);
    let (status, _) = post_json(app(), "/runs/query", serde_json::json!({"filter": "done"})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    // The static `/runs/query` route does not shadow a run's detail.
    let (status, _) = get_json(app(), "/runs/r1").await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn run_detail_can_leave_out_its_tasks() {
    let app = || api::router(state_with_tasks());
    let (_, full) = get_json(app(), "/runs/r1").await;
    assert_eq!(full["tasks"].as_array().unwrap().len(), 3);
    let (status, light) = get_json(app(), "/runs/r1?tasks=false").await;
    assert_eq!(status, StatusCode::OK);
    assert!(light["tasks"].as_array().unwrap().is_empty());
    assert_eq!(light["run_id"], "r1");
}

fn grouped_task(key: &str, group: &str, state: &str, cached: bool) -> biffix_api::Task {
    let mut t = task_view(key, state, None, None);
    t.group = Some(group.into());
    t.cached = cached;
    t
}

#[tokio::test]
async fn a_run_s_dag_unions_declared_and_observed_groups_and_drops_dangling_edges() {
    let mut detail = testing::run_detail("r1", "running");
    detail.dag_nodes = vec!["align".into(), "declared_only".into()];
    detail.dag_edges = vec![
        ("align".into(), "sort".into()),
        ("align".into(), "ghost".into()),
        ("declared_only".into(), "align".into()),
    ];
    detail.tasks = vec![
        grouped_task("align:s=1", "align", "succeeded", true),
        grouped_task("align:s=2", "align", "succeeded", false),
        grouped_task("align:s=3", "align", "queued", false),
        grouped_task("align:s=4", "align", "running", false),
        // A group the declared graph never named still renders.
        grouped_task("sort:s=1", "sort", "failed", false),
    ];
    let (status, v) = get_json(api::router(state_with_run_detail(detail)), "/runs/r1").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let node = |id: &str, pending: i64, running: i64, succeeded: i64, failed: i64, cached: i64| {
        serde_json::json!({"id": id, "pending": pending, "running": running,
            "succeeded": succeeded, "failed": failed, "cached": cached})
    };
    assert_eq!(
        v["dag"],
        serde_json::json!({
            "nodes": [
                // Queued rolls up onto pending; a cached task counts as succeeded AND cached.
                node("align", 1, 1, 2, 0, 1),
                // Declared with no tasks: an empty node.
                node("declared_only", 0, 0, 0, 0, 0),
                node("sort", 0, 0, 0, 1, 0),
            ],
            // The edge to an unknown node is dropped.
            "edges": [["align", "sort"], ["declared_only", "align"]],
        })
    );
}

#[tokio::test]
async fn listing_runs_with_an_unknown_filter_is_a_client_error() {
    // The 400 says what was wrong and what is accepted, by query or by body.
    let accepted =
        "unknown filter `bogus` (all, active, submitted, completed, succeeded or failed)";
    let (status, v) = get_json(api::router(state_with_one_run()), "/runs?filter=bogus").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(v, accepted);
    let (status, v) = post_json(
        api::router(state_with_one_run()),
        "/runs/query",
        serde_json::json!({"filter": "bogus"}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(v, accepted);
    // As does one naming more runs than a page holds.
    let ids: Vec<String> = (0..201).map(|i| format!("r{i}")).collect();
    let (status, v) = post_json(
        api::router(state_with_one_run()),
        "/runs/query",
        serde_json::json!({"ids": ids}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(v, "`ids` names 201 runs; at most 200 per request");
    let (status, v) = get_json(api::router(state_with_one_run()), "/runs?filter=submitted").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["total"], 1);
}

#[tokio::test]
async fn listing_runs_scopes_searches_orders_and_pages_like_the_real_store() {
    let store = MemStore::new();
    // (run id, submitted at secs, workflow version, workflow name, submitter)
    for (id, submitted, version, name, submitter) in [
        ("r-old", Some(100.0), "v1", "rnaseq", "user:alice"),
        ("r-mid", Some(200.0), "v2", "sarek", "user:bob"),
        ("r-new", Some(300.0), "v1", "rnaseq", "user:bob"),
        ("r-unanchored", None, "v2", "sarek", "user:carol"),
    ] {
        let mut detail = testing::run_detail(id, "running");
        detail.run.submitted_at = submitted;
        detail.run.workflow = Some(version.to_string().into());
        detail.run.workflow_name = Some(name.into());
        detail.run.submitter = Some(submitter.into());
        detail.run.engine = Some("snakemake".into());
        // The pipeline title the Lab shows, as the real store joins it from the frozen form.
        detail.run.workflow_title = (name == "sarek").then(|| "Tumour calling".into());
        store.insert(detail);
    }
    let mut state = state_with_one_run();
    state.store = Arc::new(store);
    let ids = |v: &serde_json::Value| -> Vec<String> {
        v["runs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["run_id"].as_str().unwrap().to_string())
            .collect()
    };
    let get = |uri: String| {
        let app = api::router(state.clone());
        async move {
            let (status, v) = get_json(app, &uri).await;
            assert_eq!(status, StatusCode::OK, "{uri}: {v}");
            v
        }
    };

    // Newest submitted first; a run with no anchor yet sorts as the newest.
    let all = get("/runs".into()).await;
    assert_eq!(ids(&all), ["r-unanchored", "r-new", "r-mid", "r-old"]);
    // The exact version scope.
    let v1 = get("/runs?workflow=v1".into()).await;
    assert_eq!((ids(&v1), v1["total"].as_i64()), (vec!["r-new".into(), "r-old".into()], Some(2)));
    // Search finds the workflow name, and never a term spread across two fields.
    assert_eq!(ids(&get("/runs?q=SAREK".into()).await), ["r-unanchored", "r-mid"]);
    assert!(ids(&get("/runs?q=alice%20snakemake".into()).await).is_empty());
    // And the pipeline's title, as the Pg store does.
    assert_eq!(ids(&get("/runs?q=tumour".into()).await), ["r-unanchored", "r-mid"]);
    // Two pages forward and one back reproduce the single page.
    let p1 = get("/runs?limit=2".into()).await;
    assert_eq!(ids(&p1), ["r-unanchored", "r-new"]);
    let after = p1["last_cursor"].as_str().unwrap();
    let p2 = get(format!("/runs?limit=2&after={after}")).await;
    assert_eq!(ids(&p2), ["r-mid", "r-old"]);
    let before = p2["first_cursor"].as_str().unwrap();
    assert_eq!(ids(&get(format!("/runs?limit=2&before={before}")).await), ids(&p1));
    // A token that does not decode is the first page, in order.
    assert_eq!(ids(&get("/runs?limit=2&before=garbage".into()).await), ids(&p1));
}

#[tokio::test]
async fn the_coordinator_host_is_labelled_with_the_deploy_s_coordinator_queue() {
    let mut detail = testing::run_detail("r1", "running");
    detail.coordinator_host = Some(biffix_api::Host::default());
    let mut state = state_with_run_detail(detail);
    state.deploy.coordinator_queue =
        "arn:aws:batch:us-east-1:123456789012:job-queue/biffix-coordinator".into();
    state.deploy.queue_capacities =
        [("biffix-coordinator".to_string(), "on_demand".to_string())].into();
    let (status, v) = get_json(api::router(state), "/runs/r1").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let host = &v["coordinator_host"];
    assert_eq!(host["queue"], "biffix-coordinator");
    assert_eq!(host["capacity"], "on_demand");
    assert_eq!(host["capacity_source"], "queue");
}

#[tokio::test]
async fn a_request_carrying_a_nul_is_a_400_never_a_retryable_503() {
    let app = api::router(state_with_one_run());
    // In the query, the path, and a JSON body: Postgres stores no NUL, so binding one fails.
    for uri in ["/runs?q=a%00b", "/runs/r1%00", "/experiments?project=x%00"] {
        let (status, v) = get_json(app.clone(), uri).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{uri}: {v}");
    }
    let (status, v) =
        post_json(app.clone(), "/runs/r1/tasks/query", serde_json::json!({"q": "a\u{0}b"})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{v}");
    assert_eq!(
        v,
        serde_json::json!(
            "the request's body holds a NUL character (`\\u0000`), which biffix never stores"
        )
    );
    // The same text without it reads as ever.
    let (status, v) = post_json(app, "/runs/r1/tasks/query", serde_json::json!({"q": "ab"})).await;
    assert_eq!(status, StatusCode::OK, "{v}");
}
