//! Registering a workflow version: roles, engine ranges, S3-staged sources, default images and the about page.

use super::*;

#[tokio::test]
async fn register_requires_developer() {
    let (state, _, _) = state();
    let res = api::router(state)
        .oneshot(post("/workflows", Some("operators"), &register_body()))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn register_rejects_an_engine_version_outside_the_supported_range() {
    let (state, _, _) = state();
    let app = api::router(state);
    for (engine, version, message) in [
        ("snakemake", "9.3", "is not an exact release"),
        ("snakemake", "8.30.0", "snakemake 8.30.0 is not supported"),
        ("nextflow", "24.10.5", "nextflow 24.10.5 is not supported"),
        ("nextflow", "25.04.0", "Nextflow 25.04.0 is no longer supported; re-register on 25.10.4"),
    ] {
        let mut body = register_body();
        body["engine"] = json!(engine);
        body["engine_version"] = json!(version);
        let res = app.clone().oneshot(post("/workflows", Some("developers"), &body)).await.unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST, "{engine} {version}");
        let text = text_body(res).await;
        assert!(text.contains(message), "{engine} {version}: {text}");
    }
}

#[tokio::test]
async fn registering_a_source_again_is_a_new_build_unless_the_inputs_are_identical() {
    let (state, _, _) = state();
    let app = api::router(state);
    let with_inputs = |image: &str| {
        let mut body = register_body();
        body["image"] = json!(image);
        body["registration_inputs"] = json!({
            "source_mode": "git",
            "cli_version": "0.0.0",
            "overrides": {"image": image, "queue_classes": ["cpu-spot"]}
        });
        body
    };
    let register = |body: Value| {
        let app = app.clone();
        async move {
            let res = app.oneshot(post("/workflows", Some("developers"), &body)).await.unwrap();
            let status = res.status();
            (status, text_body(res).await)
        }
    };
    let first = format!("sha256:{}", "a".repeat(64));
    let second = format!("sha256:{}", "b".repeat(64));

    let (status, body) = register(with_inputs(&first)).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let id = |body: &str| serde_json::from_str::<Value>(body).unwrap()["id"].clone();
    assert_eq!(id(&body), "abc:hash:0", "{body}");

    // The same source and inputs again: nothing new to freeze.
    let (status, body) = register(with_inputs(&first)).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(body.starts_with("identical to build 0 (abc:hash:0)"), "{body}");

    // A different image: the next build of the same source.
    let (status, body) = register(with_inputs(&second)).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(id(&body), "abc:hash:1", "{body}");

    // The same commit from where its repository has moved: a build whose coordinator fetches
    // from there, not "identical" to one whose location no longer resolves.
    let mut moved = with_inputs(&second);
    moved["source"]["repo"] = json!("github.com/example-moved/genomics");
    let (status, body) = register(moved).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert!(body.contains(r#""id":"abc:hash:2""#), "{body}");

    // The detail records the inputs and the build.
    let res = app.clone().oneshot(get("/workflows/abc%3Ahash%3A1", "viewers")).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let detail = json_body(res).await;
    assert_eq!(detail["build"], json!(1));
    assert_eq!(detail["registration_inputs"]["overrides"]["image"], json!(second));
}

#[tokio::test]
async fn engines_lists_each_engine_s_supported_range() {
    let (state, _, _) = state();
    let res = api::router(state).oneshot(get("/engines", "viewers")).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = json_body(res).await;
    assert_eq!(
        v,
        json!([
            {"engine": "snakemake", "min_version": "9.0.0", "below_version": "10.0.0"},
            {
                "engine": "nextflow",
                "min_version": "25.10.4",
                "below_version": "26.0.0",
                "releases": ["25.10.4"]
            }
        ])
    );
}

/// A register body whose `source` is S3-staged at the given uri (everything else valid).
fn register_body_s3(uri: &str) -> Value {
    let mut body = register_body();
    body["source"] = json!({"kind": "s3", "uri": uri, "sha256": "a".repeat(64)});
    body
}

#[tokio::test]
async fn source_upload_requires_developer() {
    let (state, _, _) = state();
    let body = json!({"version_id": "abc:hash", "project": "demo"});
    let res = api::router(state)
        .oneshot(post("/workflows/source-upload", Some("operators"), &body))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn source_upload_returns_a_presigned_url_for_the_staging_key() {
    let (state, _, _) = state();
    let body = json!({"version_id": "abc:hash", "project": "demo"});
    let res = api::router(state)
        .oneshot(post("/workflows/source-upload", Some("developers"), &body))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let out = json_body(res).await;
    // The uri lands under the sanctioned staging prefix, keyed by the version id.
    let uri = out["uri"].as_str().unwrap();
    assert!(uri.ends_with("workflow-sources/abc:hash/source.tar.gz"), "uri was {uri}");
    assert!(out["url"].as_str().unwrap().starts_with("https://"));
}

#[tokio::test]
async fn source_upload_rejects_a_traversing_version_id() {
    // A traversal, a lone `.`, a backslash, whitespace inside, a control character.
    for id in ["../../etc/passwd", ".", "a\\b", "abc hash", "abc\u{7}:hash"] {
        let (state, _, _) = state();
        let body = json!({"version_id": id, "project": "demo"});
        let res = api::router(state)
            .oneshot(post("/workflows/source-upload", Some("developers"), &body))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST, "{id:?}");
        assert_eq!(text_body(res).await, format!("invalid version_id: {id:?}"));
    }
}

#[tokio::test]
async fn register_accepts_an_in_bucket_s3_source() {
    let (state, _, _) = state();
    let uri = "s3://test-bucket/workflow-sources/abc:hash/source.tar.gz";
    let res = api::router(state)
        .oneshot(post("/workflows", Some("developers"), &register_body_s3(uri)))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CREATED);
}

#[tokio::test]
async fn register_rejects_an_s3_source_in_another_bucket() {
    // The coordinator fetches and executes this uri, so a foreign bucket is a code-execution
    // surface — it must be rejected at register, not fetched at run time.
    let (state, _, _) = state();
    let uri = "s3://attacker-bucket/workflow-sources/abc:hash/source.tar.gz";
    let res = api::router(state)
        .oneshot(post("/workflows", Some("developers"), &register_body_s3(uri)))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let text = text_body(res).await;
    assert!(
        text.contains("s3 source must be in the project bucket (test-bucket), not attacker-bucket"),
        "{text}"
    );
}

#[tokio::test]
async fn register_rejects_an_s3_source_outside_the_staging_prefix() {
    let (state, _, _) = state();
    let uri = "s3://test-bucket/runs/somebody-elses-data/source.tar.gz";
    let res = api::router(state)
        .oneshot(post("/workflows", Some("developers"), &register_body_s3(uri)))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let text = text_body(res).await;
    assert!(text.contains("s3 source key must be under the workflow-sources/ prefix"), "{text}");
}

#[tokio::test]
async fn a_bare_image_name_is_read_as_docker_reads_it() {
    // `--image name` is a Docker Hub image, as `docker pull name` would read it; it no longer
    // names a tag in the deploy's own repository.
    let (state, _, _) = state();
    let registry = state.registry.clone();
    let mut body = register_body();
    body["image"] = json!("generic-worker-0123abc");
    let res =
        api::router(state).oneshot(post("/workflows", Some("developers"), &body)).await.unwrap();
    assert!(res.status().is_success(), "{}", res.status());
    let id = json_body(res).await["id"].as_str().unwrap().to_string();
    let rec = registry.get_version(&id).await.unwrap();
    assert!(
        rec.images[0]
            .source_ref
            .starts_with("docker.io/library/generic-worker-0123abc:latest@sha256:"),
        "{}",
        rec.images[0].source_ref
    );
}

#[tokio::test]
async fn registering_records_the_default_image_pinned_for_import() {
    // A bare digest lives in the deploy's worker repo. The version records it pinned, keyed for
    // its import (which keep-tags it).
    let (app_state, _, _) = state();
    let registry = app_state.registry.clone();
    let res = api::router(app_state)
        .oneshot(post("/workflows", Some("developers"), &register_body()))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CREATED);
    let id = json_body(res).await["id"].as_str().unwrap().to_string();
    let digest = format!("sha256:{}", "a".repeat(64));
    let rec = registry.get_version(&id).await.unwrap();
    assert_eq!(rec.images.len(), 1);
    let image = &rec.images[0];
    assert_eq!(image.name, "default");
    assert_eq!(
        image.source_ref,
        format!("111122223333.dkr.ecr.us-east-1.amazonaws.com/biffix-workers@{digest}")
    );
    assert_eq!(image.content_key, biffix_workflow::images::content_key(&digest));
    assert_eq!(rec.version.image_digest, digest);
}

