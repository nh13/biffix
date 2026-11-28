//! Submitting a single-job run: the whole run as one Batch job on one host, on the deploy's
//! single-job queue-class and the project's single-job template.

use super::*;

use biffix_backend::config::single_job::SingleJobQueue;

const SINGLE_QUEUE: &str = "arn:aws:batch:us-east-1:123:job-queue/biffix-t-single";
const SINGLE_TEMPLATE: &str =
    "arn:aws:batch:us-east-1:123:job-definition/biffix-t-coordinator-single-demo:1";

/// A deploy with one single-job queue-class (`queues`, else `single` of c6id.8xlarge and
/// m6id.8xlarge) and, when `template`, the project's single-job template.
fn single_job_state(
    queues: Option<Vec<SingleJobQueue>>,
    template: bool,
) -> (AppState, Arc<MemEventSink>, Arc<FakeCloud>) {
    let (mut state, events, cloud) = state();
    let queues = queues.unwrap_or_else(|| {
        vec![SingleJobQueue {
            name: "single".into(),
            queue: SINGLE_QUEUE.into(),
            instance_types: vec!["m6id.8xlarge".into(), "c6id.8xlarge".into()],
        }]
    });
    let mut project = testing::project("demo", "test-bucket");
    project.single_job_job_definition = template.then(|| SINGLE_TEMPLATE.to_string());
    state.deploy.single_job_queues = queues;
    state.projects = testing::one_project(project);
    (state, events, cloud)
}

/// `register_body()` whose version allows single-job runs with `single_job` as its default.
fn allowing(single_job: Value) -> Value {
    let mut body = register_body();
    body["form_schema"]["single_job"] = single_job;
    body
}

fn submit_body(single_job: Value) -> Value {
    json!({
        "workflow_version_id": "abc:hash:0",
        "params": {"reads": "s3://b/in.fastq", "outdir": "s3://b/out/"},
        "single_job": single_job,
    })
}

async fn submit(state: AppState, register: Value, submit: Value) -> (StatusCode, String) {
    let app = api::router(state);
    setup(&app, "/workflows", "developers", &register).await;
    let res = app.oneshot(post("/runs", Some("operators"), &submit)).await.unwrap();
    let status = res.status();
    (status, text_body(res).await)
}

#[tokio::test]
async fn a_single_job_run_launches_alone_on_its_queue_class_from_the_single_job_template() {
    let (state, events, cloud) = single_job_state(None, true);
    let (status, body) = submit(state, allowing(json!({})), submit_body(json!({}))).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");

    let launches = cloud.launched.lock().unwrap().clone();
    assert_eq!(launches.len(), 1);
    let launch = &launches[0];
    assert_eq!(launch.coordinator_queue, SINGLE_QUEUE);
    // No size given anywhere: the queue-class's smallest type, the whole host.
    let resources = launch.single_job.expect("a single-job launch");
    assert_eq!(resources.vcpus, 32);
    assert_eq!(resources.memory_mib, 61_184);
    // Its variant is a clone of the project's single-job template.
    assert_eq!(cloud.variant_templates.lock().unwrap().last().unwrap(), SINGLE_TEMPLATE);
    // ... as a single-job variant (named apart, so IAM keeps it to the single-job queues).
    assert_eq!(cloud.variant_single_job.lock().unwrap().last(), Some(&true));
    // ... and the coordinator was launched on that clone.
    assert!(
        launch.variant.job_definition.ends_with("-biffix-t-coordinator-single-demo:1"),
        "{}",
        launch.variant.job_definition
    );
    // Its tasks run inside it: no task job definition, no map of them; the image map still.
    assert!(cloud.task_job_definitions.lock().unwrap().is_empty());
    let maps = launch.maps.as_ref().expect("the image map still");
    assert_eq!(maps.job_definitions, None);

    let anchor = recorded_anchor(&events);
    let used = anchor.single_job.expect("the anchor records the single-job launch");
    assert_eq!(used.queue_class, "single");
    assert_eq!(used.instance_type, "c6id.8xlarge");
    assert_eq!((used.vcpus, used.memory_mib), (resources.vcpus, resources.memory_mib));
}

