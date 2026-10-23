//! The launch sweep: a parked run launches in its own project, or fails saying what to do.

use super::*;

/// Park run `run_id` of `version` with `input_tables` (as submit would, before its images were
/// ready), recorded as preparing images.
fn park(h: &Harness, run_id: &str, version: &str, input_tables: Vec<(&str, String)>) {
    park_plan(h, run_id, version, input_tables, None, vec![]);
}

/// [`park`] with the plan's recorded `output_prefix` and `input_uris` too.
fn park_plan(
    h: &Harness,
    run_id: &str,
    version: &str,
    input_tables: Vec<(&str, String)>,
    output_prefix: Option<String>,
    input_uris: Vec<String>,
) {
    let plan = LaunchPlan {
        version: 1,
        version_id: version.into(),
        actor: "local".into(),
        submitter_label: None,
        params: json!({"reads": format!("s3://{BUCKET_B}/in.fastq"), "outdir": format!("s3://{BUCKET_B}/out/")}),
        input_uris,
        input_provenance: Default::default(),
        target: None,
        output_prefix,
        retries: None,
        keep_going: None,
        engine_session_id: None,
        resumed_from: None,
        run_name: None,
        dropped_inactive: vec![],
        input_tables: input_tables
            .into_iter()
            .map(|(param, uri)| (param.to_string(), InputTable { uri, etag: None, rows: vec![] }))
            .collect(),
        reference_bundle: None,
        sample_count: None,
        samples: None,
        retention: None,
        images: vec![],
        experiment_id: None,
    };
    h.store.pending.lock().unwrap().insert(
        run_id.into(),
        PendingLaunch {
            run_id: RunId::new(run_id),
            created_at_micros: biffix_core::clock::epoch_micros(),
            plan: serde_json::to_value(&plan).unwrap(),
        },
    );
    h.store.insert(testing::run_detail(run_id, "preparing_images"));
}

#[tokio::test]
async fn a_parked_run_of_project_b_launches_in_b() {
    let h = harness();
    let version = h.register(PROJECT_B, "consensus").await;
    let sheet = format!("s3://{BUCKET_B}/biffix/runs/run-p/inputs/samples.csv");
    park(&h, "run-p", &version, vec![("samples", sheet)]);
    let report = biffix_backend::runs::launch::sweep(&h.state).await.unwrap();
    assert_eq!(report.launched, 1, "{report:?} {:?}", h.run_failure());
    let launched = h.cloud.launched.lock().unwrap()[0].clone();
    assert_eq!(launched.project.id, PROJECT_B);
    assert_eq!(launched.project.bucket, BUCKET_B);
    assert_eq!(
        launched.output_prefix.as_deref(),
        Some(format!("s3://{BUCKET_B}/biffix/runs/run-p/").as_str())
    );
}

#[tokio::test]
async fn a_run_parked_with_its_samplesheet_in_another_bucket_fails_with_resubmit() {
    let h = harness();
    let version = h.register(PROJECT_B, "consensus").await;
    // Parked while its version's project was another: its samplesheet went to that bucket.
    let sheet = format!("s3://{BUCKET_A}/biffix/runs/run-p/inputs/samples.csv");
    park(&h, "run-p", &version, vec![("samples", sheet.clone())]);
    let report = biffix_backend::runs::launch::sweep(&h.state).await.unwrap();
    assert_eq!(report.failed, 1, "{report:?}");
    assert!(h.cloud.launched.lock().unwrap().is_empty());
    let reason = h.run_failure().unwrap();
    assert!(reason.contains("is outside this run's project's bucket"), "{reason}");
    assert!(reason.contains("resubmit"), "{reason}");
    assert!(reason.contains(&sheet), "{reason}");
}

#[tokio::test]
async fn a_run_parked_with_its_prefix_or_inputs_in_another_project_fails_with_resubmit() {
    let h = harness();
    let version = h.register(PROJECT_B, "consensus").await;
    let prefix = format!("s3://{BUCKET_A}/results/");
    park_plan(&h, "run-p", &version, vec![], Some(prefix.clone()), vec![]);
    let report = biffix_backend::runs::launch::sweep(&h.state).await.unwrap();
    assert_eq!(report.failed, 1, "{report:?}");
    let reason = h.run_failure().unwrap();
    assert!(reason.contains(&prefix) && reason.contains("resubmit"), "{reason}");
    assert!(h.cloud.launched.lock().unwrap().is_empty());

    let h = harness();
    let version = h.register(PROJECT_B, "consensus").await;
    let input = format!("s3://{BUCKET_A}/uploads/in.fastq");
    park_plan(&h, "run-q", &version, vec![], None, vec![input.clone()]);
    let report = biffix_backend::runs::launch::sweep(&h.state).await.unwrap();
    assert_eq!(report.failed, 1, "{report:?}");
    assert!(h.cloud.launched.lock().unwrap().is_empty());
    let reason = h.run_failure().unwrap();
    assert!(
        reason.contains(&input) && reason.contains(&format!("project `{PROJECT_A}`")),
        "{reason}"
    );
}

