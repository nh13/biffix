//! Images given at submit: launching, parking a run while they import, refusals and secrets.

use super::*;

/// The recorded `run_preparing` event: its run id and payload.
fn recorded_preparing(
    events: &MemEventSink,
) -> (Option<biffix_core::ids::RunId>, biffix_core::event::RunPreparing) {
    events
        .events
        .lock()
        .unwrap()
        .iter()
        .find_map(|e| match &e.data {
            biffix_core::event::EventData::RunPreparing(p) => {
                Some((e.run_id.clone(), (**p).clone()))
            }
            _ => None,
        })
        .expect("run_preparing recorded")
}

/// The version image a submit-time `reference` is imported as (the fake resolves a tag to the
/// digest of its spelling).
async fn submit_version_image(
    cloud: &FakeCloud,
    reference: &str,
) -> biffix_backend::store::registry::VersionImage {
    use biffix_backend::aws::registry::ImageRegistry;
    let parsed = biffix_workflow::images::ImageRef::parse(reference).unwrap();
    let digest = cloud.resolve_image_digest(&parsed).await.unwrap();
    biffix_backend::store::registry::VersionImage {
        name: parsed.to_string(),
        declared_ref: parsed.to_string(),
        source_ref: parsed.pinned(&digest).to_string(),
        content_key: biffix_workflow::images::content_key(&digest),
        own_runtime: false,
        build: None,
    }
}

#[tokio::test]
async fn a_runnable_submit_time_image_launches_at_once_and_wins_its_spellings() {
    let (_, app, registry, events, cloud) = images_app().await;
    let image = submit_version_image(&cloud, SAMTOOLS).await;
    let runtime_key = registry.versions.lock().unwrap()["abc:hash:0"].version.runtime_key.clone();
    let derived = testing::fake_derived_artifact(&image, &runtime_key);
    {
        let mut artifacts = registry.artifacts.lock().unwrap();
        artifacts.insert(image.content_key.clone(), testing::fake_artifact(&image));
        artifacts.insert(derived.content_key.clone(), derived.clone());
    }
    // Blanks are dropped and a repeat is one image.
    let body = images_submit(json!([SAMTOOLS, " ", format!(" {SAMTOOLS} ")]));
    let res = app.clone().oneshot(post("/runs", Some("developers"), &body)).await.unwrap();
    assert_eq!(res.status(), StatusCode::ACCEPTED);
    let out = json_body(res).await;
    assert_eq!(out["state"], "submitted", "{out}");
    assert_eq!(out["coordinator_job_id"], "coord-1");
    // Imported and derived already: nothing to import or derive.
    assert!(cloud.image_imports.lock().unwrap().is_empty());
    assert!(cloud.image_derives.lock().unwrap().is_empty());
    assert_eq!(cloud.launched.lock().unwrap().len(), 1);

    let anchor = recorded_anchor(&events);
    assert_eq!(
        anchor.submit_images,
        Some(vec![biffix_core::event::SubmitImage {
            declared_ref: image.declared_ref.clone(),
            source_ref: image.source_ref.clone(),
            content_key: image.content_key.clone(),
        }])
    );
    let map = anchor.image_map.unwrap();
    assert_eq!(map[SAMTOOLS], derived.image);
    assert_eq!(map[&image.source_ref], derived.image);
    // The run's default is still the version's.
    let default = &map[biffix_backend::images::map::DEFAULT_KEY];
    assert!(default.ends_with(&cloud.launched.lock().unwrap()[0].image_digest));
    assert_ne!(*default, derived.image);
}

