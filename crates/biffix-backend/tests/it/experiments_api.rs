//! Experiment endpoints over the in-memory doubles: CRUD with name uniqueness and archive/restore,
//! a run's membership recorded as its `experiment_set` event and folded, the run list's experiment
//! filter, and the detail's rollups (runs, samples across them, total cost). Each route's
//! capability gate is covered by `capability_gates.rs`.

use std::sync::Arc;

use axum::http::{Method, StatusCode};
use serde_json::{json, Value};

use biffix_backend::register::workflow::{WorkflowSource, WorkflowVersion};
use biffix_backend::store::experiments::{ExperimentRecord, ExperimentStore};
use biffix_backend::store::registry::{RegisteredVersion, RegistryStore};
use biffix_backend::testing::MemExperimentStore;
use biffix_backend::testing::{
    self, call, get_json, post_json, MemEventSink, MemRegistry, MemStore, TestState,
};
use biffix_backend::{api, AppState};
use biffix_workflow::engines::Engine;

/// A registered version in `project`, so the project is one biffix knows.
fn version_in(project: &str, id: &str) -> RegisteredVersion {
    RegisteredVersion {
        name: "wf".into(),
        project: project.into(),
        source: WorkflowSource::S3 { uri: "s3://b/src/".into(), sha256: None },
        version: WorkflowVersion {
            id: id.into(),
            git_sha: id.split(':').next().unwrap().into(),
            source_content_hash: "hash".into(),
            image_digest: "sha256:1a2b".into(),
            image_repository: None,
            image_registry_id: None,
            image_region: None,
            engine: Engine::Snakemake,
            engine_version: "9.27".into(),
            entrypoint: "Snakefile".into(),
            target: None,
            form_schema: json!({"fields": [], "warnings": []}),
            queue_classes: vec![],
            about_markdown: None,
            runtime_key: "rt".into(),
            runtime_recipe: json!({"engine": "snakemake"}),
            build: 0,
        },
        created_by: None,
        created_by_label: None,
        created_at_micros: 0,
        registration_inputs: Default::default(),
        registration_inputs_hash: String::new(),
        images: Vec::new(),
    }
}

/// A run of project `project` submitted at `submitted_at` (epoch seconds).
fn run_in(
    id: &str,
    state: &str,
    project: &str,
    submitted_at: f64,
) -> biffix_backend::store::RunDetailView {
    let mut d = testing::run_detail(id, state);
    d.run.project = Some(project.into());
    d.run.submitted_at = Some(submitted_at);
    d
}

/// The doubles a test drives: projects `demo` and `other` are known; recorded events fold into the
/// store.
struct Harness {
    store: Arc<MemStore>,
    events: Arc<MemEventSink>,
    experiments: Arc<MemExperimentStore>,
    state: AppState,
}

async fn harness() -> Harness {
    let store = Arc::new(MemStore::new());
    let events = Arc::new(MemEventSink::folding_into(store.clone()));
    let experiments = Arc::new(MemExperimentStore::default());
    let registry = MemRegistry::new();
    registry.put_version(&version_in("demo", "v1:h:0")).await.unwrap();
    registry.put_version(&version_in("other", "v2:h:0")).await.unwrap();
    let state = TestState::new()
        .with_store(store.clone())
        .with_events(events.clone())
        .with_experiments(experiments.clone())
        .with_registry(Arc::new(registry))
        .build();
    Harness { store, events, experiments, state }
}

impl Harness {
    fn app(&self) -> axum::Router {
        api::router(self.state.clone())
    }

    /// Create `name` in `project` and return its id.
    async fn create(&self, project: &str, name: &str) -> String {
        let (status, v) =
            post_json(self.app(), "/experiments", json!({"project": project, "name": name})).await;
        assert_eq!(status, StatusCode::CREATED, "{v}");
        v["id"].as_str().unwrap().to_string()
    }

    async fn put(&self, id: &str, body: Value) -> (StatusCode, Value) {
        call(self.app(), Method::PUT, &format!("/experiments/{id}"), Some(body)).await
    }

    async fn set(&self, run: &str, experiment: Option<&str>) -> (StatusCode, Value) {
        post_json(
            self.app(),
            &format!("/runs/{run}/experiment"),
            json!({"experiment_id": experiment}),
        )
        .await
    }

    /// The row `GET /runs/{run}` serves is in the experiment a refold of the whole log says.
    async fn assert_served_row_is_the_refold(&self, run: &str) {
        let (status, v) = get_json(self.app(), &format!("/runs/{run}")).await;
        assert_eq!(status, StatusCode::OK, "{v}");
        let served = v.get("experiment_id").and_then(Value::as_str).map(str::to_string);
        assert_eq!(served, self.events.folded().runs[run].experiment_id(), "{v}");
    }

    async fn names(&self, uri: &str) -> Vec<String> {
        let (status, v) = get_json(self.app(), uri).await;
        assert_eq!(status, StatusCode::OK, "{v}");
        v["experiments"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["name"].as_str().unwrap().to_string())
            .collect()
    }
}

