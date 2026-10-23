//! Submit-pipeline tests — no AWS, via the FakeCloud.

use std::sync::Arc;

use biffix_backend::aws::CloudProvider;
use biffix_backend::register::workflow::{WorkflowSource, WorkflowVersion};
use biffix_backend::runs::launch::LaunchPlan;
use biffix_backend::runs::submit::{submit, SubmitError, SubmitOutcome, SubmitRequest};
use biffix_core::event::{EventData, RunSubmitted};
use biffix_core::ids::RunId;
use biffix_workflow::engines::Engine;
use biffix_workflow::formschema::REDACTED_SECRET_PLACEHOLDER;

use biffix_backend::testing::FakeCloud;

fn version() -> WorkflowVersion {
    WorkflowVersion {
        id: "abc:hash".into(),
        git_sha: "abc".into(),
        source_content_hash: "hash".into(),
        image_digest: "sha256:x".into(),
        image_repository: None,
        image_registry_id: None,
        image_region: None,
        engine: Engine::Snakemake,
        engine_version: "9.3".into(),
        entrypoint: "workflow/Snakefile".into(),
        target: None,
        form_schema: serde_json::json!({
            "fields": [
                {
                    "name": "api_key",
                    "widget": "text",
                    "required": false,
                    "is_output": false,
                    "is_secret": true,
                    "is_path_input": false
                },
                {
                    "name": "outdir",
                    "widget": "s3_dir",
                    "required": true,
                    "is_output": true,
                    "is_secret": false,
                    "is_path_input": false
                },
                {
                    "name": "reads",
                    "widget": "s3_file",
                    "required": true,
                    "is_output": false,
                    "is_secret": false,
                    "is_path_input": true
                }
            ],
            "warnings": []
        }),
        queue_classes: vec!["cpu-spot".into()],
        about_markdown: None,
        runtime_key: "rt".into(),
        runtime_recipe: serde_json::json!({"engine": "snakemake"}),
        build: 0,
    }
}

fn req(params: serde_json::Value, inputs: Vec<String>) -> SubmitRequest {
    SubmitRequest {
        run_id: RunId::new("r1"),
        version: version(),
        source: WorkflowSource::Git { repo: "github.com/org/repo".into(), git_ref: "main".into() },
        plan: LaunchPlan {
            version: 1,
            version_id: "abc:hash".into(),
            actor: "alice".into(),
            submitter_label: Some("alice@example.com".into()),
            params,
            input_uris: inputs,
            input_provenance: std::collections::BTreeMap::new(),
            target: None,
            output_prefix: None,
            retries: None,
            keep_going: None,
            engine_session_id: None,
            resumed_from: None,
            run_name: None,
            dropped_inactive: Vec::new(),
            input_tables: Default::default(),
            reference_bundle: None,
            sample_count: None,
            samples: None,
            retention: None,
            images: vec![],
            experiment_id: None,
        },
        coordinator_queue: "coordinator".into(),
        project: biffix_backend::aws::batch::LaunchProject {
            id: "demo".into(),
            bucket: "b".into(),
            run_role_arn: String::new(),
        },
        variant: biffix_backend::aws::batch::LaunchVariant {
            job_definition:
                "arn:aws:batch:us-east-1:123:job-definition/biffix-t-coordinator-rt-k-b:1".into(),
            image: "123.dkr.ecr.us-east-1.amazonaws.com/coord@sha256:variant".into(),
            engine_version: "9.27.0".into(),
            runtime_key: "k".into(),
        },
        image_source_digest: None,
        image_map: None,
    }
}

/// [`req`] with the standard valid params (`reads`, `outdir`) and `reads` as the one input.
fn default_req() -> SubmitRequest {
    req(
        serde_json::json!({"reads": "s3://b/in.fastq", "outdir": "s3://b/out/"}),
        vec!["s3://b/in.fastq".into()],
    )
}

/// A FakeCloud and the same double as the pipeline's `CloudProvider`.
fn cloud() -> (Arc<FakeCloud>, Arc<dyn CloudProvider>) {
    let fake = Arc::new(FakeCloud::new());
    let cloud: Arc<dyn CloudProvider> = fake.clone();
    (fake, cloud)
}

/// The `run_submitted` anchor a submit built.
fn anchor(out: &SubmitOutcome) -> &RunSubmitted {
    let EventData::RunSubmitted(anchor) = &out.run_submitted.data else {
        panic!("expected run_submitted, got {:?}", out.run_submitted.kind());
    };
    anchor
}

