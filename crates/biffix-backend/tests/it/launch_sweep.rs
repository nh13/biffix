//! The launch sweep (`launch::sweep`): a run parked for its submit-time images launches once they
//! are runnable, fails with why when one never will be (or once it has waited too long), and is
//! dropped when it was cancelled meanwhile; a cancel that lands mid-launch stops the coordinator.
//! Also the sweep Lambda's build-event fold. In-memory doubles; no AWS, no DB.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{json, Value};
use tower::ServiceExt;

use biffix_backend::images::build_follow::{fold_build_event, BuildEventError, BuildEventFold};
use biffix_backend::runs::launch;
use biffix_backend::store::{EventSink, StoreError};
use biffix_backend::testing::{self, FakeCloud, MemEventSink, MemRegistry, MemStore, TestState};
use biffix_backend::{api, AppState, DeployConfig};
use biffix_core::builds::{BuildProjection, BuildState};
use biffix_core::event::{EventData, Kind, RunEvent};
use biffix_core::ids::RunId;
use biffix_core::time::Timestamp;

const SAMTOOLS: &str = "quay.io/biocontainers/samtools:1.21--h50ea8bc_0";

/// The documented bound on a parked run's wait (`launch::IMAGE_PREPARE_TIMEOUT`: a parked run times
/// out after 6 h), restated so a bound that drifts from it fails these tests.
const TIMEOUT: Duration = Duration::from_secs(6 * 3600);

/// How long a parked launch whose run was never recorded is kept (`launch::ORPHAN_AFTER`).
const ORPHAN_AFTER: Duration = Duration::from_secs(10 * 60);

/// A minute: how far either side of a bound the tests age a row.
const MINUTE: Duration = Duration::from_secs(60);

/// An event sink that, as it records a run's anchor, marks the run cancelled in the store: a cancel
/// landing while the sweep launches the coordinator.
struct CancelOnLaunch {
    inner: Arc<MemEventSink>,
    store: Arc<MemStore>,
}

#[async_trait]
impl EventSink for CancelOnLaunch {
    async fn record_all(&self, evs: &[RunEvent]) -> Result<usize, StoreError> {
        for ev in evs {
            if ev.kind() == Kind::RunSubmitted {
                let run = ev.run_id.as_ref().unwrap().to_string();
                if let Some(d) = self.store.runs.lock().unwrap().get_mut(&run) {
                    d.run.state = "cancelled".into();
                }
            }
        }
        self.inner.record_all(evs).await
    }
}

struct Harness {
    state: AppState,
    store: Arc<MemStore>,
    registry: Arc<MemRegistry>,
    events: Arc<MemEventSink>,
    cloud: Arc<FakeCloud>,
    run_id: RunId,
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

async fn post(state: &AppState, uri: &str, body: &Value) -> (StatusCode, Value) {
    let res = api::router(state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 20).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

/// A registered version and a run submitted with a still-to-import image: parked, and projected as
/// `preparing_images` (the store double does not fold).
async fn parked() -> Harness {
    parked_in(None).await
}

/// [`parked`], launched into `experiment` (created first, live, in the version's project) when
/// given.
async fn parked_in(experiment: Option<&str>) -> Harness {
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
            worker_image_repo: "111122223333.dkr.ecr.us-east-1.amazonaws.com/biffix-workers".into(),
            ..Default::default()
        })
        .with_projects(testing::bucket_project("test-bucket"))
        .build();
    let (status, _) = post(&state, "/workflows", &register_body()).await;
    assert!(status.is_success(), "register: {status}");
    let mut submit = json!({"workflow_version_id": "abc:hash:0",
            "params": {"reads": "s3://b/in.fastq", "outdir": "s3://b/out/"},
            "images": [SAMTOOLS]});
    if let Some(id) = experiment {
        state
            .experiments
            .create(&biffix_backend::store::experiments::ExperimentRecord {
                id: id.into(),
                project: "genomics".into(),
                name: id.into(),
                description: None,
                created_by: None,
                created_by_label: None,
                created_at_micros: 1,
                archived_at_micros: None,
            })
            .await
            .unwrap();
        submit["experiment"] = json!(id);
    }
    let (status, out) = post(&state, "/runs", &submit).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{out}");
    assert_eq!(out["state"], "preparing_images", "{out}");
    let run_id = RunId::new(out["run_id"].as_str().unwrap());
    store.insert(testing::run_detail(run_id.as_str(), "preparing_images"));
    Harness { state, store, registry, events, cloud, run_id }
}

impl Harness {
    /// The parked submit-time image, as a version image.
    fn image(&self) -> biffix_backend::store::registry::VersionImage {
        let pending = self.store.pending.lock().unwrap();
        let plan = &pending[self.run_id.as_str()].plan["images"][0];
        biffix_backend::store::registry::VersionImage {
            name: SAMTOOLS.into(),
            declared_ref: SAMTOOLS.into(),
            source_ref: plan["source_ref"].as_str().unwrap().into(),
            content_key: plan["content_key"].as_str().unwrap().into(),
            own_runtime: false,
            build: None,
        }
    }

