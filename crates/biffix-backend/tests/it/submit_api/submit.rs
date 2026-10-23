//! Submitting a run: the happy path, output prefixes, retries, gated params and run names.

use super::*;

#[tokio::test]
async fn developer_registers_then_operator_submits() {
    let (state, events, cloud) = state();
    // The input is a versioned object: its version id is the run's provenance tag for it.
    cloud.objects.lock().unwrap().insert("s3://b/in.fastq".into(), b"ACGT".to_vec());
    cloud.versions.lock().unwrap().insert("s3://b/in.fastq".into(), "v-in-1".into());
    let app = api::router(state);

    // Developer registers a version.
    let res = app
        .clone()
        .oneshot(post("/workflows", Some("developers"), &register_body()))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CREATED);
    let v = json_body(res).await;
    let version_id = v["id"].as_str().unwrap().to_string();
    assert_eq!(version_id, "abc:hash:0");
    assert_eq!(v["image_digest"].as_str().unwrap(), format!("sha256:{}", "a".repeat(64)));

    // Viewer cannot submit.
    let submit_body = json!({
        "workflow_version_id": version_id,
        "params": {"reads": "s3://b/in.fastq", "outdir": "s3://b/out/"}
    });
    let res = app.clone().oneshot(post("/runs", Some("viewers"), &submit_body)).await.unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);

    // Operator submits successfully.
    let res = app.clone().oneshot(post("/runs", Some("operators"), &submit_body)).await.unwrap();
    assert_eq!(res.status(), StatusCode::ACCEPTED);
    let out = json_body(res).await;
    assert!(out["run_id"].as_str().unwrap().starts_with("run-"));
    assert_eq!(out["coordinator_job_id"].as_str().unwrap(), "coord-1");

    // The anchor AND the coordinator→run binding were persisted (both are required: without the
    // binding the coordinator's AWS lifecycle events never attribute to the run).
    {
        let recorded = events.events.lock().unwrap();
        assert_eq!(recorded.len(), 2, "run_submitted + coordinator_submitted");
        let kinds: Vec<_> = recorded.iter().map(biffix_core::event::RunEvent::kind).collect();
        assert!(
            kinds.contains(&biffix_core::event::Kind::RunSubmitted),
            "run_submitted anchor missing: {kinds:?}"
        );
        assert!(
            kinds.contains(&biffix_core::event::Kind::CoordinatorSubmitted),
            "coordinator_submitted binding missing: {kinds:?}"
        );
        // The binding must carry the coordinator's batch_job_id, or attribution/reconciliation
        // has nothing to key off.
        let binding = recorded
            .iter()
            .find(|e| e.kind() == biffix_core::event::Kind::CoordinatorSubmitted)
            .unwrap();
        assert!(binding.batch_job_id.is_some(), "binding lacks batch_job_id");
    }
    let launched = cloud.launched.lock().unwrap();
    assert_eq!(launched.len(), 1);
    assert_eq!(launched[0].coordinator_queue, "coordinator");
    // A Snakemake version's tasks run its image derived with the engine runtime; the anchor keeps
    // the declared digest.
    let source = format!("sha256:{}", "a".repeat(64));
    let derived = biffix_workflow::images::derived_key(&source, &launched[0].variant.runtime_key);
    assert_eq!(launched[0].image_digest, format!("sha256:{derived}"));
    drop(launched);
    let anchor = recorded_anchor(&events);
    assert_eq!(anchor.image_source_digest, Some(source));
    // Each input's provenance tag is the version id its existence check saw.
    assert_eq!(
        anchor.input_provenance,
        Some(BTreeMap::from([("s3://b/in.fastq".to_string(), "v-in-1".to_string())]))
    );
}

