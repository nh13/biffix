//! Reference bundles: managing them and filling a submit's reference params from one.

use super::*;

#[tokio::test]
async fn admins_manage_reference_bundles_and_viewers_list_them() {
    let (state, _, cloud) = state();
    let app = api::router(state);
    let grch38 = bundle_body(
        "grch38",
        json!({"fasta": "s3://test-bucket/refs/hg38.fa", "gtf": "s3://test-bucket/refs/hg38.gtf"}),
    );
    // Only admins write.
    let res = app.clone().oneshot(post("/references", Some("developers"), &grch38)).await.unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);

    // A missing file (no object, folder or index files of that name) is a warning, not a
    // refusal.
    let gtf = "s3://test-bucket/refs/hg38.gtf";
    cloud.absent.lock().unwrap().extend([gtf.to_string(), format!("{gtf}/"), format!("{gtf}.")]);
    let res = app.clone().oneshot(post("/references", Some("admins"), &grch38)).await.unwrap();
    assert_eq!(res.status(), StatusCode::CREATED);
    let saved = json_body(res).await;
    assert_eq!(saved["bundle"]["created_by"], "u");
    assert_eq!(saved["warnings"], json!(["gtf: nothing found at s3://test-bucket/refs/hg38.gtf"]));
    // The id is taken.
    let res = app.clone().oneshot(post("/references", Some("admins"), &grch38)).await.unwrap();
    assert_eq!(res.status(), StatusCode::CONFLICT);
    // Outside every readable source.
    let elsewhere = bundle_body("mm39", json!({"fasta": "s3://other/mm39.fa"}));
    let res = app.clone().oneshot(post("/references", Some("admins"), &elsewhere)).await.unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    let fasta_only = bundle_body("t2t", json!({"fasta": "s3://test-bucket/refs/t2t.fa"}));
    setup(&app, "/references", "admins", &fasta_only).await;
    let list = |q: &str| get(&format!("/references{q}"), "viewers");
    let all = json_body(app.clone().oneshot(list("")).await.unwrap()).await;
    assert_eq!(all.as_array().unwrap().len(), 2);
    let with_gtf =
        json_body(app.clone().oneshot(list("?kind=assembly&keys=fasta,gtf")).await.unwrap()).await;
    assert_eq!(with_gtf.as_array().unwrap().len(), 1);
    assert_eq!(with_gtf[0]["id"], "grch38");

    // Edit keeps the creation stamp; the id cannot change.
    let mut edited = fasta_only.clone();
    edited["display_name"] = json!("Human T2T");
    let res = app
        .clone()
        .oneshot(request("PUT", "/references/grch38", Some("admins"), Some(&edited)))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let res = app
        .clone()
        .oneshot(request("PUT", "/references/t2t", Some("admins"), Some(&edited)))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let saved = json_body(res).await;
    assert_eq!(saved["bundle"]["display_name"], "Human T2T");
    assert_eq!(saved["bundle"]["created_by"], "u");

    let res = app
        .clone()
        .oneshot(request("DELETE", "/references/t2t", Some("admins"), None))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NO_CONTENT);
    let res = app
        .clone()
        .oneshot(request("DELETE", "/references/t2t", Some("admins"), None))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}

/// A version whose form fills `fasta` and `gtf` from a reference bundle that must have `fasta`.
fn reference_register_body() -> Value {
    let mut body = register_body();
    body["git_sha"] = json!("refs");
    body["form_schema"] = json!({
        "fields": [
            {"name": "fasta", "widget": "s3_file", "required": true,
             "is_output": false, "is_secret": false, "is_path_input": true},
            {"name": "gtf", "widget": "s3_file", "required": false,
             "is_output": false, "is_secret": false, "is_path_input": true}
        ],
        "warnings": [],
        "references": {"kind": "assembly", "fill": {"fasta": "fasta", "gtf": "gtf"},
                       "required": ["fasta"], "label": "Reference genome", "custom": true}
    });
    body
}

