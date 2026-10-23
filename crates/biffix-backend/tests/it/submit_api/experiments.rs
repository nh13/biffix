//! A run launched into an experiment.

use super::*;

/// Seed experiment `id` in `project`, archived when `archived`.
async fn seed_experiment(state: &AppState, id: &str, project: &str, archived: bool) {
    state
        .experiments
        .create(&biffix_backend::store::experiments::ExperimentRecord {
            id: id.into(),
            project: project.into(),
            name: id.into(),
            description: None,
            created_by: None,
            created_by_label: None,
            created_at_micros: 1,
            archived_at_micros: archived.then_some(2),
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn a_run_launched_into_an_experiment_carries_it_from_birth() {
    let (state, events, cloud) = state();
    cloud.objects.lock().unwrap().insert("s3://b/in.fastq".into(), b"ACGT".to_vec());
    seed_experiment(&state, "exp-live", "genomics", false).await;
    seed_experiment(&state, "exp-old", "genomics", true).await;
    seed_experiment(&state, "exp-elsewhere", "other-project", false).await;
    let app = api::router(state);
    let res = app
        .clone()
        .oneshot(post("/workflows", Some("developers"), &register_body()))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CREATED);
    let body = |experiment: &str| {
        json!({
            "workflow_version_id": "abc:hash:0",
            "params": {"reads": "s3://b/in.fastq", "outdir": "s3://b/out/"},
            "experiment": experiment,
        })
    };

    // Unknown, archived, or of another project: refused before anything is written or launched.
    for (id, says) in [
        ("exp-nope", "unknown experiment"),
        ("exp-old", "archived"),
        ("exp-elsewhere", "project other-project"),
    ] {
        let res = app.clone().oneshot(post("/runs", Some("operators"), &body(id))).await.unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST, "{id}");
        let msg = text_body(res).await;
        assert!(msg.contains("`experiment`") && msg.contains(says), "{msg}");
    }
    assert!(events.events.lock().unwrap().is_empty());
    assert!(cloud.launched.lock().unwrap().is_empty());
    // Nor was S3 touched: no input was existence-checked and nothing (a samplesheet) was written.
    assert!(cloud.object_checks.lock().unwrap().is_empty());
    assert!(cloud.heads.lock().unwrap().is_empty());
    assert_eq!(cloud.objects.lock().unwrap().keys().collect::<Vec<_>>(), ["s3://b/in.fastq"]);

    let res =
        app.clone().oneshot(post("/runs", Some("operators"), &body("exp-live"))).await.unwrap();
    assert_eq!(res.status(), StatusCode::ACCEPTED);
    assert_eq!(recorded_anchor(&events).experiment_id.as_deref(), Some("exp-live"));
    let run = json_body(res).await["run_id"].as_str().unwrap().to_string();
    assert_eq!(events.folded().runs[run.as_str()].experiment_id().as_deref(), Some("exp-live"));
}
