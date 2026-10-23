//! Per-project routing on a deploy of two projects (`testing::two_projects`): A
//! (`genomics`, the default) and B (`imported`, its own bucket, run role,
//! coordinator template and task log group, uploads off). Every request takes its project's
//! context: submit and launch the version's, registration and the data browser the one named,
//! readers the run's (the bucket of its recorded prefix; a run that recorded none predates
//! routing and ran in the default project).

use std::sync::Arc;

use axum::http::StatusCode;
use serde_json::{json, Value};

use biffix_backend::runs::launch::LaunchPlan;
use biffix_backend::store::outputs::OutputRow;
use biffix_backend::store::pending::PendingLaunch;
use biffix_backend::testing::{
    self, get_json, post_json, FakeCloud, MemEventSink, MemRegistry, MemStore, TestState, BUCKET_A,
    BUCKET_B, LOG_GROUP_B, PROJECT_A, PROJECT_B, RUN_ROLE_B, TEMPLATE_B,
};
use biffix_backend::{api, AppState, DeployConfig};
use biffix_core::event::{EventData, InputTable};
use biffix_core::ids::RunId;

mod data;
mod launch_sweep;
mod readers;
mod registration;
mod submit;

/// The two-project deploy over doubles the test keeps handles on, able to launch (a capability
/// secret) and with the coordinator queue and queue-class menu registration and submit check.
struct Harness {
    state: AppState,
    store: Arc<MemStore>,
    registry: Arc<MemRegistry>,
    events: Arc<MemEventSink>,
    cloud: Arc<FakeCloud>,
}

fn harness() -> Harness {
    let store = Arc::new(MemStore::new());
    let registry = Arc::new(MemRegistry::new());
    let events = Arc::new(MemEventSink::new());
    let cloud = Arc::new(FakeCloud::new());
    let state = TestState::new()
        .with_store(store.clone())
        .with_registry(registry.clone())
        .with_events(events.clone())
        .with_cloud(cloud.clone())
        .with_deploy(DeployConfig {
            coordinator_queue: "coordinator".into(),
            queue_classes: vec!["cpu-spot".into()],
            worker_image_repo: testing::FAKE_WORKFLOW_REPO.into(),
            ..Default::default()
        })
        .with_projects(testing::fixed_projects(testing::two_projects()))
        .build();
    Harness { state, store, registry, events, cloud }
}

impl Harness {
    fn app(&self) -> axum::Router {
        api::router(self.state.clone())
    }

    async fn post(&self, uri: &str, body: Value) -> (StatusCode, Value) {
        post_json(self.app(), uri, body).await
    }

    async fn get(&self, uri: &str) -> (StatusCode, Value) {
        get_json(self.app(), uri).await
    }

    /// Register workflow `name` in `project` (from a git source); returns the version id.
    async fn register(&self, project: &str, name: &str) -> String {
        let (status, v) = self.post("/workflows", register_body(project, name)).await;
        assert!(status.is_success(), "register {project}/{name}: {status} {v}");
        v["id"].as_str().unwrap().to_string()
    }

    /// The recorded `RunSubmitted` anchor (the first).
    fn anchor(&self) -> biffix_core::event::RunSubmitted {
        self.events
            .events
            .lock()
            .unwrap()
            .iter()
            .find_map(|e| match &e.data {
                EventData::RunSubmitted(anchor) => Some((**anchor).clone()),
                _ => None,
            })
            .expect("anchor recorded")
    }

    fn run_failure(&self) -> Option<String> {
        self.events.events.lock().unwrap().iter().find_map(|e| match &e.data {
            EventData::RunFailed { reason, .. } => Some(reason.clone().unwrap_or_default()),
            _ => None,
        })
    }
}

fn register_body(project: &str, name: &str) -> Value {
    json!({
        "name": name,
        "project": project,
        "source": {"kind": "git", "repo": format!("github.com/example/{name}"), "git_ref": "main"},
        "git_sha": "abc",
        "source_content_hash": format!("hash-{name}"),
        "registration_inputs": {"source_mode": "git", "cli_version": "0.0.0"},
        "image": format!("sha256:{}", "a".repeat(64)),
        "engine_version": "9.3.0",
        "entrypoint": "workflow/Snakefile",
        "form_schema": {
            "fields": [
                {"name": "reads", "widget": "s3_file", "required": true,
                 "is_output": false, "is_secret": false, "is_path_input": true},
                {"name": "outdir", "widget": "s3_dir", "required": true,
                 "is_output": true, "is_secret": false, "is_path_input": false}
            ],
            "warnings": []
        },
        "queue_classes": ["cpu-spot"]
    })
}

fn submit_body(version: &str, bucket: &str) -> Value {
    json!({
        "workflow_version_id": version,
        "params": {"reads": format!("s3://{bucket}/in.fastq"), "outdir": format!("s3://{bucket}/out/")},
    })
}

fn output_row(run: &str, uri: &str) -> OutputRow {
    testing::output_row(run, uri, Some("t"), "output")
}

fn output_id(run: &str, uri: &str) -> String {
    biffix_core::reducer::output_id(run, uri)
}

/// The `(s3_uri, servable)` of every output on an outputs page, sorted.
fn servable_outputs(page: &Value) -> Vec<(String, bool)> {
    let mut got: Vec<(String, bool)> = page["outputs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| (o["s3_uri"].as_str().unwrap().to_string(), o["servable"].as_bool().unwrap()))
        .collect();
    got.sort();
    got
}