#[tokio::test]
async fn a_chosen_bundle_fills_the_reference_params_and_is_recorded() {
    let (state, events, _) = state();
    let app = api::router(state);
    for (id, uris) in [
        (
            "grch38",
            json!({"fasta": "s3://test-bucket/refs/hg38.fa", "fai": "s3://test-bucket/refs/hg38.fa.fai"}),
        ),
        ("gtf-only", json!({"gtf": "s3://test-bucket/refs/x.gtf"})),
    ] {
        setup(&app, "/references", "admins", &bundle_body(id, uris)).await;
    }
    setup(&app, "/workflows", "developers", &reference_register_body()).await;
    let submit = |reference: &str| {
        json!({
            "workflow_version_id": "refs:hash:0",
            "params": {"fasta": "s3://test-bucket/mine.fa"},
            "reference": reference
        })
    };

    // Unknown, or lacking a required key: refused.
    for (reference, message) in [
        ("nope", "unknown reference bundle nope"),
        ("gtf-only", "reference bundle gtf-only has no fasta"),
    ] {
        let res = app
            .clone()
            .oneshot(post("/runs", Some("operators"), &submit(reference)))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        assert_eq!(text_body(res).await, message);
    }

    let res =
        app.clone().oneshot(post("/runs", Some("operators"), &submit("grch38"))).await.unwrap();
    assert_eq!(res.status(), StatusCode::ACCEPTED);
    let biffix_core::event::RunSubmitted { params, input_uris, reference_bundle, .. } =
        &recorded_anchor(&events);
    // The bundle's fasta replaced the given one; it has no gtf, so gtf stays unset.
    assert_eq!(params["fasta"], "s3://test-bucket/refs/hg38.fa");
    assert!(params.get("gtf").is_none());
    // Only the bundle's fasta is checked: not the one it replaced.
    assert_eq!(input_uris, &["s3://test-bucket/refs/hg38.fa"]);
    let used = reference_bundle.as_ref().unwrap();
    assert_eq!(used.id, "grch38");
    assert_eq!(
        used.uris.clone().into_iter().collect::<Vec<_>>(),
        [("fasta".to_string(), "s3://test-bucket/refs/hg38.fa".to_string())]
    );
}

#[tokio::test]
async fn a_bundle_for_a_version_without_a_reference_dropdown_is_refused() {
    let (state, _, _) = state();
    let app = api::router(state);
    setup(&app, "/workflows", "developers", &register_body()).await;
    let body = json!({
        "workflow_version_id": "abc:hash:0",
        "params": {"reads": "s3://b/in.fastq", "outdir": "s3://b/out/"},
        "reference": "grch38"
    });
    let res = app.oneshot(post("/runs", Some("operators"), &body)).await.unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    // Refused for the version, not for the (never created) bundle it names.
    assert_eq!(text_body(res).await, "this workflow version takes no reference bundle");
}

#[tokio::test]
async fn an_index_a_bundle_names_by_its_base_path_is_accepted_where_the_bundle_fills_it() {
    let (state, events, cloud) = state();
    let app = api::router(state);
    // `genome` is neither an object nor a folder: only `genome.amb`, `genome.bwt`, … exist.
    let base = "s3://test-bucket/refs/bwa/genome";
    cloud.absent.lock().unwrap().extend([base.to_string(), format!("{base}/")]);
    let res = app
        .clone()
        .oneshot(post("/references", Some("admins"), &bundle_body("bwa", json!({"fasta": base}))))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CREATED);
    // The bundle's check reads it as the submit does: the object, the folder, then the index's
    // files (`genome.`), never a bare prefix that `genome2/` would satisfy.
    assert_eq!(json_body(res).await["warnings"], json!([]));
    assert_eq!(
        *cloud.object_checks.lock().unwrap(),
        [base.to_string(), format!("{base}/"), format!("{base}.")]
    );
    setup(&app, "/workflows", "developers", &reference_register_body()).await;

    // Filled from the bundle, the base path is accepted.
    let res = app
        .clone()
        .oneshot(post(
            "/runs",
            Some("operators"),
            &json!({"workflow_version_id": "refs:hash:0", "params": {}, "reference": "bwa"}),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::ACCEPTED);
    let anchor = recorded_anchor(&events);
    assert_eq!(anchor.input_uris, [base]);
    assert_eq!(anchor.input_provenance.unwrap().get(base).map(String::as_str), Some(""));

    // Given as a param by hand, it is a file that does not exist.
    let res = app
        .clone()
        .oneshot(post(
            "/runs",
            Some("operators"),
            &json!({"workflow_version_id": "refs:hash:0", "params": {"fasta": base}}),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    assert_eq!(text_body(res).await, format!("input(s) missing in S3: {base}"));

    // A base path with no index files beside it is a warning at save, and missing at submit.
    cloud.absent.lock().unwrap().insert(format!("{base}."));
    let res = app
        .clone()
        .oneshot(post("/references", Some("admins"), &bundle_body("bwa2", json!({"fasta": base}))))
        .await
        .unwrap();
    assert_eq!(
        json_body(res).await["warnings"],
        json!([format!("fasta: nothing found at {base}")])
    );
    let res = app
        .clone()
        .oneshot(post(
            "/runs",
            Some("operators"),
            &json!({"workflow_version_id": "refs:hash:0", "params": {}, "reference": "bwa2"}),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}
