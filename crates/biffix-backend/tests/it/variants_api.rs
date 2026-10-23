//! Coordinator variants through the HTTP API: registration starts the build of a version's pinned
//! engine runtime, submit launches only on a built variant (the current base's, else an older
//! base's), and the sweep finishes and retries builds. In-memory doubles: builds are set on the
//! registry as the ingest fold would.

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
use biffix_backend::testing::{
    self, fake_variant, runtime_fields, FakeCloud, MemEventSink, MemRegistry, TestState,
    FAKE_BASE_DIGEST,
};
use biffix_backend::{api, AppState, AuthContext, DeployConfig};
use biffix_core::builds::{BuildProjection, BuildState};
use biffix_core::event::EventData;
use biffix_core::time::Timestamp;
use biffix_workflow::runtime::coordinator_subject;

const SECRET: &[u8] = b"itsasecret";
const ISS: &str = "https://cognito-idp.us-east-1.amazonaws.com/pool";
const AUD: &str = "biffix-web";

struct Harness {
    state: AppState,
    registry: Arc<MemRegistry>,
    events: Arc<MemEventSink>,
    cloud: Arc<FakeCloud>,
}

/// A harness whose registry does NOT give registered versions a ready variant, so the tests see
/// the builds registration and submit start.
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
    registry.auto_variants.store(false, Ordering::SeqCst);
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

fn request(method: &str, uri: &str, group: &str, body: Option<&Value>) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {}", token(group)))
        .body(body.map_or_else(Body::empty, |b| Body::from(b.to_string())))
        .unwrap()
}

async fn call(state: &AppState, req: Request<Body>) -> (StatusCode, Value) {
    let res = api::router(state.clone()).oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let body = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into()));
    (status, body)
}

/// A Snakemake workflow with no image of its own (its tasks run on the coordinator variant).
fn register_body() -> Value {
    json!({
        "name": "smoke",
        "project": "p",
        "source": {"kind": "git", "repo": "github.com/org/smoke", "git_ref": "main"},
        "git_sha": "abc",
        "source_content_hash": "hash",
        "registration_inputs": {"source_mode": "git", "cli_version": "0.0.0"},
        "engine_version": "9.27.0",
        "entrypoint": "Snakefile",
        "form_schema": {"fields": [], "warnings": []},
        "queue_classes": ["cpu-spot"]
    })
}

async fn register(h: &Harness) -> String {
    let (status, body) =
        call(&h.state, request("POST", "/workflows", "developers", Some(&register_body()))).await;
    // Accepted: its runtime (not built yet in these tests) is still pending.
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    assert_eq!(body["state"], "pending");
    body["id"].as_str().unwrap().to_string()
}

async fn submit(h: &Harness, version: &str) -> (StatusCode, Value) {
    let body = json!({"workflow_version_id": version, "params": {}});
    call(&h.state, request("POST", "/runs", "operators", Some(&body))).await
}

fn key() -> String {
    runtime_fields("snakemake", "9.27.0").0
}

/// The variant build on `base`, as the ingest fold would record it. A build still in progress was
/// heard from just now, so the sweep follows it on its normal path rather than the stale-build
/// backstop (which asks CodeBuild); a finished one reports when it ended, long ago.
fn set_build(h: &Harness, base: &str, state: BuildState, builds: u32, image: Option<&str>) {
    let subject = coordinator_subject(&key(), base);
    let updated = if state.is_terminal() {
        Timestamp::from_micros(1)
    } else {
        Timestamp::from_micros(
            SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_micros() as i64
        )
    };
    h.registry.builds.lock().unwrap().insert(
        subject.clone(),
        BuildProjection {
            subject,
            state,
            build_arn: format!("arn:build:{builds}"),
            outputs: image
                .map(|i| BTreeMap::from([("VARIANT_IMAGE".to_string(), i.to_string())]))
                .unwrap_or_default(),
            reason: (state == BuildState::Failed).then(|| "FAILED: BUILD: exit 1".into()),
            logs_url: Some("https://logs/x".into()),
            request: Value::Null,
            builds,
            updated,
        },
    );
}

