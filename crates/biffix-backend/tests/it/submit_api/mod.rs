//! HTTP register + submit endpoint tests via tower oneshot.
//! Exercises RBAC gating and the register→submit flow with in-memory doubles (no AWS, no DB).

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use jsonwebtoken::{encode, Algorithm, DecodingKey, EncodingKey, Header};
use serde_json::{json, Value};
use tower::ServiceExt;

use biffix_backend::auth::rbac::{Rbac, Role};
use biffix_backend::store::StoreError;
use biffix_backend::testing::{self, FakeCloud, MemEventSink, MemRegistry, MemStore, TestState};
use biffix_backend::{api, AppState, AuthContext, DeployConfig};

mod catalog;
mod checks;
mod experiments;
mod images;
mod references;
mod register;
mod resume;
mod samplesheets;
mod submit;
mod try_again;

const SECRET: &[u8] = b"itsasecret";
const ISS: &str = "https://cognito-idp.us-east-1.amazonaws.com/pool";
const AUD: &str = "biffix-web";

fn state() -> (AppState, Arc<MemEventSink>, Arc<FakeCloud>) {
    state_with_store(MemStore::new())
}

fn state_with_store(store: MemStore) -> (AppState, Arc<MemEventSink>, Arc<FakeCloud>) {
    let mut m = BTreeMap::new();
    m.insert("viewers".to_string(), Role::Viewer);
    m.insert("operators".to_string(), Role::Operator);
    m.insert("developers".to_string(), Role::Developer);
    m.insert("admins".to_string(), Role::Admin);
    let auth = AuthContext {
        verifier: biffix_backend::auth::JwtVerifier::static_key(
            DecodingKey::from_secret(SECRET),
            Algorithm::HS256,
        ),
        issuer: ISS.into(),
        audiences: vec![AUD.into()],
        rbac: Arc::new(Rbac::new(m)),
    };
    let events = Arc::new(MemEventSink::new());
    let cloud = Arc::new(FakeCloud::new());
    let state = TestState::new()
        .with_store(Arc::new(store))
        .with_events(events.clone())
        .with_cloud(cloud.clone())
        .with_auth(auth)
        .with_deploy(DeployConfig {
            coordinator_queue: "coordinator".into(),
            queue_classes: vec!["cpu-spot".into()],
            worker_image_repo: "111122223333.dkr.ecr.us-east-1.amazonaws.com/biffix-workers".into(),
            queue_capacities: Default::default(),
            retention_days: None,
            ..Default::default()
        })
        .with_projects(testing::bucket_project("test-bucket"))
        .build();
    (state, events, cloud)
}

fn token(group: &str) -> String {
    let claims = json!({
        "sub": "u", "cognito:groups": [group], "iss": ISS, "aud": AUD,
        "exp": biffix_core::clock::epoch_secs_u64() + 3600
    });
    encode(&Header::new(Algorithm::HS256), &claims, &EncodingKey::from_secret(SECRET)).unwrap()
}

