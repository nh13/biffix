//! Images built from a recipe the workflow ships: per-architecture builds, contexts and failures.

use super::*;

#[tokio::test]
async fn a_pixi_default_is_built_per_architecture_indexed_and_run() {
    use biffix_workflow::images::{build_content_key, build_subject, Arch, BuildKind};
    let h = harness();
    let (status, view) = register(&h, &pixi_body("abc")).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{view}");
    let id = view["id"].as_str().unwrap().to_string();

    // Keyed by what it is built from, with this server's generator; both architectures started.
    let key = content_key(&h, &id, "default");
    assert_eq!(
        key,
        build_content_key(BuildKind::Pixi, TARBALL_SHA, None, &[Arch::Amd64, Arch::Arm64])
    );
    let rec = h.registry.versions.lock().unwrap()[&id].clone();
    assert_eq!(rec.version.image_digest, "");
    assert_eq!(rec.images[0].source_ref, "pixi:env");
    let builds = h.cloud.image_builds.lock().unwrap().clone();
    let subjects: Vec<&str> = builds.iter().map(|b| b.subject.as_str()).collect();
    assert_eq!(
        subjects,
        [build_subject(&key, Some(Arch::Amd64)), build_subject(&key, Some(Arch::Arm64))]
    );
    for b in &builds {
        assert_eq!(b.context_uri, staged_context());
        assert_eq!(b.context_sha256, TARBALL_SHA);
        assert_eq!(b.dockerfile, None);
        assert_eq!(
            b.dockerfile_text.as_deref(),
            Some(biffix_workflow::pixi::dockerfile().as_str())
        );
    }
    let d = detail(&h, &id).await;
    assert_eq!(d["images"][0]["kind"], "pixi");
    assert_eq!(d["images"][0]["generated_dockerfile"], biffix_workflow::pixi::dockerfile());
    assert_eq!(d["images"][0]["arch_builds"].as_array().unwrap().len(), 2);

    // One architecture running: no index yet. Both pushed: the index joins them, in arch order.
    let amd = format!("{}@sha256:amd", testing::FAKE_WORKFLOW_REPO);
    let arm = format!("{}@sha256:arm", testing::FAKE_WORKFLOW_REPO);
    set_build(&h, subjects[0], BuildState::Succeeded, 1, &[("IMAGE", &amd)]);
    set_build(&h, subjects[1], BuildState::Running, 1, &[]);
    sweep_ok(&h).await;
    assert!(h.cloud.image_indexes.lock().unwrap().is_empty());
    assert_eq!(h.cloud.image_builds.lock().unwrap().len(), 2);
    set_build(&h, subjects[1], BuildState::Succeeded, 1, &[("IMAGE", &arm)]);
    sweep_ok(&h).await;
    let indexes = h.cloud.image_indexes.lock().unwrap().clone();
    assert_eq!(indexes.len(), 1);
    assert_eq!(indexes[0].subject, build_subject(&key, None));
    assert_eq!(indexes[0].manifests, [amd.clone(), arm.clone()]);

    // The index's result is recorded like an import; the version runs the built image.
    let done = imported("bash=1,libc=glibc");
    set_build(&h, &indexes[0].subject, BuildState::Succeeded, 1, &outputs(&done));
    sweep_ok(&h).await;
    let d = detail(&h, &id).await;
    assert_eq!(d["state"], "ready", "{d}");
    let submit = json!({"workflow_version_id": id, "params": {}});
    let (status, body) = call(&h.state, "POST", "/runs", "operators", Some(&submit)).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let launched = h.cloud.launched.lock().unwrap().clone();
    assert_eq!(launched[0].image_digest, "sha256:imp");
    let map_uri = launched[0].image_map_uri.clone().unwrap();
    let written: BTreeMap<String, String> =
        serde_json::from_slice(&h.cloud.objects.lock().unwrap()[&map_uri]).unwrap();
    let built = format!("{}@sha256:imp", testing::FAKE_WORKFLOW_REPO);
    // A built image is named by its images: name only (no reference spellings).
    assert_eq!(
        written,
        BTreeMap::from([
            ("default".to_string(), built.clone()),
            (built.clone(), built.clone()),
            ("$default".to_string(), built),
        ])
    );
}