#[tokio::test]
async fn register_without_an_image_succeeds_with_an_empty_digest() {
    // A workflow whose every task declares its own container needs no default image. Register must
    // accept a missing `image` and freeze a version with an empty digest (no default worker image).
    let (state, _, _) = state();
    let mut body = register_body();
    body.as_object_mut().unwrap().remove("image");
    let res =
        api::router(state).oneshot(post("/workflows", Some("developers"), &body)).await.unwrap();
    assert_eq!(res.status(), StatusCode::CREATED);
    let view = json_body(res).await;
    assert_eq!(
        view["image_digest"].as_str(),
        Some(""),
        "a no-image version freezes an empty digest"
    );
}

#[tokio::test]
async fn a_versions_about_page_is_kept_and_capped() {
    let (state, _, _) = state();
    let app = api::router(state);
    let mut body = register_body();
    body["about_markdown"] = json!("# Bench\n\nAligns reads.");
    let res = app.clone().oneshot(post("/workflows", Some("developers"), &body)).await.unwrap();
    assert_eq!(res.status(), StatusCode::CREATED);
    let detail =
        json_body(app.clone().oneshot(get("/workflows/abc:hash:0", "viewers")).await.unwrap())
            .await;
    assert_eq!(detail["about_markdown"], "# Bench\n\nAligns reads.");
    // The list carries only a one-line summary, taken from the About page's first sentence.
    let list = json_body(app.clone().oneshot(get("/workflows", "viewers")).await.unwrap()).await;
    assert_eq!(list[0]["summary"], "Aligns reads.");
    assert!(list[0].get("about_markdown").is_none());

    // Over the cap: refused rather than stored.
    let mut body = register_body();
    body["git_sha"] = json!("def");
    body["about_markdown"] = json!("x".repeat(biffix_api::MAX_ABOUT_BYTES + 1));
    let res = app.oneshot(post("/workflows", Some("developers"), &body)).await.unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    assert_eq!(text_body(res).await, "about_markdown is larger than 256 KiB");
}