/// A resume (or re-run) re-sends its prior run's image as the prior spelling at the digest it ran
/// (`ubuntu:24.04@sha256:…`): that digest is used as given, and the spelling the prior run's tasks
/// name (`ubuntu:24.04`) runs it.
#[tokio::test]
async fn a_resumed_image_given_with_its_digest_maps_the_spelling_tasks_name() {
    let (_, app, registry, events, cloud) = images_app().await;
    let digest = format!("sha256:{}", "c".repeat(64));
    let given = format!("ubuntu:24.04@{digest}");
    let image = submit_version_image(&cloud, &given).await;
    let runtime_key = registry.versions.lock().unwrap()["abc:hash:0"].version.runtime_key.clone();
    let derived = testing::fake_derived_artifact(&image, &runtime_key);
    {
        let mut artifacts = registry.artifacts.lock().unwrap();
        artifacts.insert(image.content_key.clone(), testing::fake_artifact(&image));
        artifacts.insert(derived.content_key.clone(), derived.clone());
    }
    // Already imported, so an Operator may name it.
    let res = app
        .clone()
        .oneshot(post("/runs", Some("operators"), &images_submit(json!([given]))))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::ACCEPTED);
    assert_eq!(cloud.launched.lock().unwrap().len(), 1);
    let anchor = recorded_anchor(&events);
    let recorded = anchor.submit_images.clone().unwrap();
    assert_eq!(
        recorded,
        vec![biffix_core::event::SubmitImage {
            declared_ref: given.clone(),
            source_ref: format!("docker.io/library/ubuntu:24.04@{digest}"),
            content_key: biffix_workflow::images::content_key(&digest),
        }]
    );
    let map = anchor.image_map.unwrap();
    for key in [given.as_str(), "ubuntu:24.04", "docker.io/library/ubuntu:24.04", &image.source_ref]
    {
        assert_eq!(map[key], derived.image, "{key}");
    }
}

#[tokio::test]
async fn a_submit_time_image_still_to_import_parks_the_run_as_preparing() {
    let (state, app, _, events, cloud) = images_app().await;
    let res = app
        .clone()
        .oneshot(post("/runs", Some("developers"), &images_submit(json!([SAMTOOLS]))))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::ACCEPTED);
    let out = json_body(res).await;
    assert_eq!(out["state"], "preparing_images", "{out}");
    assert!(out.get("coordinator_job_id").is_none(), "{out}");
    let run_id = out["run_id"].as_str().unwrap().to_string();

    // Its import started now; nothing launched.
    let imports = cloud.image_imports.lock().unwrap().clone();
    assert_eq!(imports.len(), 1);
    assert!(imports[0].source.starts_with(&format!("{SAMTOOLS}@sha256:")));
    assert!(cloud.launched.lock().unwrap().is_empty());

    // The run is recorded as preparing (no anchor), its launch request parked.
    let preparing = recorded_preparing(&events);
    assert_eq!(preparing.0.unwrap().as_str(), run_id);
    assert_eq!(preparing.1.workflow_version_id, "abc:hash:0");
    let image = submit_version_image(&cloud, SAMTOOLS).await;
    assert_eq!(
        preparing.1.images,
        vec![biffix_core::event::SubmitImage {
            declared_ref: SAMTOOLS.into(),
            source_ref: image.source_ref,
            content_key: image.content_key,
        }]
    );
    assert_eq!(
        preparing.1.output_prefix.as_deref(),
        Some(format!("s3://test-bucket/biffix/runs/{run_id}/").as_str())
    );
    assert!(!events
        .events
        .lock()
        .unwrap()
        .iter()
        .any(|e| e.kind() == biffix_core::event::Kind::RunSubmitted));
    let pending = state.store.list_pending_launches().await.unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].run_id.as_str(), run_id);
    assert_eq!(pending[0].plan["version"], 1);
    assert_eq!(pending[0].plan["images"], serde_json::to_value(&preparing.1.images).unwrap());
    assert_eq!(pending[0].plan["params"]["reads"], "s3://b/in.fastq");
}