#[tokio::test]
async fn submit_unknown_version_is_404() {
    let (state, _, _) = state();
    let body = json!({"workflow_version_id": "nope:nope", "params": {}});
    let res = api::router(state).oneshot(post("/runs", Some("operators"), &body)).await.unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn submit_output_prefix_must_be_within_project_bucket() {
    let (state, _events, cloud) = state();
    let app = api::router(state);
    // Register a version to submit against (project_bucket is "test-bucket").
    setup(&app, "/workflows", "developers", &register_body()).await;

    // A prefix outside the project bucket is rejected (the run role can't write there, and it would
    // let a caller aim outputs at an arbitrary bucket).
    // So is one in a bucket whose name merely starts with the project bucket's, or inside biffix's
    // own namespaces (another run's work dir, inputs and provenance; its caches; workflow sources).
    for (prefix, message) in [
        ("s3://someone-elses-bucket/x/", "within the project bucket s3://test-bucket/"),
        ("s3://test-bucket-evil/x/", "within the project bucket s3://test-bucket/"),
        (
            "s3://test-bucket/biffix/runs/run-other/",
            "must not be inside s3://test-bucket/biffix/runs/",
        ),
        // biffix's caches (the cache-expiry sweep deletes under them) and staged workflow sources.
        (
            "s3://test-bucket/biffix/cache/snakemake/",
            "must not be inside s3://test-bucket/biffix/cache/",
        ),
        (
            "s3://test-bucket/workflow-sources/x/",
            "must not be inside s3://test-bucket/workflow-sources/",
        ),
        // Spellings of the runs namespace a normalizing consumer would resolve into it.
        ("s3://test-bucket//biffix/runs/run-other/", "has an empty, `.` or `..` path segment"),
        ("s3://test-bucket/./biffix/runs/run-other/", "has an empty, `.` or `..` path segment"),
        ("s3://test-bucket/x/../biffix/runs/run-other/", "has an empty, `.` or `..` path segment"),
    ] {
        let bad = json!({
            "workflow_version_id": "abc:hash:0",
            "params": {"reads": "s3://b/in.fastq", "outdir": "s3://b/out/"},
            "output_prefix": prefix
        });
        let res = app.clone().oneshot(post("/runs", Some("operators"), &bad)).await.unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST, "{prefix}");
        let text = text_body(res).await;
        assert!(text.contains(message), "{prefix}: {text}");
    }
    assert!(cloud.launched.lock().unwrap().is_empty());
    assert!(cloud.objects.lock().unwrap().is_empty());

    // A prefix within the project bucket is accepted and reaches the coordinator launch ending in
    // `/`, since the coordinator appends `work`, `.nextflow/cache`, … to it directly.
    for (prefix, launched_as) in [
        ("s3://test-bucket/", "s3://test-bucket/"),
        ("s3://test-bucket", "s3://test-bucket/"),
        ("s3://test-bucket/results", "s3://test-bucket/results/"),
    ] {
        let good = json!({
            "workflow_version_id": "abc:hash:0",
            "params": {"reads": "s3://b/in.fastq", "outdir": "s3://b/out/"},
            "output_prefix": prefix
        });
        let res = app.clone().oneshot(post("/runs", Some("operators"), &good)).await.unwrap();
        assert_eq!(res.status(), StatusCode::ACCEPTED, "{prefix}");
        let launched = cloud.launched.lock().unwrap();
        assert_eq!(launched.last().unwrap().output_prefix.as_deref(), Some(launched_as));
    }
}

#[tokio::test]
async fn submit_retries_and_keep_going_reach_the_coordinator_and_are_capped() {
    let (state, _events, cloud) = state();
    let app = api::router(state);
    setup(&app, "/workflows", "developers", &register_body()).await;
    let body = |extra: Value| {
        let mut b = json!({
            "workflow_version_id": "abc:hash:0",
            "params": {"reads": "s3://b/in.fastq", "outdir": "s3://b/out/"},
        });
        b.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        b
    };

    // More retries than the cap is rejected before anything launches.
    let res = app
        .clone()
        .oneshot(post("/runs", Some("operators"), &body(json!({"retries": 11}))))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    assert_eq!(text_body(res).await, "retries must be at most 10");
    assert!(cloud.launched.lock().unwrap().is_empty());

    // Set: passed to the coordinator launch.
    let res = app
        .clone()
        .oneshot(post(
            "/runs",
            Some("operators"),
            &body(json!({"retries": 5, "keep_going": false})),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::ACCEPTED);
    // Unset: left to the coordinator's defaults.
    let res =
        app.clone().oneshot(post("/runs", Some("operators"), &body(json!({})))).await.unwrap();
    assert_eq!(res.status(), StatusCode::ACCEPTED);

    // The cap itself is allowed.
    let res = app
        .clone()
        .oneshot(post("/runs", Some("operators"), &body(json!({"retries": 10}))))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::ACCEPTED);

    let launched = cloud.launched.lock().unwrap();
    assert_eq!(launched.len(), 3);
    assert_eq!((launched[0].retries, launched[0].keep_going), (Some(5), Some(false)));
    assert_eq!((launched[1].retries, launched[1].keep_going), (None, None));
    assert_eq!(launched[2].retries, Some(10));
}