#[tokio::test]
async fn a_parked_run_whose_project_is_not_configured_fails_with_re_register() {
    let h = harness();
    let version = h.register(PROJECT_B, "consensus").await;
    h.registry.versions.lock().unwrap().get_mut(&version).unwrap().project = "consensus".into();
    park(&h, "run-p", &version, vec![]);
    let report = biffix_backend::runs::launch::sweep(&h.state).await.unwrap();
    assert_eq!(report.failed, 1, "{report:?}");
    let reason = h.run_failure().unwrap();
    assert!(reason.contains("project `consensus` is not configured on this deploy"), "{reason}");
    assert!(
        reason.contains(&format!("biffix workflows reregister {version} --project")),
        "{reason}"
    );
}

/// Serves [`testing::two_projects`] less project B on its first read, all of it after: a process
/// that loaded its projects before a deploy added B.
struct AddsB {
    reads: std::sync::atomic::AtomicUsize,
}

#[async_trait::async_trait]
impl biffix_backend::config::projects::ProjectsLoader for AddsB {
    async fn load(
        &self,
    ) -> Result<
        biffix_backend::config::projects::Projects,
        biffix_backend::config::projects::ProjectsError,
    > {
        let all = testing::two_projects();
        let first = self.reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0;
        let contexts = all
            .iter()
            .filter(|p| !(first && p.id == PROJECT_B))
            .cloned()
            // A third project, so the deploy has several even without B.
            .chain([testing::project("other", "other-bucket")])
            .collect();
        biffix_backend::config::projects::Projects::new(contexts, PROJECT_A)
    }
}

#[tokio::test]
async fn a_parked_run_of_a_project_this_process_has_not_seen_reads_the_projects_again() {
    let mut h = harness();
    let version = h.register(PROJECT_B, "consensus").await;
    park(&h, "run-p", &version, vec![]);
    // This process loaded its projects just before B was added; they are not due for minutes.
    let loader = Arc::new(AddsB { reads: Default::default() });
    h.state.projects = Arc::new(
        biffix_backend::config::projects::ProjectsHandle::load(
            loader.clone(),
            std::time::Duration::from_secs(300),
        )
        .await
        .unwrap(),
    );
    let report = biffix_backend::runs::launch::sweep(&h.state).await.unwrap();
    assert_eq!(report.launched, 1, "{report:?} {:?}", h.run_failure());
    assert_eq!(h.cloud.launched.lock().unwrap()[0].project.id, PROJECT_B);
    assert_eq!(loader.reads.load(std::sync::atomic::Ordering::SeqCst), 2);
}

#[tokio::test]
async fn a_version_whose_staged_files_are_in_another_project_s_bucket_cannot_run() {
    let h = harness();
    let version = h.register(PROJECT_B, "consensus").await;
    // Registered on a single-project deploy (staged in A's bucket) under a label that is now B.
    let staged = format!("s3://{BUCKET_A}/workflow-sources/abc:hash-consensus/source.tar.gz");
    h.registry.versions.lock().unwrap().get_mut(&version).unwrap().source =
        biffix_backend::register::workflow::WorkflowSource::S3 {
            uri: staged.clone(),
            sha256: None,
        };
    let (status, v) = h.post("/runs", submit_body(&version, BUCKET_B)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{v}");
    let msg = v.as_str().unwrap();
    assert!(msg.contains(&staged), "{msg}");
    assert!(msg.contains(&format!("re-register the workflow into project `{PROJECT_B}`")), "{msg}");
    assert!(h.cloud.launched.lock().unwrap().is_empty());
    // A run parked before the deploy changed fails the same way at launch.
    park(&h, "run-p", &version, vec![]);
    let report = biffix_backend::runs::launch::sweep(&h.state).await.unwrap();
    assert_eq!(report.failed, 1, "{report:?}");
    assert!(h.run_failure().unwrap().contains(&staged));
}