#[tokio::test]
async fn a_failed_architecture_fails_the_image_and_a_rebuild_restarts_only_it() {
    use biffix_workflow::images::{build_subject, Arch};
    let h = harness();
    let (_, view) = register(&h, &pixi_body("abc")).await;
    let id = view["id"].as_str().unwrap().to_string();
    let key = content_key(&h, &id, "default");
    let amd = build_subject(&key, Some(Arch::Amd64));
    let arm = build_subject(&key, Some(Arch::Arm64));
    let pushed = format!("{}@sha256:amd", testing::FAKE_WORKFLOW_REPO);
    set_build(&h, &amd, BuildState::Succeeded, 1, &[("IMAGE", &pushed)]);
    // One build short of the limit, the failed architecture (only) is retried; at it, it is not.
    set_build(
        &h,
        &arm,
        BuildState::Failed,
        biffix_backend::images::build_follow::AUTO_BUILDS - 1,
        &[],
    );
    sweep_ok(&h).await;
    let builds = h.cloud.image_builds.lock().unwrap().clone();
    assert_eq!(builds.len(), 3);
    assert_eq!(builds[2].subject, arm);
    set_build(&h, &arm, BuildState::Failed, biffix_backend::images::build_follow::AUTO_BUILDS, &[]);
    sweep_ok(&h).await;
    assert_eq!(h.cloud.image_builds.lock().unwrap().len(), 3);
    let d = detail(&h, &id).await;
    assert_eq!(d["state"], "failed", "{d}");
    assert_eq!(d["images"][0]["logs_url"], format!("https://logs/{arm}"));
    let not_ready = d["not_ready"][0].as_str().unwrap();
    assert!(
        not_ready.starts_with("image `default` failed to build (arm64): FAILED: BUILD"),
        "{not_ready}"
    );
    let states: Vec<&str> = d["images"][0]["arch_builds"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["state"].as_str().unwrap())
        .collect();
    assert_eq!(states, ["succeeded", "failed"]);

    let uri = format!("/workflows/{id}/images/default/rebuild");
    let (status, body) = call(&h.state, "POST", &uri, "developers", None).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let builds = h.cloud.image_builds.lock().unwrap().clone();
    assert_eq!(builds.len(), 4);
    assert_eq!(builds[3].subject, arm);
}