    /// The submit-time image's import (and derive) finished.
    fn images_ready(&self) {
        let image = self.image();
        let runtime_key =
            self.registry.versions.lock().unwrap()["abc:hash:0"].version.runtime_key.clone();
        let derived = testing::fake_derived_artifact(&image, &runtime_key);
        let mut artifacts = self.registry.artifacts.lock().unwrap();
        artifacts.insert(image.content_key.clone(), testing::fake_artifact(&image));
        artifacts.insert(derived.content_key.clone(), derived);
    }

    fn parked(&self) -> bool {
        self.store.pending.lock().unwrap().contains_key(self.run_id.as_str())
    }

    /// The control plane's `run_failed`: its reason and stage.
    fn failure(&self) -> Option<(String, String)> {
        self.events.events.lock().unwrap().iter().find_map(|e| match &e.data {
            EventData::RunFailed { reason, stage } => {
                Some((reason.clone().unwrap_or_default(), stage.clone().unwrap_or_default()))
            }
            _ => None,
        })
    }

    /// Make the parked launch `age` older (successive calls add up).
    fn age(&self, age: Duration) {
        let mut pending = self.store.pending.lock().unwrap();
        let row = pending.get_mut(self.run_id.as_str()).unwrap();
        row.created_at_micros -= age.as_micros() as i64;
    }

    /// The coordinator job the run is bound to (its `coordinator_submitted`), if it is.
    fn bound_coordinator(&self) -> Option<String> {
        self.events
            .events
            .lock()
            .unwrap()
            .iter()
            .find(|e| e.kind() == Kind::CoordinatorSubmitted)
            .and_then(|e| e.coordinator_job_id.as_ref().map(|j| j.as_str().to_string()))
    }

    fn recorded(&self, kind: Kind) -> bool {
        self.events.events.lock().unwrap().iter().any(|e| e.kind() == kind)
    }

    /// Age the parked launch to a minute under [`TIMEOUT`], sweep, and check the run still waits
    /// (parked, not failed); then age it to a minute past the bound, for the caller's next sweep.
    async fn waits_until_the_bound(&self) {
        self.age(TIMEOUT - MINUTE);
        let report = launch::sweep(&self.state).await.unwrap();
        assert_eq!((report.waiting, report.failed), (1, 0), "{report:?}");
        assert!(self.parked());
        assert!(self.failure().is_none());
        self.age(2 * MINUTE);
    }
}

#[tokio::test]
async fn a_parked_run_waits_for_its_images_then_launches() {
    let h = parked().await;
    let image = h.image();
    let report = launch::sweep(&h.state).await.unwrap();
    assert_eq!((report.pending, report.waiting), (1, 1), "{report:?}");
    assert!(h.parked());
    assert!(h.cloud.launched.lock().unwrap().is_empty());

    h.images_ready();
    let report = launch::sweep(&h.state).await.unwrap();
    assert_eq!(report.launched, 1, "{report:?}");
    assert!(!h.parked());
    assert_eq!(h.cloud.launched.lock().unwrap().len(), 1);
    assert!(h.recorded(Kind::CoordinatorSubmitted));
    let anchor = h
        .events
        .events
        .lock()
        .unwrap()
        .iter()
        .find_map(|e| match &e.data {
            EventData::RunSubmitted(a) => Some((**a).clone()),
            _ => None,
        })
        .expect("anchor recorded");
    assert_eq!(
        anchor.submit_images,
        Some(vec![biffix_core::event::SubmitImage {
            declared_ref: SAMTOOLS.into(),
            source_ref: image.source_ref.clone(),
            content_key: image.content_key.clone(),
        }])
    );
    // The run's image map sends the submit-time reference to its derived image.
    let key = h.registry.versions.lock().unwrap()["abc:hash:0"].version.runtime_key.clone();
    assert_eq!(
        anchor.image_map.unwrap()[SAMTOOLS],
        testing::fake_derived_artifact(&image, &key).image
    );
}

/// A run parked in an experiment is in it while it waits (its `run_preparing` says so), and the
/// sweep's later anchor records the same experiment, so it stays in it once launched.
#[tokio::test]
async fn a_parked_run_is_in_its_experiment_while_it_waits_and_once_launched() {
    let h = parked_in(Some("exp-1")).await;
    let preparing = h
        .events
        .events
        .lock()
        .unwrap()
        .iter()
        .find_map(|e| match &e.data {
            EventData::RunPreparing(p) => Some((**p).clone()),
            _ => None,
        })
        .expect("run_preparing recorded");
    assert_eq!(preparing.experiment_id.as_deref(), Some("exp-1"));
    let in_experiment = |h: &Harness| h.events.folded().runs[h.run_id.as_str()].experiment_id();
    assert_eq!(in_experiment(&h).as_deref(), Some("exp-1"));
    let pending = h.store.pending.lock().unwrap()[h.run_id.as_str()].plan["experiment_id"].clone();
    assert_eq!(pending, json!("exp-1"));

    h.images_ready();
    let report = launch::sweep(&h.state).await.unwrap();
    assert_eq!(report.launched, 1, "{report:?}");
    let anchor = h
        .events
        .events
        .lock()
        .unwrap()
        .iter()
        .find_map(|e| match &e.data {
            EventData::RunSubmitted(a) => Some((**a).clone()),
            _ => None,
        })
        .expect("anchor recorded");
    assert_eq!(anchor.experiment_id.as_deref(), Some("exp-1"));
    assert_eq!(in_experiment(&h).as_deref(), Some("exp-1"));
}