#[tokio::test]
async fn submit_time_images_need_the_developer_role() {
    let (_, app, _, events, cloud) = images_app().await;
    let res = app
        .clone()
        .oneshot(post("/runs", Some("operators"), &images_submit(json!([SAMTOOLS]))))
        .await
        .unwrap();
    // Refused by the image-import gate (an Operator may launch), leaving nothing behind.
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
    let text = text_body(res).await;
    assert!(text.contains(&format!("image `{SAMTOOLS}` is not yet imported")), "{text}");
    assert!(events.events.lock().unwrap().is_empty());
    assert!(cloud.image_imports.lock().unwrap().is_empty());
    // Blank ones name no image, so an Operator may send them.
    let res = app
        .clone()
        .oneshot(post("/runs", Some("operators"), &images_submit(json!(["", "  "]))))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::ACCEPTED);
    assert!(recorded_anchor(&events).submit_images.is_none());
    assert!(cloud.image_imports.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_bad_submit_time_image_is_refused_before_anything_is_written() {
    let (_, app, _, events, cloud) = images_app().await;
    cloud.missing_images.lock().unwrap().insert("quay.io/b/gone:1".into());
    let too_many: Vec<String> = (0..33).map(|i| format!("quay.io/b/t:{i}")).collect();
    for (images, message) in [
        (json!(["not a ref"]), "image `not a ref`"),
        (json!(["quay.io/b/gone:1"]), "quay.io/b/gone:1"),
        (json!(["quay.io/b/gone:1"]), "not found"),
        (json!(too_many), "at most 32 submit-time images"),
    ] {
        let res = app
            .clone()
            .oneshot(post("/runs", Some("developers"), &images_submit(images)))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST, "{message}");
        let text = text_body(res).await;
        assert!(text.contains(message), "{text}");
    }
    assert!(events.events.lock().unwrap().is_empty());
    assert!(cloud.image_imports.lock().unwrap().is_empty());
    assert!(cloud.objects.lock().unwrap().is_empty());
}

#[tokio::test]
async fn the_submit_time_image_cap_counts_distinct_spellings() {
    // 64 entries, but each of 32 spellings twice (once padded): at the cap, so accepted.
    let (_, app, _, _, cloud) = images_app().await;
    let images: Vec<String> =
        (0..32).flat_map(|i| [format!("quay.io/b/t:{i}"), format!(" quay.io/b/t:{i} ")]).collect();
    let res = app
        .clone()
        .oneshot(post("/runs", Some("developers"), &images_submit(json!(images))))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::ACCEPTED, "{}", text_body(res).await);
    assert_eq!(cloud.image_imports.lock().unwrap().len(), 32);
}

#[tokio::test]
async fn a_bare_digest_at_submit_names_the_deploy_s_workflow_repo() {
    // As at registration, `sha256:<hex>` is an image in the deploy's own workflow repository.
    let (_, app, _, events, cloud) = images_app().await;
    let digest = format!("sha256:{}", "d".repeat(64));
    let res = app
        .clone()
        .oneshot(post("/runs", Some("developers"), &images_submit(json!([digest]))))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::ACCEPTED, "{}", text_body(res).await);
    let in_repo = format!("{}@{digest}", testing::FAKE_WORKFLOW_REPO);
    assert_eq!(
        recorded_preparing(&events).1.images,
        vec![biffix_core::event::SubmitImage {
            declared_ref: in_repo.clone(),
            source_ref: in_repo.clone(),
            content_key: biffix_workflow::images::content_key(&digest),
        }]
    );
    let imports = cloud.image_imports.lock().unwrap();
    assert_eq!(imports.len(), 1);
    assert_eq!(imports[0].source, in_repo);
}

#[tokio::test]
async fn a_submit_time_image_that_cannot_serve_the_version_refuses_the_run() {
    let (_, app, registry, events, cloud) = images_app().await;
    let image = submit_version_image(&cloud, SAMTOOLS).await;
    let musl = biffix_backend::store::registry::ImageArtifact {
        capabilities: biffix_workflow::images::Capabilities::parse("bash=1,libc=musl"),
        ..testing::fake_artifact(&image)
    };
    registry.artifacts.lock().unwrap().insert(image.content_key.clone(), musl);
    let res = app
        .clone()
        .oneshot(post("/runs", Some("developers"), &images_submit(json!([SAMTOOLS]))))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CONFLICT);
    let text = text_body(res).await;
    assert!(text.starts_with("this run cannot launch: "), "{text}");
    assert!(text.contains("no glibc (it is musl)"), "{text}");
    assert!(events.events.lock().unwrap().is_empty());
    assert!(cloud.launched.lock().unwrap().is_empty());
}

