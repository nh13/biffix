//! Snakemake images: deriving the runtime into them and the checks before a derive.

use super::*;

fn set_derive(h: &Harness, subject: &str, state: BuildState) {
    let outputs = [
        ("IMAGE", format!("{}@sha256:der", testing::FAKE_WORKFLOW_REPO)),
        ("IMAGE_DIGEST", "sha256:der".into()),
        ("BASE_DIGEST", "sha256:imp".into()),
        ("ARCHS", "amd64".into()),
        ("CAPABILITIES", "bash=1,libc=glibc,snakemake=9.27.0,storage_s3=0.3.6".into()),
        ("KEEP_TAG", "biffix-keep-der".into()),
        ("ARCHIVED", "1".into()),
    ];
    put_build(
        h,
        BuildProjection {
            build_arn: "arn:derive:1".into(),
            // Only a finished derive reports its image.
            outputs: if state == BuildState::Succeeded {
                outputs.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
            } else {
                BTreeMap::new()
            },
            request: json!({"source": format!("{}@sha256:imp", testing::FAKE_WORKFLOW_REPO)}),
            ..projection(subject, state, 1)
        },
    );
}

#[tokio::test]
async fn a_snakemake_image_gets_the_runtime_before_it_runs() {
    let h = harness();
    let (status, view) = register(&h, &snakemake_body("abc", SAMTOOLS, false)).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{view}");
    let id = view["id"].as_str().unwrap().to_string();
    let key = runtime_key(&h, &id);

    // Imported, but the runtime's layer is not built yet: the derive waits for it.
    let done = imported("bash=1,libc=glibc");
    set_import(&h, &content_key(&h, &id, "default"), BuildState::Succeeded, 1, &outputs(&done));
    sweep_ok(&h).await;
    assert!(h.cloud.image_derives.lock().unwrap().is_empty());
    let d = detail(&h, &id).await;
    assert_eq!(d["state"], "pending");
    assert!(
        d["not_ready"][0].as_str().unwrap().contains("waiting for the Snakemake 9.27.0 runtime"),
        "{d}"
    );

    // Once it is, the sweep derives the imported image with it.
    set_runtime_layer(&h, &key);
    let report = sweep_ok(&h).await;
    assert_eq!(report.derives, 1);
    let derives = h.cloud.image_derives.lock().unwrap().clone();
    assert_eq!(derives.len(), 1, "{derives:?}");
    let derived_key = biffix_workflow::images::derived_key("sha256:imp", &key);
    assert_eq!(derives[0].subject, format!("image:derived:{derived_key}"));
    assert_eq!(derives[0].derived_key, derived_key);
    assert_eq!(derives[0].base, format!("{}@sha256:imp", testing::FAKE_WORKFLOW_REPO));
    assert_eq!(
        (derives[0].layer_amd64.as_str(), derives[0].layer_arm64.as_str()),
        ("coord@sha256:layer-amd64", "coord@sha256:layer-arm64")
    );
    assert_eq!(derives[0].snakemake_version, "9.27.0");
    let d = detail(&h, &id).await;
    assert_eq!(d["images"][0]["derived_state"], "unbuilt", "{d}");
    // In progress, not failed (`… runtime failed: …`), and the default image's.
    assert_eq!(d["not_ready"][0], "image `default`: adding the Snakemake 9.27.0 runtime", "{d}");
    let submit = json!({"workflow_version_id": id, "params": {}});
    let (status, body) = call(&h.state, "POST", "/runs", "operators", Some(&submit)).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    let body = body.as_str().unwrap();
    assert!(body.contains("image `default`: adding the Snakemake 9.27.0 runtime"), "{body}");
    assert!(!body.contains("runtime failed"), "{body}");

    // A running derive is not started again; a finished one is recorded and the version is ready.
    set_derive(&h, &derives[0].subject, BuildState::Running);
    sweep_ok(&h).await;
    assert_eq!(h.cloud.image_derives.lock().unwrap().len(), 1);
    set_derive(&h, &derives[0].subject, BuildState::Succeeded);
    sweep_ok(&h).await;
    let d = detail(&h, &id).await;
    assert_eq!(d["state"], "ready", "{d}");
    assert_eq!(d["images"][0]["derived_state"], "ready");
    assert_eq!(
        d["images"][0]["derived_image"],
        format!("{}@sha256:der", testing::FAKE_WORKFLOW_REPO)
    );

    // Tasks run the derived image; the anchor keeps the imported (= declared) digest too.
    let source_digest = h.registry.versions.lock().unwrap()[&id].version.image_digest.clone();
    let (status, body) = call(&h.state, "POST", "/runs", "operators", Some(&submit)).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let launched = h.cloud.launched.lock().unwrap().clone();
    assert_eq!(launched[0].image_digest, "sha256:der");
    let events = h.events.events.lock().unwrap();
    let (anchor, run_id, image_map) = events
        .iter()
        .find_map(|e| match &e.data {
            EventData::RunSubmitted(anchor) => Some((
                (anchor.image_digest.clone(), anchor.image_source_digest.clone()),
                e.run_id.clone().unwrap(),
                anchor.image_map.clone().unwrap(),
            )),
            _ => None,
        })
        .unwrap();
    assert_eq!(anchor, ("sha256:der".to_string(), Some(source_digest)));

    // The run's image map: the declared ref (already in its normalized spelling), its pinned
    // source reference and its `images:` name run the derived image, which, as the engine's
    // global default, maps to itself and is `$default`. The pinned reference's tag-only spelling
    // is the declared ref itself. Nothing maps to the imported (underived) copy.
    let derived = format!("{}@sha256:der", testing::FAKE_WORKFLOW_REPO);
    let pinned = h.registry.versions.lock().unwrap()[&id].images[0].source_ref.clone();
    assert!(pinned.starts_with(&format!("{SAMTOOLS}@sha256:")), "{pinned}");
    let uri = format!("s3://test-bucket/biffix/runs/{}/inputs/images.json", run_id.as_str());
    assert_eq!(launched[0].image_map_uri.as_deref(), Some(uri.as_str()));
    let written: BTreeMap<String, String> =
        serde_json::from_slice(&h.cloud.objects.lock().unwrap()[&uri]).unwrap();
    assert_eq!(written, image_map);
    assert_eq!(
        written,
        BTreeMap::from([
            (SAMTOOLS.to_string(), derived.clone()),
            (pinned, derived.clone()),
            ("default".to_string(), derived.clone()),
            (derived.clone(), derived.clone()),
            ("$default".to_string(), derived),
        ])
    );
}