fn register_body() -> Value {
    json!({
        "name": "genomics",
        "project": "genomics",
        "source": {"kind": "git", "repo": "github.com/example/genomics", "git_ref": "main"},
        "git_sha": "abc",
        "source_content_hash": "hash",
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

/// A `method` request to `uri`, as a member of `group` (`None`: unauthenticated), with a JSON body.
fn request(method: &str, uri: &str, group: Option<&str>, body: Option<&Value>) -> Request<Body> {
    let mut builder =
        Request::builder().method(method).uri(uri).header("content-type", "application/json");
    if let Some(g) = group {
        builder = builder.header("authorization", format!("Bearer {}", token(g)));
    }
    builder.body(body.map_or_else(Body::empty, |b| Body::from(b.to_string()))).unwrap()
}

fn post(uri: &str, group: Option<&str>, body: &Value) -> Request<Body> {
    request("POST", uri, group, Some(body))
}

fn get(uri: &str, group: &str) -> Request<Body> {
    request("GET", uri, Some(group), None)
}

/// Read a JSON response body.
async fn json_body(res: axum::response::Response) -> Value {
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

/// Read a response body as text.
async fn text_body(res: axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
    String::from_utf8_lossy(&bytes).into_owned()
}

/// The recorded `run_submitted` anchor.
fn recorded_anchor(events: &MemEventSink) -> biffix_core::event::RunSubmitted {
    events
        .events
        .lock()
        .unwrap()
        .iter()
        .find_map(|e| match &e.data {
            biffix_core::event::EventData::RunSubmitted(anchor) => Some((**anchor).clone()),
            _ => None,
        })
        .expect("anchor recorded")
}

/// POST `body` to `/workflows` (or `/references`) as `group`, asserting the setup write succeeded.
async fn setup(app: &axum::Router, uri: &str, group: &str, body: &Value) {
    let res = app.clone().oneshot(post(uri, Some(group), body)).await.unwrap();
    let status = res.status();
    assert!(status.is_success(), "setup {uri}: {status} {}", text_body(res).await);
}

/// A finished Nextflow run of `genomics/genomics` with a recorded session and prefix —
/// resumable; each rejection test perturbs one fact.
fn resumable_prior(run_id: &str) -> biffix_backend::store::RunDetailView {
    let mut prior = testing::run_detail(run_id, "failed");
    prior.run.engine = Some("nextflow".into());
    prior.run.workflow_name = Some("genomics".into());
    prior.run.project = Some("genomics".into());
    prior.engine_session_id = Some("sess-prior".into());
    prior.output_prefix = Some(format!("s3://test-bucket/biffix/runs/{run_id}/"));
    prior
}

fn nextflow_register_body() -> Value {
    let mut b = register_body();
    b["engine"] = json!("nextflow");
    b["engine_version"] = json!("25.10.4");
    b["entrypoint"] = json!("main.nf");
    b
}

fn resume_body(prior: &str, extra: Value) -> Value {
    let mut b = json!({
        "workflow_version_id": "abc:hash:0",
        "params": {"reads": "s3://b/in.fastq", "outdir": "s3://b/out/"},
        "resume_from": prior,
    });
    b.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
    b
}

/// Register `register` and submit `submit` against a store holding `prior`; the submit's status,
/// body text, and what reached the cloud/event log.
async fn resume_submit(
    prior: Option<biffix_backend::store::RunDetailView>,
    register: Value,
    submit: Value,
    group: &str,
) -> (StatusCode, String, Arc<FakeCloud>, Arc<MemEventSink>) {
    let store = MemStore::new();
    if let Some(p) = prior {
        store.insert(p);
    }
    let (state, events, cloud) = state_with_store(store);
    let app = api::router(state);
    setup(&app, "/workflows", "developers", &register).await;
    let res = app.oneshot(post("/runs", Some(group), &submit)).await.unwrap();
    let status = res.status();
    (status, text_body(res).await, cloud, events)
}

/// A version whose `input` param is a samplesheet with declared columns.
fn sheet_register_body() -> Value {
    let mut body = register_body();
    body["git_sha"] = json!("sheet");
    body["form_schema"] = json!({
        "fields": [
            {"name": "input", "widget": "samplesheet", "required": true,
             "is_output": false, "is_secret": false, "is_path_input": true,
             "samplesheet": {"columns": [
                 {"name": "sample", "role": "sample_id", "required": true},
                 {"name": "fastq_1", "role": "read1", "required": true, "file": true},
                 {"name": "fastq_2", "role": "read2", "file": true}
             ]}}
        ],
        "warnings": []
    });
    body
}

fn bundle_body(id: &str, uris: Value) -> Value {
    json!({"id": id, "kind": "assembly", "display_name": "Human GRCh38", "uris": uris})
}

/// An event sink whose store is down for any batch holding a `fail_on` event (recording a run's
/// anchor, or its `run_preparing`); everything else, reads included, is the inner sink's.
struct FailingSink {
    inner: Arc<MemEventSink>,
    fail_on: biffix_core::event::Kind,
}

impl FailingSink {
    fn new(inner: Arc<MemEventSink>, fail_on: biffix_core::event::Kind) -> Self {
        Self { inner, fail_on }
    }
}

#[async_trait::async_trait]
impl biffix_backend::store::EventSink for FailingSink {
    async fn record_all(&self, evs: &[biffix_core::event::RunEvent]) -> Result<usize, StoreError> {
        if evs.iter().any(|ev| ev.kind() == self.fail_on) {
            return Err(StoreError::Unavailable("database down".into()));
        }
        self.inner.record_all(evs).await
    }

    async fn job_claims(
        &self,
        job_ids: &[biffix_core::ids::BatchJobId],
    ) -> Result<Vec<(String, biffix_core::ids::RunId)>, StoreError> {
        self.inner.job_claims(job_ids).await
    }
}

const SAMTOOLS: &str = "quay.io/biocontainers/samtools:1.21--h50ea8bc_0";

/// A state whose registry the test can reach, with `register_body()` registered.
async fn images_app(
) -> (AppState, axum::Router, Arc<MemRegistry>, Arc<MemEventSink>, Arc<FakeCloud>) {
    images_app_with(MemStore::new(), register_body()).await
}

/// [`images_app`] over `store`, with `register` registered.
async fn images_app_with(
    store: MemStore,
    register: Value,
) -> (AppState, axum::Router, Arc<MemRegistry>, Arc<MemEventSink>, Arc<FakeCloud>) {
    let (mut state, events, cloud) = state_with_store(store);
    let registry = Arc::new(MemRegistry::new());
    state.registry = registry.clone();
    let app = api::router(state.clone());
    setup(&app, "/workflows", "developers", &register).await;
    (state, app, registry, events, cloud)
}

fn images_submit(images: Value) -> Value {
    json!({"workflow_version_id": "abc:hash:0",
           "params": {"reads": "s3://b/in.fastq", "outdir": "s3://b/out/"},
           "images": images})
}

/// A register body for version `git_sha` of workflow `name`, its form carrying `form` merged over
/// the default one and its About page `about`.
fn catalog_body(name: &str, git_sha: &str, form: Value, about: Option<&str>) -> Value {
    let mut body = register_body();
    body["name"] = json!(name);
    body["git_sha"] = json!(git_sha);
    for (k, v) in form.as_object().unwrap() {
        body["form_schema"][k] = v.clone();
    }
    if let Some(about) = about {
        body["about_markdown"] = json!(about);
    }
    body
}