#[tokio::test]
async fn build_requests_are_validated() {
    let h = harness();
    let with = |f: &dyn Fn(&mut Value)| {
        let mut body = pixi_body("abc");
        f(&mut body["images"]["default"]);
        body
    };
    let cases: Vec<(Value, &str)> = vec![
        (
            with(&|d| {
                d["build"]["context_uri"] =
                    json!("s3://other/workflow-sources/x/contexts/t.tar.gz");
            }),
            "must be staged in the project bucket",
        ),
        (
            with(&|d| {
                d["build"]["context_uri"] =
                    json!(format!("s3://test-bucket/elsewhere/contexts/{TREE}.tar.gz"));
            }),
            "must be staged in the project bucket",
        ),
        (with(&|d| d["build"]["kind"] = json!("conda")), "unknown build kind"),
        (with(&|d| d["build"]["archs"] = json!(["riscv"])), "unknown architecture"),
        (with(&|d| d["build"]["archs"] = json!([])), "no architecture"),
        (with(&|d| d["build"]["dockerfile"] = json!("Dockerfile")), "no Dockerfile"),
        (with(&|d| d["build"]["tree_hash"] = json!("short")), "sha256 hex"),
        (with(&|d| d["ref"] = json!("ubuntu:24.04")), "exactly one of `ref` and `build`"),
        // The paths reach the build unquoted: nothing may climb out of the context.
        (with(&|d| d["build"]["context"] = json!("../env")), "must be a relative path"),
        (with(&|d| d["build"]["context"] = json!("/env")), "must be a relative path"),
    ];
    for (body, want) in cases {
        let (status, msg) = register(&h, &body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{want}: {msg}");
        assert!(msg.as_str().unwrap().contains(want), "{want}: {msg}");
    }
    // A built default and `image:` both name the default.
    let mut both = pixi_body("abc");
    both["image"] = json!("ubuntu:24.04");
    let (status, msg) = register(&h, &both).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{msg}");
    assert!(
        msg.as_str().unwrap().contains("`image:` and `images.default` both name the default image"),
        "{msg}"
    );
}

#[tokio::test]
async fn a_build_context_is_staged_beside_the_source() {
    let h = harness();
    let (status, body) = call(
        &h.state,
        "POST",
        "/workflows/source-upload",
        "developers",
        Some(&json!({"version_id": "abc:hash", "context_tree_hash": TREE, "project": "demo"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        body["uri"]
            .as_str()
            .unwrap()
            .ends_with(&format!("/workflow-sources/abc:hash/contexts/{TREE}.tar.gz")),
        "{body}"
    );
    let (status, body) = call(
        &h.state,
        "POST",
        "/workflows/source-upload",
        "developers",
        Some(&json!({"version_id": "abc:hash", "context_tree_hash": "../x", "project": "demo"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(body.as_str().unwrap().contains("invalid context_tree_hash: \"../x\""), "{body}");
}

#[tokio::test]
async fn a_built_snakemake_image_is_derived_like_an_imported_one() {
    use biffix_workflow::images::{build_content_key, build_subject, Arch, BuildKind};
    let h = harness();
    let mut body = snakemake_body("abc", SAMTOOLS, false);
    body["images"]["align"] = json!({"build": {
        "kind": "dockerfile",
        "context": "docker/align",
        "dockerfile": "Dockerfile",
        "context_uri": staged_context(),
        "context_sha256": TARBALL_SHA,
        "tree_hash": TREE,
        "archs": ["amd64"],
    }});
    let (status, view) = register(&h, &body).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{view}");
    let id = view["id"].as_str().unwrap().to_string();
    let key = content_key(&h, &id, "align");
    assert_eq!(
        key,
        build_content_key(BuildKind::Dockerfile, TARBALL_SHA, Some("Dockerfile"), &[Arch::Amd64])
    );
    let rec = h.registry.versions.lock().unwrap()[&id].clone();
    let align = rec.images.iter().find(|i| i.name == "align").unwrap();
    assert_eq!(align.source_ref, "dockerfile:docker/align/Dockerfile");
    let builds = h.cloud.image_builds.lock().unwrap().clone();
    assert_eq!(builds.len(), 1);
    assert_eq!(builds[0].arch, Arch::Amd64);
    assert_eq!(builds[0].dockerfile.as_deref(), Some("Dockerfile"));
    assert_eq!(builds[0].dockerfile_text, None);

    set_runtime_layer(&h, &runtime_key(&h, &id));
    let pushed = format!("{}@sha256:amd", testing::FAKE_WORKFLOW_REPO);
    set_build(
        &h,
        &build_subject(&key, Some(Arch::Amd64)),
        BuildState::Succeeded,
        1,
        &[("IMAGE", &pushed)],
    );
    sweep_ok(&h).await;
    let index = h.cloud.image_indexes.lock().unwrap()[0].clone();
    assert_eq!(index.manifests, [pushed]);
    let mut done = imported("bash=1,libc=glibc");
    done[0].1 = format!("{}@sha256:built", testing::FAKE_WORKFLOW_REPO);
    done[1].1 = "sha256:built".into();
    set_build(&h, &index.subject, BuildState::Succeeded, 1, &outputs(&done));
    sweep_ok(&h).await;
    let derives = h.cloud.image_derives.lock().unwrap().clone();
    assert!(
        derives.iter().any(|d| d.base == format!("{}@sha256:built", testing::FAKE_WORKFLOW_REPO)),
        "{derives:?}"
    );
}

#[tokio::test]
async fn a_dockerfile_build_s_paths_are_checked_and_spelled_one_way() {
    use biffix_workflow::images::{build_content_key, Arch, BuildKind};
    let h = harness();
    let body = |context: &str, dockerfile: &str| {
        let mut body = snakemake_body("abc", SAMTOOLS, false);
        body["images"]["align"] = json!({"build": {
            "kind": "dockerfile",
            "context": context,
            "dockerfile": dockerfile,
            "context_uri": staged_context(),
            "context_sha256": TARBALL_SHA,
            "tree_hash": TREE,
            "archs": ["amd64"],
        }});
        body
    };
    // `./docker/align/` + `./Dockerfile` is the same build as `docker/align` + `Dockerfile`.
    let (status, view) = register(&h, &body("./docker/align/", "./Dockerfile")).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{view}");
    let id = view["id"].as_str().unwrap().to_string();
    assert_eq!(
        content_key(&h, &id, "align"),
        build_content_key(BuildKind::Dockerfile, TARBALL_SHA, Some("Dockerfile"), &[Arch::Amd64])
    );
    let rec = h.registry.versions.lock().unwrap()[&id].clone();
    let align = rec.images.iter().find(|i| i.name == "align").unwrap();
    assert_eq!(align.source_ref, "dockerfile:docker/align/Dockerfile");
    let recipe = align.build.as_ref().unwrap();
    assert_eq!(recipe.context, "docker/align");
    assert_eq!(recipe.dockerfile.as_deref(), Some("Dockerfile"));

    for (context, dockerfile, want) in [
        ("docker", "../../etc/Dockerfile", "must be a relative path"),
        ("docker", "/Dockerfile", "must be a relative path"),
        ("docker", "./", "names its Dockerfile"),
    ] {
        let (status, msg) = register(&h, &body(context, dockerfile)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{dockerfile}: {msg}");
        assert!(msg.as_str().unwrap().contains(want), "{want}: {msg}");
    }
}

#[tokio::test]
async fn the_engine_version_is_recorded_in_its_canonical_spelling() {
    let h = harness();
    for (sha, engine_version, recorded) in
        [("n1", "25.10.04", "25.10.4"), ("n2", "25.010.4", "25.10.4")]
    {
        let mut body = register_body(sha);
        body["engine_version"] = json!(engine_version);
        let (status, view) = register(&h, &body).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{view}");
        let id = view["id"].as_str().unwrap().to_string();
        let rec = h.registry.versions.lock().unwrap()[&id].clone();
        assert_eq!(rec.version.engine_version, recorded, "{engine_version}");
    }
    let mut body = snakemake_body("s1", SAMTOOLS, false);
    body["engine_version"] = json!("9.027.0");
    let (status, view) = register(&h, &body).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{view}");
    let id = view["id"].as_str().unwrap().to_string();
    assert_eq!(h.registry.versions.lock().unwrap()[&id].version.engine_version, "9.27.0");
}