#[tokio::test]
async fn happy_path_launches_coordinator_and_builds_anchor() {
    let (fake, cloud) = cloud();
    let out = submit(&cloud, default_req()).await.expect("should submit");
    assert_eq!(out.coordinator_job_id, "coord-1");
    // The run session's role, as minted.
    assert_eq!(out.assumed_role_arn, "arn:aws:iam::123:role/biffix-run");
    assert_eq!(out.run_submitted.kind(), biffix_core::event::Kind::RunSubmitted);
    // Who-launched-the-run attribution is snapshotted onto the immutable anchor here and nowhere
    // else, so assert it: a mutation dropping it would otherwise pass the whole suite silently.
    let RunSubmitted { submitter, submitter_label, input_provenance, .. } = anchor(&out);
    assert_eq!(submitter.as_deref(), Some("user:alice"));
    assert_eq!(submitter_label.as_deref(), Some("alice@example.com"));
    assert_eq!(*input_provenance, None, "an empty provenance map stays None");
    // The minted capability token and the request's queue/image must actually reach the launch,
    // not just be constructed and dropped.
    let launched = fake.launched.lock().unwrap();
    assert_eq!(launched.len(), 1);
    assert_eq!(launched[0].capability_token, "tok-abc");
    assert_eq!(launched[0].coordinator_queue, "coordinator");
    assert_eq!(launched[0].image_digest, "sha256:x");
}

#[tokio::test]
async fn input_provenance_round_trips_onto_the_anchor() {
    // A non-empty provenance map (S3 version-id/ETag captured at the existence check) must land on
    // the anchor as `Some(map)` — the `if !is_empty()` branch in the pipeline that no other test
    // hits.
    let (_, cloud) = cloud();
    let mut request = default_req();
    request.plan.input_provenance.insert("s3://b/in.fastq".into(), "etag-123".into());

    let out = submit(&cloud, request).await.expect("should submit");

    let RunSubmitted { submitter, input_provenance, .. } = anchor(&out);
    assert_eq!(submitter.as_deref(), Some("user:alice"));
    let provenance =
        input_provenance.as_ref().expect("a non-empty provenance map is recorded on the anchor");
    assert_eq!(provenance.get("s3://b/in.fastq").map(String::as_str), Some("etag-123"));
}

#[tokio::test]
async fn the_submit_target_wins_over_the_version_s_and_a_blank_one_is_unset() {
    // One frozen version can drive `smoke` vs `full` without re-registering: a submit
    // `--target` wins, a blank one is unset (it does not blank out the version's), and with neither
    // there is none. The effective target is on BOTH the launch and the anchor.
    for (case, version_target, submit_target, expected) in [
        ("override", Some("all"), Some("smoke"), Some("smoke")),
        ("no override", Some("full"), None, Some("full")),
        ("blank override", Some("all"), Some("   "), Some("all")),
        ("neither", None, None, None),
    ] {
        let (fake, cloud) = cloud();
        let mut request = default_req();
        request.version.target = version_target.map(String::from);
        request.plan.target = submit_target.map(String::from);

        let out = submit(&cloud, request).await.expect("should submit");

        assert_eq!(anchor(&out).target.as_deref(), expected, "{case}: anchor");
        assert_eq!(fake.launched.lock().unwrap()[0].target.as_deref(), expected, "{case}: launch");
    }
}

#[tokio::test]
async fn worker_image_repo_uri_is_derived_from_the_version_ecr_coords() {
    // A version whose image was registered from a private ECR reference records that repo's coords,
    // so the coordinator pulls the default worker image from the image's OWN repo — not only the
    // deploy's worker repo. Derives `<id>.dkr.ecr.<region>.amazonaws.com/<repo>`.
    let (fake, cloud) = cloud();
    let mut request = default_req();
    request.version.image_registry_id = Some("123456789012".into());
    request.version.image_region = Some("us-east-1".into());
    request.version.image_repository = Some("my/workflow".into());

    submit(&cloud, request).await.expect("should submit");

    let launched = fake.launched.lock().unwrap();
    assert_eq!(
        launched[0].worker_image_repo_uri.as_deref(),
        Some("123456789012.dkr.ecr.us-east-1.amazonaws.com/my/workflow")
    );
    assert_eq!(launched[0].image_digest, "sha256:x");
}

#[tokio::test]
async fn a_bare_digest_version_carries_no_repo_uri_and_falls_back_to_the_deploy_repo() {
    // No recorded coords (bare-digest registration) ⇒ `None`, so the provider uses the deploy's
    // worker repo. `version()` pins a bare digest with no coords.
    let (fake, cloud) = cloud();
    submit(&cloud, default_req()).await.expect("should submit");
    let launched = fake.launched.lock().unwrap();
    assert_eq!(launched[0].worker_image_repo_uri, None);
    assert_eq!(launched[0].image_digest, "sha256:x");
}

