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

    // A queue-class whose instance type EC2 describes as Graviton, or with a GPU: Batch could
    // place the run there.
    for (arch, accelerator, why) in
        [("arm64", None, "is not x86_64"), ("x86_64", Some("GPU"), "carries an accelerator")]
    {
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
