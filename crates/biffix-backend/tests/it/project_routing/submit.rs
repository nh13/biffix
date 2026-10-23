//! Submitting and resuming a run in its version's project.

use super::*;

#[tokio::test]
async fn a_version_of_project_b_launches_with_b_s_context() {
    let h = harness();
    let version = h.register(PROJECT_B, "consensus").await;
    let (status, v) = h.post("/runs", submit_body(&version, BUCKET_B)).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{v}");
    // The run is recorded with B's run role.
    assert_eq!(v["assumed_role_arn"], RUN_ROLE_B);

    let launched = h.cloud.launched.lock().unwrap()[0].clone();
    assert_eq!(launched.project.id, PROJECT_B);
    assert_eq!(launched.project.bucket, BUCKET_B);
    assert_eq!(launched.project.run_role_arn, RUN_ROLE_B);
    let prefix = launched.output_prefix.unwrap();
    assert!(prefix.starts_with(&format!("s3://{BUCKET_B}/biffix/runs/")), "{prefix}");
    // The image map is written into B's bucket too.
    assert!(launched.image_map_uri.unwrap().starts_with(&format!("s3://{BUCKET_B}/")));
    // The coordinator runs on B's own clone of the variant (B's template, so B's coordinator
    // role), found by name; the recorded variant stays the default project's.
    let runtime_key = h.registry.versions.lock().unwrap()[&version].version.runtime_key.clone();
    assert_eq!(
        launched.variant.job_definition,
        testing::fake_project_variant_job_definition(
            &runtime_key,
            testing::FAKE_BASE_DIGEST,
            1,
            TEMPLATE_B
        )
    );
    assert!(h.cloud.variant_templates.lock().unwrap().contains(&TEMPLATE_B.to_string()));
    let sessions = h.cloud.sessions.lock().unwrap();
    assert_eq!(sessions[0].1.id, PROJECT_B);
}

#[tokio::test]
async fn a_version_of_the_default_project_launches_as_before() {
    let h = harness();
    let version = h.register(PROJECT_A, "bwa").await;
    let registered = h.cloud.variant_job_definitions.lock().unwrap().len();
    let (status, v) = h.post("/runs", submit_body(&version, BUCKET_A)).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{v}");
    // Nothing cloned at submit: the recorded variant is A's own.
    assert_eq!(h.cloud.variant_job_definitions.lock().unwrap().len(), registered);
    let launched = h.cloud.launched.lock().unwrap()[0].clone();
    assert_eq!(launched.project.id, PROJECT_A);
    assert_eq!(launched.project.bucket, BUCKET_A);
    // The recorded variant itself: no project clone is registered.
    let runtime_key = h.registry.versions.lock().unwrap()[&version].version.runtime_key.clone();
    assert_eq!(
        launched.variant.job_definition,
        testing::fake_variant(&runtime_key, testing::FAKE_BASE_DIGEST).job_definition_arn
    );
    assert!(!h.cloud.variant_templates.lock().unwrap().contains(&TEMPLATE_B.to_string()));
}

#[tokio::test]
async fn an_output_prefix_in_another_project_s_bucket_is_refused() {
    let h = harness();
    let version = h.register(PROJECT_B, "consensus").await;
    let mut body = submit_body(&version, BUCKET_B);
    body["output_prefix"] = json!(format!("s3://{BUCKET_A}/results/"));
    let (status, v) = h.post("/runs", body).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{v}");
    assert_eq!(
        v,
        json!(format!("output_prefix must be within the project bucket s3://{BUCKET_B}/"))
    );
    // Its own bucket is fine.
    let mut body = submit_body(&version, BUCKET_B);
    body["output_prefix"] = json!(format!("s3://{BUCKET_B}/runs/5f472f5/"));
    let (status, v) = h.post("/runs", body).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{v}");
    assert_eq!(
        h.cloud.launched.lock().unwrap()[0].output_prefix.as_deref(),
        Some(format!("s3://{BUCKET_B}/runs/5f472f5/").as_str())
    );
}

