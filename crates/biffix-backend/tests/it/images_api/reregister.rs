//! Registering a version again: what it keeps, moving onto the current runtime, and what counts as identical.

use super::*;

/// As if version `id` was registered under an older pin table: what it froze differs from what
/// registering its inputs again would, so a re-registration is a new build.
fn under_older_pins(h: &Harness, id: &str) {
    h.registry.versions.lock().unwrap().get_mut(id).unwrap().registration_inputs_hash =
        "hash-under-older-pins".into();
}

#[tokio::test]
async fn reregistering_keeps_what_the_version_recorded_and_refuses_no_change() {
    let h = harness();
    let mut body = register_body("abc");
    body["about_markdown"] = json!("# About");
    body["form_schema"] = json!({"fields": [{"name": "reads", "widget": "s3_file", "required": true, "is_output": false, "is_secret": false, "is_path_input": true}], "warnings": []});
    let (status, view) = register(&h, &body).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{view}");
    let first = view["id"].as_str().unwrap().to_string();
    under_older_pins(&h, &first);

    // Registered under older pins, so the first re-register is a new build.
    let (status, view) = reregister(&h, &first, json!({})).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{view}");
    let second = view["id"].as_str().unwrap().to_string();
    assert_ne!(first, second);
    let (old, new) = {
        let versions = h.registry.versions.lock().unwrap();
        (versions[&first].clone(), versions[&second].clone())
    };
    assert_eq!(new.version.build, 1);
    assert_eq!(new.version.form_schema, old.version.form_schema);
    assert_eq!(new.version.about_markdown.as_deref(), Some("# About"));
    assert_eq!(new.version.entrypoint, old.version.entrypoint);
    assert_eq!(new.version.queue_classes, old.version.queue_classes);
    assert_eq!(new.source, old.source);
    assert_eq!(new.images, old.images);
    assert_eq!(new.version.image_digest, old.version.image_digest);
    let inputs = new.registration_inputs;
    assert_eq!(
        inputs.reregistered_from.as_ref().map(biffix_core::ids::WorkflowVersionId::as_str),
        Some(first.as_str())
    );
    assert_eq!(inputs.source_mode, "git");
    assert_eq!(inputs.cli_version, "0.0.0");

    // Again: nothing changed since build 1.
    let versions = h.registry.versions.lock().unwrap().len();
    let (status, message) = reregister(&h, &second, json!({})).await;
    assert_eq!(status, StatusCode::CONFLICT, "{message}");
    assert!(
        message.as_str().unwrap().contains(&format!("identical to build 1 ({second})")),
        "{message}"
    );
    let (status, plan) = reregister(&h, &first, json!({"dry_run": true})).await;
    assert_eq!(status, StatusCode::OK, "{plan}");
    assert_eq!(plan["identical_to"], json!(second));
    assert_eq!(plan["from"], json!(first));
    assert_eq!(plan["images"].as_array().unwrap().len(), 2);
    assert!(plan["images"][0].get("previous_source_ref").is_none());
    assert_eq!(h.registry.versions.lock().unwrap().len(), versions);
}

#[tokio::test]
async fn reregistering_moves_a_version_onto_the_current_runtime() {
    let h = harness();
    let (_, view) = register(&h, &snakemake_body("abc", SAMTOOLS, false)).await;
    let first = view["id"].as_str().unwrap().to_string();
    let current = runtime_key(&h, &first);
    // As if registered under an older pin table.
    {
        let mut versions = h.registry.versions.lock().unwrap();
        let rec = versions.get_mut(&first).unwrap();
        rec.version.runtime_key = "old-runtime".into();
        rec.registration_inputs = biffix_api::RegistrationInputs {
            source_mode: "git".into(),
            cli_version: "0.1.0".into(),
            ..Default::default()
        };
        rec.registration_inputs_hash = "hash-under-the-old-pins".into();
    }

    let (status, plan) = reregister(&h, &first, json!({"dry_run": true})).await;
    assert_eq!(status, StatusCode::OK, "{plan}");
    assert!(plan.get("identical_to").is_none(), "{plan}");
    assert_eq!(plan["build"], 1);
    assert_eq!(plan["runtime_key"], json!(current));
    assert_eq!(plan["engine_version"], "9.27.0");

    let (status, view) = reregister(&h, &first, json!({})).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{view}");
    let second = view["id"].as_str().unwrap();
    assert_eq!(runtime_key(&h, second), current);
    let rec = h.registry.versions.lock().unwrap()[second].clone();
    // The recorded CLI release is kept: it derived the form the version carries.
    assert_eq!(rec.registration_inputs.cli_version, "0.1.0");
}

