//! HTTP tests for the run reads (in-memory doubles, auth disabled, no network): the run list and
//! detail (DAG, lifecycle, resume, output prefix, preparing images, launch estimate), a run's
//! tasks (paging, filters, the POST query form), task attempts, task and coordinator logs, its
//! outputs (listing, paging, sizes, preview, view and download links), reports and QC stats, and
//! its failure summary; the page-size and preview caps; plus the workflow-version read.
//! Readiness, status, settings, usage and the task list's parameter errors are `rest_api.rs`'s.

use std::sync::Arc;

use axum::http::StatusCode;

use biffix_backend::api;
use biffix_backend::register::workflow::{WorkflowSource, WorkflowVersion};
use biffix_backend::store::registry::{RegisteredVersion, RegistryStore};
use biffix_backend::testing::{
    self, get_json, post_json, task_view, FakeCloud, MemRegistry, MemStore, TestState,
};
use biffix_backend::AppState;
use biffix_workflow::engines::Engine;

mod caps;
mod estimates;
mod failure_summary;
mod logs;
mod outputs;
mod preparing_images;
mod reports;
mod runs;

fn state_with_one_run() -> AppState {
    let store = MemStore::new();
    store.insert(testing::run_detail("r1", "submitted"));
    TestState::new().with_store(Arc::new(store)).build()
}

/// State seeded with one registered workflow version carrying a form_schema.
async fn state_with_one_version() -> AppState {
    let registry = MemRegistry::new();
    let form_schema = serde_json::json!({
        "fields": [
            {"name": "reads", "widget": "s3_file", "required": true,
             "is_output": false, "is_secret": false, "is_path_input": true}
        ],
        "warnings": []
    });
    registry
        .put_version(&RegisteredVersion {
            name: "genomics".into(),
            project: "genomics".into(),
            source: WorkflowSource::S3 {
                uri: "s3://acme-genomics/biffix/smoke/".into(),
                sha256: None,
            },
            version: WorkflowVersion {
                id: "abc:hash".into(),
                git_sha: "abc".into(),
                source_content_hash: "hash".into(),
                image_digest: "sha256:1a2b".into(),
                image_repository: None,
                image_registry_id: None,
                image_region: None,
                engine: Engine::Snakemake,
                engine_version: "9.27".into(),
                entrypoint: "Snakefile".into(),
                target: None,
                form_schema,
                queue_classes: vec!["c7i".into()],
                about_markdown: None,
                runtime_key: testing::runtime_fields("snakemake", "9.27.0").0,
                runtime_recipe: testing::runtime_fields("snakemake", "9.27.0").1,
                build: 0,
            },
            created_by: Some("user:alice".into()),
            created_by_label: Some("alice@lab.example.org".into()),
            created_at_micros: 0,
            registration_inputs: Default::default(),
            registration_inputs_hash: String::new(),
            images: Vec::new(),
        })
        .await
        .unwrap();
    TestState::new().with_registry(Arc::new(registry)).build()
}

fn state_with_tasks() -> AppState {
    let store = MemStore::new();
    let mut detail = testing::run_detail("r1", "running");
    detail.tasks = vec![
        task_view("started", "running", Some("stream-A"), Some("job-A")),
        task_view("waiting", "queued", None, Some("job-B")),
        task_view("gone", "failed", Some("missing"), Some("job-C")),
    ];
    store.insert(detail);
    TestState::new().with_store(Arc::new(store)).build()
}

fn attempt_view(job: &str, attempt: i64, state: &str, log_stream: &str) -> biffix_api::TaskAttempt {
    biffix_api::TaskAttempt {
        batch_job_id: job.into(),
        attempt: Some(attempt),
        state: state.into(),
        exit_code: None,
        termination_category: None,
        status_reason: None,
        log_stream: Some(log_stream.into()),
        queue: None,
        instance_id: None,
        started_at: None,
        finished_at: None,
        display: false,
        host: None,
        spot_warned_at: None,
        requested_vcpus: None,
        requested_memory_mib: None,
        cost: None,
    }
}

fn state_with_run_detail(detail: biffix_backend::store::RunDetailView) -> AppState {
    let store = MemStore::new();
    store.insert(detail);
    TestState::new().with_store(Arc::new(store)).with_projects(testing::bucket_project("b")).build()
}

use testing::output_row;

/// Run `r1` (output prefix `s3://proj/biffix/runs/r1/`) with outputs inside its prefix, a log, a
/// work-dir intermediate, one outside the prefix, one in a foreign bucket and one under the staged
/// workflow-source prefix; run `r2` with its own output.
fn state_with_outputs() -> AppState {
    let store = MemStore::new();
    let mut r1 = testing::run_detail("r1", "succeeded");
    r1.output_prefix = Some("s3://proj/biffix/runs/r1/".into());
    store.insert(r1);
    store.insert(testing::run_detail("r2", "succeeded"));
    store.insert_outputs(
        "r1",
        vec![
            output_row("r1", "s3://proj/biffix/runs/r1/results/a.bam", Some("align"), "output"),
            output_row("r1", "s3://proj/biffix/runs/r1/results/b.bam", Some("align2"), "output"),
            output_row("r1", "s3://proj/biffix/runs/r1/logs/a.log", Some("align"), "log"),
            output_row(
                "r1",
                "s3://proj/biffix/runs/r1/work/ab/cd/x",
                Some("align"),
                "intermediate",
            ),
            output_row("r1", "s3://proj/runs/sha/summary.tsv", None, "published"),
            output_row("r1", "s3://other/x.txt", None, "output"),
            output_row("r1", "s3://proj/workflow-sources/v1/source.tar.gz", None, "output"),
        ],
    );
    store.insert_outputs(
        "r2",
        vec![output_row("r2", "s3://proj/biffix/runs/r2/z.txt", Some("t"), "output")],
    );
    let mut state = state_with_one_run();
    state.store = Arc::new(store);
    state.projects = testing::bucket_project("proj");
    state
}

/// [`state_with_outputs`] with S3 holding `results/a.bam` (10 bytes) and `summary.tsv` — but not
/// `b.bam` or `a.log` — plus the fake cloud, to count HEADs.
fn state_with_objects() -> (AppState, Arc<FakeCloud>) {
    let cloud = Arc::new(FakeCloud::new());
    {
        let mut objects = cloud.objects.lock().unwrap();
        objects.insert("s3://proj/biffix/runs/r1/results/a.bam".into(), vec![0u8; 10]);
        objects.insert("s3://proj/runs/sha/summary.tsv".into(), b"sample\treads\nA\t10\n".to_vec());
    }
    let mut state = state_with_outputs();
    state.cloud = cloud.clone();
    (state, cloud)
}

fn failed_task(
    key: &str,
    group: &str,
    finished_at: f64,
    exit: i64,
    stream: &str,
) -> biffix_api::Task {
    let mut t = task_view(key, "failed", Some(stream), None);
    t.group = Some(group.into());
    t.finished_at = Some(finished_at);
    t.exit_code = Some(exit);
    t.termination_category = Some("app_error".into());
    t
}
