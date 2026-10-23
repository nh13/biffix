//! The submit pipeline's order (fill, prune, validate, check, write) and what a refused or unrecordable submit leaves behind.

use super::*;

/// Register `register` (and the `grch38` bundle), returning the app and its doubles.
async fn app_with(register: &Value) -> (axum::Router, Arc<MemEventSink>, Arc<FakeCloud>) {
    let (state, events, cloud) = state();
    let app = api::router(state);
    let bundle = bundle_body(
        "grch38",
        json!({"fasta": "s3://test-bucket/refs/hg38.fa", "star": "s3://test-bucket/refs/star/"}),
    );
    setup(&app, "/references", "admins", &bundle).await;
    setup(&app, "/workflows", "developers", register).await;
    (app, events, cloud)
}

fn form_body(git_sha: &str, form_schema: Value) -> Value {
    let mut body = register_body();
    body["git_sha"] = json!(git_sha);
    body["form_schema"] = form_schema;
    body
}

#[tokio::test]
async fn a_bundle_filled_param_that_a_gate_hides_is_pruned_not_sent() {
    // `star_index` is shown only for the STAR aligner, and the bundle would fill it.
    let register = form_body(
        "gatedref",
        json!({
            "fields": [
                {"name": "aligner", "widget": "enum", "required": false,
                 "enum_values": ["bwa", "star"], "default": "bwa",
                 "is_output": false, "is_secret": false, "is_path_input": false},
                {"name": "fasta", "widget": "s3_file", "required": true,
                 "is_output": false, "is_secret": false, "is_path_input": true},
                {"name": "star_index", "widget": "s3_dir", "required": true,
                 "show_if": [{"field": "aligner", "values": ["star"]}],
                 "is_output": false, "is_secret": false, "is_path_input": true}
            ],
            "warnings": [],
            "references": {"kind": "assembly", "fill": {"fasta": "fasta", "star_index": "star"},
                           "required": ["fasta"], "label": "Reference genome"}
        }),
    );
    let (app, events, cloud) = app_with(&register).await;
    // Were the hidden param checked, its absent folder would refuse the run.
    cloud.absent.lock().unwrap().insert("s3://test-bucket/refs/star/".into());
    let body = json!({
        "workflow_version_id": "gatedref:hash:0",
        "params": {"aligner": "bwa", "star_index": "s3://test-bucket/mine/"},
        "reference": "grch38"
    });
    let res = app.clone().oneshot(post("/runs", Some("operators"), &body)).await.unwrap();
    assert_eq!(res.status(), StatusCode::ACCEPTED, "{}", text_body(res).await);
    assert_eq!(
        cloud.launched.lock().unwrap()[0].params,
        json!({"aligner": "bwa", "fasta": "s3://test-bucket/refs/hg38.fa"})
    );
    let anchor = recorded_anchor(&events);
    assert_eq!(anchor.dropped_inactive.as_deref(), Some(&["star_index".to_string()][..]));
    assert_eq!(anchor.input_uris, vec!["s3://test-bucket/refs/hg38.fa".to_string()]);
    // The bundle records only the URI it filled into a param the run kept.
    let used = anchor.reference_bundle.unwrap();
    assert_eq!(
        used.uris.into_iter().collect::<Vec<_>>(),
        [("fasta".to_string(), "s3://test-bucket/refs/hg38.fa".to_string())]
    );

    // With the gate on, the bundle's folder is filled, checked and sent.
    cloud.absent.lock().unwrap().clear();
    let body = json!({
        "workflow_version_id": "gatedref:hash:0",
        "params": {"aligner": "star"},
        "reference": "grch38"
    });
    let res = app.clone().oneshot(post("/runs", Some("operators"), &body)).await.unwrap();
    assert_eq!(res.status(), StatusCode::ACCEPTED);
    assert_eq!(
        cloud.launched.lock().unwrap()[1].params["star_index"],
        json!("s3://test-bucket/refs/star/")
    );
}