#[tokio::test]
async fn the_runs_size_overrides_the_versions_default() {
    let (state, events, _cloud) = single_job_state(None, true);
    let register = allowing(json!({"instance": "c6id.8xlarge"}));
    let (status, body) =
        submit(state, register, submit_body(json!({"cpus": 16, "memory_gib": 100}))).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    assert_eq!(recorded_anchor(&events).single_job.unwrap().instance_type, "m6id.8xlarge");
}

#[tokio::test]
async fn a_run_naming_the_default_queue_class_keeps_the_versions_size() {
    // The version names no queue-class, so its size is for the deploy's default (`single`).
    let (state, events, _cloud) = single_job_state(None, true);
    let register = allowing(json!({"instance": "m6id.8xlarge"}));
    let (status, body) =
        submit(state, register, submit_body(json!({"queue_class": "single"}))).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    assert_eq!(recorded_anchor(&events).single_job.unwrap().instance_type, "m6id.8xlarge");
}

#[tokio::test]
async fn a_version_with_an_invalid_single_job_size_is_not_registered() {
    let (state, _, _) = single_job_state(None, true);
    let app = api::router(state);
    let register = allowing(json!({"cpus": 8}));
    let res = app.oneshot(post("/workflows", Some("developers"), &register)).await.unwrap();
    let status = res.status();
    let body = text_body(res).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(body.contains("cpus and memory_gib must be given together"), "{body}");
}

#[tokio::test]
async fn a_run_one_job_per_task_is_unchanged() {
    let (state, events, cloud) = single_job_state(None, true);
    let mut body = submit_body(json!({}));
    body.as_object_mut().unwrap().remove("single_job");
    let (status, text) = submit(state, allowing(json!({})), body).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{text}");
    let launch = cloud.launched.lock().unwrap()[0].clone();
    assert_eq!(launch.coordinator_queue, "coordinator");
    assert_eq!(launch.single_job, None);
    assert_eq!(recorded_anchor(&events).single_job, None);
}

