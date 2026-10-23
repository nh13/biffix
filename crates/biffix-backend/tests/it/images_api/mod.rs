//! Workflow images through the HTTP API: registration pins every declared image to a digest and
//! starts its import, a version runs only once its images are imported (and suit the engine), the
//! sweep records finished imports and retries failed ones, and two versions naming the same bytes
//! share one import. In-memory doubles: builds are set on the registry as the ingest fold would;
//! the engine runtime is ready at once (these tests are about images).

use std::collections::BTreeMap;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use jsonwebtoken::{encode, Algorithm, DecodingKey, EncodingKey, Header};
use serde_json::{json, Value};
use tower::ServiceExt;

use biffix_backend::auth::rbac::{Rbac, Role};
use biffix_backend::testing::{self, FakeCloud, MemEventSink, MemRegistry, TestState};
use biffix_backend::{api, AppState, AuthContext, DeployConfig};
use biffix_core::builds::{BuildProjection, BuildState};
use biffix_core::event::EventData;
use biffix_core::time::Timestamp;

mod builds;
mod checks;
mod imports;
mod reregister;
mod runtime;

const SECRET: &[u8] = b"itsasecret";
const ISS: &str = "https://cognito-idp.us-east-1.amazonaws.com/pool";
const AUD: &str = "biffix-web";
const FASTQC: &str = "quay.io/biocontainers/fastqc:0.12.1--hdfd78af_0";

struct Harness {
    state: AppState,
    registry: Arc<MemRegistry>,
    events: Arc<MemEventSink>,
    cloud: Arc<FakeCloud>,
}

fn harness() -> Harness {
    let rbac = Rbac::new(BTreeMap::from([
        ("operators".to_string(), Role::Operator),
        ("developers".to_string(), Role::Developer),
    ]));
    let auth = AuthContext {
        verifier: biffix_backend::auth::JwtVerifier::static_key(
            DecodingKey::from_secret(SECRET),
            Algorithm::HS256,
        ),
        issuer: ISS.into(),
        audiences: vec![AUD.into()],
        rbac: Arc::new(rbac),
    };
    let registry = Arc::new(MemRegistry::new());
    registry.auto_images.store(false, Ordering::SeqCst);
    let events = Arc::new(MemEventSink::new());
    let cloud = Arc::new(FakeCloud::new());
    let state = TestState::new()
        .with_registry(registry.clone())
        .with_events(events.clone())
        .with_cloud(cloud.clone())
        .with_auth(auth)
        .with_deploy(DeployConfig {
            coordinator_queue: "coordinator".into(),
            queue_classes: vec!["cpu-spot".into()],
            worker_image_repo: testing::FAKE_WORKFLOW_REPO.into(),
            queue_capacities: Default::default(),
            retention_days: None,
            ..Default::default()
        })
        .with_projects(testing::bucket_project("test-bucket"))
        .build();
    Harness { state, registry, events, cloud }
}

fn token(group: &str) -> String {
    let exp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() + 3600;
    let claims = json!({"sub": "u", "cognito:groups": [group], "iss": ISS, "aud": AUD, "exp": exp});
    encode(&Header::new(Algorithm::HS256), &claims, &EncodingKey::from_secret(SECRET)).unwrap()
}

async fn call(
    state: &AppState,
    method: &str,
    uri: &str,
    group: &str,
    body: Option<&Value>,
) -> (StatusCode, Value) {
    let req = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {}", token(group)))
        .body(body.map_or_else(Body::empty, |b| Body::from(b.to_string())))
        .unwrap();
    let res = api::router(state.clone()).oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let body = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into()));
    (status, body)
}

/// A Nextflow workflow with a default image and a named one.
fn register_body(sha: &str) -> Value {
    json!({
        "name": "nf",
        "project": "p",
        "source": {"kind": "git", "repo": "github.com/org/nf", "git_ref": "main"},
        "git_sha": sha,
        "source_content_hash": "hash",
        "registration_inputs": {"source_mode": "git", "cli_version": "0.0.0"},
        "engine": "nextflow",
        "engine_version": "25.10.4",
        "entrypoint": "main.nf",
        "image": "ubuntu:24.04",
        "images": {"fastqc": {"ref": FASTQC}},
        "form_schema": {"fields": [], "warnings": []},
        "queue_classes": ["cpu-spot"]
    })
}

