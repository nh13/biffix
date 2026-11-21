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

use biffix_backend::aws::AwsError;
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
    parked_with(experiment, false).await
}

const SINGLE_QUEUE: &str = "arn:aws:batch:us-east-1:123:job-queue/biffix-t-single";
const SINGLE_TEMPLATE: &str =
    "arn:aws:batch:us-east-1:123:job-definition/biffix-t-coordinator-single-demo:1";

/// [`parked_in`]; with `single_job`, a single-job run on a deploy with one single-job queue-class
/// (`SINGLE_QUEUE`, c6id.8xlarge) and the project's single-job template (`SINGLE_TEMPLATE`).
async fn parked_with(experiment: Option<&str>, single_job: bool) -> Harness {
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
            single_job_queues: vec![biffix_backend::config::single_job::SingleJobQueue {
                name: "single".into(),
                queue: SINGLE_QUEUE.into(),
                instance_types: vec!["c6id.8xlarge".into()],
            }],
            ..Default::default()
        })
        .with_projects(testing::one_project(biffix_backend::config::projects::ProjectContext {
            single_job_job_definition: Some(SINGLE_TEMPLATE.into()),
            ..testing::project("demo", "test-bucket")
        }))
        .build();
    let mut register = register_body();
    if single_job {
        register["form_schema"]["single_job"] = json!({});
    }
    let (status, _) = post(&state, "/workflows", &register).await;
    assert!(status.is_success(), "register: {status}");
    let mut submit = json!({"workflow_version_id": "abc:hash:0",
            "params": {"reads": "s3://b/in.fastq", "outdir": "s3://b/out/"},
            "images": [SAMTOOLS]});
    if single_job {
        submit["single_job"] = json!({});
    }
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

    /// Set the store's run state to the fold of everything recorded, as Postgres's sink folds each
    /// record (the double only records).
    fn fold(&self) {
        let folded = self.events.folded_state(self.run_id.as_str()).expect("the run was recorded");
        let mut runs = self.store.runs.lock().unwrap();
        runs.get_mut(self.run_id.as_str()).unwrap().run.state = folded.as_str().into();
    }

    /// The coordinator jobs stopped, in order.
    fn terminated(&self) -> Vec<String> {
        self.cloud.terminated.lock().unwrap().iter().map(|(j, _)| j.clone()).collect()
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

/// A parked single-job run is written in the newer plan format (an older biffix leaves it parked
/// rather than launch it one Batch job per task) and launches at the size submit resolved, on its
/// queue-class's queue and the project's single-job template.
#[tokio::test]
async fn a_parked_single_job_run_launches_as_one_job() {
    let h = parked_with(None, true).await;
    let plan = h.store.pending.lock().unwrap()[h.run_id.as_str()].plan.clone();
    assert_eq!(plan["version"], json!(2));
    assert_eq!(plan["single_job"]["instance_type"], "c6id.8xlarge");
    h.images_ready();
    let report = launch::sweep(&h.state).await.unwrap();
    assert_eq!(report.launched, 1, "{report:?}");
    let launch = h.cloud.launched.lock().unwrap()[0].clone();
    assert_eq!(launch.coordinator_queue, SINGLE_QUEUE);
    assert_eq!(launch.single_job.map(|s| s.vcpus), Some(32));
    assert_eq!(h.cloud.variant_templates.lock().unwrap().last().unwrap(), SINGLE_TEMPLATE);
    assert!(h.cloud.task_job_definitions.lock().unwrap().is_empty());

    // A run one job per task still parks in the format every build reads.
    let h = parked().await;
    let plan = h.store.pending.lock().unwrap()[h.run_id.as_str()].plan.clone();
    assert_eq!(plan["version"], json!(1));
    assert!(plan.get("single_job").is_none());
}

/// A parked single-job run is single-job while it waits (its `run_preparing` says so, at the size
/// submit resolved), not only once its anchor is recorded; a run one job per task says nothing.
#[tokio::test]
async fn a_parked_single_job_run_is_single_job_while_it_waits() {
    let preparing = |h: &Harness| {
        h.events
            .events
            .lock()
            .unwrap()
            .iter()
            .find_map(|e| match &e.data {
                EventData::RunPreparing(p) => Some((**p).clone()),
                _ => None,
            })
            .expect("run_preparing recorded")
    };
    let h = parked_with(None, true).await;
    let single = preparing(&h).single_job.expect("run_preparing names the single-job launch");
    assert_eq!(single.queue_class, "single");
    assert_eq!(single.instance_type, "c6id.8xlarge");
    assert_eq!(single.vcpus, 32);
    assert_eq!(h.events.folded().runs[h.run_id.as_str()].single_job, Some(single));

    let h = parked().await;
    assert_eq!(preparing(&h).single_job, None);
}

/// A parked single-job run whose queue-class a deploy removed while it waited fails at launch.
#[tokio::test]
async fn a_parked_single_job_run_whose_queue_class_is_gone_fails() {
    let mut h = parked_with(None, true).await;
    h.state.deploy.single_job_queues.clear();
    h.images_ready();
    let report = launch::sweep(&h.state).await.unwrap();
    assert_eq!(report.failed, 1, "{report:?}");
    let (reason, _) = h.failure().unwrap();
    assert!(reason.contains("`single` is no longer configured"), "{reason}");
    assert!(h.cloud.launched.lock().unwrap().is_empty());
}

/// A parked single-job run whose queue-class a deploy gave a type of another vCPU count while it
/// waited fails at launch: its whole-host job could fit twice on the larger host.
#[tokio::test]
async fn a_parked_single_job_run_whose_queue_class_mixes_vcpu_counts_fails() {
    let mut h = parked_with(None, true).await;
    h.state.deploy.single_job_queues[0].instance_types =
        vec!["c6id.8xlarge".into(), "c6id.16xlarge".into()];
    h.images_ready();
    let report = launch::sweep(&h.state).await.unwrap();
    assert_eq!(report.failed, 1, "{report:?}");
    let (reason, _) = h.failure().unwrap();
    assert!(reason.contains("`c6id.16xlarge` (64)"), "{reason}");
    assert!(h.cloud.launched.lock().unwrap().is_empty());
}

/// A parked single-job run whose queue-class a deploy gave a type EC2 does not describe fails at
/// launch: that host's vCPUs cannot be checked, and Batch may place the run there.
#[tokio::test]
async fn a_parked_single_job_run_whose_queue_class_has_an_undescribed_type_fails() {
    let mut h = parked_with(None, true).await;
    h.state.deploy.single_job_queues[0].instance_types =
        vec!["c6id.8xlarge".into(), "z9.huge".into()];
    h.images_ready();
    let report = launch::sweep(&h.state).await.unwrap();
    assert_eq!(report.failed, 1, "{report:?}");
    let (reason, _) = h.failure().unwrap();
    assert!(reason.contains("EC2 does not describe `z9.huge`"), "{reason}");
    assert!(h.cloud.launched.lock().unwrap().is_empty());
}

/// A parked single-job run whose project lost its single-job template (a deploy removed the
/// project's single-job queue-classes while it waited) fails at launch, before any variant.
#[tokio::test]
async fn a_parked_single_job_run_whose_project_has_no_single_job_template_fails() {
    let mut h = parked_with(None, true).await;
    h.state.projects = testing::one_project(testing::project("demo", "test-bucket"));
    h.images_ready();
    let report = launch::sweep(&h.state).await.unwrap();
    assert_eq!(report.failed, 1, "{report:?}");
    let (reason, _) = h.failure().unwrap();
    assert!(reason.contains("has no single-job job definition"), "{reason}");
    assert!(h.cloud.launched.lock().unwrap().is_empty());
}

/// A deploy change that refuses a parked run for good, and what the refusal says.
type Refusal = (fn(&mut Harness), &'static str);

/// Every refusal a deploy change can make of a parked run after a launch of it whose answer was
/// lost: the run's queue-class removed, given a host its job no longer fills or one EC2 does not
/// describe, its project's single-job template removed, or its version deleted.
const REFUSALS: [Refusal; 5] = [
    (|h| h.state.deploy.single_job_queues.clear(), "`single` is no longer configured"),
    (
        |h| {
            h.state.deploy.single_job_queues[0].instance_types =
                vec!["c6id.8xlarge".into(), "c6id.16xlarge".into()];
        },
        "`c6id.16xlarge` (64)",
    ),
    (
        |h| {
            h.state.deploy.single_job_queues[0].instance_types =
                vec!["c6id.8xlarge".into(), "z9.huge".into()];
        },
        "EC2 does not describe `z9.huge`",
    ),
    (
        |h| h.state.projects = testing::one_project(testing::project("demo", "test-bucket")),
        "has no single-job job definition",
    ),
    (|h| h.registry.versions.lock().unwrap().clear(), "no longer exists"),
];

/// A parked single-job run whose launch's answer was lost before Batch listed the job it made: it
/// waits, its queue recorded as attempted, and that coordinator (`coord-lost-1`) runs unbound.
async fn lost_single_job_launch() -> Harness {
    use std::sync::atomic::Ordering::SeqCst;
    let h = parked_with(None, true).await;
    h.images_ready();
    h.cloud.lose_launch_replies.store(1, SeqCst);
    h.cloud.hide_live_coordinators.store(true, SeqCst);
    let report = launch::sweep(&h.state).await.unwrap();
    assert_eq!(report.waiting, 1, "{report:?}");
    let plan = h.store.pending.lock().unwrap()[h.run_id.as_str()].plan.clone();
    assert_eq!(plan["attempted_queues"], json!([SINGLE_QUEUE]), "{plan}");
    h.cloud.hide_live_coordinators.store(false, SeqCst);
    h
}

/// A run refused for good stops the coordinator an earlier launch of it may have started (on the
/// queue that launch tried) before it fails: failing drops the parked request, and with it the
/// last chance to find that coordinator, which would otherwise run unbound.
#[tokio::test]
async fn a_refused_run_stops_an_earlier_launch_s_coordinator_before_it_fails() {
    for (refuse, want) in REFUSALS {
        let mut h = lost_single_job_launch().await;
        refuse(&mut h);
        let report = launch::sweep(&h.state).await.unwrap();
        assert_eq!(report.failed, 1, "{want}: {report:?}");
        let (reason, _) = h.failure().unwrap();
        assert!(reason.contains(want), "{reason}");
        assert_eq!(
            h.cloud.terminated.lock().unwrap().iter().map(|(j, _)| j.as_str()).collect::<Vec<_>>(),
            ["coord-lost-1"],
            "{want}"
        );
        assert!(!h.parked());
        assert_eq!(h.cloud.launched.lock().unwrap().len(), 1, "{want}");
    }
}

/// A refused run whose earlier launch's coordinator cannot be looked for (Batch unavailable), or
/// cannot be stopped, is not failed yet: it waits, still parked, so a later sweep stops it.
#[tokio::test]
async fn a_refused_run_whose_earlier_coordinator_cannot_be_stopped_waits() {
    use std::sync::atomic::Ordering::SeqCst;
    for (refuse, want) in REFUSALS {
        for unlistable in [true, false] {
            let mut h = lost_single_job_launch().await;
            refuse(&mut h);
            if unlistable {
                h.cloud.fail_live_coordinators.store(true, SeqCst);
            } else {
                h.cloud.fail_terminate.store(true, SeqCst);
            }
            let report = launch::sweep(&h.state).await.unwrap();
            assert_eq!((report.waiting, report.failed), (1, 0), "{want}: {report:?}");
            assert!(h.parked(), "{want}");
            assert!(h.failure().is_none(), "{want}");
            // Once it can be stopped, the run is stopped and fails.
            h.cloud.fail_live_coordinators.store(false, SeqCst);
            h.cloud.fail_terminate.store(false, SeqCst);
            let report = launch::sweep(&h.state).await.unwrap();
            assert_eq!(report.failed, 1, "{want}: {report:?}");
            assert_eq!(h.cloud.terminated.lock().unwrap().len(), 1, "{want}");
            assert!(!h.parked(), "{want}");
        }
    }
}

/// Two runs launched on two queues each keep their own coordinator: Batch lists run A's on the
/// queue it was launched on after run B's launches elsewhere.
#[tokio::test]
async fn each_launch_is_its_own_job_on_its_own_queue() {
    use biffix_backend::aws::batch::BatchJobs;
    const MOVED: &str = "arn:aws:batch:us-east-1:123:job-queue/biffix-t-moved";
    let mut h = parked().await;
    h.images_ready();
    let report = launch::sweep(&h.state).await.unwrap();
    assert_eq!(report.launched, 1, "{report:?}");
    let first = h.bound_coordinator().unwrap();
    h.state.deploy.coordinator_queue = MOVED.into();
    let (status, out) = post(
        &h.state,
        "/runs",
        &json!({"workflow_version_id": "abc:hash:0",
            "params": {"reads": "s3://b/in.fastq", "outdir": "s3://b/out/"},
            "images": [SAMTOOLS]}),
    )
    .await;
    assert!(status.is_success(), "{status}: {out}");
    let second = RunId::new(out["run_id"].as_str().unwrap());
    assert_eq!(h.cloud.launched.lock().unwrap().len(), 2);
    assert_eq!(
        h.cloud.live_coordinators("coordinator", &h.run_id).await.unwrap(),
        std::slice::from_ref(&first)
    );
    let other = h.cloud.live_coordinators(MOVED, &second).await.unwrap();
    assert_eq!(other.len(), 1, "{other:?}");
    assert_ne!(other[0], first);
}

/// Launches made at once each get their own job, and Batch lists on each queue only the jobs
/// launched on it, even while the others are being made: the fake numbers a job, records it and
/// its queue in one critical section. Each round's launches start together (a barrier) and each
/// lists its queue at once, so a job numbered twice, or listed before its queue is recorded (on
/// every queue), shows.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn launches_made_at_once_are_each_their_own_job() {
    use biffix_backend::aws::batch::BatchJobs;
    const ROUNDS: usize = 200;
    const AT_ONCE: usize = 8;
    let h = parked().await;
    h.images_ready();
    let report = launch::sweep(&h.state).await.unwrap();
    assert_eq!(report.launched, 1, "{report:?}");
    let template = h.cloud.launched.lock().unwrap()[0].clone();
    let mut jobs = std::collections::BTreeSet::from([h.bound_coordinator().unwrap()]);
    for round in 0..ROUNDS {
        let start = Arc::new(tokio::sync::Barrier::new(AT_ONCE));
        let launches: Vec<_> = (0..AT_ONCE)
            .map(|i| {
                let (cloud, start) = (Arc::clone(&h.cloud), Arc::clone(&start));
                let mut launch = template.clone();
                launch.coordinator_queue = format!("queue-{round}-{i}");
                tokio::spawn(async move {
                    start.wait().await;
                    let job = cloud.launch_coordinator(&launch).await.unwrap().to_string();
                    let listed = cloud.live_coordinators(&launch.coordinator_queue, &launch.run_id);
                    (job, listed.await.unwrap(), launch.coordinator_queue)
                })
            })
            .collect();
        for launched in launches {
            let (job, listed, queue) = launched.await.unwrap();
            assert!(jobs.insert(job.clone()), "{job} was given twice");
            assert_eq!(listed, [job], "{queue}");
        }
    }
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

/// A run parked across an upgrade whose version's runtime predates the task job definitions biffix
/// registers (its engine would register its own, which no coordinator may): failed at launch with
/// why, never launched to have every task fail at submit.
#[tokio::test]
async fn a_parked_run_whose_runtime_predates_registered_job_definitions_fails() {
    let h = parked().await;
    h.images_ready();
    {
        let mut versions = h.registry.versions.lock().unwrap();
        let version = &mut versions.get_mut("abc:hash:0").unwrap().version;
        version.runtime_recipe["pins"]["executor_sha"] =
            json!("a0aaf7a725bfb5120721413afc920f4a21737c53");
    }
    let report = launch::sweep(&h.state).await.unwrap();
    assert_eq!(report.failed, 1, "{report:?}");
    let (reason, stage) = h.failure().unwrap();
    assert_eq!(stage, "launch");
    assert!(reason.contains("re-register it on the current pins"), "{reason}");
    assert!(!h.parked());
    assert!(h.cloud.launched.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_run_whose_task_job_definitions_cannot_be_prepared_waits_or_fails() {
    // AWS unavailable: parked, and the next sweep launches it.
    let h = parked().await;
    h.images_ready();
    *h.cloud.fail_task_job_definitions.lock().unwrap() =
        Some(|| AwsError::Unavailable("batch throttled".into()));
    let report = launch::sweep(&h.state).await.unwrap();
    assert_eq!(report.waiting, 1, "{report:?}");
    assert!(h.parked());
    assert!(h.failure().is_none());
    assert!(h.cloud.launched.lock().unwrap().is_empty());
    *h.cloud.fail_task_job_definitions.lock().unwrap() = None;
    let report = launch::sweep(&h.state).await.unwrap();
    assert_eq!(report.launched, 1, "{report:?}");
    assert!(!h.parked());

    // Refused: failed, at launch, naming the image.
    let h = parked().await;
    h.images_ready();
    *h.cloud.fail_task_job_definitions.lock().unwrap() =
        Some(|| AwsError::Forbidden("RegisterJobDefinition denied".into()));
    let report = launch::sweep(&h.state).await.unwrap();
    assert_eq!(report.failed, 1, "{report:?}");
    let (reason, stage) = h.failure().unwrap();
    assert_eq!(stage, "launch");
    assert!(
        reason.starts_with(
            "preparing the run's task job definitions failed: the definition of image `"
        ),
        "{reason}"
    );
    assert!(reason.contains("RegisterJobDefinition denied"), "{reason}");
    assert!(!h.parked());
    assert!(h.cloud.launched.lock().unwrap().is_empty());

    // Their map cannot be written (an S3 refusal): failed too, and no coordinator runs.
    let h = parked().await;
    h.images_ready();
    let map_uri = biffix_backend::aws::task_definitions::uri_for("test-bucket", h.run_id.as_str());
    h.cloud.fail_puts.lock().unwrap().insert(map_uri.clone());
    let report = launch::sweep(&h.state).await.unwrap();
    assert_eq!(report.failed, 1, "{report:?}");
    let (reason, stage) = h.failure().unwrap();
    assert_eq!(stage, "launch");
    assert!(
        reason.starts_with(&format!(
            "preparing the run's task job definitions failed: writing their map {map_uri}: "
        )),
        "{reason}"
    );
    assert!(!h.parked());
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
    // Kept, for cleanup only, while coord-1 may run: once it can be stopped, it is, and the
    // request dropped.
    assert!(h.parked());
    h.fold();
    h.cloud.fail_terminate.store(false, std::sync::atomic::Ordering::SeqCst);
    let report = launch::sweep(&h.state).await.unwrap();
    assert_eq!(report.finished, 1, "{report:?}");
    assert_eq!(h.terminated(), ["coord-1"]);
    assert!(!h.parked());
    assert_eq!(h.cloud.launched.lock().unwrap().len(), 1);
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
    // Kept, for cleanup only, while coord-1 may run.
    assert!(h.parked());
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
    assert_eq!(h.bound_coordinator().as_deref(), Some("coord-2"));
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

/// A launch whose answer was lost is looked for on the queue it was made on, not only the deploy's
/// queue now: a deploy that moves the coordinator queue, or a single-job queue-class's, while the
/// run waits would otherwise hide the coordinator that launch may have started, and the run would
/// launch beside it.
#[tokio::test]
async fn a_lost_launch_is_looked_for_on_the_queue_it_tried_after_a_deploy_moves_it() {
    use std::sync::atomic::Ordering::SeqCst;
    const MOVED: &str = "arn:aws:batch:us-east-1:123:job-queue/biffix-t-moved";
    for single_job in [false, true] {
        let mut h = parked_with(None, single_job).await;
        h.images_ready();
        let tried = if single_job { SINGLE_QUEUE } else { "coordinator" };
        h.cloud.lose_launch_replies.store(1, SeqCst);
        h.cloud.hide_live_coordinators.store(true, SeqCst);
        let report = launch::sweep(&h.state).await.unwrap();
        assert_eq!(report.waiting, 1, "{report:?}");
        // Recorded before SubmitJob, so a launch the process does not outlive is still found.
        let plan = h.store.pending.lock().unwrap()[h.run_id.as_str()].plan.clone();
        assert_eq!(plan["attempted_queues"], json!([tried]), "{plan}");
        h.cloud.hide_live_coordinators.store(false, SeqCst);
        if single_job {
            h.state.deploy.single_job_queues[0].queue = MOVED.into();
        } else {
            h.state.deploy.coordinator_queue = MOVED.into();
        }
        let report = launch::sweep(&h.state).await.unwrap();
        assert_eq!(report.launched, 1, "{report:?}");
        assert_eq!(
            h.cloud.terminated.lock().unwrap().iter().map(|(j, _)| j.as_str()).collect::<Vec<_>>(),
            ["coord-lost-1"]
        );
        let launched = h.cloud.launched.lock().unwrap();
        assert_eq!(launched.len(), 2);
        assert_eq!(launched[1].coordinator_queue, MOVED);
        assert!(h.cloud.live_coordinator_queues.lock().unwrap().iter().any(|q| q == tried));
        assert!(!h.parked());
    }
}

/// An earlier launch's queue that Batch no longer lists (a deploy deleted it, and Batch terminates a
/// deleted queue's jobs) does not keep the run from launching; one Batch cannot be asked about now
/// does, as the current queue does.
#[tokio::test]
async fn an_earlier_launch_s_deleted_queue_is_passed_over_but_an_unreachable_one_waits() {
    use std::sync::atomic::Ordering::SeqCst;
    const MOVED: &str = "arn:aws:batch:us-east-1:123:job-queue/biffix-t-moved";
    let mut h = parked().await;
    h.images_ready();
    h.cloud.fail_launch.lock().unwrap().replace(true);
    let report = launch::sweep(&h.state).await.unwrap();
    assert_eq!(report.waiting, 1, "{report:?}");
    *h.cloud.fail_launch.lock().unwrap() = None;
    h.state.deploy.coordinator_queue = MOVED.into();
    h.cloud.fail_live_coordinators.store(true, SeqCst);
    let report = launch::sweep(&h.state).await.unwrap();
    assert_eq!(report.waiting, 1, "{report:?}");
    h.cloud.fail_live_coordinators.store(false, SeqCst);
    h.cloud.missing_queues.lock().unwrap().insert("coordinator".into());
    let report = launch::sweep(&h.state).await.unwrap();
    assert_eq!(report.launched, 1, "{report:?}");
    assert_eq!(h.cloud.launched.lock().unwrap().last().unwrap().coordinator_queue, MOVED);
}

/// An earlier launch's queue Batch refuses to list with an opaque error (a denied listing can read
/// as a 400 `ClientException`, not an access-denied code) is passed over only once DescribeJobQueues
/// says it is gone (no such queue, or deleted): a queue that still exists, or one Batch cannot be
/// asked about, keeps the run parked, launched (or released, for one that will never launch) only
/// once it can be listed.
#[tokio::test]
async fn an_earlier_launch_s_unlistable_queue_is_passed_over_only_when_batch_says_it_is_gone() {
    use std::sync::atomic::Ordering::SeqCst;
    const MOVED: &str = "arn:aws:batch:us-east-1:123:job-queue/biffix-t-moved";
    for ended in [false, true] {
        let mut h = parked().await;
        h.images_ready();
        h.cloud.fail_launch.lock().unwrap().replace(true);
        let report = launch::sweep(&h.state).await.unwrap();
        assert_eq!(report.waiting, 1, "{report:?}");
        *h.cloud.fail_launch.lock().unwrap() = None;
        h.state.deploy.coordinator_queue = MOVED.into();
        if ended {
            h.store.insert(testing::run_detail(h.run_id.as_str(), "failed"));
        }
        let kept = |report: &launch::LaunchSweepReport| {
            if ended {
                report.cleaning_up
            } else {
                report.waiting
            }
        };
        // It exists (its listing denied): kept, not launched beside what it may run.
        h.cloud.unlistable_queues.lock().unwrap().insert("coordinator".into());
        let report = launch::sweep(&h.state).await.unwrap();
        assert_eq!(kept(&report), 1, "ended: {ended}: {report:?}");
        assert!(h.parked(), "ended: {ended}");
        assert_eq!(*h.cloud.job_queue_gone_asks.lock().unwrap(), ["coordinator"]);
        // Batch cannot say whether it is gone: kept too.
        h.cloud.fail_job_queue_gone.store(true, SeqCst);
        let report = launch::sweep(&h.state).await.unwrap();
        assert_eq!(kept(&report), 1, "ended: {ended}: {report:?}");
        assert!(h.parked(), "ended: {ended}");
        h.cloud.fail_job_queue_gone.store(false, SeqCst);
        assert!(h.cloud.launched.lock().unwrap().is_empty(), "ended: {ended}");
        // Gone (deleted): passed over.
        h.cloud.missing_queues.lock().unwrap().insert("coordinator".into());
        let report = launch::sweep(&h.state).await.unwrap();
        if ended {
            assert_eq!(report.finished, 1, "{report:?}");
            assert!(h.cloud.launched.lock().unwrap().is_empty());
        } else {
            assert_eq!(report.launched, 1, "{report:?}");
            assert_eq!(h.cloud.launched.lock().unwrap().last().unwrap().coordinator_queue, MOVED);
        }
        assert!(!h.parked(), "ended: {ended}");
        // The deploy's queue for the run is never asked about: it is never passed over.
        assert!(!h.cloud.job_queue_gone_asks.lock().unwrap().iter().any(|q| q == MOVED));
    }
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

/// An unbound coordinator of a run that ended, which cannot be stopped, keeps the run's request,
/// for cleanup only (scrubbed of the run's params), until a later sweep stops it.
#[tokio::test]
async fn an_unbound_coordinator_that_cannot_be_stopped_keeps_its_request_until_it_is() {
    use std::sync::atomic::Ordering::SeqCst;
    for (state, cancelled, finished) in [("cancelled", 1, 0), ("failed", 0, 1)] {
        let h = orphaned_in(state).await;
        h.cloud.fail_terminate.store(true, SeqCst);
        let report = launch::sweep(&h.state).await.unwrap();
        assert_eq!(report.cleaning_up, 1, "{state}: {report:?}");
        assert!(h.parked(), "{state}: kept while coord-9 may run");
        let row = h.store.pending.lock().unwrap()[h.run_id.as_str()].clone();
        assert_eq!(row.orphan_job_id(), Some("coord-9"), "{state}");
        assert!(row.plan.get("params").is_none(), "{state}: {}", row.plan);
        h.cloud.fail_terminate.store(false, SeqCst);
        let report = launch::sweep(&h.state).await.unwrap();
        assert_eq!(
            (report.cancelled, report.finished),
            (cancelled, finished),
            "{state}: {report:?}"
        );
        assert_eq!(h.terminated(), ["coord-9"], "{state}");
        assert!(h.failure().is_none(), "{state}");
        assert!(!h.parked(), "{state}");
    }
}

/// The unbound coordinator a run's request names is stopped once a sweep: Batch still lists it
/// (on the queue its launch was submitted to) until it ends, and the queue's own listing leaves it
/// alone rather than stop it again, or count it as another coordinator.
#[tokio::test]
async fn an_unbound_coordinator_stopped_by_id_is_not_stopped_again_from_its_queue() {
    use std::sync::atomic::Ordering::SeqCst;
    const QUEUE: &str = "arn:aws:batch:us-east-1:123:job-queue/biffix-t-coordinator";
    let h = orphaned_in("cancelled").await;
    h.cloud.coordinators.lock().unwrap().push(("coord-9".into(), h.run_id.to_string()));
    h.cloud.coordinator_queues.lock().unwrap().insert("coord-9".into(), QUEUE.into());
    {
        let mut pending = h.store.pending.lock().unwrap();
        let row = pending.get_mut(h.run_id.as_str()).unwrap();
        row.plan["attempted_queues"] = json!([QUEUE]);
    }
    h.cloud.list_terminated.store(true, SeqCst);
    let report = launch::sweep(&h.state).await.unwrap();
    assert_eq!(report.cancelled, 1, "{report:?}");
    assert_eq!(h.terminated(), ["coord-9"]);
    // And the deploy's queue for the run, looked on always.
    assert_eq!(*h.cloud.live_coordinator_queues.lock().unwrap(), [QUEUE, "coordinator"]);
    assert!(!h.parked());
}

/// A run failed because its launch left a coordinator it cannot stop is failed once: while that
/// coordinator still cannot be stopped later sweeps keep its request, for cleanup only, and never
/// fail it again, though the run's end is not folded yet (or the run was never recorded).
#[tokio::test]
async fn an_unbound_coordinator_s_run_is_failed_once_while_it_cannot_be_stopped() {
    use std::sync::atomic::Ordering::SeqCst;
    for (state, then_dropped) in [("preparing_images", false), ("unrecorded", true)] {
        let h = orphaned_in("preparing_images").await;
        if state == "unrecorded" {
            h.store.runs.lock().unwrap().clear();
        }
        h.cloud.fail_terminate.store(true, SeqCst);
        let report = launch::sweep(&h.state).await.unwrap();
        assert_eq!(report.failed, 1, "{state}: {report:?}");
        assert!(h.parked(), "{state}");
        let report = launch::sweep(&h.state).await.unwrap();
        assert_eq!((report.cleaning_up, report.failed), (1, 0), "{state}: {report:?}");
        h.cloud.fail_terminate.store(false, SeqCst);
        let report = launch::sweep(&h.state).await.unwrap();
        let swept = if then_dropped { report.dropped } else { report.finished };
        assert_eq!(swept, 1, "{state}: {report:?}");
        assert_eq!(h.terminated(), ["coord-9"], "{state}");
        assert!(!h.parked(), "{state}");
        let failures =
            h.events.events.lock().unwrap().iter().filter(|e| e.kind() == Kind::RunFailed).count();
        assert_eq!(failures, 1, "{state}");
    }
}

/// How long after it was parked the sweep keeps trying to stop a coordinator an earlier launch of
/// a run that will never launch may have left running (`launch::CLEANUP_GIVE_UP_AFTER`: 7 days),
/// restated so a bound that drifts from it fails these tests.
const CLEANUP_GIVE_UP_AFTER: Duration = Duration::from_secs(7 * 24 * 3600);

/// One way a parked run fails: its name, and what makes it fail on the next sweep.
type Failing = (&'static str, fn(&Harness));

/// The ways a parked run fails while the coordinator an earlier launch of it started cannot be
/// stopped: it times out waiting (its launch held back by that coordinator), it times out on a
/// step that keeps failing, or its launch request no longer reads.
const FAILS_WHILE_UNSTOPPABLE: [Failing; 3] = [
    ("timed out waiting", |h| h.age(TIMEOUT + MINUTE)),
    ("timed out erring", |h| {
        h.registry.versions.lock().unwrap().get_mut("abc:hash:0").unwrap().version.runtime_recipe =
            json!("not a recipe");
        h.age(TIMEOUT + MINUTE);
    }),
    ("unreadable", |h| {
        h.store.pending.lock().unwrap().get_mut(h.run_id.as_str()).unwrap().plan =
            json!({"attempted_queues": [SINGLE_QUEUE]});
    }),
];

/// A run that fails while the coordinator an earlier launch of it started cannot be stopped fails
/// on time, but its parked request is kept, for cleanup only: later sweeps never launch it, and keep
/// trying to stop that coordinator; once one does, the request is dropped.
#[tokio::test]
async fn a_run_failed_while_an_earlier_coordinator_cannot_be_stopped_keeps_stopping_it() {
    use std::sync::atomic::Ordering::SeqCst;
    for (how, fails) in FAILS_WHILE_UNSTOPPABLE {
        let h = lost_single_job_launch().await;
        h.cloud.fail_terminate.store(true, SeqCst);
        fails(&h);
        let report = launch::sweep(&h.state).await.unwrap();
        assert_eq!(report.failed, 1, "{how}: {report:?}");
        assert!(h.failure().is_some(), "{how}");
        assert!(h.parked(), "{how}: kept while coord-lost-1 may run");
        assert_eq!(h.cloud.launched.lock().unwrap().len(), 1, "{how}");
        // Kept for cleanup only: scrubbed of the run's params (secret values), naming only what
        // the cleanup reads.
        let plan = h.store.pending.lock().unwrap()[h.run_id.as_str()].plan.clone();
        assert!(plan.get("params").is_none(), "{how}: {plan}");
        assert_eq!(plan["attempted_queues"], json!([SINGLE_QUEUE]), "{how}: {plan}");
        // Still unstoppable: kept, never launched, and not failed again, before the run's end is
        // folded and after.
        for fold in [false, true] {
            if fold {
                h.fold();
            }
            let report = launch::sweep(&h.state).await.unwrap();
            assert_eq!((report.cleaning_up, report.failed), (1, 0), "{how}: {report:?}");
            assert!(h.parked(), "{how}");
        }
        h.cloud.fail_terminate.store(false, SeqCst);
        let report = launch::sweep(&h.state).await.unwrap();
        assert_eq!(report.finished, 1, "{how}: {report:?}");
        assert_eq!(h.terminated(), ["coord-lost-1"], "{how}");
        assert!(!h.parked(), "{how}");
        assert_eq!(h.cloud.launched.lock().unwrap().len(), 1, "{how}");
        assert_eq!(
            h.events.events.lock().unwrap().iter().filter(|e| e.kind() == Kind::RunFailed).count(),
            1,
            "{how}"
        );
    }
}

/// Cancelling a parked run that an earlier launch may have left a coordinator for cancels it at
/// once, but leaves stopping that coordinator to the next sweep (no AWS call inside the cancel's
/// request): its request is kept, for the sweep to stop the coordinator, never to launch the run.
/// So is one whose launch left a coordinator it knows by id (stopped by id, though Batch does not
/// list it yet).
#[tokio::test]
async fn a_cancelled_run_keeps_stopping_an_earlier_launch_s_coordinator() {
    use std::sync::atomic::Ordering::SeqCst;
    let cancel = |h: &Harness| {
        let (state, uri) = (h.state.clone(), format!("/runs/{}/cancel", h.run_id));
        async move { post(&state, &uri, &json!({})).await }
    };
    for unstoppable in [false, true] {
        let h = lost_single_job_launch().await;
        h.cloud.fail_terminate.store(unstoppable, SeqCst);
        let asked = h.cloud.live_coordinator_queues.lock().unwrap().len();
        let (status, out) = cancel(&h).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{out}");
        assert!(h.recorded(Kind::RunCancelled));
        assert!(h.parked(), "left for the sweep");
        assert_eq!(h.cloud.live_coordinator_queues.lock().unwrap().len(), asked, "no listing");
        assert!(h.terminated().is_empty());
        h.fold();
        if unstoppable {
            let report = launch::sweep(&h.state).await.unwrap();
            assert_eq!(report.cleaning_up, 1, "{report:?}");
            assert!(h.parked(), "kept while coord-lost-1 may run");
            h.cloud.fail_terminate.store(false, SeqCst);
        }
        let report = launch::sweep(&h.state).await.unwrap();
        assert_eq!(report.cancelled, 1, "{report:?}");
        assert_eq!(h.terminated(), ["coord-lost-1"]);
        assert!(!h.parked());
        assert_eq!(h.cloud.launched.lock().unwrap().len(), 1);
    }

    let h = orphaned_in("preparing_images").await;
    h.cloud.hide_live_coordinators.store(true, SeqCst);
    let (status, out) = cancel(&h).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{out}");
    assert!(h.parked(), "left for the sweep");
    assert!(h.terminated().is_empty());
    h.fold();
    let report = launch::sweep(&h.state).await.unwrap();
    assert_eq!(report.cancelled, 1, "{report:?}");
    assert_eq!(h.terminated(), ["coord-9"]);
    assert!(!h.parked());
    assert!(h.failure().is_none());
}

/// Each sweep looks on every queue an earlier launch of the run tried: one Batch cannot list now
/// does not keep it from stopping a coordinator on another, though the run stays parked (kept for
/// cleanup, or waiting to fail) until that one can be listed too.
#[tokio::test]
async fn every_queue_an_earlier_launch_tried_is_looked_on_though_one_cannot_be_listed() {
    const UNREACHABLE: &str = "arn:aws:batch:us-east-1:123:job-queue/biffix-t-unreachable";
    for refused in [false, true] {
        let mut h = lost_single_job_launch().await;
        h.store.pending.lock().unwrap().get_mut(h.run_id.as_str()).unwrap().plan
            ["attempted_queues"] = json!([UNREACHABLE, SINGLE_QUEUE]);
        h.cloud.unavailable_queues.lock().unwrap().insert(UNREACHABLE.into());
        if refused {
            (REFUSALS[0].0)(&mut h);
        } else {
            h.store.insert(testing::run_detail(h.run_id.as_str(), "failed"));
        }
        let report = launch::sweep(&h.state).await.unwrap();
        if refused {
            assert_eq!((report.waiting, report.failed), (1, 0), "{report:?}");
        } else {
            assert_eq!(report.cleaning_up, 1, "{report:?}");
        }
        assert_eq!(h.terminated(), ["coord-lost-1"], "refused: {refused}");
        assert!(h.parked(), "refused: {refused}");
        h.cloud.unavailable_queues.lock().unwrap().clear();
        let report = launch::sweep(&h.state).await.unwrap();
        let swept = if refused { report.failed } else { report.finished };
        assert_eq!(swept, 1, "refused: {refused}: {report:?}");
        assert_eq!(h.terminated(), ["coord-lost-1"], "refused: {refused}");
        assert!(!h.parked(), "refused: {refused}");
    }
}

/// The queue the deploy uses for the run now is never passed over when Batch refuses to list it
/// (an opaque error, as for a deleted queue): a run that will never launch keeps its request,
/// sweep after sweep. A queue the deploy no longer uses is passed over, and the request dropped.
#[tokio::test]
async fn the_deploy_s_queue_for_the_run_is_never_passed_over_when_batch_refuses_to_list_it() {
    const MOVED: &str = "arn:aws:batch:us-east-1:123:job-queue/biffix-t-moved";
    for moved in [false, true] {
        let mut h = lost_single_job_launch().await;
        h.store.insert(testing::run_detail(h.run_id.as_str(), "failed"));
        h.cloud.missing_queues.lock().unwrap().insert(SINGLE_QUEUE.into());
        if moved {
            h.state.deploy.single_job_queues[0].queue = MOVED.into();
            let report = launch::sweep(&h.state).await.unwrap();
            assert_eq!(report.finished, 1, "{report:?}");
            assert!(!h.parked());
            continue;
        }
        // Scrubbed after the first, the request still names the run's queue-class.
        for _ in 0..2 {
            let report = launch::sweep(&h.state).await.unwrap();
            assert_eq!(report.cleaning_up, 1, "{report:?}");
            assert!(h.parked());
        }
    }
}

/// A parked run that ended otherwise (cancelled, failed or succeeded), or was never recorded, has
/// its request kept while the coordinator an earlier launch of it started cannot be stopped (or
/// looked for), and dropped once it is stopped.
#[tokio::test]
async fn a_run_that_will_never_launch_keeps_its_request_until_an_earlier_coordinator_is_stopped() {
    use std::sync::atomic::Ordering::SeqCst;
    for state in ["cancelled", "failed", "succeeded", "unrecorded"] {
        for unlistable in [false, true] {
            let h = lost_single_job_launch().await;
            if state == "unrecorded" {
                h.store.runs.lock().unwrap().clear();
                h.age(ORPHAN_AFTER + MINUTE);
            } else {
                h.store.insert(testing::run_detail(h.run_id.as_str(), state));
            }
            if unlistable {
                h.cloud.fail_live_coordinators.store(true, SeqCst);
            } else {
                h.cloud.fail_terminate.store(true, SeqCst);
            }
            let report = launch::sweep(&h.state).await.unwrap();
            assert_eq!(report.cleaning_up, 1, "{state}: {report:?}");
            assert!(h.parked(), "{state}");
            assert!(h.terminated().is_empty(), "{state}");
            h.cloud.fail_live_coordinators.store(false, SeqCst);
            h.cloud.fail_terminate.store(false, SeqCst);
            let report = launch::sweep(&h.state).await.unwrap();
            let swept = match state {
                "cancelled" => report.cancelled,
                "unrecorded" => report.dropped,
                _ => report.finished,
            };
            assert_eq!(swept, 1, "{state}: {report:?}");
            assert_eq!(h.terminated(), ["coord-lost-1"], "{state}");
            assert!(!h.parked(), "{state}");
            assert_eq!(h.cloud.launched.lock().unwrap().len(), 1, "{state}");
            assert!(h.failure().is_none(), "{state}");
        }
    }
}

/// The sweep gives up stopping an earlier launch's coordinator `CLEANUP_GIVE_UP_AFTER` after the run
/// was parked: the request is dropped, not kept for ever.
#[tokio::test]
async fn the_sweep_gives_up_stopping_an_earlier_coordinator_after_a_week() {
    use std::sync::atomic::Ordering::SeqCst;
    let h = lost_single_job_launch().await;
    h.store.insert(testing::run_detail(h.run_id.as_str(), "failed"));
    h.cloud.fail_terminate.store(true, SeqCst);
    h.age(CLEANUP_GIVE_UP_AFTER - MINUTE);
    let report = launch::sweep(&h.state).await.unwrap();
    assert_eq!(report.cleaning_up, 1, "{report:?}");
    assert!(h.parked());
    h.age(2 * MINUTE);
    let report = launch::sweep(&h.state).await.unwrap();
    assert_eq!(report.finished, 1, "{report:?}");
    assert!(!h.parked());
    assert!(h.terminated().is_empty());
    assert_eq!(h.cloud.launched.lock().unwrap().len(), 1);
}

/// A run whose launch was never attempted is dropped once Batch lists nothing of it on the deploy's
/// queue for it (a plan from before `attempted_queues` was recorded reads the same, so that queue
/// is always looked on), however it ends: one listing, of the current queue alone. A cancel never
/// drops the request itself (no AWS call inside its request): the next sweep does. One already
/// launched is dropped at once: its live coordinator is its own (every earlier one was stopped
/// before that launch).
#[tokio::test]
async fn a_run_with_no_attempted_launch_is_dropped_after_one_listing_or_at_once_if_launched() {
    for state in ["cancelled", "failed", "unrecorded", "timeout", "cancel"] {
        let h = parked().await;
        match state {
            "unrecorded" => {
                h.store.runs.lock().unwrap().clear();
                h.age(ORPHAN_AFTER + MINUTE);
            }
            "timeout" => h.age(TIMEOUT + MINUTE),
            "cancel" => {
                let uri = format!("/runs/{}/cancel", h.run_id);
                let (status, out) = post(&h.state, &uri, &json!({})).await;
                assert_eq!(status, StatusCode::ACCEPTED, "{out}");
                assert!(h.parked(), "left for the sweep");
                assert!(h.cloud.live_coordinator_queues.lock().unwrap().is_empty());
                h.fold();
            }
            _ => h.store.insert(testing::run_detail(h.run_id.as_str(), state)),
        }
        let report = launch::sweep(&h.state).await.unwrap();
        let swept = match state {
            "timeout" => report.failed,
            "cancelled" | "cancel" => report.cancelled,
            "failed" => report.finished,
            "unrecorded" => report.dropped,
            _ => unreachable!("{state}"),
        };
        assert_eq!((swept, report.cleaning_up), (1, 0), "{state}: {report:?}");
        assert!(!h.parked(), "{state}");
        assert_eq!(*h.cloud.live_coordinator_queues.lock().unwrap(), ["coordinator"], "{state}");
        assert!(h.terminated().is_empty(), "{state}");
    }
    // Launched already: its coordinator is its own, left running.
    let h = lost_single_job_launch().await;
    h.store.insert(testing::run_detail(h.run_id.as_str(), "running"));
    let report = launch::sweep(&h.state).await.unwrap();
    assert_eq!(report.dropped, 1, "{report:?}");
    assert!(!h.parked());
    assert!(h.terminated().is_empty());
}

/// A run whose launch's answer was lost before Batch listed the coordinator it made
/// (`coord-lost-1`, on the deploy's coordinator queue), parked by a biffix from before
/// `attempted_queues` was recorded: its plan names no queue it tried, as one that tried none.
async fn pre_upgrade_lost_launch() -> Harness {
    use std::sync::atomic::Ordering::SeqCst;
    let h = parked().await;
    h.images_ready();
    h.cloud.lose_launch_replies.store(1, SeqCst);
    h.cloud.hide_live_coordinators.store(true, SeqCst);
    let report = launch::sweep(&h.state).await.unwrap();
    assert_eq!(report.waiting, 1, "{report:?}");
    h.cloud.hide_live_coordinators.store(false, SeqCst);
    let mut pending = h.store.pending.lock().unwrap();
    let plan = pending.get_mut(h.run_id.as_str()).unwrap().plan.as_object_mut().unwrap();
    assert!(plan.remove("attempted_queues").is_some());
    drop(pending);
    h
}

/// A pre-upgrade plan's lost launch is looked for on the deploy's queue for the run when the run
/// times out: its request is kept while that coordinator cannot be stopped, then it is stopped and
/// the request dropped.
#[tokio::test]
async fn a_pre_upgrade_lost_launch_is_stopped_on_the_current_queue_when_its_run_times_out() {
    use std::sync::atomic::Ordering::SeqCst;
    let h = pre_upgrade_lost_launch().await;
    h.cloud.fail_terminate.store(true, SeqCst);
    h.age(TIMEOUT + MINUTE);
    let report = launch::sweep(&h.state).await.unwrap();
    assert_eq!(report.failed, 1, "{report:?}");
    assert!(h.parked(), "kept while coord-lost-1 may run");
    let report = launch::sweep(&h.state).await.unwrap();
    assert_eq!((report.cleaning_up, report.failed), (1, 0), "{report:?}");
    assert!(h.parked());
    h.cloud.fail_terminate.store(false, SeqCst);
    let report = launch::sweep(&h.state).await.unwrap();
    assert_eq!(report.finished, 1, "{report:?}");
    assert_eq!(h.terminated(), ["coord-lost-1"]);
    assert!(!h.parked());
    assert_eq!(h.cloud.launched.lock().unwrap().len(), 1);
}

/// Cancelling a run with a pre-upgrade plan's lost launch leaves its request to the sweep, which
/// keeps it while the coordinator on the deploy's queue for the run cannot be stopped, then stops
/// it and drops the request: never dropped while that coordinator may run.
#[tokio::test]
async fn a_cancelled_pre_upgrade_lost_launch_is_kept_until_its_coordinator_is_stopped() {
    use std::sync::atomic::Ordering::SeqCst;
    let h = pre_upgrade_lost_launch().await;
    let uri = format!("/runs/{}/cancel", h.run_id);
    let (status, out) = post(&h.state, &uri, &json!({})).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{out}");
    assert!(h.parked(), "left for the sweep");
    h.fold();
    h.cloud.fail_terminate.store(true, SeqCst);
    let report = launch::sweep(&h.state).await.unwrap();
    assert_eq!(report.cleaning_up, 1, "{report:?}");
    assert!(h.parked(), "kept while coord-lost-1 may run");
    h.cloud.fail_terminate.store(false, SeqCst);
    let report = launch::sweep(&h.state).await.unwrap();
    assert_eq!(report.cancelled, 1, "{report:?}");
    assert_eq!(h.terminated(), ["coord-lost-1"]);
    assert!(!h.parked());
    assert_eq!(h.cloud.launched.lock().unwrap().len(), 1);
}