#[tokio::test]
async fn a_direct_registration_s_reports_are_normalized_like_the_cli_s() {
    let (state, _, _) = state();
    let registry = state.registry.clone();
    let app = api::router(state);
    // A per-sample report whose path has no `{sample}`, an unknown kind and an escaping path.
    let body = catalog_body(
        "rnaseq",
        "abc",
        json!({"reports": [
            {"name": "QC", "path": "qc/all.html", "per_sample": true},
            {"name": "Counts", "path": "counts.tsv", "kind": "spreadsheet"},
            {"name": "Elsewhere", "path": "../x.html"},
            {"name": "Alignments", "path": "star/{sample}.bam", "per_sample": true}
        ]}),
        None,
    );
    let res = app.oneshot(post("/workflows", Some("developers"), &body)).await.unwrap();
    assert_eq!(res.status(), StatusCode::CREATED);
    let id = json_body(res).await["id"].as_str().unwrap().to_string();
    let form: biffix_workflow::formschema::FormSchema =
        serde_json::from_value(registry.get_version(&id).await.unwrap().version.form_schema)
            .unwrap();
    let got: Vec<(&str, bool, Option<&str>)> =
        form.reports.iter().map(|r| (r.name.as_str(), r.per_sample, r.kind.as_deref())).collect();
    assert_eq!(got, [("QC", false, None), ("Counts", false, None), ("Alignments", true, None)]);
    // Exactly these three warnings, one each (none leaked for the valid `Alignments`).
    assert_eq!(form.warnings.len(), 3, "{:?}", form.warnings);
    for why in ["per_sample but its path has no", "spreadsheet", "relative to the run"] {
        assert_eq!(
            form.warnings.iter().filter(|w| w.contains(why)).count(),
            1,
            "{why}: {:?}",
            form.warnings
        );
    }
}

#[tokio::test]
async fn a_direct_registration_s_catalog_is_normalized_like_the_cli_s() {
    let (state, _, _) = state();
    let app = api::router(state);
    // 20 tags, with exact and case-only duplicates, empties and one over-long tag, and a
    // multi-KB description: what a client bypassing the CLI could send.
    let mut tags: Vec<String> = (0..16).map(|i| format!("t{i}")).collect();
    tags.extend(["t0".into(), "T1".into(), " ".into(), "x".repeat(100)]);
    let body = catalog_body(
        "rnaseq",
        "abc",
        json!({"catalog": {"description": "d".repeat(5000), "tags": tags, "verified": true}}),
        None,
    );
    let res = app.clone().oneshot(post("/workflows", Some("developers"), &body)).await.unwrap();
    assert_eq!(res.status(), StatusCode::CREATED, "{}", text_body(res).await);
    let list = json_body(app.oneshot(get("/workflows", "viewers")).await.unwrap()).await;
    let catalog = &list[0]["catalog"];
    let tags: Vec<&str> =
        catalog["tags"].as_array().unwrap().iter().map(|t| t.as_str().unwrap()).collect();
    assert_eq!(
        tags,
        (0..biffix_workflow::formschema::MAX_CATALOG_TAGS)
            .map(|i| format!("t{i}"))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        catalog["description"].as_str().unwrap().chars().count(),
        biffix_workflow::formschema::MAX_CATALOG_DESCRIPTION_CHARS
    );
    assert_eq!(catalog["verified"], true);
}