#[tokio::test]
async fn a_gate_on_a_bundle_filled_param_is_judged_on_the_bundle_s_uri() {
    // `on_bundle` shows for the bundle's fasta, `on_mine` for the one the caller gave.
    let register = form_body(
        "refgate",
        json!({
            "fields": [
                {"name": "fasta", "widget": "s3_file", "required": true,
                 "is_output": false, "is_secret": false, "is_path_input": true},
                {"name": "on_bundle", "widget": "text", "required": true,
                 "show_if": [{"field": "fasta", "values": ["s3://test-bucket/refs/hg38.fa"]}],
                 "is_output": false, "is_secret": false, "is_path_input": false},
                {"name": "on_mine", "widget": "text", "required": false,
                 "show_if": [{"field": "fasta", "values": ["s3://test-bucket/mine.fa"]}],
                 "is_output": false, "is_secret": false, "is_path_input": false}
            ],
            "warnings": [],
            "references": {"kind": "assembly", "fill": {"fasta": "fasta"},
                           "required": ["fasta"], "label": "Reference genome"}
        }),
    );
    let (app, events, cloud) = app_with(&register).await;
    let body = |params: Value| json!({"workflow_version_id": "refgate:hash:0", "params": params, "reference": "grch38"});
    let res = app
        .clone()
        .oneshot(post(
            "/runs",
            Some("operators"),
            &body(
                json!({"fasta": "s3://test-bucket/mine.fa", "on_bundle": "kept", "on_mine": "x"}),
            ),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::ACCEPTED, "{}", text_body(res).await);
    assert_eq!(
        cloud.launched.lock().unwrap()[0].params,
        json!({"fasta": "s3://test-bucket/refs/hg38.fa", "on_bundle": "kept"})
    );
    assert_eq!(
        recorded_anchor(&events).dropped_inactive.as_deref(),
        Some(&["on_mine".to_string()][..])
    );
    // The bundle's value turns on a required field, so leaving it out is a validation error.
    let res = app
        .clone()
        .oneshot(post(
            "/runs",
            Some("operators"),
            &body(json!({"fasta": "s3://test-bucket/mine.fa"})),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    assert!(text_body(res).await.contains("on_bundle: required"));
}

#[tokio::test]
async fn a_rejected_submit_writes_nothing() {
    let mut register = sheet_register_body();
    register["form_schema"]["fields"]
        .as_array_mut()
        .unwrap()
        .push(json!({"name": "threads", "widget": "number", "required": false, "max": 8,
                     "is_output": false, "is_secret": false, "is_path_input": false}));
    let (app, events, cloud) = app_with(&register).await;
    let rows = json!({"input": [
        {"sample": "P01", "fastq_1": "s3://b/P01_R1.fq.gz"},
        {"sample": "P02", "fastq_1": "s3://b/P02_R1.fq.gz"}
    ]});
    cloud
        .absent
        .lock()
        .unwrap()
        .extend(["s3://b/P01_R1.fq.gz".to_string(), "s3://b/P02_R1.fq.gz".to_string()]);
    let body = |params: Value| json!({"workflow_version_id": "sheet:hash:0", "params": params, "tables": rows});

    // A missing input: every one listed, for people.
    let res =
        app.clone().oneshot(post("/runs", Some("operators"), &body(json!({})))).await.unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        text_body(res).await,
        "input(s) missing in S3: s3://b/P01_R1.fq.gz, s3://b/P02_R1.fq.gz"
    );

    // A param out of range (inputs present).
    cloud.absent.lock().unwrap().clear();
    let res = app
        .clone()
        .oneshot(post("/runs", Some("operators"), &body(json!({"threads": 99}))))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    assert!(text_body(res).await.contains("threads: must be at most 8"));

    assert!(
        cloud.objects.lock().unwrap().is_empty(),
        "no samplesheet or image map for a rejected run"
    );
    assert!(cloud.launched.lock().unwrap().is_empty());
    assert!(events.events.lock().unwrap().is_empty());
}

#[tokio::test]
async fn null_params_are_none_given_and_other_non_objects_are_refused() {
    let (app, _, cloud) = app_with(&sheet_register_body()).await;
    let body = |params: Value| {
        json!({"workflow_version_id": "sheet:hash:0", "params": params,
               "tables": {"input": [{"sample": "P01", "fastq_1": "s3://b/P01_R1.fq.gz"}]}})
    };
    let res =
        app.clone().oneshot(post("/runs", Some("operators"), &body(Value::Null))).await.unwrap();
    assert_eq!(res.status(), StatusCode::ACCEPTED);
    let run_id = json_body(res).await["run_id"].as_str().unwrap().to_string();
    assert_eq!(
        cloud.launched.lock().unwrap()[0].params["input"],
        json!(format!("s3://test-bucket/biffix/runs/{run_id}/inputs/input.csv"))
    );
    for params in [json!("x"), json!([1]), json!(3)] {
        let res = app
            .clone()
            .oneshot(post("/runs", Some("operators"), &body(params.clone())))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST, "{params}");
        assert_eq!(text_body(res).await, "params must be a JSON object");
    }
    assert_eq!(cloud.launched.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn a_malformed_input_is_refused_before_any_aws_call() {
    let (app, _, cloud) = app_with(&register_body()).await;
    // What the setup checked (its reference bundle's files).
    let checked = cloud.object_checks.lock().unwrap().len();
    for reads in ["foo", "s3://test-bucket/", "s3:///k"] {
        let body = json!({"workflow_version_id": "abc:hash:0",
                          "params": {"reads": reads, "outdir": "s3://b/out/"}});
        let res = app.clone().oneshot(post("/runs", Some("operators"), &body)).await.unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST, "{reads}");
        assert_eq!(
            text_body(res).await,
            format!("input is not a valid s3://bucket/key URI: {reads}")
        );
    }
    assert!(cloud.launched.lock().unwrap().is_empty());
    assert!(cloud.objects.lock().unwrap().is_empty());
    // Refused before any AWS call: nothing more was existence-checked.
    assert_eq!(cloud.object_checks.lock().unwrap().len(), checked);
    assert!(cloud.heads.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_folder_input_is_probed_below_its_slash() {
    let register = form_body(
        "dir",
        json!({
            "fields": [
                {"name": "data", "widget": "s3_dir", "required": true,
                 "is_output": false, "is_secret": false, "is_path_input": true}
            ],
            "warnings": []
        }),
    );
    let (app, _, cloud) = app_with(&register).await;
    // Only keys under `data/` count: `data2/…` would satisfy a bare `data` prefix.
    cloud.absent.lock().unwrap().insert("s3://test-bucket/data/".into());
    let body = json!({"workflow_version_id": "dir:hash:0",
                      "params": {"data": "s3://test-bucket/data"}});
    let res = app.clone().oneshot(post("/runs", Some("operators"), &body)).await.unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    assert_eq!(text_body(res).await, "input(s) missing in S3: s3://test-bucket/data");
    // A bucket's root is a folder too.
    let body = json!({"workflow_version_id": "dir:hash:0",
                      "params": {"data": "s3://test-bucket/"}});
    let res = app.clone().oneshot(post("/runs", Some("operators"), &body)).await.unwrap();
    assert_eq!(res.status(), StatusCode::ACCEPTED);
}

#[tokio::test]
async fn delete_on_success_is_refused_where_the_sweep_cannot_honor_it() {
    let retention = json!({"retention": "delete_on_success"});
    let mut custom = resume_body("r0", retention.clone());
    custom.as_object_mut().unwrap().remove("resume_from");
    custom["output_prefix"] = json!("s3://test-bucket/results/");
    let (status, body, cloud, _) =
        resume_submit(None, nextflow_register_body(), custom, "operators").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body.starts_with(
            "retention delete_on_success applies only to a run in its own per-run prefix"
        ),
        "{body}"
    );
    assert!(cloud.launched.lock().unwrap().is_empty());

    // A resume of a run in a custom prefix is in that custom prefix too.
    let mut prior = resumable_prior("r0");
    prior.output_prefix = Some("s3://test-bucket/results/".into());
    let (status, body, _, _) = resume_submit(
        Some(prior),
        nextflow_register_body(),
        resume_body("r0", retention.clone()),
        "operators",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body.starts_with(
            "retention delete_on_success applies only to a run in its own per-run prefix"
        ),
        "{body}"
    );

    // A resume in the prior run's own per-run prefix is swept with it.
    let (status, body, _, _) = resume_submit(
        Some(resumable_prior("r0")),
        nextflow_register_body(),
        resume_body("r0", retention),
        "operators",
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
}

#[tokio::test]
async fn a_coordinator_whose_run_cannot_be_recorded_is_stopped() {
    let (mut state, _, cloud) = state();
    state.events = Arc::new(FailingSink::new(
        Arc::new(MemEventSink::new()),
        biffix_core::event::Kind::RunSubmitted,
    ));
    let app = api::router(state);
    let res = app
        .clone()
        .oneshot(post("/workflows", Some("developers"), &register_body()))
        .await
        .unwrap();
    assert!(res.status().is_success());
    let body = json!({"workflow_version_id": "abc:hash:0",
                      "params": {"reads": "s3://b/in.fastq", "outdir": "s3://b/out/"}});
    let res = app.clone().oneshot(post("/runs", Some("operators"), &body)).await.unwrap();
    assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
    let text = text_body(res).await;
    assert!(text.contains("its coordinator (coord-1) was stopped"), "{text}");
    assert_eq!(
        cloud.terminated.lock().unwrap().clone(),
        vec![("coord-1".to_string(), "biffix: the run could not be recorded".to_string())]
    );

    // If stopping it fails too, the caller is told it may still be running.
    cloud.fail_terminate.store(true, std::sync::atomic::Ordering::SeqCst);
    let res = app.clone().oneshot(post("/runs", Some("operators"), &body)).await.unwrap();
    assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(text_body(res).await.contains("may still be running"));
}

#[tokio::test]
async fn a_target_that_is_not_one_engine_target_is_refused_before_anything_launches() {
    let (state, events, cloud) = state();
    let app = api::router(state);
    setup(&app, "/workflows", "developers", &register_body()).await;
    let submit = |target: &str| {
        json!({
            "workflow_version_id": "abc:hash:0",
            "params": {"reads": "s3://b/in.fastq", "outdir": "s3://b/out/"},
            "target": target
        })
    };
    // An option (it would override the coordinator's own), more than one word, a shell word.
    for target in ["--snakefile=evil.smk", "-n", "all other", "all;x"] {
        let res =
            app.clone().oneshot(post("/runs", Some("operators"), &submit(target))).await.unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST, "{target}");
        let msg = text_body(res).await;
        assert!(msg.starts_with("target: a snakemake target must be"), "{msg}");
    }
    assert!(cloud.launched.lock().unwrap().is_empty());
    assert!(events.events.lock().unwrap().is_empty());
    // A rule name or a relative file path is the coordinator's target, trimmed.
    let res = app
        .clone()
        .oneshot(post("/runs", Some("operators"), &submit(" results/S1.bam ")))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::ACCEPTED);
    assert_eq!(cloud.launched.lock().unwrap()[0].target.as_deref(), Some("results/S1.bam"));
    // A registration's own target is held to the same rule.
    let mut register = register_body();
    register["git_sha"] = json!("badtarget");
    register["target"] = json!("--retries=1000");
    let res = app.clone().oneshot(post("/workflows", Some("developers"), &register)).await.unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    assert!(text_body(res).await.starts_with("target: "));
}

#[tokio::test]
async fn without_a_capability_secret_a_submit_is_refused_as_the_sweep_refuses_it() {
    let (state, events, cloud) = state();
    let state = testing::TestState::from(state).without_capability().build();
    let app = api::router(state);
    setup(&app, "/workflows", "developers", &register_body()).await;
    let res = app
        .clone()
        .oneshot(post(
            "/runs",
            Some("operators"),
            &json!({
                "workflow_version_id": "abc:hash:0",
                "params": {"reads": "s3://b/in.fastq", "outdir": "s3://b/out/"}
            }),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        text_body(res).await,
        "launching is disabled: this deploy has no capability secret \
         (BIFFIX_CAPABILITY_SECRET_ARN), so a run's coordinator could not report"
    );
    // Nothing launched, written or recorded.
    assert!(cloud.launched.lock().unwrap().is_empty());
    assert!(cloud.object_checks.lock().unwrap().is_empty());
    assert!(events.events.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_caller_chosen_prefix_another_run_is_using_is_refused_until_it_ends() {
    let store = MemStore::new();
    let mut active = testing::run_detail("r-active", "running");
    active.output_prefix = Some("s3://test-bucket/results/".into());
    store.insert(active);
    let (state, events, cloud) = state_with_store(store);
    let app = api::router(state.clone());
    setup(&app, "/workflows", "developers", &register_body()).await;
    let submit = json!({
        "workflow_version_id": "abc:hash:0",
        "params": {"reads": "s3://b/in.fastq", "outdir": "s3://b/out/"},
        "output_prefix": "s3://test-bucket/results"
    });
    // Two runs in one prefix would share its work dir.
    let res = app.clone().oneshot(post("/runs", Some("operators"), &submit)).await.unwrap();
    assert_eq!(res.status(), StatusCode::CONFLICT);
    assert_eq!(
        text_body(res).await,
        "run r-active is still using s3://test-bucket/results/; wait for it to finish, or \
         choose another output_prefix"
    );
    assert!(cloud.launched.lock().unwrap().is_empty());
    assert!(events.events.lock().unwrap().is_empty());
    // Another submit into it holding its claim: a 409 too.
    let held = state.store.claim_output_prefix("s3://test-bucket/other/").await.unwrap();
    let mut other = submit.clone();
    other["output_prefix"] = json!("s3://test-bucket/other/");
    let res = app.clone().oneshot(post("/runs", Some("operators"), &other)).await.unwrap();
    assert_eq!(res.status(), StatusCode::CONFLICT);
    drop(held);
    // Once it is free, the run launches there.
    let res = app.clone().oneshot(post("/runs", Some("operators"), &other)).await.unwrap();
    assert_eq!(res.status(), StatusCode::ACCEPTED);
}