#[tokio::test]
async fn an_image_with_its_own_snakemake_must_match_the_pinned_release() {
    let h = harness();
    let (_, view) = register(&h, &snakemake_body("abc", SAMTOOLS, true)).await;
    let id = view["id"].as_str().unwrap().to_string();
    set_runtime_layer(&h, &runtime_key(&h, &id));
    let sm8 = imported("bash=1,libc=glibc,snakemake=8.30.0,storage_s3=0.3.6");
    set_import(&h, &content_key(&h, &id, "default"), BuildState::Succeeded, 1, &outputs(&sm8));
    sweep_ok(&h).await;
    let d = detail(&h, &id).await;
    assert_eq!(d["state"], "failed", "{d}");
    assert!(
        d["not_ready"][0]
            .as_str()
            .unwrap()
            .contains("carries Snakemake 8.30.0, but this version runs Snakemake 9.27.0"),
        "{d}"
    );
    // An image with its own Snakemake is never derived.
    assert!(h.cloud.image_derives.lock().unwrap().is_empty());

    // The same declaration over an image with the pinned release runs it as imported.
    let own_image = "docker.io/snakemake/snakemake:v9.27.0";
    let (_, view) = register(&h, &snakemake_body("def", own_image, true)).await;
    let id = view["id"].as_str().unwrap().to_string();
    let sm9 = imported("bash=1,libc=musl,snakemake=9.27.0,storage_s3=0.3.6");
    set_import(&h, &content_key(&h, &id, "default"), BuildState::Succeeded, 1, &outputs(&sm9));
    sweep_ok(&h).await;
    let d = detail(&h, &id).await;
    assert_eq!(d["state"], "ready", "{d}");
    assert!(d["images"][0].get("derived_state").is_none(), "{d}");
    assert!(h.cloud.image_derives.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_snakemake_image_without_glibc_is_refused_before_any_derive() {
    let h = harness();
    let (_, view) = register(&h, &snakemake_body("abc", SAMTOOLS, false)).await;
    let id = view["id"].as_str().unwrap().to_string();
    set_runtime_layer(&h, &runtime_key(&h, &id));
    let musl = imported("bash=1,libc=musl");
    set_import(&h, &content_key(&h, &id, "default"), BuildState::Succeeded, 1, &outputs(&musl));
    sweep_ok(&h).await;
    let d = detail(&h, &id).await;
    assert_eq!(d["state"], "failed");
    assert!(d["not_ready"][0].as_str().unwrap().contains("no glibc (it is musl)"), "{d}");
    assert!(h.cloud.image_derives.lock().unwrap().is_empty());
}