#[tokio::test]
async fn an_operator_may_name_an_image_already_imported_but_not_a_new_one() {
    let (_, app, registry, events, cloud) = images_app().await;
    // A Developer imported it before (registering it, or naming it at an earlier submit), and it
    // is derived with this version's runtime, so it is runnable as it is.
    let image = submit_version_image(&cloud, SAMTOOLS).await;
    let runtime_key = registry.versions.lock().unwrap()["abc:hash:0"].version.runtime_key.clone();
    let derived = testing::fake_derived_artifact(&image, &runtime_key);
    {
        let mut artifacts = registry.artifacts.lock().unwrap();
        artifacts.insert(image.content_key.clone(), testing::fake_artifact(&image));
        artifacts.insert(derived.content_key.clone(), derived);
    }
    let res = app
        .clone()
        .oneshot(post("/runs", Some("operators"), &images_submit(json!([SAMTOOLS]))))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::ACCEPTED);
    // Nothing new entered the tenant (no import, no derive): the run launches at once.
    let out = json_body(res).await;
    assert_eq!(out["state"], "submitted", "{out}");
    assert!(cloud.image_imports.lock().unwrap().is_empty());
    assert!(cloud.image_derives.lock().unwrap().is_empty());
    assert_eq!(cloud.launched.lock().unwrap().len(), 1);

    // A new image beside it needs a Developer: refused before anything is written or imported.
    let before = events.events.lock().unwrap().len();
    let res = app
        .clone()
        .oneshot(post(
            "/runs",
            Some("operators"),
            &images_submit(json!([SAMTOOLS, "quay.io/b/new:1"])),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
    let text = text_body(res).await;
    assert!(text.contains("image `quay.io/b/new:1` is not yet imported"), "{text}");
    assert_eq!(events.events.lock().unwrap().len(), before);
    assert!(cloud.image_imports.lock().unwrap().is_empty());
}

#[tokio::test]
async fn submit_time_imports_start_only_once_every_other_check_passes() {
    let (_, app, _, events, cloud) = images_app().await;
    // Params that do not validate, and an input that does not exist.
    let mut bad_params = images_submit(json!([SAMTOOLS]));
    bad_params["params"] = json!({"outdir": "s3://b/out/"});
    let mut missing_input = images_submit(json!([SAMTOOLS]));
    missing_input["params"]["reads"] = json!("s3://b/absent.fastq");
    cloud.absent.lock().unwrap().insert("s3://b/absent.fastq".into());
    // Each refused by the check it names, not by the image path.
    for (body, message) in [
        (bad_params, "reads: required"),
        (missing_input, "input(s) missing in S3: s3://b/absent.fastq"),
    ] {
        let res = app.clone().oneshot(post("/runs", Some("developers"), &body)).await.unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST, "{message}");
        let text = text_body(res).await;
        assert!(text.contains(message), "{message}: {text}");
    }
    assert!(cloud.image_imports.lock().unwrap().is_empty());
    assert!(events.events.lock().unwrap().is_empty());

    // The same image with every other check passing does start its import.
    let res = app
        .clone()
        .oneshot(post("/runs", Some("developers"), &images_submit(json!([SAMTOOLS]))))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::ACCEPTED);
    assert_eq!(cloud.image_imports.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn each_spelling_of_one_image_is_kept_pinned_to_one_digest_and_imported_once() {
    let (state, app, _, events, cloud) = images_app().await;
    let spellings = ["ubuntu", "ubuntu:latest", "docker.io/library/ubuntu:latest"];
    let body = images_submit(json!([spellings[0], spellings[1], spellings[2], " ubuntu "]));
    let res = app.clone().oneshot(post("/runs", Some("developers"), &body)).await.unwrap();
    assert_eq!(res.status(), StatusCode::ACCEPTED);
    // One import for the one image.
    assert_eq!(cloud.image_imports.lock().unwrap().len(), 1);
    let image = submit_version_image(&cloud, "ubuntu").await;
    let recorded = recorded_preparing(&events).1.images;
    // Each spelling as given (a task may name any of them), all pinned to the one digest.
    assert_eq!(
        recorded,
        spellings
            .iter()
            .map(|s| biffix_core::event::SubmitImage {
                declared_ref: s.to_string(),
                source_ref: image.source_ref.clone(),
                content_key: image.content_key.clone(),
            })
            .collect::<Vec<_>>()
    );
    let pending = state.store.list_pending_launches().await.unwrap();
    assert_eq!(pending[0].plan["images"], serde_json::to_value(&recorded).unwrap());
}

/// A register body whose form has a secret param.
fn secret_register_body() -> Value {
    let mut body = register_body();
    body["form_schema"]["fields"]
        .as_array_mut()
        .unwrap()
        .push(json!({"name": "token", "widget": "text", "required": false,
                     "is_output": false, "is_secret": true, "is_path_input": false}));
    body
}

#[tokio::test]
async fn a_parked_run_s_secrets_are_redacted_in_the_log_but_kept_for_its_launch() {
    let (state, app, _, events, _) = images_app_with(MemStore::new(), secret_register_body()).await;
    let mut body = images_submit(json!([SAMTOOLS]));
    body["params"]["token"] = json!("hunter2");
    let res = app.clone().oneshot(post("/runs", Some("developers"), &body)).await.unwrap();
    assert_eq!(res.status(), StatusCode::ACCEPTED);
    let params = recorded_preparing(&events).1.params;
    assert_eq!(params["token"], biffix_workflow::formschema::REDACTED_SECRET_PLACEHOLDER);
    assert!(!params.to_string().contains("hunter2"), "{params}");
    // The coordinator needs the value: the parked request keeps it (and is never an event).
    let pending = state.store.list_pending_launches().await.unwrap();
    assert_eq!(pending[0].plan["params"]["token"], "hunter2");
}

#[tokio::test]
async fn a_run_that_cannot_be_parked_or_recorded_is_an_error() {
    // Its launch cannot be parked: nothing is recorded.
    let store = MemStore::new();
    store.fail_put_pending.store(true, std::sync::atomic::Ordering::SeqCst);
    let (state, app, _, events, _) = images_app_with(store, register_body()).await;
    let res = app
        .clone()
        .oneshot(post("/runs", Some("developers"), &images_submit(json!([SAMTOOLS]))))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(text_body(res).await.starts_with("parking the run's launch failed"));
    // (Only the import it started is recorded, which is no run's.)
    assert!(!events.events.lock().unwrap().iter().any(|e| e.run_id.is_some()));
    assert!(state.store.list_pending_launches().await.unwrap().is_empty());

    // Parked, but not recorded: an error, and the parked launch stays (the record may have landed;
    // if not, the sweep drops a launch whose run was never recorded).
    let (mut state, _, _, events, _) = images_app().await;
    state.events =
        Arc::new(FailingSink::new(events.clone(), biffix_core::event::Kind::RunPreparing));
    let res = api::router(state.clone())
        .oneshot(post("/runs", Some("developers"), &images_submit(json!([SAMTOOLS]))))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(text_body(res).await.starts_with("failed to persist run"));
    // (Only the import it started is recorded, which is no run's.)
    assert!(!events.events.lock().unwrap().iter().any(|e| e.run_id.is_some()));
    assert_eq!(state.store.list_pending_launches().await.unwrap().len(), 1);
}
