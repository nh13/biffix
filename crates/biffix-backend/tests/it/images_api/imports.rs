//! Pinning and importing a version's images: one import per image, retries, and refusals at registration.

use super::*;

#[tokio::test]
async fn registering_pins_every_image_and_starts_its_import() {
    let h = harness();
    let (status, view) = register(&h, &register_body("abc")).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{view}");
    assert_eq!(view["state"], "pending");
    let id = view["id"].as_str().unwrap();

    let rec = h.registry.versions.lock().unwrap()[id].clone();
    let names: Vec<&str> = rec.images.iter().map(|i| i.name.as_str()).collect();
    assert_eq!(names, ["default", "fastqc"]);
    for image in &rec.images {
        assert!(image.source_ref.contains("@sha256:"), "{}", image.source_ref);
    }
    assert_eq!(rec.images[0].declared_ref, "ubuntu:24.04");
    assert!(rec.images[0].source_ref.starts_with("docker.io/library/ubuntu:24.04@sha256:"));
    // The default image runs from the tenant repository, under its source digest.
    assert_eq!(rec.version.image_digest, rec.images[0].source_ref.rsplit_once('@').unwrap().1);
    assert_eq!(rec.version.image_repository.as_deref(), Some("biffix-workers"));

    // Each image is imported from its pinned reference, never the tag it was declared by.
    let imports = h.cloud.image_imports.lock().unwrap().clone();
    assert_eq!(imports.len(), 2);
    for image in &rec.images {
        let import = imports
            .iter()
            .find(|i| i.content_key == image.content_key)
            .unwrap_or_else(|| panic!("no import of {}: {imports:?}", image.name));
        assert_eq!(import.source, image.source_ref);
        let (_, digest) = import.source.rsplit_once('@').unwrap();
        digest
            .parse::<biffix_workflow::digest::ImageDigest>()
            .unwrap_or_else(|e| panic!("{}: {e}", import.source));
        assert_eq!(import.subject, format!("image:{}", import.content_key));
    }
    let events = h.events.events.lock().unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(&e.data, EventData::BuildRequested { subject, .. } if subject.starts_with("image:")))
            .count(),
        2
    );
}

#[tokio::test]
async fn a_version_runs_only_once_its_images_are_imported() {
    let h = harness();
    let (_, view) = register(&h, &register_body("abc")).await;
    let id = view["id"].as_str().unwrap().to_string();
    let submit = json!({"workflow_version_id": id, "params": {}});
    let (status, body) = call(&h.state, "POST", "/runs", "operators", Some(&submit)).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    // The fake event sink does not fold `build_requested`, so the imports read as not started.
    assert!(body.as_str().unwrap().contains("is waiting to be imported"), "{body}");

    // Both imports finish; the sweep records them.
    let done = imported("bash=1,libc=glibc,snakemake=");
    for name in ["default", "fastqc"] {
        set_import(&h, &content_key(&h, &id, name), BuildState::Succeeded, 1, &outputs(&done));
    }
    let report = sweep_ok(&h).await;
    assert_eq!((report.imports, report.ready), (2, 2));

    let d = detail(&h, &id).await;
    assert_eq!(d["state"], "ready", "{d}");
    assert_eq!(d["images"][1]["name"], "fastqc");
    assert_eq!(d["images"][1]["state"], "ready");
    assert_eq!(d["images"][1]["keep_tag"], "biffix-keep-imp");
    assert_eq!(d["images"][1]["archived"], true);
    let (status, body) = call(&h.state, "POST", "/runs", "operators", Some(&submit)).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
}

#[tokio::test]
async fn versions_naming_the_same_image_share_one_import() {
    let h = harness();
    let (_, first) = register(&h, &register_body("abc")).await;
    // The imports it started are in progress (as the fold of their build_requested would say).
    let first = first["id"].as_str().unwrap().to_string();
    for name in ["default", "fastqc"] {
        set_import(&h, &content_key(&h, &first, name), BuildState::Running, 1, &[]);
    }
    let (status, view) = register(&h, &register_body("def")).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    // The second registration found both imports in progress and started none.
    assert_eq!(h.cloud.image_imports.lock().unwrap().len(), 2);

    // Once imported, a third version with the same images is ready at once: 201.
    let id = view["id"].as_str().unwrap().to_string();
    let done = imported("bash=1,libc=glibc");
    for name in ["default", "fastqc"] {
        set_import(&h, &content_key(&h, &id, name), BuildState::Succeeded, 1, &outputs(&done));
    }
    sweep_ok(&h).await;
    let (status, view) = register(&h, &register_body("ghi")).await;
    assert_eq!(status, StatusCode::CREATED, "{view}");
    assert_eq!(view["state"], "ready");
}

