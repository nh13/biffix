//! Registration's checks, each one a test that fails without it; dry runs and the builds repository.

use super::*;

#[tokio::test]
async fn a_built_image_s_name_runtime_and_context_are_checked() {
    let h = harness();
    let with = |f: &dyn Fn(&mut Value)| {
        let mut body = pixi_body("abc");
        f(&mut body["images"]["default"]);
        body
    };
    let other_tree = "c3".repeat(32);
    let cases: Vec<(Value, &str)> = vec![
        // Staged, but not the tarball of the tree it claims.
        (
            with(&|d| {
                d["build"]["context_uri"] = json!(format!(
                    "s3://test-bucket/workflow-sources/abc:hash/contexts/{other_tree}.tar.gz"
                ));
            }),
            "must be staged in the project bucket",
        ),
        (
            with(&|d| {
                d["build"]["context_uri"] =
                    json!(format!("s3://test-bucket/workflow-sources/../x/contexts/{TREE}.tar.gz"));
            }),
            "must be staged in the project bucket",
        ),
        (with(&|d| d["build"]["context_sha256"] = json!("short")), "sha256 hex"),
        (with(&|d| d["build"]["tree_hash"] = json!(TREE.to_uppercase())), "sha256 hex"),
        (
            with(&|d| {
                d["build"]["kind"] = json!("dockerfile");
            }),
            "names its Dockerfile",
        ),
    ];
    for (body, want) in cases {
        let (status, msg) = register(&h, &body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{want}: {msg}");
        assert!(msg.as_str().unwrap().contains(want), "{want}: {msg}");
    }

    // The name is an images: key, so it is checked on a body of its own.
    let mut body = pixi_body("abc");
    let build = body["images"]["default"].take();
    body["images"] = json!({ "Bad": build });
    let (status, msg) = register(&h, &body).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{msg}");
    assert!(msg.as_str().unwrap().contains("image name `Bad` is invalid"), "{msg}");

    // On Snakemake, where `runtime: own` is allowed, any other `runtime` is refused too.
    let mut body = snakemake_body("abc", SAMTOOLS, false);
    body["images"]["align"] = json!({"runtime": "foo", "build": {
        "kind": "dockerfile",
        "context": "docker/align",
        "dockerfile": "Dockerfile",
        "context_uri": staged_context(),
        "context_sha256": TARBALL_SHA,
        "tree_hash": TREE,
        "archs": ["amd64"],
    }});
    let (status, msg) = register(&h, &body).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{msg}");
    assert!(msg.as_str().unwrap().contains("`runtime` may only be `own`"), "{msg}");
    assert!(h.cloud.image_builds.lock().unwrap().is_empty());
}

#[tokio::test]
async fn an_s3_source_must_be_staged_on_this_deploy() {
    let staged = |uri: &str| {
        let mut body = register_body("abc");
        body["source"] = json!({"kind": "s3", "uri": uri, "sha256": "a".repeat(64)});
        body
    };
    let h = harness();
    let (status, view) =
        register(&h, &staged("s3://test-bucket/workflow-sources/abc:hash/source.tar.gz")).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{view}");
    for (uri, want) in [
        ("s3://test-bucket/workflow-sources/../elsewhere/source.tar.gz", "with no `..`"),
        ("s3://test-bucket/elsewhere/source.tar.gz", "prefix"),
        ("s3://other/workflow-sources/abc:hash/source.tar.gz", "project bucket"),
    ] {
        let (status, msg) = register(&h, &staged(uri)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{uri}: {msg}");
        assert!(msg.as_str().unwrap().contains(want), "{uri}: {msg}");
    }

    // A deploy with no project bucket stages nothing, so it has no s3 source.
    let mut h = harness();
    h.state.projects = testing::bucket_project("");
    let (status, msg) =
        register(&h, &staged("s3:///workflow-sources/abc:hash/source.tar.gz")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{msg}");
    assert!(msg.as_str().unwrap().contains("not configured"), "{msg}");
}

async fn source_upload(h: &Harness, body: Value) -> (StatusCode, Value) {
    call(&h.state, "POST", "/workflows/source-upload", "developers", Some(&body)).await
}

#[tokio::test]
async fn a_staged_source_is_write_once_and_a_context_is_not() {
    let h = harness();
    // A version runs whatever its source's key holds, so the PUT may only create it.
    let (status, body) =
        source_upload(&h, json!({"version_id": "abc:hash", "project": "demo"})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["headers"], json!({"if-none-match": "*"}), "{body}");
    assert!(body["uri"].as_str().unwrap().ends_with("/workflow-sources/abc:hash/source.tar.gz"));
    // A context's build checks its sha256, so it may be staged again.
    let (status, body) = source_upload(
        &h,
        json!({"version_id": "abc:hash", "context_tree_hash": TREE, "project": "demo"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.get("headers").is_none(), "{body}");

    for version_id in ["a/b", "a..b", " "] {
        let (status, msg) =
            source_upload(&h, json!({ "version_id": version_id, "project": "demo" })).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{version_id:?}: {msg}");
        assert!(msg.as_str().unwrap().contains("invalid version_id"), "{version_id:?}: {msg}");
    }
}

#[tokio::test]
async fn a_private_ecr_tag_that_does_not_exist_is_the_caller_s_error() {
    let h = harness();
    let private = "123456789012.dkr.ecr.us-east-1.amazonaws.com/tools:nope";
    h.cloud.missing_images.lock().unwrap().insert(private.to_string());
    let mut body = register_body("abc");
    body["images"] = json!({"tools": {"ref": private}});
    let (status, msg) = register(&h, &body).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{msg}");
    assert!(
        msg.as_str().unwrap().contains(&format!("{private}: not found in its registry")),
        "{msg}"
    );
    assert_nothing_registered(&h);
}

#[tokio::test]
async fn a_built_default_has_no_image_digest_even_with_an_at_in_its_path() {
    let h = harness();
    let mut body = pixi_body("abc");
    body["images"]["default"]["build"]["context"] = json!("env@2");
    let (status, view) = register(&h, &body).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{view}");
    let id = view["id"].as_str().unwrap().to_string();
    let rec = h.registry.versions.lock().unwrap()[&id].clone();
    assert_eq!(rec.images[0].source_ref, "pixi:env@2");
    assert_eq!(rec.version.image_digest, "");
    assert_eq!(rec.version.image_repository, None);
}

#[tokio::test]
async fn only_a_version_with_a_default_image_links_to_the_workflow_repository() {
    let h = harness();
    let (_, view) = register(&h, &register_body("abc")).await;
    let with_default = view["id"].as_str().unwrap().to_string();
    let mut body = register_body("def");
    body.as_object_mut().unwrap().remove("image");
    let (status, view) = register(&h, &body).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{view}");
    let without = view["id"].as_str().unwrap().to_string();
    assert!(view["image_repository"].is_null(), "{view}");
    assert_eq!(detail(&h, &with_default).await["image_repository"], "biffix-workers");
    assert!(detail(&h, &without).await["image_repository"].is_null());
    let (status, list) = call(&h.state, "GET", "/workflows", "operators", None).await;
    assert_eq!(status, StatusCode::OK, "{list}");
    let repo = |id: &str| {
        list.as_array().unwrap().iter().find(|v| v["id"] == id).unwrap()["image_repository"].clone()
    };
    assert_eq!(repo(&with_default), "biffix-workers");
    assert!(repo(&without).is_null());
}

#[tokio::test]
async fn a_dry_run_says_what_changed_and_what_is_built_already() {
    let h = harness();
    let (_, view) = register(&h, &register_body("abc")).await;
    let id = view["id"].as_str().unwrap().to_string();
    // As if FASTQC's tag named another image when the version was registered.
    let stale = "quay.io/biocontainers/fastqc@sha256:0000000000000000000000000000000000000000000000000000000000000000";
    {
        let mut versions = h.registry.versions.lock().unwrap();
        let rec = versions.get_mut(&id).unwrap();
        rec.images.iter_mut().find(|i| i.name == "fastqc").unwrap().source_ref = stale.into();
    }
    h.registry.variants.lock().unwrap().clear();
    let image = |plan: &Value, name: &str| {
        plan["images"].as_array().unwrap().iter().find(|i| i["name"] == name).unwrap().clone()
    };

    let before = dry_run(&h, &id).await;
    assert_eq!(before["runtime_cached"], false, "{before}");
    assert_eq!(image(&before, "fastqc")["previous_source_ref"], stale);
    assert!(image(&before, "default").get("previous_source_ref").is_none());
    assert_eq!(image(&before, "fastqc")["cached"], false);
    assert_eq!(image(&before, "default")["cached"], false);

    // Its runtime is built and FASTQC imported: both reused.
    let rec = h.registry.versions.lock().unwrap()[&id].clone();
    let fastqc = rec.images.iter().find(|i| i.name == "fastqc").unwrap();
    // (Its content key is its digest's, which pinning the tag again gives again.)
    let imported = testing::fake_artifact(fastqc);
    h.registry.artifacts.lock().unwrap().insert(imported.content_key.clone(), imported);
    h.registry.variants.lock().unwrap().push(testing::fake_variant(
        before["runtime_key"].as_str().unwrap(),
        testing::FAKE_BASE_DIGEST,
    ));
    let after = dry_run(&h, &id).await;
    assert_eq!(after["runtime_cached"], true, "{after}");
    assert_eq!(image(&after, "fastqc")["cached"], true, "{after}");
    assert_eq!(image(&after, "default")["cached"], false, "{after}");
}

#[tokio::test]
async fn a_snakemake_image_is_cached_only_once_it_is_derived_with_the_runtime() {
    let h = harness();
    let (_, view) = register(&h, &snakemake_body("abc", SAMTOOLS, false)).await;
    let id = view["id"].as_str().unwrap().to_string();
    let rec = h.registry.versions.lock().unwrap()[&id].clone();
    let default = rec.images[0].clone();
    // Imported, but not derived with the runtime: there is still a build to do.
    let imported = testing::fake_artifact(&default);
    h.registry.artifacts.lock().unwrap().insert(imported.content_key.clone(), imported);
    assert_eq!(dry_run(&h, &id).await["images"][0]["cached"], false);
    let derived = testing::fake_derived_artifact(&default, &runtime_key(&h, &id));
    h.registry.artifacts.lock().unwrap().insert(derived.content_key.clone(), derived);
    assert_eq!(dry_run(&h, &id).await["images"][0]["cached"], true);

    // An image with its own Snakemake is never derived: imported is cached.
    let h = harness();
    let (_, view) = register(&h, &snakemake_body("own", SAMTOOLS, true)).await;
    let id = view["id"].as_str().unwrap().to_string();
    let default = h.registry.versions.lock().unwrap()[&id].images[0].clone();
    let imported = testing::fake_artifact(&default);
    h.registry.artifacts.lock().unwrap().insert(imported.content_key.clone(), imported);
    let plan = dry_run(&h, &id).await;
    assert_eq!(plan["images"][0]["cached"], true, "{plan}");
}

/// Whether a dry-run re-registration of `id` would reuse its (first) image.
async fn plan_cached(h: &Harness, id: &str) -> Value {
    let (status, plan) = reregister(h, id, json!({"dry_run": true})).await;
    assert_eq!(status, StatusCode::OK, "{plan}");
    plan["images"][0]["cached"].clone()
}

/// The deploy's builds repository in these tests (where the image-build projects push).
const BUILDS_REPO: &str = "111122223333.dkr.ecr.us-east-1.amazonaws.com/biffix-builds";

#[tokio::test]
async fn a_build_recorded_outside_the_builds_repository_is_built_again_once_into_it() {
    use biffix_workflow::images::{build_subject, Arch};
    let mut h = harness();
    h.state.deploy.builds_image_repo = BUILDS_REPO.into();
    let (status, view) = register(&h, &pixi_body("abc")).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{view}");
    let id = view["id"].as_str().unwrap().to_string();
    let key = content_key(&h, &id, "default");
    let (amd, arm) =
        (build_subject(&key, Some(Arch::Amd64)), build_subject(&key, Some(Arch::Arm64)));
    assert_eq!(h.cloud.image_builds.lock().unwrap().len(), 2);

    // Built, indexed and recorded in the workflow repository: a build there may have reused a
    // build-* tag third-party workflow CI pushed first.
    let old = |d: &str| format!("{}@sha256:{d}", testing::FAKE_WORKFLOW_REPO);
    set_build(&h, &amd, BuildState::Succeeded, 1, &[("IMAGE", &old("amd"))]);
    set_build(&h, &arm, BuildState::Succeeded, 1, &[("IMAGE", &old("arm"))]);
    let old_index = imported("bash=1,libc=glibc");
    set_build(&h, &build_subject(&key, None), BuildState::Succeeded, 1, &outputs(&old_index));
    let recorded = biffix_backend::store::registry::ImageArtifact {
        content_key: key.clone(),
        source_ref: "pixi:env".into(),
        image: old("imp"),
        digest: "sha256:imp".into(),
        archs: vec!["amd64".into()],
        // A result that would otherwise run: only where it lives keeps it from being used.
        capabilities: biffix_workflow::images::Capabilities::parse("bash=1,libc=glibc"),
        keep_tag: Some("biffix-keep-imp".into()),
        archived: true,
        archive_bytes: None,
        created_at: 0,
    };
    h.registry.artifacts.lock().unwrap().insert(key.clone(), recorded);
    // Not trusted: the version is not ready (its image is building again), and cannot run.
    let d = detail(&h, &id).await;
    assert_eq!(
        (d["state"].as_str(), d["images"][0]["state"].as_str()),
        (Some("pending"), Some("building")),
        "{d}"
    );
    assert_eq!(plan_cached(&h, &id).await, false);
    let submit = json!({"workflow_version_id": id, "params": {}});

    // The sweep builds both architectures again (new starts, not the first ones repeated).
    sweep_ok(&h).await;
    let builds = h.cloud.image_builds.lock().unwrap().clone();
    assert_eq!(builds.len(), 4, "{builds:?}");
    assert_eq!(
        [builds[2].subject.as_str(), builds[3].subject.as_str()],
        [amd.as_str(), arm.as_str()]
    );
    assert!(h.cloud.image_indexes.lock().unwrap().is_empty());

    // Pushed into the builds repository: joined from there, and the new index recorded in place
    // of the old result.
    let new = |d: &str| format!("{BUILDS_REPO}@sha256:{d}");
    set_build(&h, &amd, BuildState::Succeeded, 2, &[("IMAGE", &new("amd"))]);
    set_build(&h, &arm, BuildState::Succeeded, 2, &[("IMAGE", &new("arm"))]);
    sweep_ok(&h).await;
    let indexes = h.cloud.image_indexes.lock().unwrap().clone();
    assert_eq!(indexes.len(), 1);
    assert_eq!(indexes[0].manifests, [new("amd"), new("arm")]);
    let rebuilt = vec![
        ("IMAGE", new("built")),
        ("IMAGE_DIGEST", "sha256:built".into()),
        ("ARCHS", "amd64,arm64".into()),
        ("CAPABILITIES", "bash=1,libc=glibc".into()),
        ("KEEP_TAG", "biffix-keep-built".into()),
        ("ARCHIVED", "1".into()),
    ];
    set_build(&h, &indexes[0].subject, BuildState::Succeeded, 2, &outputs(&rebuilt));
    sweep_ok(&h).await;
    assert_eq!(h.registry.artifacts.lock().unwrap()[&key].image, new("built"));
    assert_eq!(detail(&h, &id).await["state"], "ready");
    let (status, body) = call(&h.state, "POST", "/runs", "operators", Some(&submit)).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let launched = h.cloud.launched.lock().unwrap().clone();
    assert_eq!(launched[0].image_digest, "sha256:built");
    assert_eq!(plan_cached(&h, &id).await, true);

    // Once: nothing more is built.
    sweep_ok(&h).await;
    assert_eq!(h.cloud.image_builds.lock().unwrap().len(), 4);
    assert_eq!(h.cloud.image_indexes.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn a_build_in_the_builds_repository_is_recorded_as_it_is() {
    use biffix_workflow::images::{build_subject, Arch};
    let mut h = harness();
    h.state.deploy.builds_image_repo = BUILDS_REPO.into();
    let (_, view) = register(&h, &pixi_body("abc")).await;
    let id = view["id"].as_str().unwrap().to_string();
    let key = content_key(&h, &id, "default");
    let new = |d: &str| format!("{BUILDS_REPO}@sha256:{d}");
    for (arch, d) in [(Arch::Amd64, "amd"), (Arch::Arm64, "arm")] {
        set_build(
            &h,
            &build_subject(&key, Some(arch)),
            BuildState::Succeeded,
            1,
            &[("IMAGE", &new(d))],
        );
    }
    sweep_ok(&h).await;
    let indexes = h.cloud.image_indexes.lock().unwrap().clone();
    assert_eq!(indexes[0].manifests, [new("amd"), new("arm")]);
    let done = vec![
        ("IMAGE", new("built")),
        ("IMAGE_DIGEST", "sha256:built".into()),
        ("ARCHS", "amd64,arm64".into()),
        ("CAPABILITIES", "bash=1,libc=glibc".into()),
        ("KEEP_TAG", "biffix-keep-built".into()),
        ("ARCHIVED", "1".into()),
    ];
    set_build(&h, &indexes[0].subject, BuildState::Succeeded, 1, &outputs(&done));
    sweep_ok(&h).await;
    assert_eq!(detail(&h, &id).await["state"], "ready");
    assert_eq!(h.cloud.image_builds.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn a_registration_declaring_too_many_images_is_refused_before_any_lookup() {
    let h = harness();
    let mut body = register_body("abc");
    // The default plus 256 named: one over the cap.
    body["images"] = (0..256)
        .map(|i| (format!("img{i}"), json!({"ref": format!("quay.io/b/x{i}:1")})))
        .collect::<serde_json::Map<_, _>>()
        .into();
    let (status, v) = register(&h, &body).await;
    assert_eq!(
        (status, v),
        (StatusCode::BAD_REQUEST, json!("a workflow version declares 257 images; at most 256"))
    );
    assert_nothing_registered(&h);
    // One fewer is accepted.
    body["images"].as_object_mut().unwrap().remove("img0");
    let (status, v) = register(&h, &body).await;
    assert!(status.is_success(), "{status}: {v}");
}

#[tokio::test]
async fn a_bare_digest_needs_the_deploy_s_worker_repository_and_a_pixi_build_no_dockerfile() {
    // A bare `sha256:` digest names an image in the worker-image repository: none configured.
    let mut h = harness();
    h.state.deploy.worker_image_repo = String::new();
    let mut body = register_body("abc");
    body["image"] = json!(format!("sha256:{}", "a".repeat(64)));
    let (status, v) = register(&h, &body).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{v}");
    assert_eq!(
        v,
        json!(
            "a bare sha256 digest names an image in the deploy's worker-image repository, which \
             this deploy does not configure; give the image's full reference"
        )
    );
    assert_nothing_registered(&h);

    // A pixi build names no Dockerfile.
    let h = harness();
    let mut body = pixi_body("abc");
    body["images"]["default"]["build"]["dockerfile"] = json!("Dockerfile");
    let (status, v) = register(&h, &body).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{v}");
    assert!(v.as_str().unwrap().contains("a pixi build has no Dockerfile"), "{v}");
    assert_nothing_registered(&h);
}