#[tokio::test]
async fn a_single_job_run_the_version_or_deploy_cannot_serve_is_refused() {
    // The version does not allow it.
    let (state, _, cloud) = single_job_state(None, true);
    let (status, body) = submit(state, register_body(), submit_body(json!({}))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body.contains("does not allow single-job runs"), "{body}");
    assert!(cloud.launched.lock().unwrap().is_empty());

    // An instance type the queue-class does not run.
    let (state, _, _) = single_job_state(None, true);
    let (status, body) =
        submit(state, allowing(json!({})), submit_body(json!({"instance": "r6id.8xlarge"}))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body.contains("is not one of single-job queue-class `single`"), "{body}");

    // A queue-class the deploy does not have.
    let (state, _, _) = single_job_state(None, true);
    let (status, body) =
        submit(state, allowing(json!({})), submit_body(json!({"queue_class": "huge"}))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body.contains("no single-job queue-class `huge`"), "{body}");

    // A deploy with no single-job queue-class at all.
    let (state, _, _) = single_job_state(Some(vec![]), true);
    let (status, body) = submit(state, allowing(json!({})), submit_body(json!({}))).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(body.contains("this deploy has no single-job queue-class"), "{body}");

    // A project the deploy gave no single-job template.
    let (state, _, cloud) = single_job_state(None, false);
    let (status, body) = submit(state, allowing(json!({})), submit_body(json!({}))).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(body.contains("has no single-job job definition"), "{body}");
    assert!(cloud.launched.lock().unwrap().is_empty());

    // A queue-class one of whose instance types EC2 describes as Graviton beside an x86_64 one,
    // or with a GPU: Batch could place the run there.
    for (arch, accelerator, why) in [
        ("arm64", None, "mixes host architectures"),
        ("x86_64", Some("GPU"), "carries an accelerator"),
    ] {
        let (state, _, cloud) = single_job_state(None, true);
        {
            let mut types = cloud.instance_types.lock().unwrap();
            let m6id = types.get_mut("m6id.8xlarge").unwrap();
            m6id.architectures = vec![arch.into()];
            m6id.accelerators = accelerator.into_iter().map(String::from).collect();
        }
        let (status, body) = submit(state, allowing(json!({})), submit_body(json!({}))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(body.contains("`m6id.8xlarge`") && body.contains(why), "{body}");
        assert!(cloud.launched.lock().unwrap().is_empty());
    }

    // A partial size.
    let (state, _, _) = single_job_state(None, true);
    let (status, body) = submit(state, allowing(json!({})), submit_body(json!({"cpus": 8}))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body.contains("cpus and memory_gib must be given together"), "{body}");
}

#[tokio::test]
async fn a_single_job_queue_with_ambiguous_architecture_is_refused_before_launch() {
    let (state, events, cloud) = single_job_state(None, true);
    cloud.instance_types.lock().unwrap().get_mut("m6id.8xlarge").unwrap().architectures =
        vec!["x86_64".into(), "arm64".into()];
    let (status, body) = submit(state, allowing(json!({})), submit_body(json!({}))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(body.contains("m6id.8xlarge") && body.contains("architecture"), "{body}");
    assert!(cloud.launched.lock().unwrap().is_empty());
    assert!(!events
        .events
        .lock()
        .unwrap()
        .iter()
        .any(|event| event.kind() == biffix_core::event::Kind::RunSubmitted));
}

/// The deploy's one single-job queue-class, of Graviton (arm64) types.
fn graviton() -> Option<Vec<SingleJobQueue>> {
    Some(vec![SingleJobQueue {
        name: "graviton".into(),
        queue: SINGLE_QUEUE.into(),
        instance_types: vec!["m7gd.8xlarge".into(), "c7gd.8xlarge".into()],
    }])
}

#[tokio::test]
async fn an_arm_single_job_run_launches_alone_on_its_queue_class() {
    let (state, events, cloud) = single_job_state(graviton(), true);
    let (status, body) = submit(state, allowing(json!({})), submit_body(json!({}))).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let launches = cloud.launched.lock().unwrap().clone();
    assert_eq!(launches.len(), 1);
    assert_eq!(launches[0].coordinator_queue, SINGLE_QUEUE);
    let resources = launches[0].single_job.expect("a single-job launch");
    assert_eq!((resources.vcpus, resources.memory_mib), (32, 61_184));
    assert_eq!(cloud.variant_templates.lock().unwrap().last().unwrap(), SINGLE_TEMPLATE);
    assert!(cloud.task_job_definitions.lock().unwrap().is_empty());
    let used = recorded_anchor(&events).single_job.expect("the anchor records the launch");
    assert_eq!(
        (used.queue_class.as_str(), used.instance_type.as_str()),
        ("graviton", "c7gd.8xlarge")
    );
}

#[tokio::test]
async fn a_task_image_is_checked_against_its_queue_classs_architecture_both_ways() {
    // (queue-class, the only architecture the version's images run on, the one the hosts need)
    for (queues, image, host) in [(graviton(), "amd64", "arm64"), (None, "arm64", "amd64")] {
        let (mut state, events, cloud) = single_job_state(queues, true);
        let registry = Arc::new(MemRegistry::new());
        state.registry = registry.clone();
        let app = api::router(state);
        setup(&app, "/workflows", "developers", &allowing(json!({}))).await;
        for artifact in registry.artifacts.lock().unwrap().values_mut() {
            artifact.archs = vec![image.into()];
        }
        let response =
            app.oneshot(post("/runs", Some("operators"), &submit_body(json!({})))).await.unwrap();
        let status = response.status();
        let body = text_body(response).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{image} on {host}: {body}");
        assert!(body.contains(&format!("require {host} images")), "{body}");
        assert!(cloud.launched.lock().unwrap().is_empty());
        assert!(!events
            .events
            .lock()
            .unwrap()
            .iter()
            .any(|event| event.kind() == biffix_core::event::Kind::RunSubmitted));
    }
}

#[tokio::test]
async fn a_single_job_run_rejects_a_known_incompatible_default_image() {
    let (mut state, events, cloud) = single_job_state(None, true);
    let registry = Arc::new(MemRegistry::new());
    state.registry = registry.clone();
    let app = api::router(state);
    setup(&app, "/workflows", "developers", &allowing(json!({}))).await;
    {
        let mut artifacts = registry.artifacts.lock().unwrap();
        assert!(!artifacts.is_empty());
        for artifact in artifacts.values_mut() {
            artifact.archs = vec!["arm64".into()];
        }
    }
    let response =
        app.oneshot(post("/runs", Some("operators"), &submit_body(json!({})))).await.unwrap();
    let status = response.status();
    let body = text_body(response).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(body.contains("amd64") && body.contains("image"), "{body}");
    assert!(cloud.launched.lock().unwrap().is_empty());
    assert!(!events
        .events
        .lock()
        .unwrap()
        .iter()
        .any(|event| event.kind() == biffix_core::event::Kind::RunSubmitted));
}

#[tokio::test]
async fn an_incompatible_default_is_refused_before_starting_submit_image_builds() {
    let (mut state, events, cloud) = single_job_state(None, true);
    let registry = Arc::new(MemRegistry::new());
    state.registry = registry.clone();
    let app = api::router(state);
    setup(&app, "/workflows", "developers", &allowing(json!({}))).await;
    for artifact in registry.artifacts.lock().unwrap().values_mut() {
        artifact.archs = vec!["arm64".into()];
    }
    let imports = cloud.image_imports.lock().unwrap().len();
    let derives = cloud.image_derives.lock().unwrap().len();
    let mut request = submit_body(json!({}));
    request["images"] = json!([SAMTOOLS]);
    let response = app.oneshot(post("/runs", Some("developers"), &request)).await.unwrap();
    let status = response.status();
    let body = text_body(response).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(body.contains("amd64") && body.contains("incompatible task images"), "{body}");
    assert_eq!(cloud.image_imports.lock().unwrap().len(), imports);
    assert_eq!(cloud.image_derives.lock().unwrap().len(), derives);
    assert!(cloud.launched.lock().unwrap().is_empty());
    assert!(!events.events.lock().unwrap().iter().any(|event| matches!(
        event.kind(),
        biffix_core::event::Kind::RunSubmitted | biffix_core::event::Kind::RunPreparing
    )));
}

#[tokio::test]
async fn a_snakemake_derived_default_uses_its_runnable_architecture_and_worker_spelling() {
    for architecture in ["amd64", "arm64"] {
        let (mut state, events, cloud) = single_job_state(None, true);
        let registry = Arc::new(MemRegistry::new());
        state.registry = registry.clone();
        let app = api::router(state);
        setup(&app, "/workflows", "developers", &allowing(json!({}))).await;
        let registered = registry.versions.lock().unwrap()["abc:hash:0"].clone();
        assert_eq!(registered.version.engine, biffix_workflow::engines::Engine::Snakemake);
        let default = registered.images.iter().find(|image| image.name == "default").unwrap();
        let mut imported = testing::fake_artifact(default);
        imported.archs = vec!["amd64".into()];
        let mut derived = testing::fake_derived_artifact(default, &registered.version.runtime_key);
        derived.archs = vec![architecture.into()];
        assert_ne!(imported.image, derived.image);
        {
            let mut artifacts = registry.artifacts.lock().unwrap();
            artifacts.insert(imported.content_key.clone(), imported);
            artifacts.insert(derived.content_key.clone(), derived.clone());
        }
        let response =
            app.oneshot(post("/runs", Some("operators"), &submit_body(json!({})))).await.unwrap();
        let status = response.status();
        let body = text_body(response).await;
        if architecture == "amd64" {
            assert_eq!(status, StatusCode::ACCEPTED, "{body}");
            let anchor = recorded_anchor(&events);
            assert_eq!(anchor.image_map.unwrap()["$default"], derived.image);
            assert_eq!(cloud.launched.lock().unwrap()[0].image_digest, derived.digest);
        } else {
            assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
            assert!(body.contains(&derived.image), "{body}");
            assert!(cloud.launched.lock().unwrap().is_empty());
            assert!(!events
                .events
                .lock()
                .unwrap()
                .iter()
                .any(|event| { event.kind() == biffix_core::event::Kind::RunSubmitted }));
        }
    }
}

#[tokio::test]
async fn a_single_job_run_allows_legacy_image_architecture_metadata() {
    for architectures in [vec![], vec![""], vec!["  "], vec![" amd64 "]] {
        let (mut state, _, cloud) = single_job_state(None, true);
        let registry = Arc::new(MemRegistry::new());
        state.registry = registry.clone();
        let app = api::router(state);
        setup(&app, "/workflows", "developers", &allowing(json!({}))).await;
        for artifact in registry.artifacts.lock().unwrap().values_mut() {
            artifact.archs = architectures.iter().map(ToString::to_string).collect();
        }
        let response =
            app.oneshot(post("/runs", Some("operators"), &submit_body(json!({})))).await.unwrap();
        assert_eq!(response.status(), StatusCode::ACCEPTED, "{architectures:?}");
        assert_eq!(cloud.launched.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn a_ready_submit_image_is_architecture_checked_only_in_single_job_mode() {
    use biffix_backend::aws::registry::ImageRegistry;
    use biffix_backend::store::registry::VersionImage;
    use biffix_workflow::images::ImageRef;

    for single_job in [true, false] {
        let (mut state, _, cloud) = single_job_state(None, true);
        let registry = Arc::new(MemRegistry::new());
        state.registry = registry.clone();
        let app = api::router(state);
        setup(&app, "/workflows", "developers", &allowing(json!({}))).await;
        let reference = ImageRef::parse(SAMTOOLS).unwrap();
        let digest = cloud.resolve_image_digest(&reference).await.unwrap();
        let image = VersionImage {
            name: SAMTOOLS.into(),
            declared_ref: SAMTOOLS.into(),
            source_ref: reference.pinned(&digest).to_string(),
            content_key: biffix_workflow::images::content_key(&digest),
            own_runtime: false,
            build: None,
        };
        let runtime_key =
            registry.versions.lock().unwrap()["abc:hash:0"].version.runtime_key.clone();
        let mut derived = testing::fake_derived_artifact(&image, &runtime_key);
        derived.archs = vec!["arm64".into()];
        let derived_image = derived.image.clone();
        {
            let mut artifacts = registry.artifacts.lock().unwrap();
            artifacts.insert(image.content_key.clone(), testing::fake_artifact(&image));
            artifacts.insert(derived.content_key.clone(), derived);
        }
        let mut body = submit_body(json!({}));
        body["images"] = json!([SAMTOOLS]);
        if !single_job {
            body.as_object_mut().unwrap().remove("single_job");
        }
        let response = app.oneshot(post("/runs", Some("developers"), &body)).await.unwrap();
        let status = response.status();
        let text = text_body(response).await;
        if single_job {
            assert_eq!(status, StatusCode::BAD_REQUEST, "{text}");
            assert!(text.contains(&derived_image), "{text}");
            assert!(cloud.launched.lock().unwrap().is_empty());
        } else {
            assert_eq!(status, StatusCode::ACCEPTED, "{text}");
            assert_eq!(cloud.launched.lock().unwrap().len(), 1);
        }
    }
}

#[tokio::test]
async fn a_single_job_run_cannot_be_a_resume() {
    let mut body = submit_body(json!({}));
    body["resume_from"] = json!("run-prior");
    let (status, text, cloud, _) =
        resume_submit(Some(resumable_prior("run-prior")), allowing(json!({})), body, "operators")
            .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(text.contains("a single-job run cannot be a resume"), "{text}");
    assert!(cloud.launched.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_nextflow_single_job_run_cannot_be_resumed() {
    let mut prior = resumable_prior("run-prior");
    prior.single_job = Some(biffix_core::event::SingleJobUse {
        queue_class: "single".into(),
        instance_type: "c6id.8xlarge".into(),
        vcpus: 32,
        memory_mib: 61_184,
    });
    let (status, text, cloud, _) = resume_submit(
        Some(prior),
        nextflow_register_body(),
        resume_body("run-prior", json!({})),
        "operators",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{text}");
    assert!(text.contains("ran as a single job"), "{text}");
    assert!(cloud.launched.lock().unwrap().is_empty());
}