/// A version whose UMI inputs are shown only while `with_umi` is on.
fn gated_register_body() -> Value {
    let mut body = register_body();
    body["git_sha"] = json!("gated");
    body["form_schema"] = json!({
        "fields": [
            {"name": "with_umi", "widget": "enum", "required": false, "enum_values": ["true", "false"],
             "default": false, "is_output": false, "is_secret": false, "is_path_input": false},
            {"name": "umi_pattern", "widget": "text", "required": true,
             "show_if": [{"field": "with_umi", "values": [true]}],
             "is_output": false, "is_secret": false, "is_path_input": false},
            {"name": "umi_whitelist", "widget": "s3_file", "required": false,
             "show_if": [{"field": "with_umi", "values": [true]}],
             "is_output": false, "is_secret": false, "is_path_input": true}
        ],
        "warnings": []
    });
    body
}

#[tokio::test]
async fn submit_drops_hidden_params_and_records_the_run_name() {
    let (state, events, cloud) = state();
    let app = api::router(state);
    let res = app
        .clone()
        .oneshot(post("/workflows", Some("developers"), &gated_register_body()))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CREATED);

    // The gate is off, so the UMI params are hidden: the required pattern is not required, and the
    // malformed whitelist path is never checked (it would be a 400 if it were).
    let body = json!({
        "workflow_version_id": "gated:hash:0",
        "params": {"with_umi": "false", "umi_pattern": "NNNN", "umi_whitelist": "not-a-uri"},
        "run_name": "  batch-12  "
    });
    let res = app.clone().oneshot(post("/runs", Some("operators"), &body)).await.unwrap();
    assert_eq!(res.status(), StatusCode::ACCEPTED);

    let biffix_core::event::RunSubmitted { params, run_name, dropped_inactive, .. } =
        &recorded_anchor(&events);
    assert_eq!(params, &json!({"with_umi": false}));
    assert_eq!(run_name.as_deref(), Some("batch-12"));
    assert_eq!(
        dropped_inactive.as_deref(),
        Some(&["umi_pattern".to_string(), "umi_whitelist".to_string()][..])
    );
    assert_eq!(cloud.launched.lock().unwrap()[0].params, json!({"with_umi": false}));
}

#[tokio::test]
async fn submit_with_the_gate_on_requires_the_gated_params() {
    let (state, _, _) = state();
    let app = api::router(state);
    setup(&app, "/workflows", "developers", &gated_register_body()).await;
    let body = json!({"workflow_version_id": "gated:hash:0", "params": {"with_umi": true}});
    let res = app.clone().oneshot(post("/runs", Some("operators"), &body)).await.unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let text = text_body(res).await;
    assert!(text.contains("umi_pattern: required"), "{text}");
}

