//! A run preparing its submit-time images.

use super::*;

const SAMTOOLS: &str = "quay.io/biocontainers/samtools:1.21--h50ea8bc_0";

/// Run `r1` of version `abc:hash`, preparing: submitted with one image and never launched.
async fn state_with_preparing_run() -> AppState {
    let mut app_state = state_with_one_version().await;
    let store = MemStore::new();
    store.insert(testing::run_detail("r1", "preparing_images"));
    let rid = biffix_core::ids::RunId::new("r1");
    store.insert_anchor(biffix_core::event::run_preparing(
        &rid,
        biffix_core::time::Timestamp::from_micros(1_000_000),
        biffix_core::event::RunPreparing {
            workflow_version_id: "abc:hash".into(),
            params: serde_json::json!({}),
            input_uris: vec![],
            submitter: None,
            submitter_label: None,
            run_name: None,
            output_prefix: None,
            resumed_from: None,
            images: vec![biffix_core::event::SubmitImage {
                declared_ref: SAMTOOLS.into(),
                source_ref: format!("{SAMTOOLS}@sha256:5a"),
                content_key: "k5a".into(),
            }],
            experiment_id: None,
        },
    ));
    app_state.store = Arc::new(store);
    app_state
}

#[tokio::test]
async fn a_run_preparing_its_images_shows_where_each_is() {
    let state = state_with_preparing_run().await;
    let (status, v) = get_json(api::router(state), "/runs/r1").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["state"], "preparing_images");
    let preparing = &v["preparing"];
    assert_eq!(preparing["since_micros"], 1_000_000);
    assert_eq!(preparing["images"][0]["declared_ref"], SAMTOOLS);
    assert_eq!(preparing["not_ready"].as_array().unwrap().len(), 1, "{v}");
    assert!(preparing["not_ready"][0].as_str().unwrap().contains(SAMTOOLS), "{v}");
    // No anchor yet: no provenance, but the images the submit gave.
    assert!(v.get("provenance").is_none(), "{v}");
    assert_eq!(
        v["submit_images"],
        serde_json::json!([{"declared_ref": SAMTOOLS, "source_ref": format!("{SAMTOOLS}@sha256:5a")}])
    );
}

#[tokio::test]
async fn a_run_that_failed_preparing_its_images_says_why() {
    let mut state = state_with_preparing_run().await;
    let reason = format!("image `{SAMTOOLS}` failed to import: FAILED: BUILD: MANIFEST_UNKNOWN");
    let rid = biffix_core::ids::RunId::new("r1");
    let store = MemStore::new();
    store.insert(testing::run_detail("r1", "failed"));
    // Accepted, then failed preparing its images: it never launched.
    let preparing = state.store.run_event(&rid, &biffix_core::event::Kind::RunPreparing).await;
    store.insert_anchor(preparing.unwrap().unwrap());
    store.insert_anchor(biffix_core::event::run_failed(
        &rid,
        biffix_core::time::Timestamp::from_micros(2_000_000),
        biffix_core::event::FAILURE_STAGE_IMAGE_PREPARATION,
        &reason,
    ));
    state.store = Arc::new(store);
    let (status, v) = get_json(api::router(state.clone()), "/runs/r1").await;
    assert_eq!(status, StatusCode::OK);
    assert!(v.get("preparing").is_none(), "{v}");
    assert!(v.get("provenance").is_none(), "{v}");
    // What a resume of it inherits.
    assert_eq!(
        v["submit_images"],
        serde_json::json!([{"declared_ref": SAMTOOLS, "source_ref": format!("{SAMTOOLS}@sha256:5a")}])
    );
    let (status, v) = get_json(api::router(state), "/runs/r1/failure-summary").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(
        v,
        serde_json::json!({
            "failed_steps": 0,
            "items": [{
                "step": "image preparation",
                "cause": "launch_failed",
                "try_again": "rerun",
                "message": reason,
                "advice": "Fix the image reference or its registry, or rebuild it from the \
                           workflow's page, then submit again.",
                "attempts": 0,
                "log_tail": [],
                "reason": reason,
                "stage": "image_preparation",
            }]
        })
    );
}

#[tokio::test]
async fn a_run_that_could_not_launch_is_not_blamed_on_its_images() {
    let base = state_with_one_version().await;
    let rid = biffix_core::ids::RunId::new("r1");
    let reason = "the version's engine runtime failed to build: boom";
    // The launch stage, and a stage this build does not know: both read as a launch failure.
    for stage in [biffix_core::event::FAILURE_STAGE_LAUNCH, "some_newer_stage"] {
        let mut state = base.clone();
        let store = MemStore::new();
        store.insert(testing::run_detail("r1", "failed"));
        store.insert_anchor(biffix_core::event::run_failed(
            &rid,
            biffix_core::time::Timestamp::from_micros(2_000_000),
            stage,
            reason,
        ));
        state.store = Arc::new(store);
        let (status, v) = get_json(api::router(state), "/runs/r1/failure-summary").await;
        assert_eq!(status, StatusCode::OK, "{v}");
        let items = v["items"].as_array().unwrap();
        assert_eq!(items.len(), 1, "{v}");
        assert_eq!(items[0]["step"], "launch", "{stage}");
        assert_eq!(items[0]["cause"], "launch_failed");
        assert_eq!(items[0]["try_again"], "rerun");
        assert_eq!(items[0]["stage"], stage, "the stage as recorded, open");
        assert_eq!(items[0]["message"], reason);
        assert_eq!(
            items[0]["advice"],
            "The run could not be launched; check the deploy and the workflow version's engine \
             runtime, then submit again.",
            "{v}"
        );
    }
}