#[tokio::test]
async fn an_image_unsuited_to_the_engine_fails_the_version() {
    let h = harness();
    let (_, view) = register(&h, &register_body("abc")).await;
    let id = view["id"].as_str().unwrap().to_string();
    let done = imported("bash=1,libc=glibc");
    let no_bash = imported("bash=0,libc=none");
    set_import(&h, &content_key(&h, &id, "default"), BuildState::Succeeded, 1, &outputs(&done));
    set_import(&h, &content_key(&h, &id, "fastqc"), BuildState::Succeeded, 1, &outputs(&no_bash));
    sweep_ok(&h).await;
    let d = detail(&h, &id).await;
    assert_eq!(d["state"], "failed");
    assert!(d["not_ready"][0].as_str().unwrap().contains("image `fastqc` has no bash"), "{d}");
    let submit = json!({"workflow_version_id": id, "params": {}});
    let (status, body) = call(&h.state, "POST", "/runs", "operators", Some(&submit)).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(body.as_str().unwrap().contains("image `fastqc` has no bash"), "{body}");
}

#[tokio::test]
async fn failed_imports_are_retried_up_to_the_limit_and_can_be_rebuilt() {
    let h = harness();
    let (_, view) = register(&h, &register_body("abc")).await;
    let id = view["id"].as_str().unwrap().to_string();
    let key = content_key(&h, &id, "fastqc");
    set_import(&h, &key, BuildState::Failed, 1, &[]);
    sweep_ok(&h).await;
    let retried = |h: &Harness| {
        h.cloud.image_imports.lock().unwrap().iter().filter(|i| i.content_key == key).count()
    };
    assert_eq!(retried(&h), 2);

    // One build short of the limit it is still retried; at the limit it is not.
    set_import(
        &h,
        &key,
        BuildState::Failed,
        biffix_backend::images::build_follow::AUTO_BUILDS - 1,
        &[],
    );
    sweep_ok(&h).await;
    assert_eq!(retried(&h), 3);
    set_import(
        &h,
        &key,
        BuildState::Failed,
        biffix_backend::images::build_follow::AUTO_BUILDS,
        &[],
    );
    sweep_ok(&h).await;
    assert_eq!(retried(&h), 3);
    let d = detail(&h, &id).await;
    assert_eq!(d["state"], "failed");
    assert_eq!(d["images"][1]["reason"], "FAILED: BUILD: MANIFEST_UNKNOWN");
    assert_eq!(d["images"][1]["logs_url"], "https://logs/import");

    let uri = format!("/workflows/{id}/images/fastqc/rebuild");
    let (status, _) = call(&h.state, "POST", &uri, "operators", None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, body) = call(&h.state, "POST", &uri, "developers", None).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    assert_eq!(retried(&h), 4);
    let (status, _) =
        call(&h.state, "POST", &format!("/workflows/{id}/images/nope/rebuild"), "developers", None)
            .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn bad_images_are_refused_at_registration() {
    // A missing named image beside a valid default: the default's import is not started either.
    let h = harness();
    h.cloud.missing_images.lock().unwrap().insert(FASTQC.to_string());
    let (status, body) = register(&h, &register_body("abc")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body.as_str().unwrap().contains(&format!("{FASTQC}: not found in its registry")),
        "{body}"
    );
    assert_nothing_registered(&h);

    let h = harness();
    let mut body = register_body("abc");
    body["images"] = json!({"Bad Name": {"ref": FASTQC}});
    let (status, body) = register(&h, &body).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(body.as_str().unwrap().contains("image name `Bad Name` is invalid"), "{body}");
    assert_nothing_registered(&h);

    let mut body = register_body("abc");
    body["images"] = json!({"x": {"ref": FASTQC, "runtime": "own"}});
    let (status, body) = register(&h, &body).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body.as_str().unwrap().contains("Snakemake images only"), "{body}");
    assert_nothing_registered(&h);
}

#[tokio::test]
async fn a_version_whose_runtime_cannot_be_read_never_holds_back_another_s_import() {
    let h = harness();
    let (_, older) = register(&h, &register_body("abc")).await;
    let older = older["id"].as_str().unwrap().to_string();
    let (_, newer) = register(&h, &register_body("def")).await;
    let newer = newer["id"].as_str().unwrap().to_string();
    {
        let mut versions = h.registry.versions.lock().unwrap();
        // The newer one is swept first; its runtime recipe no longer reads.
        versions.get_mut(&older).unwrap().created_at_micros = 1;
        versions.get_mut(&newer).unwrap().created_at_micros = 2;
        versions.get_mut(&newer).unwrap().version.runtime_recipe = json!("unreadable");
    }
    let done = imported("bash=1,libc=glibc");
    for name in ["default", "fastqc"] {
        set_import(&h, &content_key(&h, &older, name), BuildState::Succeeded, 1, &outputs(&done));
    }
    // The newer version's step fails, but the older one, naming the same images, records them.
    let report = biffix_backend::images::sweep(&h.state).await.unwrap();
    assert_eq!((report.errors, report.ready), (2, 2), "{report:?}");
    assert_eq!(detail(&h, &older).await["state"], "ready");
}