#[tokio::test]
async fn an_experiment_is_created_in_a_known_project_with_a_live_unique_name() {
    let h = harness().await;
    let (status, v) = post_json(
        h.app(),
        "/experiments",
        json!({"project": "demo", "name": "  RNA batch 12 ", "description": " why "}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{v}");
    assert!(v["id"].as_str().unwrap().starts_with("exp-"), "{v}");
    assert_eq!(v["project"], "demo");
    assert_eq!(v["name"], "RNA batch 12");
    assert_eq!(v["description"], "why");
    assert_eq!(v["created_by"], "local");
    assert_eq!(v["run_count"], 0);
    assert!(v.get("archived_at").is_none() && v.get("last_run_at").is_none());

    // The same name in another case is taken; the 409 names it.
    let (status, v) =
        post_json(h.app(), "/experiments", json!({"project": "demo", "name": "rna BATCH 12"}))
            .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(v.as_str().unwrap().contains("rna BATCH 12"), "{v}");
    // Another project may use it.
    h.create("other", "RNA batch 12").await;
    // A project no workflow is in, and a blank name, are refused.
    let (status, _) =
        post_json(h.app(), "/experiments", json!({"project": "nowhere", "name": "x"})).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) =
        post_json(h.app(), "/experiments", json!({"project": "demo", "name": "  "})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn an_experiment_is_renamed_described_archived_and_restored() {
    let h = harness().await;
    let a = h.create("demo", "A").await;
    h.create("demo", "B").await;

    let (status, v) = h.put(&a, json!({"name": "A2", "description": "first"})).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!((v["name"].clone(), v["description"].clone()), (json!("A2"), json!("first")));
    // An absent field keeps its value; `null` (or blank) clears the description.
    let (_, v) = h.put(&a, json!({})).await;
    assert_eq!(v["description"], "first");
    let (_, v) = h.put(&a, json!({"description": null})).await;
    assert!(v.get("description").is_none(), "{v}");
    // A rename onto a live name clashes.
    let (status, _) = h.put(&a, json!({"name": "b"})).await;
    assert_eq!(status, StatusCode::CONFLICT);

    // Archived: out of the default list, in the archived one; its name is free.
    let (status, v) = h.put(&a, json!({"archived": true})).await;
    assert_eq!(status, StatusCode::OK);
    assert!(v["archived_at"].as_f64().is_some(), "{v}");
    assert_eq!(h.names("/experiments?project=demo").await, ["B"]);
    let mut all = h.names("/experiments?project=demo&archived=true").await;
    all.sort();
    assert_eq!(all, ["A2", "B"]);
    let taker = h.create("demo", "A2").await;
    // Restoring it now clashes; once the name is free again it comes back.
    let (status, _) = h.put(&a, json!({"archived": false})).await;
    assert_eq!(status, StatusCode::CONFLICT);
    h.put(&taker, json!({"name": "A3"})).await;
    let (status, v) = h.put(&a, json!({"archived": false})).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert!(v.get("archived_at").is_none());

    let (status, _) = h.put("exp-nope", json!({"name": "x"})).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = get_json(h.app(), "/experiments/exp-nope").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_run_joins_moves_and_leaves_an_experiment_by_event() {
    let h = harness().await;
    h.store.insert(run_in("r1", "running", "demo", 10.0));
    h.store.insert(run_in("r2", "running", "demo", 20.0));
    let a = h.create("demo", "A").await;
    let b = h.create("demo", "B").await;

    let (status, v) = h.set("r1", Some(&a)).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["experiment_id"], json!(a));
    let listed = |v: &Value| -> Vec<String> {
        v["runs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["run_id"].as_str().unwrap().to_string())
            .collect()
    };
    let (_, v) = get_json(h.app(), &format!("/runs?experiment={a}")).await;
    assert_eq!(listed(&v), ["r1"]);
    assert_eq!(v["runs"][0]["experiment_id"], json!(a));

    // Moved to B, then out of any; each change is one `experiment_set` in the log.
    h.set("r1", Some(&b)).await;
    h.assert_served_row_is_the_refold("r1").await;
    let (_, v) = get_json(h.app(), &format!("/runs?experiment={a}")).await;
    assert_eq!(listed(&v), Vec::<String>::new());
    let (_, v) = post_json(h.app(), "/runs/query", json!({"experiment": b})).await;
    assert_eq!(listed(&v), ["r1"]);
    let (status, v) = h.set("r1", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(v.get("experiment_id").is_none(), "{v}");
    let sets: Vec<Option<String>> = h
        .events
        .events
        .lock()
        .unwrap()
        .iter()
        .filter_map(|e| match &e.data {
            biffix_core::event::EventData::ExperimentSet { experiment_id, actor } => {
                assert_eq!(actor.as_deref(), Some("local"));
                Some(experiment_id.clone())
            }
            _ => None,
        })
        .collect();
    assert_eq!(sets, [Some(a.clone()), Some(b.clone()), None]);
    // The row the API serves is what a refold of the whole log gives: in none.
    assert_eq!(h.events.folded().runs["r1"].experiment_id(), None);
    h.assert_served_row_is_the_refold("r1").await;
}

#[tokio::test]
async fn joining_an_experiment_answers_with_the_run_s_cost_as_the_list_has_it() {
    let h = harness().await;
    let mut d = run_in("r1", "succeeded", "demo", 10.0);
    d.cost = Some(biffix_api::RunCost { pending_jobs: 1, ..cost(4.25) });
    h.store.insert(d);
    let a = h.create("demo", "A").await;
    let (status, v) = h.set("r1", Some(&a)).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let (_, detail) = get_json(h.app(), "/runs/r1").await;
    assert_eq!(v["cost_usd"], 4.25, "{v}");
    assert_eq!(v["cost_pending"], true, "{v}");
    assert_eq!(
        (&v["cost_usd"], &v["cost_pending"]),
        (&detail["cost_usd"], &detail["cost_pending"])
    );
}

#[tokio::test]
async fn a_membership_body_must_name_experiment_id() {
    let h = harness().await;
    h.store.insert(run_in("r1", "running", "demo", 10.0));
    let a = h.create("demo", "A").await;
    h.set("r1", Some(&a)).await;
    // Absent or misspelled: a 400, not "take it out". Nothing is recorded.
    let recorded = h.events.events.lock().unwrap().len();
    for body in [json!({}), json!({"experimentId": null}), json!({"experiment_id": 7})] {
        let (status, v) = post_json(h.app(), "/runs/r1/experiment", body.clone()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(
            v,
            json!(
                "the body must name `experiment_id`: an experiment id, or null to take the run \
                 out of its experiment"
            ),
            "{body}"
        );
    }
    assert_eq!(h.events.events.lock().unwrap().len(), recorded);
    let (_, v) = get_json(h.app(), "/runs/r1").await;
    assert_eq!(v["experiment_id"], json!(a), "still in A");
    // A padded id is trimmed; a blank one takes the run out, as `null` does.
    let (status, v) = h.set("r1", None).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let padded = format!("  {a} ");
    let (status, v) = h.set("r1", Some(&padded)).await;
    assert_eq!((status, v["experiment_id"].clone()), (StatusCode::OK, json!(a)));
    let (status, v) = h.set("r1", Some("  ")).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert!(v.get("experiment_id").is_none(), "{v}");
}

#[tokio::test]
async fn a_run_with_no_project_cannot_join_an_experiment() {
    let h = harness().await;
    h.store.insert(testing::run_detail("r1", "running"));
    let a = h.create("demo", "A").await;
    let (status, v) = h.set("r1", Some(&a)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(v, json!("run r1 has no project, so it cannot be put in an experiment"));
    // Taking it out of none is still fine.
    let (status, _) = h.set("r1", None).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn a_run_joins_only_a_live_experiment_of_its_project() {
    let h = harness().await;
    h.store.insert(run_in("r1", "running", "demo", 10.0));
    let elsewhere = h.create("other", "X").await;
    let archived = h.create("demo", "Old").await;
    h.put(&archived, json!({"archived": true})).await;

    let (status, v) = h.set("r1", Some(&elsewhere)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(v.as_str().unwrap().contains("project other"), "{v}");
    let (status, v) = h.set("r1", Some(&archived)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(v.as_str().unwrap().contains("archived"), "{v}");
    let (status, _) = h.set("r1", Some("exp-nope")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = h.set("nope", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // Nothing was recorded for any refused change.
    assert!(h.events.events.lock().unwrap().is_empty());
}

fn cost(usd: f64) -> biffix_api::RunCost {
    biffix_api::RunCost {
        estimated_usd: usd,
        compute_usd: usd,
        ebs_usd: 0.0,
        coordinator_usd: 0.0,
        winning_usd: usd,
        priced_jobs: 1,
        pending_jobs: 0,
        price_sources: vec![],
        by_instance_type: vec![],
    }
}

/// A run in `exp` with sheet `ids`, each sample's one `align` task in `task_state`.
fn sample_run(
    id: &str,
    state: &str,
    submitted_at: f64,
    exp: &str,
    ids: &[&str],
    task_state: &str,
    usd: Option<f64>,
) -> (biffix_backend::store::RunDetailView, biffix_core::event::SampleSpec) {
    let mut d = run_in(id, state, "demo", submitted_at);
    d.run.experiment_id = Some(exp.into());
    d.dag_nodes = vec!["align".into()];
    for s in ids {
        let mut t = testing::task_view(&format!("align:sample={s}"), task_state, None, None);
        t.group = Some("align".into());
        d.tasks.push(t);
    }
    d.cost = usd.map(cost);
    let spec = biffix_core::event::SampleSpec {
        ids: ids.iter().map(std::string::ToString::to_string).collect(),
        ..Default::default()
    };
    (d, spec)
}

#[tokio::test]
async fn an_experiment_s_detail_rolls_up_its_runs_samples_and_cost() {
    let h = harness().await;
    let a = h.create("demo", "A").await;
    let quiet = h.create("demo", "Quiet").await;
    for (d, spec) in [
        sample_run("r1", "succeeded", 10.0, &a, &["S1", "S2"], "succeeded", Some(1.5)),
        sample_run("r2", "failed", 20.0, &a, &["S2", "S3"], "failed", None),
        sample_run("r3", "running", 30.0, &a, &["S3"], "running", Some(2.0)),
    ] {
        h.store.sample_specs.lock().unwrap().insert(d.run.run_id.to_string(), spec);
        h.store.insert(d);
    }
    // A run of no experiment is not in it.
    h.store.insert(run_in("r9", "running", "demo", 40.0));

    let (status, v) = get_json(h.app(), &format!("/experiments/{a}")).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let runs: Vec<_> =
        v["runs"].as_array().unwrap().iter().map(|r| r["run_id"].as_str().unwrap()).collect();
    assert_eq!(runs, ["r3", "r2", "r1"]);
    assert_eq!(v["runs"][1]["terminal"], true);
    assert_eq!(v["experiment"]["run_count"], 3);
    assert_eq!(v["runs_total"], 3);
    assert_eq!(v["experiment"]["last_run_at"], 30.0);
    assert_eq!(v["total_cost_usd"], 3.5);
    // Each run carries its own cost, as the run list does; r2 has none.
    let costs: Vec<_> =
        v["runs"].as_array().unwrap().iter().map(|r| r["cost_usd"].as_f64()).collect();
    assert_eq!(costs, [Some(2.0), None, Some(1.5)]);
    // Samples in first-seen order over the runs oldest first, each with its state per run.
    assert_eq!(
        v["samples"],
        json!([
            {"id": "S1", "by_run": {"r1": "done"}},
            {"id": "S2", "by_run": {"r1": "done", "r2": "failed"}},
            {"id": "S3", "by_run": {"r2": "failed", "r3": "running"}},
        ])
    );

    // An experiment with no runs: no samples, no cost. One whose only run has no priced job has
    // no cost either.
    let (_, v) = get_json(h.app(), &format!("/experiments/{quiet}")).await;
    assert_eq!(v["runs"], json!([]));
    assert_eq!(v["runs_total"], 0);
    assert_eq!(v["samples"], json!([]));
    assert!(v.get("total_cost_usd").is_none(), "{v}");
    let pending = h.create("demo", "Pending").await;
    let mut d = run_in("r8", "running", "demo", 5.0);
    d.run.experiment_id = Some(pending.clone());
    d.cost = Some(biffix_api::RunCost { priced_jobs: 0, pending_jobs: 2, ..cost(0.0) });
    h.store.insert(d);
    let (_, v) = get_json(h.app(), &format!("/experiments/{pending}")).await;
    assert_eq!(v["runs_total"], 1);
    assert!(v.get("total_cost_usd").is_none(), "{v}");

    // The list, with counts: newest activity first (Quiet was created now, after A's runs).
    let (_, v) = get_json(h.app(), "/experiments").await;
    let rows: Vec<_> = v["experiments"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| (e["name"].clone(), e["run_count"].clone()))
        .collect();
    // Pending and Quiet were both created now (in either order, should the clock not tick between
    // them); A's newest run is from long ago.
    let mut created_now = rows[..2].to_vec();
    created_now.sort_by_key(|(name, _)| name.to_string());
    assert_eq!(created_now, [(json!("Pending"), json!(1)), (json!("Quiet"), json!(0))]);
    assert_eq!(rows[2], (json!("A"), json!(3)));
}

#[tokio::test]
async fn the_list_orders_by_newest_activity() {
    let h = harness().await;
    for (id, name, created) in [("e-old", "Old", 1), ("e-new", "New", 2)] {
        h.experiments
            .create(&ExperimentRecord {
                id: id.into(),
                project: "demo".into(),
                name: name.into(),
                description: None,
                created_by: None,
                created_by_label: None,
                created_at_micros: created,
                archived_at_micros: None,
            })
            .await
            .unwrap();
    }
    assert_eq!(h.names("/experiments").await, ["New", "Old"]);
    // A run in the older one makes it the most recently active.
    let mut d = run_in("r1", "running", "demo", 100.0);
    d.run.experiment_id = Some("e-old".into());
    h.store.insert(d);
    assert_eq!(h.names("/experiments").await, ["Old", "New"]);
    assert_eq!(h.names("/experiments?project=other").await, Vec::<String>::new());
}

#[tokio::test]
async fn a_new_experiment_holding_an_older_run_sorts_by_its_creation() {
    let h = harness().await;
    let at = |secs: i64| secs * 1_000_000;
    for (id, name, created) in [("e-new", "New", at(2_000)), ("e-mid", "Mid", at(1_500))] {
        h.experiments
            .create(&ExperimentRecord {
                id: id.into(),
                project: "demo".into(),
                name: name.into(),
                description: None,
                created_by: None,
                created_by_label: None,
                created_at_micros: created,
                archived_at_micros: None,
            })
            .await
            .unwrap();
    }
    // A run submitted long before either existed is moved into the newer one: its activity is
    // still its creation, so it stays first, and its newest run is still the old one.
    h.store.insert(run_in("r-old", "succeeded", "demo", 100.0));
    let (status, _) = h.set("r-old", Some("e-new")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(h.names("/experiments").await, ["New", "Mid"]);
    let (_, v) = get_json(h.app(), "/experiments").await;
    assert_eq!(v["experiments"][0]["last_run_at"], 100.0);
    assert_eq!(v["experiments"][0]["run_count"], 1);
}

#[tokio::test]
async fn an_experiment_s_detail_returns_its_newest_runs_and_counts_and_costs_them_all() {
    let h = harness().await;
    let a = h.create("demo", "Big").await;
    let max = biffix_backend::store::runs::MAX_RUN_PAGE;
    // MAX_RUN_PAGE + 1 runs, oldest first; only the oldest names sample S-old.
    for i in 0..=max {
        let id = format!("r{i:03}");
        let sample = if i == 0 { "S-old" } else { "S" };
        let (d, spec) =
            sample_run(&id, "succeeded", 10.0 + i as f64, &a, &[sample], "succeeded", Some(1.0));
        h.store.sample_specs.lock().unwrap().insert(d.run.run_id.to_string(), spec);
        h.store.insert(d);
    }
    let (status, v) = get_json(h.app(), &format!("/experiments/{a}")).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let runs = v["runs"].as_array().unwrap();
    assert_eq!(runs.len() as i64, max, "at most the newest page");
    assert_eq!(runs[0]["run_id"], format!("r{max:03}"));
    assert!(runs.iter().all(|r| r["run_id"] != "r000"), "the oldest is cut");
    assert_eq!(v["runs_total"], max + 1);
    assert_eq!(v["experiment"]["run_count"], max + 1);
    // The cost is over every run, the samples over the runs returned.
    assert_eq!(v["total_cost_usd"], (max + 1) as f64);
    let samples: Vec<_> =
        v["samples"].as_array().unwrap().iter().map(|s| s["id"].as_str().unwrap()).collect();
    assert_eq!(samples, ["S"]);
    assert_eq!(v["samples"][0]["by_run"].as_object().unwrap().len() as i64, max);
}

/// Store experiment `id` named `name` in project `project`, made by `by` at `created` (micros),
/// archived when `archived`.
async fn stored(
    h: &Harness,
    (id, project): (&str, &str),
    name: &str,
    by: &str,
    created: i64,
    archived: bool,
) {
    h.experiments
        .create(&ExperimentRecord {
            id: id.into(),
            project: project.into(),
            name: name.into(),
            description: None,
            created_by: Some(by.into()),
            created_by_label: None,
            created_at_micros: created,
            archived_at_micros: archived.then_some(created + 1),
        })
        .await
        .unwrap();
}

/// A failed run of `user`'s in experiment `exp`, whose failure notification `user` has not read.
fn failed_run_of(h: &Harness, run: &str, user: &str, exp: &str, submitted_at: f64) {
    failed_run_notifying(h, run, (user, user), exp, submitted_at);
}

/// A failed run `submitter` submitted into experiment `exp`, whose failure notification only
/// `notified` has (unread).
fn failed_run_notifying(
    h: &Harness,
    run: &str,
    (submitter, notified): (&str, &str),
    exp: &str,
    submitted_at: f64,
) {
    let mut d = run_in(run, "failed", "demo", submitted_at);
    d.run.submitter = Some(format!("user:{submitter}"));
    d.run.experiment_id = Some(exp.into());
    h.store.insert(d);
    h.store.insert_notification(run, notified, biffix_core::state::RunState::Failed, 0);
}

#[tokio::test]
async fn mine_keeps_the_experiments_the_caller_made_or_has_a_run_in() {
    // The test caller (auth disabled) is `local`; its runs record submitter `user:local`.
    let h = harness().await;
    h.create("demo", "Made by me").await;
    stored(&h, ("e-joined", "demo"), "Joined", "bob", 1, false).await;
    stored(&h, ("e-theirs", "demo"), "Theirs", "bob", 2, false).await;
    stored(&h, ("e-old", "demo"), "Old of mine", "local", 3, true).await;
    failed_run_of(&h, "r-mine", "local", "e-joined", 100.0);
    failed_run_of(&h, "r-bob", "bob", "e-theirs", 200.0);
    // Beside the caller's unread failure in the same experiment: Bob's own unread failure (not the
    // caller's run), and a failed run of the caller's whose only unread notification is Bob's (not
    // the caller's to read). A read that drops the submitter scope counts the first; one that
    // drops the user scope, the second.
    failed_run_of(&h, "r-bob-joined", "bob", "e-joined", 50.0);
    failed_run_notifying(&h, "r-mine-bobs-note", ("local", "bob"), "e-joined", 60.0);

    // Newest activity first, as the unfiltered list orders them.
    assert_eq!(h.names("/experiments").await, ["Made by me", "Theirs", "Joined"]);
    assert_eq!(h.names("/experiments?mine=true").await, ["Made by me", "Joined"]);
    assert_eq!(
        h.names("/experiments?mine=true&archived=true").await,
        ["Made by me", "Joined", "Old of mine"]
    );
    assert_eq!(h.names("/experiments?mine=false").await, ["Made by me", "Theirs", "Joined"]);
    assert_eq!(h.names("/experiments?mine=true&project=other").await, Vec::<String>::new());

    // Each carries whether it is the caller's and how many of the caller's runs in it need their
    // attention: Bob's failures are not the caller's to look at.
    let (_, v) = get_json(h.app(), "/experiments").await;
    let caller_view: Vec<(&str, bool, i64)> = v["experiments"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            (
                e["name"].as_str().unwrap(),
                e["mine"].as_bool().unwrap(),
                e["needs_attention"].as_i64().unwrap(),
            )
        })
        .collect();
    assert_eq!(caller_view, [("Made by me", true, 0), ("Theirs", false, 0), ("Joined", true, 1)]);
    let (status, v) = get_json(h.app(), "/experiments/e-joined").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(
        (&v["experiment"]["needs_attention"], &v["experiment"]["mine"]),
        (&json!(1), &json!(true))
    );
    let (status, v) = get_json(h.app(), "/experiments/e-theirs").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(
        (&v["experiment"]["needs_attention"], &v["experiment"]["mine"]),
        (&json!(0), &json!(false))
    );
    let (_, v) = h.put("e-joined", json!({"description": "why"})).await;
    assert_eq!((&v["needs_attention"], &v["mine"]), (&json!(1), &json!(true)));
    let (_, v) = h.put("e-theirs", json!({"description": "why"})).await;
    assert_eq!((&v["needs_attention"], &v["mine"]), (&json!(0), &json!(false)));

    // Read, the failure still needs attention; dismissed, it no longer does, and the experiment
    // stays the caller's.
    let (status, v) =
        post_json(h.app(), "/me/notifications/read", json!({"ids": ["r-mine:failed"]})).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let (_, v) = get_json(h.app(), "/experiments/e-joined").await;
    assert_eq!(v["experiment"]["needs_attention"], 1);
    let (status, v) =
        post_json(h.app(), "/me/attention/dismiss", json!({"run_ids": ["r-mine"]})).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let (_, v) = get_json(h.app(), "/experiments/e-joined").await;
    assert_eq!(v["experiment"]["needs_attention"], 0);
    assert_eq!(h.names("/experiments?mine=true").await, ["Made by me", "Joined"]);

    // Mine and a project combine: of project `other`'s two, only the caller's.
    stored(&h, ("e-other-mine", "other"), "Other of mine", "local", 4, false).await;
    stored(&h, ("e-other-bob", "other"), "Other of Bob's", "bob", 5, false).await;
    assert_eq!(h.names("/experiments?project=other").await, ["Other of Bob's", "Other of mine"]);
    assert_eq!(h.names("/experiments?mine=true&project=other").await, ["Other of mine"]);
}

#[tokio::test]
async fn an_update_is_saved_and_answered_when_the_callers_runs_cannot_be_read() {
    let h = harness().await;
    stored(&h, ("e-joined", "demo"), "Joined", "bob", 1, false).await;
    failed_run_of(&h, "r-mine", "local", "e-joined", 100.0);
    h.store.fail_experiment_runs_of.store(true, std::sync::atomic::Ordering::SeqCst);

    // The save went through, so it answers 200 with the saved experiment; the caller's attention
    // in it reads as none until the next read.
    let (status, v) = h.put("e-joined", json!({"description": "why"})).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["description"], "why");
    assert_eq!((&v["needs_attention"], &v["mine"]), (&json!(0), &json!(false)));
    // A read that needs it still fails loudly.
    let (status, _) = get_json(h.app(), "/experiments/e-joined").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);

    h.store.fail_experiment_runs_of.store(false, std::sync::atomic::Ordering::SeqCst);
    let (_, v) = get_json(h.app(), "/experiments/e-joined").await;
    assert_eq!(
        (&v["experiment"]["description"], &v["experiment"]["needs_attention"]),
        (&json!("why"), &json!(1))
    );
}

/// `GET uri`: its status, headers and body as text.
async fn get_text(app: axum::Router, uri: &str) -> (StatusCode, axum::http::HeaderMap, String) {
    use tower::ServiceExt;
    let req = axum::http::Request::get(uri).body(axum::body::Body::empty()).unwrap();
    let res = app.oneshot(req).await.unwrap();
    let status = res.status();
    let headers = res.headers().clone();
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
    (status, headers, String::from_utf8(bytes.to_vec()).unwrap())
}

const CSV_HEADER: &str = "Sample,Sample status,Reused,Analysis,Run ID,Analysis status,Pipeline,\
Pipeline version,Engine,Submitted by,Submitted (UTC),Finished (UTC)";

/// The CSV's data lines (after the byte-order mark and the header), each without its CRLF.
fn csv_rows(body: &str) -> Vec<String> {
    let body = body.strip_prefix('\u{feff}').expect("a UTF-8 byte-order mark");
    assert!(body.ends_with("\r\n"), "{body:?}");
    let mut lines = body.trim_end_matches("\r\n").split("\r\n");
    assert_eq!(lines.next(), Some(CSV_HEADER));
    lines.map(str::to_string).collect()
}

/// Put `(run, spec)` pairs in the store.
fn insert_sample_runs(
    h: &Harness,
    runs: Vec<(biffix_backend::store::RunDetailView, biffix_core::event::SampleSpec)>,
) {
    for (d, spec) in runs {
        h.store.sample_specs.lock().unwrap().insert(d.run.run_id.to_string(), spec);
        h.store.insert(d);
    }
}

#[tokio::test]
async fn an_experiment_s_samples_download_as_csv_one_row_per_sample_per_run() {
    let h = harness().await;
    let a = h.create("demo", "Liver, café/1").await;
    // r1 (oldest): two samples, one named like a formula; r2: S2 again, every step of it reused.
    let (mut d1, spec1) = sample_run(
        "r1",
        "succeeded",
        1_759_449_600.0,
        &a,
        &["=cmd|' /C calc'!A0", "S2"],
        "succeeded",
        None,
    );
    d1.run.run_name = Some("batch \"one\"".into());
    d1.run.workflow = Some("v1:h:0".to_string().into());
    d1.run.workflow_name = Some("rnaseq".into());
    d1.run.engine = Some("snakemake".into());
    d1.run.submitter = Some("user:abc".into());
    d1.run.submitter_label = Some("Priya Ng".into());
    d1.run.completed_at = Some(1_759_453_261.0);
    let (mut d2, spec2) =
        sample_run("r2", "failed", 1_759_500_000.0, &a, &["S2"], "succeeded", None);
    for t in &mut d2.tasks {
        t.cached = true;
    }
    d2.run.submitter = Some("user:xyz".into());
    let r3 = sample_run("r3", "failed", 1_759_600_000.0, &a, &["S3"], "failed", None);
    // A run (with a sample) of another experiment is not in it.
    let other = h.create("demo", "Other").await;
    let r9 = sample_run("r9", "succeeded", 40.0, &other, &["S9"], "succeeded", None);
    insert_sample_runs(&h, vec![(d1, spec1), (d2, spec2), r3, r9]);

    let (status, headers, body) =
        get_text(h.app(), &format!("/experiments/{a}/samples.csv?tz=Europe/London")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(headers["content-type"], "text/csv; charset=utf-8");
    assert_eq!(
        headers["content-disposition"],
        "attachment; filename=\"Liver, caf_-1-samples.csv\"; \
         filename*=UTF-8''Liver%2C%20caf%C3%A9-1-samples.csv"
    );
    assert_eq!(
        csv_rows(&body),
        [
            "'=cmd|' /C calc'!A0,Done,no,\"batch \"\"one\"\"\",r1,Succeeded,rnaseq,v1:h:0,\
             snakemake,Priya Ng,2025-10-03 00:00:00,2025-10-03 01:01:01",
            "S2,Done,no,\"batch \"\"one\"\"\",r1,Succeeded,rnaseq,v1:h:0,snakemake,Priya Ng,\
             2025-10-03 00:00:00,2025-10-03 01:01:01",
            // No name: the one the UI derives (no pipeline or sample count known here, so its
            // start time, in the downloader's zone: London, on summer time). No label: the
            // submitter's id. No
            // workflow or end time: empty.
            "S2,Done,yes,\"Oct 3, 2025 15:00\",r2,Failed,,,,user:xyz,2025-10-03 14:00:00,",
            "S3,Failed,no,\"Oct 4, 2025 18:46\",r3,Failed,,,,,2025-10-04 17:46:40,",
        ]
    );

    // An experiment with no runs: the header only. An unknown one: a 404.
    let empty = h.create("demo", "Empty").await;
    let (status, _, body) = get_text(h.app(), &format!("/experiments/{empty}/samples.csv")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(csv_rows(&body), Vec::<String>::new());
    let (status, _, _) = get_text(h.app(), "/experiments/nope/samples.csv").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // A time zone the server does not know: a 400 that names it.
    let (status, _, body) =
        get_text(h.app(), &format!("/experiments/{empty}/samples.csv?tz=UTC%2B1")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body, "unknown time zone `UTC+1` (an IANA name, such as `Europe/Berlin`)");
}

#[tokio::test]
async fn an_experiment_s_samples_csv_covers_every_run_not_only_the_newest_page() {
    let h = harness().await;
    let a = h.create("demo", "Big").await;
    let max = biffix_backend::store::runs::MAX_RUN_PAGE;
    // MAX_RUN_PAGE + 1 runs, oldest first; only the oldest names sample S-old, which the detail
    // (the newest page of runs) leaves out.
    let runs = (0..=max)
        .map(|i| {
            let sample = if i == 0 { "S-old" } else { "S" };
            let id = format!("r{i:03}");
            sample_run(&id, "succeeded", 10.0 + i as f64, &a, &[sample], "succeeded", None)
        })
        .collect();
    insert_sample_runs(&h, runs);
    let (status, _, body) = get_text(h.app(), &format!("/experiments/{a}/samples.csv")).await;
    assert_eq!(status, StatusCode::OK);
    let rows = csv_rows(&body);
    assert_eq!(rows.len() as i64, max + 1);
    // Oldest run first.
    // (Sample, status, reused, analysis name, run id, …)
    let run_of = |row: &str| row.split(',').nth(5).map(str::to_string);
    assert!(rows[0].starts_with("S-old,Done,no,"), "{}", rows[0]);
    assert_eq!(run_of(&rows[0]).as_deref(), Some("r000"), "{}", rows[0]);
    let last = &rows[max as usize];
    assert!(last.starts_with("S,Done,no,"), "{last}");
    assert_eq!(run_of(last), Some(format!("r{max:03}")), "{last}");
}

/// A two-step run (`trim` then `align`) in experiment `exp`, in run state `state`, whose spec names
/// `ids`, with the tasks `(sample, step, task state, cached)`.
fn two_step_run(
    id: &str,
    state: &str,
    submitted_at: f64,
    exp: &str,
    ids: &[&str],
    tasks: &[(&str, &str, &str, bool)],
) -> (biffix_backend::store::RunDetailView, biffix_core::event::SampleSpec) {
    let (mut d, mut spec) = sample_run(id, state, submitted_at, exp, &[], "succeeded", None);
    d.dag_nodes = vec!["trim".into(), "align".into()];
    d.dag_edges = vec![("trim".into(), "align".into())];
    for (sample, step, task_state, cached) in tasks {
        let mut t = testing::task_view(&format!("{step}:sample={sample}"), task_state, None, None);
        t.group = Some((*step).into());
        t.cached = *cached;
        d.tasks.push(t);
    }
    spec.ids = ids.iter().map(std::string::ToString::to_string).collect();
    (d, spec)
}

#[tokio::test]
async fn a_sample_reads_reused_only_when_it_finished_with_every_step_reused() {
    let h = harness().await;
    let a = h.create("demo", "Resumes").await;
    let runs = vec![
        // Finished, every step reused: yes. Finished with one step run again: no.
        two_step_run(
            "r1",
            "succeeded",
            10.0,
            &a,
            &["all", "part"],
            &[
                ("all", "trim", "succeeded", true),
                ("all", "align", "succeeded", true),
                ("part", "trim", "succeeded", true),
                ("part", "align", "succeeded", false),
            ],
        ),
        // Still going, its first step reused and the next not started: no.
        two_step_run("r2", "running", 20.0, &a, &["live"], &[("live", "trim", "succeeded", true)]),
        // Stopped by the run's failure after a reused first step: no. A sheet sample no task
        // named: no.
        two_step_run(
            "r3",
            "failed",
            30.0,
            &a,
            &["cut", "none", "bad"],
            &[("cut", "trim", "succeeded", true), ("bad", "trim", "failed", false)],
        ),
    ];
    insert_sample_runs(&h, runs);
    let (status, _, body) = get_text(h.app(), &format!("/experiments/{a}/samples.csv")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let rows: Vec<String> = csv_rows(&body)
        .iter()
        .map(|r| r.split(',').take(3).collect::<Vec<_>>().join(","))
        .collect();
    assert_eq!(
        rows,
        [
            "all,Done,yes",
            "part,Done,no",
            "live,Running,no",
            "cut,Stopped,no",
            "none,Stopped,no",
            "bad,Failed,no",
        ]
    );
}

#[tokio::test]
async fn an_experiment_with_too_many_runs_for_one_download_is_a_413_saying_what_to_do() {
    let h = harness().await;
    let a = h.create("demo", "Huge").await;
    let max = 1_000;
    for i in 0..=max {
        let mut d = run_in(&format!("r{i:04}"), "succeeded", "demo", i as f64);
        d.run.experiment_id = Some(a.clone());
        h.store.insert(d);
    }
    let (status, headers, body) = get_text(h.app(), &format!("/experiments/{a}/samples.csv")).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    assert!(headers["content-type"].to_str().unwrap().starts_with("text/plain"));
    assert_eq!(
        body,
        "This experiment has 1001 runs (analyses), more than the 1000 one samples download can \
         cover. Split the experiment into smaller ones (move some of its runs into another \
         experiment) and download the samples of each."
    );
}