/// `variants::sweep`, which must have met no error: it counts a failed step and carries on, so a
/// test asserting that the sweep did nothing would otherwise pass when a step failed instead.
async fn sweep_ok(h: &Harness) -> biffix_backend::images::variants::VariantSweepReport {
    let report = biffix_backend::images::variants::sweep(&h.state).await.unwrap();
    assert_eq!(report.errors, 0, "{report:?}");
    report
}

/// The one run launched so far.
fn launched_once(h: &Harness) -> biffix_backend::aws::batch::CoordinatorLaunch {
    let launched = h.cloud.launched.lock().unwrap().clone();
    assert_eq!(launched.len(), 1, "{launched:?}");
    launched.into_iter().next().unwrap()
}

#[tokio::test]
async fn registering_freezes_the_runtime_and_starts_its_variant_build() {
    let h = harness();
    let id = register(&h).await;
    let rec = h.state.registry.get_version(&id).await.unwrap();
    assert_eq!(rec.version.runtime_key, key());
    assert_eq!(rec.version.runtime_recipe["engine_version"], "9.27.0");

    let builds = h.cloud.runtime_builds.lock().unwrap().clone();
    assert_eq!(builds.len(), 1);
    assert_eq!(builds[0].subject, coordinator_subject(&key(), FAKE_BASE_DIGEST));
    assert_eq!(
        builds[0].base_image,
        format!("{}@{FAKE_BASE_DIGEST}", testing::FAKE_COORDINATOR_REPO)
    );
    assert_eq!(builds[0].env["BIFFIX_RT_ENGINE_VERSION"], "9.27.0");
    assert_eq!(builds[0].env["BIFFIX_RT_KEY"], key());
    // The request is on the log, so the build projection knows it before CodeBuild reports.
    let events = h.events.events.lock().unwrap();
    assert!(events.iter().any(|e| matches!(
        &e.data,
        EventData::BuildRequested { subject, .. } if *subject == builds[0].subject
    )));
}