#[tokio::test]
async fn a_built_image_is_reregistered_from_its_recipe_without_building_again() {
    let h = harness();
    let (_, view) = register(&h, &pixi_body("abc")).await;
    let first = view["id"].as_str().unwrap().to_string();
    // Its architecture builds are in progress (as the fold of their build_requested would say).
    let key = content_key(&h, &first, "default");
    for arch in biffix_workflow::images::Arch::ALL {
        let subject = biffix_workflow::images::build_subject(&key, Some(arch));
        set_build(&h, &subject, BuildState::Running, 1, &[]);
    }
    let builds = h.cloud.image_builds.lock().unwrap().len();
    under_older_pins(&h, &first);

    let (status, view) = reregister(&h, &first, json!({})).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{view}");
    let (old, new) = {
        let versions = h.registry.versions.lock().unwrap();
        (versions[&first].clone(), versions[view["id"].as_str().unwrap()].clone())
    };
    assert_eq!(new.images, old.images);
    assert_eq!(new.images[0].kind(), "pixi");
    assert_eq!(h.cloud.image_builds.lock().unwrap().len(), builds);
}

#[tokio::test]
async fn reregistering_is_for_developers_and_existing_versions() {
    let h = harness();
    let (_, view) = register(&h, &register_body("abc")).await;
    let id = view["id"].as_str().unwrap();
    let (status, _) = call(
        &h.state,
        "POST",
        &format!("/workflows/{id}/reregister"),
        "operators",
        Some(&json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = reregister(&h, "nope:nope", json!({})).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn an_import_that_succeeded_without_an_image_fails_and_can_be_rebuilt() {
    let h = harness();
    let (_, view) = register(&h, &register_body("abc")).await;
    let id = view["id"].as_str().unwrap().to_string();
    let key = content_key(&h, &id, "fastqc");
    let imports = |h: &Harness| {
        h.cloud.image_imports.lock().unwrap().iter().filter(|i| i.content_key == key).count()
    };
    // One build short of the limit, a build that reported no image is imported again.
    set_import(
        &h,
        &key,
        BuildState::Succeeded,
        biffix_backend::images::build_follow::AUTO_BUILDS - 1,
        &[],
    );
    sweep_ok(&h).await;
    assert_eq!(imports(&h), 2);
    // Its automatic builds are used up, and the last one reported no image.
    set_import(
        &h,
        &key,
        BuildState::Succeeded,
        biffix_backend::images::build_follow::AUTO_BUILDS,
        &[],
    );
    sweep_ok(&h).await;
    assert_eq!(imports(&h), 2);
    let d = detail(&h, &id).await;
    assert_eq!(d["state"], "failed", "{d}");
    assert_eq!(d["images"][1]["state"], "failed");
    assert_eq!(d["images"][1]["reason"], "the build succeeded but reported no image", "{d}");
    assert_eq!(
        d["not_ready"][1],
        "image `fastqc` failed to import: the build succeeded but reported no image (build log: \
         https://logs/import)",
        "{d}"
    );

    let uri = format!("/workflows/{id}/images/fastqc/rebuild");
    let (status, body) = call(&h.state, "POST", &uri, "developers", None).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    assert_eq!(imports(&h), 3);
}

#[tokio::test]
async fn identical_means_the_same_frozen_version_whatever_the_client_says_it_overrode() {
    let h = harness();
    let mut body = register_body("frozen");
    // The CLI recorded no overrides: the engine version came from `biffix.yaml`.
    body["registration_inputs"] = json!({"source_mode": "git", "cli_version": "0.1.0"});
    let (status, view) = register(&h, &body).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{view}");
    let first = view["id"].as_str().unwrap().to_string();

    // The same body again, and a re-register naming the engine version it already records, both
    // freeze the same version: identical.
    let (status, message) = register(&h, &body).await;
    assert_eq!(status, StatusCode::CONFLICT, "{message}");
    assert!(
        message.as_str().unwrap().contains(&format!("identical to build 0 ({first})")),
        "{message}"
    );
    let (status, message) = reregister(&h, &first, json!({"engine_version": "25.10.4"})).await;
    assert_eq!(status, StatusCode::CONFLICT, "{message}");
    assert!(
        message.as_str().unwrap().contains(&format!("identical to build 0 ({first})")),
        "{message}"
    );

    // The same recorded inputs, but what is frozen differs (a FormSchema a newer backend adapts
    // differently; queue classes the recorded overrides do not mention): new builds.
    let mut schema = body.clone();
    schema["form_schema"] = json!({"fields": [{"name": "reads", "widget": "s3_file", "required": true, "is_output": false, "is_secret": false, "is_path_input": true}], "warnings": []});
    let (status, view) = register(&h, &schema).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{view}");
    assert_eq!(view["id"], json!("frozen:hash:1"));
    let mut queues = body.clone();
    queues["queue_classes"] = json!([]);
    let (status, view) = register(&h, &queues).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{view}");
    assert_eq!(view["id"], json!("frozen:hash:2"));
}