#[tokio::test]
async fn a_file_or_folder_input_may_name_a_folder() {
    let (state, _, cloud) = state();
    let app = api::router(state);
    let mut register = register_body();
    register["git_sha"] = json!("either");
    // `p` is an nf-schema `format: path` (a file or a folder); `f` takes a file only.
    register["form_schema"] = json!({
        "fields": [
            {"name": "p", "widget": "s3_file", "required": false, "accepts_folder": true,
             "is_output": false, "is_secret": false, "is_path_input": true},
            {"name": "f", "widget": "s3_file", "required": false,
             "is_output": false, "is_secret": false, "is_path_input": true}
        ],
        "warnings": []
    });
    let res = app.clone().oneshot(post("/workflows", Some("developers"), &register)).await.unwrap();
    assert_eq!(res.status(), StatusCode::CREATED);
    let folder = "s3://test-bucket/refs/star_index/";
    cloud.folders.lock().unwrap().insert(folder.into());
    let submit =
        |param: &str| json!({"workflow_version_id": "either:hash:0", "params": {param: folder}});
    let res = app.clone().oneshot(post("/runs", Some("operators"), &submit("p"))).await.unwrap();
    assert_eq!(res.status(), StatusCode::ACCEPTED);
    let res = app.clone().oneshot(post("/runs", Some("operators"), &submit("f"))).await.unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    assert_eq!(text_body(res).await, "input(s) missing in S3: s3://test-bucket/refs/star_index/");
    assert_eq!(cloud.launched.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn submit_rejects_an_overlong_run_name() {
    let (state, _, _) = state();
    let app = api::router(state);
    setup(&app, "/workflows", "developers", &register_body()).await;
    let body = json!({
        "workflow_version_id": "abc:hash:0",
        "params": {"reads": "s3://b/in.fastq", "outdir": "s3://b/out/"},
        "run_name": "x".repeat(201)
    });
    let res = app.clone().oneshot(post("/runs", Some("operators"), &body)).await.unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    assert_eq!(text_body(res).await, "run_name must be at most 200 characters");
}

#[tokio::test]
async fn a_submit_whose_launch_answer_was_lost_binds_its_job_or_parks_the_run() {
    use std::sync::atomic::Ordering::SeqCst;
    let (state, events, cloud) = state();
    let app = api::router(state.clone());
    setup(&app, "/workflows", "developers", &register_body()).await;
    let body = json!({
        "workflow_version_id": "abc:hash:0",
        "params": {"reads": "s3://b/in.fastq", "outdir": "s3://b/out/"}
    });
    // SubmitJob's answer was lost, and Batch lists the job it created: the run is bound to it.
    cloud.lose_launch_replies.store(1, SeqCst);
    let res = app.clone().oneshot(post("/runs", Some("operators"), &body)).await.unwrap();
    assert_eq!(res.status(), StatusCode::ACCEPTED);
    let out = json_body(res).await;
    assert_eq!(out["state"], "submitted", "{out}");
    assert_eq!(out["coordinator_job_id"], "coord-lost-1", "{out}");

    // Batch does not list it yet: whether there is one is unknown, so the run is not launched
    // again now but parked, for the launch sweep (which stops any it finds first).
    cloud.lose_launch_replies.store(1, SeqCst);
    cloud.hide_live_coordinators.store(true, SeqCst);
    let res = app.clone().oneshot(post("/runs", Some("operators"), &body)).await.unwrap();
    assert_eq!(res.status(), StatusCode::ACCEPTED);
    let out = json_body(res).await;
    assert_eq!(out["state"], "preparing_images", "{out}");
    assert!(out["coordinator_job_id"].is_null(), "{out}");
    assert_eq!(cloud.launched.lock().unwrap().len(), 2);
    let run_id = biffix_core::ids::RunId::new(out["run_id"].as_str().unwrap());
    assert!(state.store.get_pending_launch(&run_id).await.unwrap().is_some());
    // Recorded as preparing, never as submitted.
    let kinds: Vec<_> = events
        .events
        .lock()
        .unwrap()
        .iter()
        .filter(|e| e.run_id.as_ref() == Some(&run_id))
        .map(|e| e.kind().as_str().to_string())
        .collect();
    assert_eq!(kinds, ["run_preparing"]);
}

#[tokio::test]
async fn a_nextflow_outdir_the_form_hides_is_never_filled_back_in() {
    let mut register = nextflow_register_body();
    register["form_schema"] = json!({
        "fields": [
            {"name": "reads", "widget": "s3_file", "required": true,
             "is_output": false, "is_secret": false, "is_path_input": true},
            {"name": "mode", "widget": "enum", "required": false,
             "enum_values": ["publish", "scratch"], "default": "scratch",
             "is_output": false, "is_secret": false, "is_path_input": false},
            {"name": "outdir", "widget": "s3_dir", "required": false,
             "show_if": [{"field": "mode", "values": ["publish"]}],
             "is_output": true, "is_secret": false, "is_path_input": false}
        ],
        "warnings": []
    });
    let submit = json!({
        "workflow_version_id": "abc:hash:0",
        "params": {"reads": "s3://b/in.fastq", "mode": "scratch"}
    });
    let (status, body, cloud, events) = resume_submit(None, register, submit, "operators").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    // Hidden, so dropped: neither the engine nor the anchor gets a default for it.
    let launched = cloud.launched.lock().unwrap();
    assert!(launched[0].params.get("outdir").is_none(), "{:?}", launched[0].params);
    let anchor = recorded_anchor(&events);
    assert!(anchor.params.get("outdir").is_none(), "{:?}", anchor.params);
    assert_eq!(anchor.dropped_inactive, Some(vec!["outdir".to_string()]));
}