#[tokio::test]
async fn a_version_without_a_default_image_launches_with_no_worker_image() {
    // An empty digest means the version declares no default worker image (every task supplies its
    // own container). The launch carries an empty digest and no repo, so the provider sets nothing.
    let (fake, cloud) = cloud();
    let mut request = default_req();
    request.version.image_digest = String::new();

    submit(&cloud, request).await.expect("should submit");

    let launched = fake.launched.lock().unwrap();
    assert!(launched[0].image_digest.is_empty());
    assert_eq!(launched[0].worker_image_repo_uri, None);
}

#[tokio::test]
async fn secret_is_redacted_before_the_run_submitted_boundary() {
    let (fake, cloud) = cloud();
    let secret = "unique-secret-that-must-never-cross-the-boundary";
    let params = serde_json::json!({
        "api_key": secret,
        "reads": "s3://b/in.fastq",
        "outdir": "s3://b/out/"
    });

    let out =
        submit(&cloud, req(params, vec!["s3://b/in.fastq".into()])).await.expect("should submit");

    let RunSubmitted { params, .. } = anchor(&out);
    assert_eq!(
        params.get("api_key"),
        Some(&serde_json::Value::String(REDACTED_SECRET_PLACEHOLDER.to_owned()))
    );
    assert_eq!(params.get("reads"), Some(&serde_json::json!("s3://b/in.fastq")));

    // The serialized event is exactly what the append-only log stores and what audit,
    // provenance, and UI projections consume. Prove the raw value cannot reach any of them.
    let serialized = serde_json::to_string(&out.run_submitted).expect("event serializes");
    assert!(!serialized.contains(secret));
    assert!(serialized.contains(REDACTED_SECRET_PLACEHOLDER));
    // Only the log's copy is redacted: the coordinator still runs with the real value.
    assert_eq!(fake.launched.lock().unwrap()[0].params["api_key"], serde_json::json!(secret));
}

#[tokio::test]
async fn malformed_frozen_form_schema_fails_closed_before_launch() {
    let (fake, cloud) = cloud();
    let mut request = default_req();
    request.version.form_schema = serde_json::json!({});

    let err = submit(&cloud, request).await.unwrap_err();

    assert!(matches!(err, SubmitError::InvalidFormSchema(_)));
    assert!(fake.launched.lock().unwrap().is_empty());
}

#[tokio::test]
async fn missing_required_param_fails_validation() {
    let (fake, cloud) = cloud();
    let params = serde_json::json!({"outdir": "s3://b/out/"}); // no reads
    let err = submit(&cloud, req(params, vec![])).await.unwrap_err();
    match err {
        SubmitError::Validation(errs) => {
            // Only the missing field: not one the caller gave.
            let fields: Vec<&str> = errs.iter().map(|e| e.field.as_str()).collect();
            assert_eq!(fields, ["reads"], "{errs:?}");
        }
        other => panic!("expected a reads validation error, got {other:?}"),
    }
    assert!(fake.launched.lock().unwrap().is_empty());
}

#[tokio::test]
async fn output_prefix_override_reaches_launch_and_anchor() {
    let (fake, cloud) = cloud();
    let mut r = default_req();
    r.plan.output_prefix = Some("s3://b/custom-root/".into());
    let out = submit(&cloud, r).await.expect("should submit");
    assert_eq!(
        fake.launched.lock().unwrap()[0].output_prefix.as_deref(),
        Some("s3://b/custom-root/")
    );
    // The anchor records the same prefix, so the run's output location is a fact of the log.
    assert_eq!(anchor(&out).output_prefix.as_deref(), Some("s3://b/custom-root/"));
}

#[tokio::test]
async fn absent_output_prefix_resolves_to_the_per_run_default_on_launch_and_anchor() {
    // No override ⇒ the isolated per-run default, passed explicitly (not left to the entrypoint's
    // fallback) and recorded on the anchor, so the two agree by construction.
    let (fake, cloud) = cloud();
    let out = submit(&cloud, default_req()).await.expect("should submit");
    assert_eq!(
        fake.launched.lock().unwrap()[0].output_prefix.as_deref(),
        Some("s3://b/biffix/runs/r1/")
    );
    assert_eq!(anchor(&out).output_prefix.as_deref(), Some("s3://b/biffix/runs/r1/"));
}

#[tokio::test]
async fn no_project_bucket_and_no_override_records_no_prefix() {
    // A local composition with no bucket has nowhere to derive a default from.
    let (fake, cloud) = cloud();
    let mut r = default_req();
    r.project.bucket = String::new();
    let out = submit(&cloud, r).await.expect("should submit");
    assert_eq!(fake.launched.lock().unwrap()[0].output_prefix, None);
    assert_eq!(anchor(&out).output_prefix, None);
}