#[tokio::test]
async fn an_input_only_another_project_can_read_is_refused() {
    let h = harness();
    let version = h.register(PROJECT_B, "consensus").await;
    let submit = |reads: &str| {
        json!({
            "workflow_version_id": version,
            "params": {"reads": reads, "outdir": format!("s3://{BUCKET_B}/out/")},
        })
    };
    // In A's bucket: B's runs cannot read it, though the backend could HEAD it.
    let input = format!("s3://{BUCKET_A}/uploads/in.fastq");
    let (status, v) = h.post("/runs", submit(&input)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{v}");
    assert_eq!(
        v,
        json!(format!(
            "input {input} is in storage of project `{PROJECT_A}`, whose runs alone can read it; \
             this version runs in project `{PROJECT_B}`"
        ))
    );
    assert!(h.cloud.launched.lock().unwrap().is_empty());
    // A's version may not read B's own data source either.
    let a_version = h.register(PROJECT_A, "bwa").await;
    let mut body = submit_body(&a_version, BUCKET_A);
    body["params"]["reads"] = json!("s3://acme-core/deliveries/r.fq");
    let (status, v) = h.post("/runs", body).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{v}");
    assert!(v.as_str().unwrap().contains(&format!("project `{PROJECT_B}`")), "{v}");
    // B's own bucket and data source, and a bucket no project owns: checked for existence only.
    for reads in [
        format!("s3://{BUCKET_B}/in.fastq"),
        "s3://acme-core/deliveries/r.fq".to_string(),
        "s3://public-refs/r.fq".to_string(),
    ] {
        let (status, v) = h.post("/runs", submit(&reads)).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{reads}: {v}");
    }
}

/// A version of `project` whose samplesheet field `input` declares a sample-id column; a path
/// input (existence-checked and refused in another project's storage) or not (a
/// `format: samplesheet` hint, which is neither).
fn sheet_register_body(project: &str, name: &str, is_path_input: bool) -> Value {
    let mut body = register_body(project, name);
    body["form_schema"]["fields"][0] = json!({
        "name": "input", "widget": "samplesheet", "required": true,
        "is_output": false, "is_secret": false, "is_path_input": is_path_input,
        "samplesheet": {"columns": [
            {"name": "sample", "role": "sample_id", "required": true},
            {"name": "fastq_1", "role": "read1"}
        ]}
    });
    body
}

#[tokio::test]
async fn a_samplesheet_file_only_another_project_can_read_is_not_read() {
    let sheet = b"sample,fastq_1\nPATIENT_1,x\n".to_vec();
    let in_a = format!("s3://{BUCKET_A}/private/samplesheet.csv");
    let in_b = format!("s3://{BUCKET_B}/sheets/samplesheet.csv");
    for (is_path_input, name) in [(false, "hinted"), (true, "pathsheet")] {
        let h = harness();
        for uri in [&in_a, &in_b] {
            h.cloud.objects.lock().unwrap().insert(uri.clone(), sheet.clone());
        }
        let (status, v) =
            h.post("/workflows", sheet_register_body(PROJECT_B, name, is_path_input)).await;
        assert!(status.is_success(), "{status} {v}");
        let version = v["id"].as_str().unwrap().to_string();
        let submit = |input: &str| {
            json!({
                "workflow_version_id": version,
                "params": {"input": input, "outdir": format!("s3://{BUCKET_B}/out/")},
            })
        };
        let (status, v) = h.post("/runs", submit(&in_a)).await;
        if is_path_input {
            // A path input there refuses the submit, before anything is read.
            assert_eq!(status, StatusCode::BAD_REQUEST, "{v}");
        } else {
            // A hinted sheet is not refused, but its file is not read: the sample set stays open.
            assert_eq!(status, StatusCode::ACCEPTED, "{v}");
            let anchor = h.anchor();
            assert_eq!((anchor.samples, anchor.sample_count), (None, None));
        }
        assert!(
            !h.cloud.ranges.lock().unwrap().contains(&in_a),
            "{name}: another project's sheet is not read"
        );
        // In the run's own project's storage, its ids are declared.
        h.events.events.lock().unwrap().clear();
        let (status, v) = h.post("/runs", submit(&in_b)).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{v}");
        assert_eq!(h.anchor().samples.unwrap().ids, ["PATIENT_1"], "{name}");
    }
}

#[tokio::test]
async fn a_reference_bundle_only_another_project_can_read_is_refused() {
    let h = harness();
    let mut body = register_body(PROJECT_B, "consensus");
    body["form_schema"]["fields"]
        .as_array_mut()
        .unwrap()
        .push(json!({"name": "fasta", "widget": "s3_file", "required": false,
                     "is_output": false, "is_secret": false, "is_path_input": true}));
    body["form_schema"]["references"] = json!({"kind": "assembly", "fill": {"fasta": "fasta"},
        "required": ["fasta"], "label": "Reference genome", "custom": true});
    let (status, v) = h.post("/workflows", body).await;
    assert!(status.is_success(), "{status} {v}");
    let version = v["id"].as_str().unwrap().to_string();
    // Bundles are tenant-wide: one in A's bucket saves.
    let fasta = format!("s3://{BUCKET_A}/refs/hg38.fa");
    for (id, uri) in [("in-a", fasta.as_str()), ("in-b", "s3://acme-core/deliveries/hg38.fa")] {
        let (status, v) = h
            .post(
                "/references",
                json!({"id": id, "kind": "assembly", "display_name": id, "uris": {"fasta": uri}}),
            )
            .await;
        assert!(status.is_success(), "{status} {v}");
    }
    let mut submit = submit_body(&version, BUCKET_B);
    submit["reference"] = json!("in-a");
    let (status, v) = h.post("/runs", submit.clone()).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{v}");
    assert!(
        v.as_str()
            .unwrap()
            .starts_with(&format!("input {fasta} is in storage of project `{PROJECT_A}`")),
        "{v}"
    );
    submit["reference"] = json!("in-b");
    let (status, v) = h.post("/runs", submit).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{v}");
}

/// Serves [`testing::two_projects`] with A or B the default, alternately on each read: a process
/// whose projects are read again (a deploy changing the default) between the reads one submit
/// makes.
struct FlipsDefault {
    reads: std::sync::atomic::AtomicUsize,
}

#[async_trait::async_trait]
impl biffix_backend::config::projects::ProjectsLoader for FlipsDefault {
    async fn load(
        &self,
    ) -> Result<
        biffix_backend::config::projects::Projects,
        biffix_backend::config::projects::ProjectsError,
    > {
        let n = self.reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let default = if n.is_multiple_of(2) { PROJECT_A } else { PROJECT_B };
        biffix_backend::config::projects::Projects::new(
            testing::two_projects().iter().cloned().collect(),
            default,
        )
    }
}

#[tokio::test]
async fn a_run_launches_on_its_own_project_s_clone_whatever_the_default_is_meanwhile() {
    for phase in 0..2 {
        let mut h = harness();
        let version = h.register(PROJECT_B, "consensus").await;
        let loader = Arc::new(FlipsDefault { reads: phase.into() });
        // Read again on every request (ttl 0): the default flips between any two reads.
        h.state.projects = Arc::new(
            biffix_backend::config::projects::ProjectsHandle::load(
                loader,
                std::time::Duration::ZERO,
            )
            .await
            .unwrap(),
        );
        let (status, v) = h.post("/runs", submit_body(&version, BUCKET_B)).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{v}");
        let runtime_key = h.registry.versions.lock().unwrap()[&version].version.runtime_key.clone();
        // B's coordinator always runs as B: on the clone of B's own template, never on one
        // recorded from A's.
        assert_eq!(
            h.cloud.launched.lock().unwrap()[0].variant.job_definition,
            testing::fake_project_variant_job_definition(
                &runtime_key,
                testing::FAKE_BASE_DIGEST,
                1,
                TEMPLATE_B
            ),
            "phase {phase}"
        );
    }
}

#[tokio::test]
async fn a_version_of_a_project_the_deploy_does_not_configure_is_a_409() {
    let h = harness();
    // Registered into a project the deploy has since removed: registration now refuses the label.
    let version = h.register(PROJECT_B, "consensus").await;
    h.registry.versions.lock().unwrap().get_mut(&version).unwrap().project = "consensus".into();
    let (status, v) = h.post("/runs", submit_body(&version, BUCKET_B)).await;
    assert_eq!(status, StatusCode::CONFLICT, "{v}");
    let msg = v.as_str().unwrap();
    assert!(
        msg.contains(&format!(
            "project `consensus` is not configured on this deploy (projects: {PROJECT_A}, {PROJECT_B})"
        )),
        "{msg}"
    );
    assert!(msg.contains("--project"), "{msg}");
    assert!(h.cloud.launched.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_run_cannot_be_resumed_into_another_project() {
    let h = harness();
    let version = h.register(PROJECT_B, "consensus").await;
    // A finished run of the same workflow, labelled B, that ran in A's bucket (launched before
    // routing, when every run ran in the default project).
    let mut prior = testing::run_detail("r-legacy", "failed");
    prior.run.engine = Some("snakemake".into());
    prior.run.workflow_name = Some("consensus".into());
    prior.run.project = Some(PROJECT_B.into());
    prior.output_prefix = Some(format!("s3://{BUCKET_A}/biffix/runs/r-legacy/"));
    h.store.insert(prior);
    let mut body = submit_body(&version, BUCKET_B);
    body["resume_from"] = json!("r-legacy");
    let (status, v) = h.post("/runs", body).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{v}");
    assert!(v.as_str().unwrap().contains("cannot resume across projects"), "{v}");
    // The page does not offer it either (its version runs in B, the run ran in A).
    let (status, detail) = h.get("/runs/r-legacy").await;
    assert_eq!(status, StatusCode::OK, "{detail}");
    assert_eq!(detail["resumable"], false, "{detail}");
    // The same run labelled A is resumable, and offered.
    let mut same = testing::run_detail("r-a", "failed");
    same.run.engine = Some("snakemake".into());
    same.run.workflow_name = Some("consensus".into());
    same.run.project = Some(PROJECT_A.into());
    same.output_prefix = Some(format!("s3://{BUCKET_A}/biffix/runs/r-a/"));
    h.store.insert(same);
    let (_, detail) = h.get("/runs/r-a").await;
    assert_eq!(detail["resumable"], true, "{detail}");
}

#[tokio::test]
async fn a_resume_whose_prior_ran_outside_every_project_bucket_is_refused() {
    let h = harness();
    let version = h.register(PROJECT_B, "consensus").await;
    // Its prefix is in a bucket no project owns now (a project since removed): by its label, B.
    let mut prior = testing::run_detail("r-gone", "failed");
    prior.run.engine = Some("snakemake".into());
    prior.run.workflow_name = Some("consensus".into());
    prior.run.project = Some(PROJECT_B.into());
    prior.output_prefix = Some("s3://removed-project/biffix/runs/r-gone/".into());
    h.store.insert(prior);
    let mut body = submit_body(&version, BUCKET_B);
    body["resume_from"] = json!("r-gone");
    let (status, v) = h.post("/runs", body).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{v}");
    assert!(
        v.as_str()
            .unwrap()
            .contains(&format!("outside project `{PROJECT_B}`'s bucket s3://{BUCKET_B}/")),
        "{v}"
    );
    assert!(h.cloud.launched.lock().unwrap().is_empty());
    let (_, detail) = h.get("/runs/r-gone").await;
    assert_eq!(detail["resumable"], false, "{detail}");
}