#[tokio::test]
async fn a_build_that_will_not_be_retried_fails_the_run_with_its_reason() {
    let h = parked().await;
    let subject = biffix_workflow::images::subject(&h.image().content_key);
    h.registry.builds.lock().unwrap().insert(
        subject.clone(),
        BuildProjection {
            subject,
            state: BuildState::Failed,
            build_arn: "arn:import:3".into(),
            outputs: Default::default(),
            reason: Some("FAILED: BUILD: MANIFEST_UNKNOWN".into()),
            logs_url: Some("https://logs/import".into()),
            request: json!({"source": SAMTOOLS}),
            builds: biffix_backend::images::build_follow::AUTO_BUILDS,
            updated: Timestamp::from_micros(biffix_core::clock::epoch_micros()),
        },
    );
    let report = launch::sweep(&h.state).await.unwrap();
    assert_eq!(report.failed, 1, "{report:?}");
    let (reason, stage) = h.failure().expect("run_failed recorded");
    assert_eq!(stage, "image_preparation");
    assert!(reason.contains("MANIFEST_UNKNOWN"), "{reason}");
    assert!(reason.contains(SAMTOOLS), "{reason}");
    assert!(!h.parked());
    assert!(h.cloud.launched.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_failed_build_with_a_retry_left_is_restarted_and_the_run_waits() {
    let h = parked().await;
    // Build events fold as the real sink folds them, so the build the sweep restarts reads back.
    h.events.fold_builds_into(h.registry.clone());
    let subject = biffix_workflow::images::subject(&h.image().content_key);
    // The import submit started, and one before it, both failed: one retry is still to come.
    let requested = h
        .events
        .events
        .lock()
        .unwrap()
        .iter()
        .find(|e| matches!(&e.data, EventData::BuildRequested { subject: s, .. } if *s == subject))
        .cloned()
        .expect("submit started the import");
    let failed = |requested: &RunEvent| {
        let EventData::BuildRequested { subject, build_arn, .. } = &requested.data else {
            unreachable!()
        };
        let mut e = requested.clone();
        e.dedup_key = format!("{}:failed", e.dedup_key);
        e.ts = Timestamp::from_micros(e.ts.as_micros() + 1);
        e.data = EventData::BuildFailed {
            subject: subject.clone(),
            build_arn: build_arn.clone(),
            reason: "FAILED: BUILD: TOOMANYREQUESTS".into(),
            logs_url: None,
        };
        e
    };
    let earlier = biffix_core::event::build_requested(
        Timestamp::from_micros(requested.ts.as_micros() - 3_600_000_000),
        &subject,
        "arn:import:earlier",
        Value::Null,
    );
    h.state
        .events
        .record_all(&[earlier.clone(), failed(&earlier), failed(&requested)])
        .await
        .unwrap();
    let before = h.registry.builds.lock().unwrap()[&subject].clone();
    assert_eq!(
        (before.state, before.builds),
        (BuildState::Failed, biffix_backend::images::build_follow::AUTO_BUILDS - 1)
    );
    let imports = h.cloud.image_imports.lock().unwrap().len();

    let report = launch::sweep(&h.state).await.unwrap();
    assert_eq!((report.waiting, report.failed), (1, 0), "{report:?}");
    assert!(h.parked());
    assert!(h.failure().is_none());
    assert_eq!(
        h.cloud.image_imports.lock().unwrap().len(),
        imports + 1,
        "the failed import is started again"
    );
    let after = h.registry.builds.lock().unwrap()[&subject].clone();
    assert_eq!(
        (after.state, after.builds),
        (BuildState::Requested, biffix_backend::images::build_follow::AUTO_BUILDS)
    );
    assert!(h.cloud.launched.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_run_that_waits_too_long_fails_with_what_it_waits_for() {
    let h = parked().await;
    h.waits_until_the_bound().await;
    let report = launch::sweep(&h.state).await.unwrap();
    assert_eq!(report.failed, 1, "{report:?}");
    let (reason, stage) = h.failure().expect("run_failed recorded");
    assert_eq!(stage, "image_preparation");
    assert!(reason.starts_with("image preparation timed out after 6 h: image `"), "{reason}");
    // What it waited for: the image still being prepared.
    assert!(reason.contains(SAMTOOLS), "{reason}");
    assert!(!h.parked());
}

#[tokio::test]
async fn a_cancelled_run_is_dropped_not_launched() {
    let h = parked().await;
    h.images_ready();
    h.store.insert(testing::run_detail(h.run_id.as_str(), "cancelled"));
    let report = launch::sweep(&h.state).await.unwrap();
    assert_eq!(report.cancelled, 1, "{report:?}");
    assert!(!h.parked());
    assert!(h.cloud.launched.lock().unwrap().is_empty());
    assert!(!h.recorded(Kind::RunSubmitted));
}

#[tokio::test]
async fn a_cancel_landing_mid_launch_stops_the_coordinator() {
    let mut h = parked().await;
    h.images_ready();
    h.state.events = Arc::new(CancelOnLaunch { inner: h.events.clone(), store: h.store.clone() });
    let report = launch::sweep(&h.state).await.unwrap();
    assert_eq!((report.launched, report.cancelled), (0, 1), "{report:?}");
    assert_eq!(h.cloud.launched.lock().unwrap().len(), 1);
    assert_eq!(
        *h.cloud.terminated.lock().unwrap(),
        vec![("coord-1".to_string(), "biffix: cancelled while preparing images".to_string())]
    );
    assert!(!h.parked());
}

#[tokio::test]
async fn without_a_capability_secret_a_ready_run_stays_parked_until_the_bound() {
    let mut h = parked().await;
    h.images_ready();
    h.state.capability = None;
    let report = launch::sweep(&h.state).await.unwrap();
    assert_eq!(report.waiting, 1, "{report:?}");
    assert!(h.parked());
    assert!(h.cloud.launched.lock().unwrap().is_empty());
    // It never launches blind: past the bound it fails, saying why.
    h.waits_until_the_bound().await;
    let report = launch::sweep(&h.state).await.unwrap();
    assert_eq!(report.failed, 1, "{report:?}");
    assert_eq!(
        h.failure(),
        Some((
            "launching timed out after 6 h: launching is disabled: this deploy has no capability \
             secret (BIFFIX_CAPABILITY_SECRET_ARN), so a run's coordinator could not report"
                .into(),
            "launch".into()
        ))
    );
    assert!(!h.parked());
    assert!(h.cloud.launched.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_launch_whose_run_was_never_recorded_is_dropped_once_old() {
    let h = parked().await;
    h.store.runs.lock().unwrap().clear();
    // A minute under the bound: a submit may still be recording it.
    h.age(ORPHAN_AFTER - MINUTE);
    let report = launch::sweep(&h.state).await.unwrap();
    assert_eq!(report.waiting, 1, "a submit may still be recording it: {report:?}");
    assert!(h.parked());
    h.age(2 * MINUTE);
    let report = launch::sweep(&h.state).await.unwrap();
    assert_eq!(report.dropped, 1, "{report:?}");
    assert!(!h.parked());
}

#[tokio::test]
async fn the_sweep_folds_a_codebuild_event_it_is_invoked_with() {
    let h = parked().await;
    let before = h.events.events.lock().unwrap().len();

    // The schedule's payload folds nothing, nor does another CodeBuild event type.
    let scheduled = json!({"source": "aws.events", "detail-type": "Scheduled Event", "detail": {}});
    assert_eq!(
        fold_build_event(&h.state, &scheduled).await.unwrap(),
        BuildEventFold::NotABuildEvent
    );

    let build = |variables: Value| {
        json!({
            "source": "aws.codebuild",
            "detail-type": "CodeBuild Build State Change",
            "time": "2026-09-28T00:00:00Z",
            "detail": {
                "build-status": "SUCCEEDED",
                "build-id": "arn:aws:codebuild:us-east-1:1:build/biffix-images:1",
                "project-name": "biffix-images",
                "additional-information": {"environment": {"environment-variables": variables}}
            }
        })
    };
    let mut phase = build(json!([]));
    phase["detail-type"] = "CodeBuild Build Phase Change".into();
    assert_eq!(fold_build_event(&h.state, &phase).await.unwrap(), BuildEventFold::NotABuildEvent);
    // A build biffix did not start (no subject) is ignored.
    assert_eq!(
        fold_build_event(&h.state, &build(json!([]))).await.unwrap(),
        BuildEventFold::Ignored
    );
    assert_eq!(h.events.events.lock().unwrap().len(), before);

    let subject = biffix_workflow::images::subject(&h.image().content_key);
    let ev =
        build(json!([{"name": "BIFFIX_BUILD_SUBJECT", "value": subject, "type": "PLAINTEXT"}]));
    assert_eq!(fold_build_event(&h.state, &ev).await.unwrap(), BuildEventFold::Recorded);
    // Recorded once: the ingest copy of the same event dedups onto it.
    assert_eq!(fold_build_event(&h.state, &ev).await.unwrap(), BuildEventFold::Recorded);
    assert_eq!(h.events.events.lock().unwrap().len(), before + 1);

    // A malformed CodeBuild event is a decode error (the sweeps still run).
    let mut bad = ev.clone();
    bad["detail"]["build-status"] = Value::Null;
    let err = fold_build_event(&h.state, &bad).await.unwrap_err();
    assert!(matches!(err, BuildEventError::Decode(_)), "{err:?}");
    assert!(err.to_string().starts_with("decoding the build event: "), "{err}");
}

#[tokio::test]
async fn a_run_already_launched_is_dropped_never_launched_again() {
    let h = parked().await;
    h.images_ready();
    // Its launch outlived it (dropping the row failed after the launch).
    h.store.insert(testing::run_detail(h.run_id.as_str(), "submitted"));
    let report = launch::sweep(&h.state).await.unwrap();
    assert_eq!((report.dropped, report.launched), (1, 0), "{report:?}");
    assert!(!h.parked());
    assert!(h.cloud.launched.lock().unwrap().is_empty());
    assert!(!h.recorded(Kind::RunSubmitted));
}

#[tokio::test]
async fn a_launch_another_holder_claims_waits() {
    let h = parked().await;
    h.images_ready();
    h.store.launch_claims.lock().unwrap().insert(h.run_id.to_string());
    let report = launch::sweep(&h.state).await.unwrap();
    assert_eq!(report.waiting, 1, "{report:?}");
    assert!(h.parked());
    assert!(h.cloud.launched.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_run_that_ended_otherwise_is_counted_as_finished_not_cancelled() {
    let h = parked().await;
    h.store.insert(testing::run_detail(h.run_id.as_str(), "failed"));
    let report = launch::sweep(&h.state).await.unwrap();
    assert_eq!((report.finished, report.cancelled), (1, 0), "{report:?}");
    assert!(!h.parked());
}

#[tokio::test]
async fn a_run_whose_version_was_deleted_fails_at_launch() {
    let h = parked().await;
    h.registry.versions.lock().unwrap().clear();
    let report = launch::sweep(&h.state).await.unwrap();
    assert_eq!(report.failed, 1, "{report:?}");
    assert_eq!(
        h.failure(),
        Some(("workflow version abc:hash:0 no longer exists".into(), "launch".into()))
    );
    assert!(!h.parked());
}

#[tokio::test]
async fn an_unreadable_launch_request_fails_its_run_and_a_newer_one_waits() {
    let h = parked().await;
    let plan = h.store.pending.lock().unwrap()[h.run_id.as_str()].plan.clone();
    // From a newer biffix: left for a sweep that knows it (the timeout fails it).
    let mut newer = plan.clone();
    newer["version"] = json!(99);
    h.store.pending.lock().unwrap().get_mut(h.run_id.as_str()).unwrap().plan = newer;
    h.images_ready();
    let report = launch::sweep(&h.state).await.unwrap();
    assert_eq!(report.waiting, 1, "{report:?}");
    assert!(h.cloud.launched.lock().unwrap().is_empty());
    h.waits_until_the_bound().await;
    let report = launch::sweep(&h.state).await.unwrap();
    assert_eq!(report.failed, 1, "{report:?}");
    let (reason, stage) = h.failure().unwrap();
    assert_eq!(stage, "launch");
    assert!(
        reason.starts_with("launching timed out after 6 h: its launch request is from a newer"),
        "{reason}"
    );
    assert!(!h.parked());
    assert!(h.cloud.launched.lock().unwrap().is_empty());

    // Unreadable: failed at once.
    let h = parked().await;
    h.store.pending.lock().unwrap().get_mut(h.run_id.as_str()).unwrap().plan = json!({});
    let report = launch::sweep(&h.state).await.unwrap();
    assert_eq!(report.failed, 1, "{report:?}");
    let (reason, stage) = h.failure().unwrap();
    assert_eq!(stage, "launch");
    assert!(reason.starts_with("the run's launch request is unreadable: "), "{reason}");
    assert!(!h.parked());
}

#[tokio::test]
async fn a_run_whose_version_images_are_gone_fails_at_launch() {
    let h = parked().await;
    h.images_ready();
    let version_keys: Vec<String> = h.registry.versions.lock().unwrap()["abc:hash:0"]
        .images
        .iter()
        .map(|i| i.content_key.clone())
        .collect();
    {
        let mut artifacts = h.registry.artifacts.lock().unwrap();
        for key in version_keys {
            artifacts.remove(&key);
        }
    }
    let report = launch::sweep(&h.state).await.unwrap();
    assert_eq!(report.failed, 1, "{report:?}");
    let (reason, stage) = h.failure().unwrap();
    assert_eq!(stage, "launch");
    assert!(reason.starts_with("the version's images are no longer runnable: "), "{reason}");
    assert!(!h.parked());
    assert!(h.cloud.launched.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_run_waits_for_its_coordinator_variant_and_fails_without_a_runtime() {
    // Its variant is being built: it waits, and past the bound times out at launching.
    let h = parked().await;
    h.images_ready();
    h.registry.variants.lock().unwrap().clear();
    let report = launch::sweep(&h.state).await.unwrap();
    assert_eq!(report.waiting, 1, "{report:?}");
    assert!(h.parked());
    assert!(h.cloud.launched.lock().unwrap().is_empty());
    h.waits_until_the_bound().await;
    let report = launch::sweep(&h.state).await.unwrap();
    assert_eq!(report.failed, 1, "{report:?}");
    let (reason, stage) = h.failure().unwrap();
    assert_eq!(stage, "launch");
    assert!(
        reason.starts_with(
            "launching timed out after 6 h: the version's engine runtime is still being built"
        ),
        "{reason}"
    );
    assert!(!h.parked());
}

#[tokio::test]
async fn a_launch_that_may_pass_waits_and_one_that_will_not_fails() {
    // AWS unavailable: parked for the next sweep.
    let h = parked().await;
    h.images_ready();
    *h.cloud.fail_launch.lock().unwrap() = Some(true);
    let report = launch::sweep(&h.state).await.unwrap();
    assert_eq!(report.waiting, 1, "{report:?}");
    assert!(h.parked());
    assert!(h.failure().is_none());
    *h.cloud.fail_launch.lock().unwrap() = None;
    let report = launch::sweep(&h.state).await.unwrap();
    assert_eq!(report.launched, 1, "{report:?}");

    // Refused: failed, at launch.
    let h = parked().await;
    h.images_ready();
    *h.cloud.fail_launch.lock().unwrap() = Some(false);
    let report = launch::sweep(&h.state).await.unwrap();
    assert_eq!(report.failed, 1, "{report:?}");
    let (reason, stage) = h.failure().unwrap();
    assert_eq!(stage, "launch");
    assert!(reason.contains("SubmitJob denied"), "{reason}");
    assert!(!h.parked());

    // Its image map cannot be written (an S3 refusal): failed too.
    let h = parked().await;
    h.images_ready();
    let map_uri = biffix_backend::images::map::uri_for("test-bucket", h.run_id.as_str());
    h.cloud.fail_puts.lock().unwrap().insert(map_uri);
    let report = launch::sweep(&h.state).await.unwrap();
    assert_eq!(report.failed, 1, "{report:?}");
    let (reason, stage) = h.failure().unwrap();
    assert_eq!(stage, "launch");
    assert!(reason.starts_with("writing the run's image map failed: "), "{reason}");
    assert!(h.cloud.launched.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_step_that_keeps_failing_fails_the_run_only_past_the_bound() {
    let h = parked().await;
    // Its version's runtime recipe no longer reads: every sweep's step errs.
    h.registry.versions.lock().unwrap().get_mut("abc:hash:0").unwrap().version.runtime_recipe =
        json!("not a recipe");
    let report = launch::sweep(&h.state).await.unwrap();
    assert_eq!((report.errors, report.failed), (1, 0), "{report:?}");
    assert!(h.parked());
    assert!(h.failure().is_none());
    // Still only an error a minute under the bound.
    h.age(TIMEOUT - MINUTE);
    let report = launch::sweep(&h.state).await.unwrap();
    assert_eq!((report.errors, report.failed), (1, 0), "{report:?}");
    assert!(h.parked());
    assert!(h.failure().is_none());
    h.age(2 * MINUTE);
    let report = launch::sweep(&h.state).await.unwrap();
    assert_eq!(report.failed, 1, "{report:?}");
    let (reason, stage) = h.failure().unwrap();
    // Launch, although the recipe is first read to derive the run's images: the version's runtime
    // is the launch's.
    assert_eq!(stage, "launch");
    assert!(
        reason.starts_with(
            "launching timed out after 6 h; last error: the version's runtime recipe is \
             unreadable"
        ),
        "{reason}"
    );
    assert!(!h.parked());
}

/// An event sink that cannot record a run's anchor (the store is down as the coordinator
/// launches), and fails its `run_failed` the first `fail_failures` times.
struct CannotRecordLaunch {
    inner: Arc<MemEventSink>,
    fail_failures: std::sync::atomic::AtomicUsize,
}

#[async_trait]
impl EventSink for CannotRecordLaunch {
    async fn record_all(&self, evs: &[RunEvent]) -> Result<usize, StoreError> {
        if evs.iter().any(|e| e.kind() == Kind::RunSubmitted) {
            return Err(StoreError::Unavailable("down".into()));
        }
        if evs.iter().any(|e| e.kind() == Kind::RunFailed)
            && self
                .fail_failures
                .fetch_update(
                    std::sync::atomic::Ordering::SeqCst,
                    std::sync::atomic::Ordering::SeqCst,
                    |n| n.checked_sub(1),
                )
                .is_ok()
        {
            return Err(StoreError::Unavailable("down".into()));
        }
        self.inner.record_all(evs).await
    }
}

#[tokio::test]
async fn an_unrecorded_launch_whose_coordinator_was_stopped_is_retried() {
    let mut h = parked().await;
    h.images_ready();
    h.state.events =
        Arc::new(CannotRecordLaunch { inner: h.events.clone(), fail_failures: 0.into() });
    let report = launch::sweep(&h.state).await.unwrap();
    assert_eq!(report.waiting, 1, "{report:?}");
    assert!(h.parked());
    assert_eq!(
        *h.cloud.terminated.lock().unwrap(),
        vec![("coord-1".to_string(), "biffix: the run could not be recorded".to_string())]
    );
    assert!(h.failure().is_none());
    // Its coordinator was stopped, so the row is not marked as leaving one running unbound (which
    // would fail the run on the next sweep rather than launch it).
    assert_eq!(h.store.pending.lock().unwrap()[h.run_id.as_str()].orphan_job_id(), None);

    // The store is back: the next sweep launches it, once more.
    h.state.events = h.events.clone();
    let report = launch::sweep(&h.state).await.unwrap();
    assert_eq!((report.launched, report.failed), (1, 0), "{report:?}");
    assert_eq!(h.cloud.launched.lock().unwrap().len(), 2);
    assert!(!h.parked());
    assert!(h.recorded(Kind::RunSubmitted));
    assert!(h.failure().is_none());
    assert_eq!(h.cloud.terminated.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn a_coordinator_left_running_unbound_is_never_launched_twice() {
    let mut h = parked().await;
    h.images_ready();
    h.state.events =
        Arc::new(CannotRecordLaunch { inner: h.events.clone(), fail_failures: 1.into() });
    h.cloud.fail_terminate.store(true, std::sync::atomic::Ordering::SeqCst);
    // Launched, unrecorded, unstoppable: marked, and failing the run fails this time too.
    let report = launch::sweep(&h.state).await.unwrap();
    assert_eq!(report.errors, 1, "{report:?}");
    assert_eq!(h.cloud.launched.lock().unwrap().len(), 1);
    assert!(h.parked());
    assert_eq!(h.store.pending.lock().unwrap()[h.run_id.as_str()].orphan_job_id(), Some("coord-1"));
    // The next sweep does not launch it again: it stops the coordinator and fails the run.
    h.cloud.fail_terminate.store(false, std::sync::atomic::Ordering::SeqCst);
    let report = launch::sweep(&h.state).await.unwrap();
    assert_eq!(report.failed, 1, "{report:?}");
    assert_eq!(h.cloud.launched.lock().unwrap().len(), 1);
    assert_eq!(
        *h.cloud.terminated.lock().unwrap(),
        vec![("coord-1".to_string(), "biffix: the run could not be recorded".to_string())]
    );
    assert_eq!(
        h.failure(),
        Some((
            "the run's coordinator (coord-1) launched but the run could not be recorded; it was \
             stopped"
                .into(),
            "launch".into()
        ))
    );
    assert!(!h.parked());
}

#[tokio::test]
async fn an_unbound_coordinator_that_cannot_be_stopped_fails_the_run_naming_it() {
    let mut h = parked().await;
    h.images_ready();
    h.state.events =
        Arc::new(CannotRecordLaunch { inner: h.events.clone(), fail_failures: 0.into() });
    h.cloud.fail_terminate.store(true, std::sync::atomic::Ordering::SeqCst);
    let report = launch::sweep(&h.state).await.unwrap();
    assert_eq!(report.failed, 1, "{report:?}");
    assert_eq!(h.cloud.launched.lock().unwrap().len(), 1);
    let (reason, stage) = h.failure().unwrap();
    assert_eq!(stage, "launch");
    assert!(reason.contains("stop AWS Batch job coord-1"), "{reason}");
    assert!(!h.parked());
}

#[tokio::test]
async fn a_sweep_that_listed_a_run_before_another_marked_it_unbound_never_launches_it() {
    let h = parked().await;
    h.images_ready();
    // Another sweep's launch can neither be recorded, nor its coordinator stopped, nor the run
    // failed: it marks the parked row as leaving coordinator `coord-1` running unbound.
    let mut other = h.state.clone();
    other.events =
        Arc::new(CannotRecordLaunch { inner: h.events.clone(), fail_failures: 1.into() });
    h.cloud.fail_terminate.store(true, std::sync::atomic::Ordering::SeqCst);
    // This sweep lists the run, then the other one sweeps it whole before this one claims it.
    *h.store.after_list_pending.lock().unwrap() = Some(Box::new(move || {
        let report = futures::executor::block_on(launch::sweep(&other)).unwrap();
        assert_eq!(report.errors, 1, "the other sweep: {report:?}");
    }));
    let report = launch::sweep(&h.state).await.unwrap();
    // Read under its claim, the row's mark keeps this sweep from launching the run a second time:
    // it fails the run, naming the coordinator it could not stop.
    assert_eq!(report.failed, 1, "{report:?}");
    assert_eq!(h.cloud.launched.lock().unwrap().len(), 1);
    let (reason, stage) = h.failure().unwrap();
    assert_eq!(stage, "launch");
    assert!(reason.contains("stop AWS Batch job coord-1"), "{reason}");
    assert!(!h.parked());
}

#[tokio::test]
async fn a_sweep_that_listed_a_run_another_has_since_dropped_leaves_it() {
    let h = parked().await;
    h.images_ready();
    let (store, run_id) = (h.store.clone(), h.run_id.clone());
    *h.store.after_list_pending.lock().unwrap() = Some(Box::new(move || {
        store.pending.lock().unwrap().remove(run_id.as_str());
    }));
    let report = launch::sweep(&h.state).await.unwrap();
    assert_eq!(report.dropped, 1, "{report:?}");
    assert!(h.cloud.launched.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_launch_whose_answer_was_lost_binds_the_job_batch_created() {
    let h = parked().await;
    h.images_ready();
    // SubmitJob created the job but its answer was lost; Batch lists the job already.
    h.cloud.lose_launch_replies.store(1, std::sync::atomic::Ordering::SeqCst);
    let report = launch::sweep(&h.state).await.unwrap();
    assert_eq!(report.launched, 1, "{report:?}");
    assert_eq!(h.cloud.launched.lock().unwrap().len(), 1);
    assert_eq!(h.bound_coordinator().as_deref(), Some("coord-lost-1"));
    assert!(h.cloud.terminated.lock().unwrap().is_empty());
    assert!(!h.parked());
}

#[tokio::test]
async fn a_launch_whose_answer_was_lost_is_never_launched_beside_the_job_it_may_have_made() {
    let h = parked().await;
    h.images_ready();
    // The answer was lost, and Batch does not list the job it created yet: whether there is one
    // is unknown, so the run waits rather than launch again.
    h.cloud.lose_launch_replies.store(1, std::sync::atomic::Ordering::SeqCst);
    h.cloud.hide_live_coordinators.store(true, std::sync::atomic::Ordering::SeqCst);
    let report = launch::sweep(&h.state).await.unwrap();
    assert_eq!(report.waiting, 1, "{report:?}");
    assert!(h.parked());
    assert!(!h.recorded(Kind::RunSubmitted));
    // Nor while Batch cannot be asked.
    h.cloud.hide_live_coordinators.store(false, std::sync::atomic::Ordering::SeqCst);
    h.cloud.fail_live_coordinators.store(true, std::sync::atomic::Ordering::SeqCst);
    let report = launch::sweep(&h.state).await.unwrap();
    assert_eq!(report.waiting, 1, "{report:?}");
    assert_eq!(h.cloud.launched.lock().unwrap().len(), 1);
    // Once Batch lists it, the next sweep stops it, then launches the run once.
    h.cloud.fail_live_coordinators.store(false, std::sync::atomic::Ordering::SeqCst);
    let report = launch::sweep(&h.state).await.unwrap();
    assert_eq!(report.launched, 1, "{report:?}");
    assert_eq!(h.cloud.launched.lock().unwrap().len(), 2);
    assert_eq!(
        *h.cloud.terminated.lock().unwrap(),
        vec![(
            "coord-lost-1".to_string(),
            "biffix: a launch of the run whose answer was lost started it".to_string()
        )]
    );
    assert_eq!(h.bound_coordinator().as_deref(), Some("coord-1"));
    assert!(!h.parked());
}

#[tokio::test]
async fn a_coordinator_that_cannot_be_stopped_keeps_its_run_from_launching() {
    let h = parked().await;
    h.images_ready();
    h.cloud.lose_launch_replies.store(1, std::sync::atomic::Ordering::SeqCst);
    h.cloud.hide_live_coordinators.store(true, std::sync::atomic::Ordering::SeqCst);
    launch::sweep(&h.state).await.unwrap();
    h.cloud.hide_live_coordinators.store(false, std::sync::atomic::Ordering::SeqCst);
    h.cloud.fail_terminate.store(true, std::sync::atomic::Ordering::SeqCst);
    let report = launch::sweep(&h.state).await.unwrap();
    assert_eq!(report.waiting, 1, "{report:?}");
    assert_eq!(h.cloud.launched.lock().unwrap().len(), 1);
    // Past the bound the run fails, naming the coordinator it could not stop.
    h.age(TIMEOUT + MINUTE);
    let report = launch::sweep(&h.state).await.unwrap();
    assert_eq!(report.failed, 1, "{report:?}");
    let (reason, _) = h.failure().unwrap();
    assert!(reason.contains("stopping coordinator coord-lost-1"), "{reason}");
    assert_eq!(h.cloud.launched.lock().unwrap().len(), 1);
}

/// Park a run whose launch left coordinator `coord-9` unbound, the run then in `state`.
async fn orphaned_in(state: &str) -> Harness {
    use biffix_backend::store::pending::PendingLaunchStore;
    let h = parked().await;
    h.store.mark_pending_launch_orphan(&h.run_id, "coord-9").await.unwrap();
    h.store.insert(testing::run_detail(h.run_id.as_str(), state));
    h
}

#[tokio::test]
async fn an_unbound_coordinator_of_a_run_recorded_after_all_is_its_own_and_left_running() {
    // The record reported failing but landed: the coordinator is the run's.
    for state in ["submitted", "running"] {
        let h = orphaned_in(state).await;
        let report = launch::sweep(&h.state).await.unwrap();
        assert_eq!(report.dropped, 1, "{state}: {report:?}");
        assert!(h.cloud.terminated.lock().unwrap().is_empty(), "{state}");
        assert!(h.failure().is_none(), "{state}");
        assert!(!h.parked(), "{state}");
        assert!(h.cloud.launched.lock().unwrap().is_empty(), "{state}");
    }
}

#[tokio::test]
async fn an_unbound_coordinator_of_a_run_that_ended_is_stopped_and_the_row_dropped() {
    for (state, cancelled, finished) in [("cancelled", 1, 0), ("failed", 0, 1), ("succeeded", 0, 1)]
    {
        let h = orphaned_in(state).await;
        let report = launch::sweep(&h.state).await.unwrap();
        assert_eq!(
            (report.cancelled, report.finished),
            (cancelled, finished),
            "{state}: {report:?}"
        );
        assert_eq!(
            *h.cloud.terminated.lock().unwrap(),
            vec![("coord-9".to_string(), "biffix: the run could not be recorded".to_string())],
            "{state}"
        );
        // Ended already: no `run_failed` of the sweep's own.
        assert!(h.failure().is_none(), "{state}");
        assert!(!h.parked(), "{state}");
    }
}