async fn register(h: &Harness, body: &Value) -> (StatusCode, Value) {
    call(&h.state, "POST", "/workflows", "developers", Some(body)).await
}

async fn detail(h: &Harness, id: &str) -> Value {
    let (status, body) =
        call(&h.state, "GET", &format!("/workflows/{id}"), "operators", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body
}

fn content_key(h: &Harness, id: &str, name: &str) -> String {
    h.registry.versions.lock().unwrap()[id]
        .images
        .iter()
        .find(|i| i.name == name)
        .unwrap()
        .content_key
        .clone()
}

/// A build of `subject` in `state` after `builds` builds, as the ingest fold would record it, with
/// no outputs, reason or logs. A build still in progress was heard from just now, so the sweep
/// follows it on its normal path (not the stale-build backstop, which asks CodeBuild); a finished
/// one reports when it ended, long ago. Callers override what their build reports with
/// struct-update syntax and store it with [`put_build`].
fn projection(subject: &str, state: BuildState, builds: u32) -> BuildProjection {
    let updated = if state.is_terminal() {
        Timestamp::from_micros(1)
    } else {
        Timestamp::from_micros(
            SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_micros() as i64
        )
    };
    BuildProjection {
        subject: subject.to_string(),
        state,
        build_arn: format!("arn:{subject}:{builds}"),
        outputs: BTreeMap::new(),
        reason: None,
        logs_url: None,
        request: Value::Null,
        builds,
        updated,
    }
}

fn put_build(h: &Harness, build: BuildProjection) {
    h.registry.builds.lock().unwrap().insert(build.subject.clone(), build);
}

fn output_map(outputs: &[(&str, &str)]) -> BTreeMap<String, String> {
    outputs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

fn set_import(h: &Harness, key: &str, state: BuildState, builds: u32, outputs: &[(&str, &str)]) {
    let subject = biffix_workflow::images::subject(key);
    put_build(
        h,
        BuildProjection {
            build_arn: format!("arn:import:{builds}"),
            outputs: output_map(outputs),
            reason: (state == BuildState::Failed).then(|| "FAILED: BUILD: MANIFEST_UNKNOWN".into()),
            logs_url: Some("https://logs/import".into()),
            request: json!({"source": FASTQC}),
            ..projection(&subject, state, builds)
        },
    );
}

/// `images::sweep`, which must have met no error: it counts a failed step and carries on, so a
/// test asserting that the sweep did nothing would otherwise pass when a step failed instead.
async fn sweep_ok(h: &Harness) -> biffix_backend::images::ImageSweepReport {
    let report = biffix_backend::images::sweep(&h.state).await.unwrap();
    assert_eq!(report.errors, 0, "{report:?}");
    report
}

fn imported(caps: &str) -> Vec<(&'static str, String)> {
    vec![
        ("IMAGE", format!("{}@sha256:imp", testing::FAKE_WORKFLOW_REPO)),
        ("IMAGE_DIGEST", "sha256:imp".into()),
        ("ARCHS", "amd64".into()),
        ("CAPABILITIES", caps.into()),
        ("KEEP_TAG", "biffix-keep-imp".into()),
        ("ARCHIVED", "1".into()),
    ]
}

fn outputs<'a>(v: &'a [(&'static str, String)]) -> Vec<(&'static str, &'a str)> {
    v.iter().map(|(k, v)| (*k, v.as_str())).collect()
}

/// A refused registration left nothing behind: no version, no import, nothing on the log.
fn assert_nothing_registered(h: &Harness) {
    assert!(h.registry.versions.lock().unwrap().is_empty());
    assert!(h.cloud.image_imports.lock().unwrap().is_empty());
    assert!(h.events.events.lock().unwrap().is_empty());
}

const SAMTOOLS: &str = "quay.io/biocontainers/samtools:1.21--h50ea8bc_0";

/// A Snakemake workflow whose default image is `image` (`runtime: own` when `own`).
fn snakemake_body(sha: &str, image: &str, own: bool) -> Value {
    let mut default = json!({"ref": image});
    if own {
        default["runtime"] = json!("own");
    }
    json!({
        "name": "sm",
        "project": "p",
        "source": {"kind": "git", "repo": "github.com/org/sm", "git_ref": "main"},
        "git_sha": sha,
        "source_content_hash": "hash",
        "registration_inputs": {"source_mode": "git", "cli_version": "0.0.0"},
        "engine": "snakemake",
        "engine_version": "9.27.0",
        "entrypoint": "Snakefile",
        "images": {"default": default},
        "form_schema": {"fields": [], "warnings": []},
        "queue_classes": ["cpu-spot"]
    })
}

fn runtime_key(h: &Harness, id: &str) -> String {
    h.registry.versions.lock().unwrap()[id].version.runtime_key.clone()
}

/// The runtime build of `runtime_key` on the fake base succeeded, reporting its layer images.
fn set_runtime_layer(h: &Harness, runtime_key: &str) {
    let subject =
        biffix_workflow::runtime::coordinator_subject(runtime_key, testing::FAKE_BASE_DIGEST);
    put_build(
        h,
        BuildProjection {
            build_arn: "arn:runtime:1".into(),
            outputs: output_map(&[
                ("VARIANT_IMAGE", "coord@sha256:variant"),
                ("RUNTIME_IMAGE_AMD64", "coord@sha256:layer-amd64"),
                ("RUNTIME_IMAGE_ARM64", "coord@sha256:layer-arm64"),
            ]),
            request: json!({}),
            ..projection(&subject, BuildState::Succeeded, 1)
        },
    );
}

const TREE: &str = "a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1";
const TARBALL_SHA: &str = "b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2";

fn staged_context() -> String {
    format!("s3://test-bucket/workflow-sources/abc:hash/contexts/{TREE}.tar.gz")
}

/// A Nextflow workflow whose default image is built from a pixi environment.
fn pixi_body(sha: &str) -> Value {
    json!({
        "name": "nf-pixi",
        "project": "p",
        "source": {"kind": "git", "repo": "github.com/org/nf", "git_ref": "main"},
        "git_sha": sha,
        "source_content_hash": "hash",
        "registration_inputs": {"source_mode": "git", "cli_version": "0.0.0"},
        "engine": "nextflow",
        "engine_version": "25.10.4",
        "entrypoint": "main.nf",
        "images": {"default": {"build": {
            "kind": "pixi",
            "context": "env",
            "context_uri": staged_context(),
            "context_sha256": TARBALL_SHA,
            "tree_hash": TREE,
            "archs": ["arm64", "amd64"],
        }}},
        "form_schema": {"fields": [], "warnings": []},
        "queue_classes": ["cpu-spot"]
    })
}

fn set_build(h: &Harness, subject: &str, state: BuildState, builds: u32, outputs: &[(&str, &str)]) {
    put_build(
        h,
        BuildProjection {
            outputs: output_map(outputs),
            reason: (state == BuildState::Failed).then(|| "FAILED: BUILD: exit status 1".into()),
            logs_url: Some(format!("https://logs/{subject}")),
            request: json!({"source": "pixi:env"}),
            ..projection(subject, state, builds)
        },
    );
}

async fn reregister(h: &Harness, id: &str, body: Value) -> (StatusCode, Value) {
    call(&h.state, "POST", &format!("/workflows/{id}/reregister"), "developers", Some(&body)).await
}

async fn dry_run(h: &Harness, id: &str) -> Value {
    let (status, plan) = reregister(h, id, json!({"dry_run": true})).await;
    assert_eq!(status, StatusCode::OK, "{plan}");
    plan
}