#[tokio::test]
async fn nextflow_submit_mints_a_fresh_session_on_launch_and_anchor() {
    // A Nextflow run gets a fresh session uuid the coordinator adopts (NXF_UUID), recorded on the
    // anchor so the run's resume cache (`<prefix>.nextflow/cache/<id>/`) is known up front.
    let (fake, cloud) = cloud();
    let mut r = default_req();
    r.version.engine = Engine::Nextflow;
    let out = submit(&cloud, r).await.expect("should submit");
    let launch = fake.launched.lock().unwrap()[0].clone();
    let session = launch.engine_session_id.expect("a Nextflow launch carries a session");
    assert!(!launch.resume, "a fresh submit does not resume");
    let parsed = uuid::Uuid::parse_str(&session).expect("session id is a uuid");
    assert_eq!(parsed.get_version_num(), 4);
    let RunSubmitted { engine_session_id, resumed_from, .. } = anchor(&out);
    assert_eq!(engine_session_id.as_deref(), Some(session.as_str()));
    assert_eq!(*resumed_from, None);
    // Fresh per submit: another run gets its own session (one shared would share a resume cache).
    let (fake2, cloud2) = self::cloud();
    let mut r2 = default_req();
    r2.version.engine = Engine::Nextflow;
    r2.run_id = RunId::new("r2");
    let out2 = submit(&cloud2, r2).await.expect("should submit");
    let session2 = fake2.launched.lock().unwrap()[0]
        .engine_session_id
        .clone()
        .expect("a Nextflow launch carries a session");
    assert_ne!(session2, session);
    assert_eq!(anchor(&out2).engine_session_id.as_deref(), Some(session2.as_str()));
}

#[tokio::test]
async fn snakemake_submit_carries_no_engine_session() {
    let (fake, cloud) = cloud();
    let out = submit(&cloud, default_req()).await.expect("should submit");
    assert_eq!(fake.launched.lock().unwrap()[0].engine_session_id, None);
    assert_eq!(anchor(&out).engine_session_id, None);
}

#[tokio::test]
async fn boolean_params_reach_launch_and_anchor_as_booleans() {
    // "false" as a string would be truthy to Nextflow; the pipeline must hand on a real boolean.
    let (fake, cloud) = cloud();
    let params = serde_json::json!({
        "reads": "s3://b/in.fastq", "outdir": "s3://b/out/", "skip_qc": "false"
    });
    let mut r = req(params, vec!["s3://b/in.fastq".into()]);
    r.version.form_schema["fields"].as_array_mut().unwrap().push(serde_json::json!({
        "name": "skip_qc", "widget": "enum", "required": false,
        "enum_values": ["true", "false"],
        "is_output": false, "is_secret": false, "is_path_input": false
    }));
    let out = submit(&cloud, r).await.expect("should submit");
    assert_eq!(fake.launched.lock().unwrap()[0].params["skip_qc"], serde_json::json!(false));
    assert_eq!(anchor(&out).params["skip_qc"], serde_json::json!(false));
}

#[tokio::test]
async fn nextflow_outdir_defaults_under_the_output_prefix_when_unset() {
    // The fixture declares a required `outdir`: unset on a Nextflow submit, it defaults into the
    // run's prefix (so the required check passes) on both the launch and the anchor.
    let (fake, cloud) = cloud();
    let mut r =
        req(serde_json::json!({"reads": "s3://b/in.fastq"}), vec!["s3://b/in.fastq".into()]);
    r.version.engine = Engine::Nextflow;
    let out = submit(&cloud, r).await.expect("should submit");
    assert_eq!(fake.launched.lock().unwrap()[0].params["outdir"], "s3://b/biffix/runs/r1/results");
    assert_eq!(anchor(&out).params["outdir"], "s3://b/biffix/runs/r1/results");
}

#[tokio::test]
async fn nextflow_outdir_given_by_the_caller_is_kept() {
    let (fake, cloud) = cloud();
    let mut r = req(
        serde_json::json!({"reads": "s3://b/in.fastq", "outdir": "s3://b/mine/"}),
        vec!["s3://b/in.fastq".into()],
    );
    r.version.engine = Engine::Nextflow;
    submit(&cloud, r).await.expect("should submit");
    assert_eq!(fake.launched.lock().unwrap()[0].params["outdir"], "s3://b/mine/");
}

#[tokio::test]
async fn snakemake_outdir_is_not_defaulted() {
    // The default is a Nextflow (nf-core) convention; a Snakemake version still has to be given it.
    let (fake, cloud) = cloud();
    let r = req(serde_json::json!({"reads": "s3://b/in.fastq"}), vec!["s3://b/in.fastq".into()]);
    match submit(&cloud, r).await {
        Err(SubmitError::Validation(errs)) => {
            let fields: Vec<&str> = errs.iter().map(|e| e.field.as_str()).collect();
            assert_eq!(fields, ["outdir"], "{errs:?}");
        }
        other => panic!("expected an outdir validation error, got {other:?}"),
    }
    assert!(fake.launched.lock().unwrap().is_empty());
}