#[tokio::test]
async fn a_version_cannot_run_until_its_variant_is_built() {
    let h = harness();
    let id = register(&h).await;
    set_build(&h, FAKE_BASE_DIGEST, BuildState::Running, 1, None);
    let (status, body) = submit(&h, &id).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(body.as_str().unwrap().contains("still being built"), "{body}");
    assert!(h.cloud.launched.lock().unwrap().is_empty());
    // Waiting is not rebuilding.
    assert_eq!(h.cloud.runtime_builds.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn a_built_variant_gets_its_job_definition_and_the_run_launches_on_it() {
    let h = harness();
    let id = register(&h).await;
    let image = "123.dkr.ecr.us-east-1.amazonaws.com/coord@sha256:built";
    set_build(&h, FAKE_BASE_DIGEST, BuildState::Succeeded, 1, Some(image));

    let (status, body) = submit(&h, &id).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    assert_eq!(
        h.cloud.variant_job_definitions.lock().unwrap().as_slice(),
        &[(key(), FAKE_BASE_DIGEST.to_string(), image.to_string())]
    );
    let variant = &launched_once(&h).variant;
    assert_eq!(variant.image, image);
    assert_eq!(variant.engine_version, "9.27.0");
    assert_eq!(variant.runtime_key, key());
    assert!(variant.job_definition.contains("coordinator-rt-"));
    // The run's anchor records what it ran on.
    let events = h.events.events.lock().unwrap();
    let runtime = events
        .iter()
        .find_map(|e| match &e.data {
            EventData::RunSubmitted(anchor) => anchor.runtime.clone(),
            _ => None,
        })
        .expect("run_submitted with a runtime");
    assert_eq!(runtime.coordinator_image, image);
    assert_eq!(runtime.runtime_key, key());
}

#[tokio::test]
async fn a_new_coordinator_template_registers_the_variant_again_without_a_rebuild() {
    let h = harness();
    let id = register(&h).await;
    let image = "123.dkr.ecr.us-east-1.amazonaws.com/coord@sha256:built";
    set_build(&h, FAKE_BASE_DIGEST, BuildState::Succeeded, 1, Some(image));
    let (status, body) = submit(&h, &id).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let first = h.cloud.launched.lock().unwrap()[0].variant.job_definition.clone();

    // A deploy changes only the template (queues, role, env…): the base image, and so the build,
    // stay as they were.
    *h.cloud.template_revision.lock().unwrap() = 2;
    let (status, body) = submit(&h, &id).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let second = h.cloud.launched.lock().unwrap()[1].variant.job_definition.clone();
    assert_ne!(first, second);
    assert!(second.ends_with("-t2:1"), "{second}");
    assert_eq!(h.cloud.variant_job_definitions.lock().unwrap().len(), 2);
    assert_eq!(h.cloud.runtime_builds.lock().unwrap().len(), 1);
    // And the registry now records the current one, so the next run reuses it.
    let (status, _) = submit(&h, &id).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(h.cloud.variant_job_definitions.lock().unwrap().len(), 2);
    assert_eq!(
        h.cloud.launched.lock().unwrap()[2].variant.job_definition,
        second,
        "the third run launches on the definition the registry now records"
    );
}

#[tokio::test]
async fn after_a_base_bump_runs_use_the_previous_variant_until_the_new_one_is_ready() {
    let h = harness();
    let id = register(&h).await;
    // A variant exists for an older base only; the current base's is still building.
    h.registry.variants.lock().unwrap().push(fake_variant(&key(), "sha256:old"));
    set_build(&h, FAKE_BASE_DIGEST, BuildState::Running, 1, None);

    let (status, body) = submit(&h, &id).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    assert_eq!(launched_once(&h).variant.image, fake_variant(&key(), "sha256:old").image);
    // Running on the older variant does not start the current base's build again.
    assert_eq!(h.cloud.runtime_builds.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn a_previous_base_s_variant_runs_on_the_new_template_too() {
    let h = harness();
    let id = register(&h).await;
    h.registry.variants.lock().unwrap().push(fake_variant(&key(), "sha256:old"));
    set_build(&h, FAKE_BASE_DIGEST, BuildState::Running, 1, None);
    *h.cloud.template_revision.lock().unwrap() = 2;

    let (status, body) = submit(&h, &id).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let launched = launched_once(&h).variant;
    assert_eq!(launched.image, fake_variant(&key(), "sha256:old").image);
    assert!(launched.job_definition.ends_with("-t2:1"), "{launched:?}");
    let registered = h.cloud.variant_job_definitions.lock().unwrap().clone();
    assert_eq!(registered.len(), 1);
    assert_eq!(registered[0].1, "sha256:old");
    assert_eq!(h.cloud.runtime_builds.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn a_stale_variant_is_registered_again_whatever_its_build_did_since() {
    let h = harness();
    let id = register(&h).await;
    // The recorded variant's build has since failed (a manual rebuild): its image still runs.
    h.registry.variants.lock().unwrap().push(fake_variant(&key(), FAKE_BASE_DIGEST));
    set_build(&h, FAKE_BASE_DIGEST, BuildState::Failed, 2, None);
    *h.cloud.template_revision.lock().unwrap() = 2;

    let (status, body) = submit(&h, &id).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let launched = launched_once(&h).variant;
    assert_eq!(launched.image, fake_variant(&key(), FAKE_BASE_DIGEST).image);
    assert!(launched.job_definition.ends_with("-t2:1"), "{launched:?}");
    // Its failed build is not retried by submit.
    assert_eq!(h.cloud.runtime_builds.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn a_failed_build_refuses_runs_with_its_reason() {
    let h = harness();
    let id = register(&h).await;
    set_build(&h, FAKE_BASE_DIGEST, BuildState::Failed, 1, None);
    let (status, body) = submit(&h, &id).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    let message = body.as_str().unwrap();
    assert!(message.contains("failed to build: FAILED: BUILD: exit 1"), "{message}");
    assert!(message.contains("https://logs/x"), "{message}");
    // Submit does not retry; that is the sweep's (and a person's) call.
    assert_eq!(h.cloud.runtime_builds.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn the_sweep_retries_failed_builds_up_to_the_limit_and_finishes_built_ones() {
    let h = harness();
    register(&h).await;
    assert_eq!(h.cloud.runtime_builds.lock().unwrap().len(), 1);

    // Retried after its first failure, and after the one before the limit.
    let auto = biffix_backend::images::build_follow::AUTO_BUILDS;
    for (builds, started) in [(1, 2), (auto - 1, 3)] {
        set_build(&h, FAKE_BASE_DIGEST, BuildState::Failed, builds, None);
        let report = sweep_ok(&h).await;
        assert_eq!((report.runtimes, report.building), (1, 1), "{builds}");
        assert_eq!(h.cloud.runtime_builds.lock().unwrap().len(), started, "{builds}");
    }

    // Not once the automatic builds are used up.
    set_build(&h, FAKE_BASE_DIGEST, BuildState::Failed, auto, None);
    let report = sweep_ok(&h).await;
    assert_eq!(report.failed, 1);
    assert_eq!(h.cloud.runtime_builds.lock().unwrap().len(), 3);

    // A person's rebuild then succeeds: the sweep registers its image on the deployed base.
    set_build(&h, FAKE_BASE_DIGEST, BuildState::Succeeded, auto + 1, Some("repo@sha256:v"));
    let report = sweep_ok(&h).await;
    assert_eq!(report.ready, 1);
    assert_eq!(
        h.cloud.variant_job_definitions.lock().unwrap().as_slice(),
        &[(key(), FAKE_BASE_DIGEST.to_string(), "repo@sha256:v".to_string())]
    );
    let variants = h.registry.variants.lock().unwrap().clone();
    assert_eq!(variants.len(), 1, "{variants:?}");
    assert_eq!(
        (
            variants[0].runtime_key.as_str(),
            variants[0].base_digest.as_str(),
            variants[0].image.as_str()
        ),
        (key().as_str(), FAKE_BASE_DIGEST, "repo@sha256:v")
    );
    assert!(variants[0].job_definition_arn.contains("coordinator-rt-"), "{variants:?}");
}

#[tokio::test]
async fn a_base_that_cannot_be_read_is_not_reported_as_a_runtime_waiting_to_be_built() {
    // An ECR outage resolving the coordinator base: the detail leaves the runtime out (unknown)
    // rather than calling a failed variant "waiting to be built".
    let h = harness();
    let id = register(&h).await;
    set_build(&h, FAKE_BASE_DIGEST, BuildState::Failed, 3, None);
    *h.cloud.fail_coordinator_base.lock().unwrap() =
        Some(biffix_backend::aws::AwsError::Ecr("throttled".into()));
    let (status, detail) =
        call(&h.state, request("GET", &format!("/workflows/{id}"), "operators", None)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(detail.get("runtime").is_none_or(Value::is_null), "{detail}");
    // Once the base reads again, the failure is reported as it is.
    let (_, detail) =
        call(&h.state, request("GET", &format!("/workflows/{id}"), "operators", None)).await;
    assert_eq!(detail["runtime"]["state"], "failed");
}

#[tokio::test]
async fn a_person_can_rebuild_a_failed_variant_and_the_detail_reports_it() {
    let h = harness();
    let id = register(&h).await;
    set_build(&h, FAKE_BASE_DIGEST, BuildState::Failed, 3, None);

    let (status, detail) =
        call(&h.state, request("GET", &format!("/workflows/{id}"), "operators", None)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(detail["runtime"]["state"], "failed");
    assert_eq!(detail["runtime"]["ready"], false);
    assert_eq!(detail["runtime"]["key"], key());

    // Operators cannot; developers (who register) can.
    let uri = format!("/workflows/{id}/runtime/rebuild");
    let (status, _) = call(&h.state, request("POST", &uri, "operators", None)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, body) = call(&h.state, request("POST", &uri, "developers", None)).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    assert_eq!(body["key"], key(), "{body}");
    // (Its `state` still reads `failed`: Postgres folds the build's request into its projection in
    // the append, but the fake sink does not fold builds into the registry double.)
    let builds = h.cloud.runtime_builds.lock().unwrap().clone();
    assert_eq!(builds.len(), 2);
    assert_eq!(builds[1].subject, coordinator_subject(&key(), FAKE_BASE_DIGEST));
    // A new start (after three builds), not a repeat of registration's first.
    assert_ne!(builds[1].idempotency_token, builds[0].idempotency_token);
}

#[tokio::test]
async fn a_rebuild_leaves_a_building_or_ready_variant_as_it_is() {
    let h = harness();
    let id = register(&h).await;
    let uri = format!("/workflows/{id}/runtime/rebuild");

    // Building: a second start would be a second CodeBuild run of the same variant.
    set_build(&h, FAKE_BASE_DIGEST, BuildState::Running, 1, None);
    let (status, body) = call(&h.state, request("POST", &uri, "developers", None)).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    assert_eq!(body["state"], "building", "{body}");
    assert_eq!(h.cloud.runtime_builds.lock().unwrap().len(), 1);

    // Ready (its variant registered, whatever its build did since): nothing to build.
    h.registry.variants.lock().unwrap().push(fake_variant(&key(), FAKE_BASE_DIGEST));
    set_build(&h, FAKE_BASE_DIGEST, BuildState::Failed, 1, None);
    let (status, body) = call(&h.state, request("POST", &uri, "developers", None)).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    assert_eq!(body["state"], "ready", "{body}");
    assert_eq!(h.cloud.runtime_builds.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn the_sweep_asks_codebuild_about_a_build_whose_end_was_never_heard() {
    use biffix_core::wire::aws_codebuild::{CodeBuildEvent, CodeBuildStatus};
    let h = harness();
    register(&h).await;
    // Running, with no news since the epoch: long past the build timeout.
    set_build(&h, FAKE_BASE_DIGEST, BuildState::Running, 1, None);
    h.registry
        .builds
        .lock()
        .unwrap()
        .get_mut(&coordinator_subject(&key(), FAKE_BASE_DIGEST))
        .unwrap()
        .updated = Timestamp::from_micros(1);
    h.cloud.build_events.lock().unwrap().insert(
        "arn:build:1".into(),
        CodeBuildEvent {
            ts: Timestamp::from_micros(2),
            build_arn: "arn:build:1".into(),
            project: "biffix-t-runtime-build".into(),
            status: CodeBuildStatus::Succeeded,
            subject: coordinator_subject(&key(), FAKE_BASE_DIGEST),
            outputs: BTreeMap::from([("VARIANT_IMAGE".into(), "repo@sha256:v".into())]),
            reason: None,
            logs_url: None,
        },
    );
    sweep_ok(&h).await;
    let events = h.events.events.lock().unwrap();
    assert!(events.iter().any(|e| matches!(
        &e.data,
        EventData::BuildSucceeded { build_arn, .. } if build_arn == "arn:build:1"
    )));
}

#[tokio::test]
async fn a_variant_build_that_reported_no_image_fails_and_can_be_rebuilt() {
    let h = harness();
    let id = register(&h).await;
    // One build short of the limit, a build that reported no image is built again.
    set_build(
        &h,
        FAKE_BASE_DIGEST,
        BuildState::Succeeded,
        biffix_backend::images::build_follow::AUTO_BUILDS - 1,
        None,
    );
    sweep_ok(&h).await;
    assert_eq!(h.cloud.runtime_builds.lock().unwrap().len(), 2);
    // Its automatic builds are used up, and the last one succeeded without an image.
    set_build(
        &h,
        FAKE_BASE_DIGEST,
        BuildState::Succeeded,
        biffix_backend::images::build_follow::AUTO_BUILDS,
        None,
    );
    sweep_ok(&h).await;
    assert_eq!(h.cloud.runtime_builds.lock().unwrap().len(), 2);

    let (status, body) = submit(&h, &id).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    let message = body.as_str().unwrap();
    assert!(
        message.contains("failed to build: the build succeeded but reported no image"),
        "{message}"
    );
    let (_, detail) =
        call(&h.state, request("GET", &format!("/workflows/{id}"), "operators", None)).await;
    assert_eq!(detail["state"], "failed", "{detail}");
    assert_eq!(detail["runtime"]["state"], "failed");

    let uri = format!("/workflows/{id}/runtime/rebuild");
    let (status, body) = call(&h.state, request("POST", &uri, "developers", None)).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    assert_eq!(h.cloud.runtime_builds.lock().unwrap().len(), 3);
}

#[tokio::test]
async fn callers_that_read_the_same_state_start_one_build() {
    let h = harness();
    // Registration starts the build; the fake registry's projection never folds the request, so
    // each sweep below reads the variant as unbuilt, as a caller racing the registration would.
    register(&h).await;
    let (a, b) = tokio::join!(
        biffix_backend::images::variants::sweep(&h.state),
        biffix_backend::images::variants::sweep(&h.state),
    );
    assert_eq!(a.unwrap().errors, 0);
    assert_eq!(b.unwrap().errors, 0);
    let builds = h.cloud.runtime_builds.lock().unwrap().clone();
    assert_eq!(builds.len(), 1);
    // Every start got the one build, and its request is on the log once.
    let events = h.events.events.lock().unwrap();
    let requested = events
        .iter()
        .filter(|e| {
            matches!(
                &e.data,
                EventData::BuildRequested { subject, .. } if *subject == builds[0].subject
            )
        })
        .count();
    assert_eq!(requested, 1);
}

/// A version registered on a release biffix has since retired (Nextflow 25.04.0) keeps its frozen
/// recipe, but it is refused by name wherever it would run or build: submit, rebuild, the detail's
/// readiness and the sweep (which builds nothing for it) — even with a built variant on hand.
#[tokio::test]
async fn a_version_on_a_retired_release_is_refused_by_name() {
    use biffix_workflow::runtime::{
        runtime_key, NextflowPins, NextflowRelease, PluginPin, RuntimeRecipe,
    };
    let h = harness();
    let id = register(&h).await;
    let pin = |version: &str| PluginPin { version: version.into(), sha256: "s".into() };
    let recipe = RuntimeRecipe::Nextflow {
        recipe_version: biffix_workflow::runtime::RECIPE_VERSION,
        engine_version: "25.04.0".into(),
        pins: NextflowPins {
            nf_biffix_version: "0.4.0".into(),
            release: NextflowRelease {
                launcher_sha256: "l".into(),
                nf_amazon: pin("2.15.0"),
                nf_cloudcache: pin("0.4.3"),
            },
            workflow_plugins: Vec::new(),
        },
    };
    let retired_key = runtime_key(&recipe);
    {
        let mut versions = h.registry.versions.lock().unwrap();
        let rec = versions.get_mut(&id).unwrap();
        rec.version.engine = biffix_workflow::engines::Engine::Nextflow;
        rec.version.engine_version = "25.04.0".into();
        rec.version.entrypoint = "main.nf".into();
        rec.version.runtime_key = retired_key.clone();
        rec.version.runtime_recipe = serde_json::to_value(&recipe).unwrap();
    }
    // A variant of it that had built, so only the refusal keeps it from launching.
    let subject = coordinator_subject(&retired_key, FAKE_BASE_DIGEST);
    h.registry.builds.lock().unwrap().insert(
        subject.clone(),
        BuildProjection {
            subject,
            state: BuildState::Succeeded,
            build_arn: "arn:build:1".into(),
            outputs: BTreeMap::from([("VARIANT_IMAGE".to_string(), "repo@sha256:old".to_string())]),
            reason: None,
            logs_url: None,
            request: Value::Null,
            builds: 1,
            updated: Timestamp::from_micros(1),
        },
    );
    let builds_before = h.cloud.runtime_builds.lock().unwrap().len();
    let message = "Nextflow 25.04.0 is no longer supported; re-register on 25.10.4";

    let (status, body) = submit(&h, &id).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body.as_str().unwrap(), message);
    assert!(h.cloud.launched.lock().unwrap().is_empty());

    let uri = format!("/workflows/{id}/runtime/rebuild");
    let (status, body) = call(&h.state, request("POST", &uri, "developers", None)).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body.as_str().unwrap(), message);

    let (status, detail) =
        call(&h.state, request("GET", &format!("/workflows/{id}"), "operators", None)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(detail["state"], "failed", "{detail}");
    assert_eq!(detail["not_ready"], json!([message]), "{detail}");

    // The sweep builds nothing for it and registers no job definition.
    let report = sweep_ok(&h).await;
    assert_eq!(report.runtimes, 0, "{report:?}");
    assert_eq!(h.cloud.runtime_builds.lock().unwrap().len(), builds_before);
    assert!(h.cloud.variant_job_definitions.lock().unwrap().is_empty());
}

/// A Nextflow workflow declaring `plugins` (each `[id, version, sha256 digit]`), from source `sha`.
fn nf_register_body(sha: &str, plugins: &[(&str, &str, char)]) -> Value {
    let mut body = register_body();
    body["git_sha"] = json!(sha);
    body["engine"] = json!("nextflow");
    body["engine_version"] = json!("25.10.4");
    body["entrypoint"] = json!("main.nf");
    body["nextflow_plugins"] = plugins
        .iter()
        .map(|(id, version, digit)| {
            json!({"id": id, "version": version, "sha256": digit.to_string().repeat(64)})
        })
        .collect();
    body
}

/// Register `body`; the version's runtime key and its runtime build's environment.
async fn register_nf(h: &Harness, body: &Value) -> (String, BTreeMap<String, String>) {
    let (status, reply) =
        call(&h.state, request("POST", "/workflows", "developers", Some(body))).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{reply}");
    let rec = h.state.registry.get_version(reply["id"].as_str().unwrap()).await.unwrap();
    let key = rec.version.runtime_key.clone();
    let env = h
        .cloud
        .runtime_builds
        .lock()
        .unwrap()
        .iter()
        .find(|b| b.env["BIFFIX_RT_KEY"] == key)
        .map(|b| b.env.clone())
        .unwrap_or_default();
    (key, env)
}

#[tokio::test]
async fn a_nextflow_workflow_s_plugins_are_built_into_its_runtime() {
    let h = harness();
    let (plain, plain_env) = register_nf(&h, &nf_register_body("a1", &[])).await;
    // A workflow without plugins keeps the release's runtime key, and builds none.
    assert_eq!(plain, runtime_fields("nextflow", "25.10.4").0);
    assert!(!plain_env.contains_key("BIFFIX_RT_NF_WORKFLOW_PLUGINS"));

    let schema = [("nf-schema", "2.7.2", 'a'), ("nf-prov", "1.4.0", 'b')];
    let (with, env) = register_nf(&h, &nf_register_body("a2", &schema)).await;
    assert_ne!(with, plain);
    assert_eq!(
        env["BIFFIX_RT_NF_WORKFLOW_PLUGINS"],
        format!("nf-prov@1.4.0@{} nf-schema@2.7.2@{}", "b".repeat(64), "a".repeat(64))
    );
    // The same plugins, in any order, share the runtime (and its one build).
    let reversed = [schema[1], schema[0]];
    let (same, _) = register_nf(&h, &nf_register_body("a3", &reversed)).await;
    assert_eq!(same, with);
    let builds = h.cloud.runtime_builds.lock().unwrap().len();
    assert_eq!(builds, 2);
    // Another version of one of them is another runtime.
    let (other, _) = register_nf(&h, &nf_register_body("a4", &[("nf-schema", "2.7.3", 'a')])).await;
    assert!(![plain.as_str(), with.as_str()].contains(&other.as_str()));

    // The recipe is frozen with them, so a re-registration keeps them.
    let rec = h
        .state
        .registry
        .list_versions()
        .await
        .unwrap()
        .into_iter()
        .find(|r| r.version.runtime_key == with)
        .unwrap();
    let again = biffix_backend::register::pipeline::reregister_request(&rec, None).unwrap();
    let plugin = |id: &str, version: &str, digit: char| biffix_api::NextflowPluginRequest {
        id: id.into(),
        version: version.into(),
        sha256: digit.to_string().repeat(64),
    };
    assert_eq!(
        again.nextflow_plugins,
        [plugin("nf-prov", "1.4.0", 'b'), plugin("nf-schema", "2.7.2", 'a')]
    );
    // A recipe that cannot be read is refused, not re-registered without its plugins.
    let mut broken = rec.clone();
    broken.version.runtime_recipe = json!({"engine": "nextflow", "pins": 7});
    let e = biffix_backend::register::pipeline::reregister_request(&broken, None).unwrap_err();
    assert!(matches!(e, biffix_backend::error::ServiceError::Internal(_)), "{e:?}");
    let m = e.to_string();
    assert!(m.contains("runtime recipe cannot be read"), "{m}");
}

#[tokio::test]
async fn bad_or_misplaced_nextflow_plugins_are_refused() {
    let h = harness();
    let refused = |body: Value| {
        let state = h.state.clone();
        async move {
            let (status, reply) =
                call(&state, request("POST", "/workflows", "developers", Some(&body))).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{reply}");
            reply.as_str().unwrap_or_default().to_string()
        }
    };
    let m = refused(nf_register_body("b1", &[("nf-amazon", "3.4.4", 'a')])).await;
    assert!(m.contains("biffix's runtime provides nf-amazon"), "{m}");
    let m = refused(nf_register_body("b5", &[("nf-biffix-ext", "1.0.0", 'a')])).await;
    assert!(m.contains("starting with `nf-biffix-`"), "{m}");
    let m = refused(nf_register_body("b2", &[("nf-schema", ">=2.0", 'a')])).await;
    assert!(m.contains("one exact release"), "{m}");
    let m = refused(nf_register_body("b3", &[("nf-schema", "2.7.2", 'A')])).await;
    assert!(m.contains("64 lowercase hex"), "{m}");
    let m = refused(nf_register_body(
        "b4",
        &[("nf-schema", "2.7.2", 'a'), ("nf-schema", "2.7.3", 'a')],
    ))
    .await;
    assert!(m.contains("more than once"), "{m}");
    // A Snakemake workflow has no Nextflow plugins.
    let mut sm = register_body();
    sm["nextflow_plugins"] =
        json!([{"id": "nf-schema", "version": "2.7.2", "sha256": "a".repeat(64)}]);
    let m = refused(sm).await;
    assert!(m.contains("only to Nextflow"), "{m}");
    assert!(h.cloud.runtime_builds.lock().unwrap().is_empty());
}

#[tokio::test]
async fn the_sweep_asks_codebuild_only_about_a_stale_unfinished_build_and_keeps_it_when_it_cannot()
{
    use biffix_core::wire::aws_codebuild::{CodeBuildEvent, CodeBuildStatus};
    let subject = coordinator_subject(&key(), FAKE_BASE_DIGEST);
    let succeeded = |arn: &str| CodeBuildEvent {
        ts: Timestamp::from_micros(2),
        build_arn: arn.into(),
        project: "biffix-t-runtime-build".into(),
        status: CodeBuildStatus::Succeeded,
        subject: subject.clone(),
        outputs: BTreeMap::from([("VARIANT_IMAGE".into(), "repo@sha256:v".into())]),
        reason: None,
        logs_url: None,
    };
    let built = |h: &Harness| {
        h.events.events.lock().unwrap().iter().any(|e| {
            matches!(&e.data, EventData::BuildSucceeded { build_arn, .. } if build_arn == "arn:build:1")
        })
    };
    // Running and heard from just now: followed on its normal path, CodeBuild not asked (though it
    // would answer).
    let h = harness();
    register(&h).await;
    set_build(&h, FAKE_BASE_DIGEST, BuildState::Running, 1, None);
    h.cloud.build_events.lock().unwrap().insert("arn:build:1".into(), succeeded("arn:build:1"));
    sweep_ok(&h).await;
    assert!(h.cloud.build_event_asks.lock().unwrap().is_empty());
    assert!(!built(&h));

    // Finished long ago: not asked either.
    let h = harness();
    register(&h).await;
    set_build(&h, FAKE_BASE_DIGEST, BuildState::Succeeded, 1, Some("repo@sha256:v"));
    sweep_ok(&h).await;
    assert!(h.cloud.build_event_asks.lock().unwrap().is_empty());

    // Stale, but CodeBuild cannot answer: asked once, and the build is kept as it was.
    let h = harness();
    register(&h).await;
    set_build(&h, FAKE_BASE_DIGEST, BuildState::Running, 1, None);
    h.registry.builds.lock().unwrap().get_mut(&subject).unwrap().updated =
        Timestamp::from_micros(1);
    h.cloud.fail_build_event.store(true, std::sync::atomic::Ordering::SeqCst);
    sweep_ok(&h).await;
    assert_eq!(*h.cloud.build_event_asks.lock().unwrap(), ["arn:build:1".to_string()]);
    assert!(!built(&h));
    assert_eq!(h.registry.builds.lock().unwrap()[&subject].state, BuildState::Running);
}
