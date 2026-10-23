//! `--resume-from`: the prior run's prefix and session, refusals, and one resume of a run at a time.

use super::*;

#[tokio::test]
async fn resume_from_forces_the_prior_prefix_and_session() {
    let (status, body, cloud, events) = resume_submit(
        Some(resumable_prior("r0")),
        nextflow_register_body(),
        resume_body("r0", json!({})),
        "operators",
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let launched = cloud.launched.lock().unwrap();
    assert_eq!(launched[0].output_prefix.as_deref(), Some("s3://test-bucket/biffix/runs/r0/"));
    assert_eq!(launched[0].engine_session_id.as_deref(), Some("sess-prior"));
    assert!(launched[0].resume);
    // The anchor records the link and the re-used session and prefix.
    let anchor = recorded_anchor(&events);
    assert_eq!(
        (anchor.resumed_from, anchor.engine_session_id, anchor.output_prefix),
        (
            Some("r0".into()),
            Some("sess-prior".into()),
            Some("s3://test-bucket/biffix/runs/r0/".into())
        )
    );
}

#[tokio::test]
async fn resume_from_accepts_an_equal_explicit_prefix_and_refuses_a_prior_without_one() {
    // An explicit prefix equal to the prior's is fine.
    let (status, body, _, _) = resume_submit(
        Some(resumable_prior("r0")),
        nextflow_register_body(),
        resume_body("r0", json!({"output_prefix": "s3://test-bucket/biffix/runs/r0/"})),
        "operators",
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");

    // A prior that recorded no prefix has nowhere to resume in.
    let mut unrecorded = resumable_prior("r0");
    unrecorded.output_prefix = None;
    let (status, body, cloud, _) = resume_submit(
        Some(unrecorded),
        nextflow_register_body(),
        resume_body("r0", json!({})),
        "operators",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(body.contains("has no output prefix to resume in"), "{body}");
    assert!(cloud.launched.lock().unwrap().is_empty());
}

#[tokio::test]
async fn resume_from_an_unknown_run_is_404() {
    let (status, body, cloud, _) =
        resume_submit(None, nextflow_register_body(), resume_body("nope", json!({})), "operators")
            .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body.contains("unknown run nope"), "{body}");
    assert!(cloud.launched.lock().unwrap().is_empty());
}

#[tokio::test]
async fn resume_from_requires_launch_permission() {
    // A viewer can see the prior run but not launch: the route's LaunchWorkflow gate comes first.
    let (status, _, cloud, _) = resume_submit(
        Some(resumable_prior("r0")),
        nextflow_register_body(),
        resume_body("r0", json!({})),
        "viewers",
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(cloud.launched.lock().unwrap().is_empty());
}

#[tokio::test]
async fn resume_from_rejects_a_non_resumable_prior() {
    type Perturb = fn(&mut biffix_backend::store::RunDetailView);
    let cases: [(&str, Perturb, &str); 7] = [
        ("still running", |p| p.run.state = "running".into(), "still running"),
        (
            "snakemake prior",
            |p| p.run.engine = Some("snakemake".into()),
            "run r0 is a Snakemake run, not a Nextflow run",
        ),
        (
            "other workflow",
            |p| p.run.workflow_name = Some("other".into()),
            "run r0 is a run of genomics/other, not genomics/genomics",
        ),
        (
            "other project",
            |p| p.run.project = Some("other".into()),
            "run r0 is a run of other/genomics, not genomics/genomics",
        ),
        ("no session", |p| p.engine_session_id = None, "no recorded Nextflow session"),
        (
            "unknown engine",
            |p| p.run.engine = None,
            "run r0 records no engine, so it is not a Nextflow run",
        ),
        ("work deleted", |p| p.work_deleted_at_micros = Some(1), "can no longer be resumed"),
    ];
    for (name, perturb, expected) in cases {
        let mut prior = resumable_prior("r0");
        perturb(&mut prior);
        let (status, body, cloud, _) = resume_submit(
            Some(prior),
            nextflow_register_body(),
            resume_body("r0", json!({})),
            "operators",
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{name}: {body}");
        assert!(body.contains(expected), "{name}: {body}");
        assert!(cloud.launched.lock().unwrap().is_empty(), "{name}");
        // A resume is resolved before any S3 call: an invalid one is a cheap 400.
        assert!(cloud.object_checks.lock().unwrap().is_empty(), "{name}");
    }
}

#[tokio::test]
async fn resume_from_rejects_an_engine_mismatch_and_a_different_prefix() {
    // A Snakemake version cannot resume a Nextflow run.
    let (status, body, cloud, _) = resume_submit(
        Some(resumable_prior("r0")),
        register_body(),
        resume_body("r0", json!({})),
        "operators",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body.contains("run r0 is a Nextflow run, not a Snakemake run"), "{body}");
    assert!(cloud.object_checks.lock().unwrap().is_empty());

    let (status, body, cloud, _) = resume_submit(
        Some(resumable_prior("r0")),
        nextflow_register_body(),
        resume_body("r0", json!({"output_prefix": "s3://test-bucket/elsewhere/"})),
        "operators",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body.contains("must equal the resumed run's"), "{body}");
    assert!(cloud.object_checks.lock().unwrap().is_empty());
}

/// A finished Snakemake run resumes in its own prefix: no engine session (Snakemake restores the
/// metadata the prior run synced there), and the anchor links the two runs.
#[tokio::test]
async fn a_snakemake_run_resumes_in_the_prior_prefix_without_a_session() {
    let mut prior = resumable_prior("r0");
    prior.run.engine = Some("snakemake".into());
    prior.engine_session_id = None;
    let (status, body, cloud, events) =
        resume_submit(Some(prior), register_body(), resume_body("r0", json!({})), "operators")
            .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    {
        let launched = cloud.launched.lock().unwrap();
        assert!(launched[0].resume);
        assert_eq!(launched[0].engine_session_id, None);
        assert_eq!(launched[0].output_prefix.as_deref(), Some("s3://test-bucket/biffix/runs/r0/"));
    }
    assert_eq!(recorded_anchor(&events).resumed_from, Some("r0".into()));

    // A fresh Snakemake run is not a resume.
    let (status, body, cloud, _) = resume_submit(
        None,
        register_body(),
        json!({"workflow_version_id": "abc:hash:0",
               "params": {"reads": "s3://b/in.fastq", "outdir": "s3://b/out/"}}),
        "operators",
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    assert!(!cloud.launched.lock().unwrap()[0].resume);
}

/// A Snakemake version that opted into the cross-run cache gets the project's cache prefix; one
/// that did not (and any Nextflow version) gets none.
#[tokio::test]
async fn the_snakemake_output_cache_follows_the_version_s_opt_in() {
    let submit = json!({"workflow_version_id": "abc:hash:0",
                        "params": {"reads": "s3://b/in.fastq", "outdir": "s3://b/out/"}});
    let mut cached = register_body();
    cached["form_schema"]["cache"] = json!(true);
    // A Nextflow version that sets the flag still gets none (its cache is its session's).
    let mut nextflow_cached = nextflow_register_body();
    nextflow_cached["form_schema"]["cache"] = json!(true);
    for (case, register, expected) in [
        ("snakemake, opted in", cached, Some("s3://test-bucket/biffix/cache/snakemake/")),
        ("snakemake, not opted in", register_body(), None),
        ("nextflow, opted in", nextflow_cached, None),
    ] {
        let (status, body, cloud, _) =
            resume_submit(None, register, submit.clone(), "operators").await;
        assert_eq!(status, StatusCode::ACCEPTED, "{case}: {body}");
        assert_eq!(cloud.launched.lock().unwrap()[0].output_cache.as_deref(), expected, "{case}");
    }
}

#[tokio::test]
async fn retention_is_recorded_for_nextflow_and_refused_for_snakemake() {
    let submit = json!({
        "workflow_version_id": "abc:hash:0",
        "params": {"reads": "s3://b/in.fastq", "outdir": "s3://b/out/"},
        "retention": "delete_on_success",
    });
    let (status, body, _, events) =
        resume_submit(None, nextflow_register_body(), submit.clone(), "operators").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    assert_eq!(
        recorded_anchor(&events).retention,
        Some(biffix_core::event::Retention::DeleteOnSuccess)
    );

    let (status, body, cloud, _) = resume_submit(None, register_body(), submit, "operators").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(body.contains("only to Nextflow"), "{body}");
    assert!(cloud.launched.lock().unwrap().is_empty());

    // `default` is accepted for any engine; an unknown value is a client error.
    let (status, body, _, _) = resume_submit(
        None,
        register_body(),
        json!({
            "workflow_version_id": "abc:hash:0",
            "params": {"reads": "s3://b/in.fastq", "outdir": "s3://b/out/"},
            "retention": "default",
        }),
        "operators",
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    // Everything else valid, so retention is the only thing wrong.
    let (status, body, cloud, _) = resume_submit(
        None,
        nextflow_register_body(),
        json!({
            "workflow_version_id": "abc:hash:0",
            "params": {"reads": "s3://b/in.fastq", "outdir": "s3://b/out/"},
            "retention": "forever",
        }),
        "operators",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body, "unknown retention `forever` (default, delete_on_success or keep)");
    assert!(cloud.launched.lock().unwrap().is_empty());
}

/// A store and app sharing it, so a test can project runs after submitting.
fn shared_store_app() -> (axum::Router, Arc<MemStore>, Arc<MemEventSink>, Arc<FakeCloud>) {
    let (mut state, events, cloud) = state();
    let store = Arc::new(MemStore::new());
    state.store = store.clone();
    (api::router(state), store, events, cloud)
}

#[tokio::test]
async fn a_second_resume_of_a_run_waits_for_the_first() {
    let (app, store, events, cloud) = shared_store_app();
    store.insert(resumable_prior("r0"));
    let res = app
        .clone()
        .oneshot(post("/workflows", Some("developers"), &nextflow_register_body()))
        .await
        .unwrap();
    assert!(res.status().is_success());
    let res = app
        .clone()
        .oneshot(post("/runs", Some("operators"), &resume_body("r0", json!({}))))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::ACCEPTED);
    let first = json_body(res).await["run_id"].as_str().unwrap().to_string();
    // The first resume is projected as the anchor records it: submitted, in r0's prefix.
    let mut resumer = testing::run_detail(&first, "submitted");
    resumer.output_prefix = recorded_anchor(&events).output_prefix;
    store.insert(resumer);

    let res = app
        .clone()
        .oneshot(post("/runs", Some("operators"), &resume_body("r0", json!({}))))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CONFLICT);
    assert_eq!(
        text_body(res).await,
        format!(
            "run {first} is still using s3://test-bucket/biffix/runs/r0/; wait for it to finish \
             before resuming run r0"
        )
    );
    assert_eq!(cloud.launched.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn a_run_waits_for_its_resumer_before_it_is_resumed_again() {
    // r1 resumed r0 and is still running in r0's prefix.
    let (app, store, _, cloud) = shared_store_app();
    store.insert(resumable_prior("r0"));
    let mut resumer = resumable_prior("r1");
    resumer.run.state = "running".into();
    resumer.output_prefix = Some("s3://test-bucket/biffix/runs/r0/".into());
    store.insert(resumer);
    let res = app
        .clone()
        .oneshot(post("/workflows", Some("developers"), &nextflow_register_body()))
        .await
        .unwrap();
    assert!(res.status().is_success());
    let res = app
        .clone()
        .oneshot(post("/runs", Some("operators"), &resume_body("r0", json!({}))))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CONFLICT);
    assert!(text_body(res).await.contains("run r1 is still using"));
    assert!(cloud.launched.lock().unwrap().is_empty());

    // Once r1 has finished, r0 (or r1) can be resumed.
    let mut finished = resumable_prior("r1");
    finished.output_prefix = Some("s3://test-bucket/biffix/runs/r0/".into());
    store.insert(finished);
    let res = app
        .clone()
        .oneshot(post("/runs", Some("operators"), &resume_body("r0", json!({}))))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::ACCEPTED);
}

#[tokio::test]
async fn a_resume_whose_prefix_another_submit_holds_is_refused() {
    // Another submit is between its check and its record for the same prefix: it holds the claim.
    let store = MemStore::new();
    store.insert(resumable_prior("r0"));
    store.prefix_claims.lock().unwrap().insert("s3://test-bucket/biffix/runs/r0/".to_string());
    let (state, events, cloud) = state_with_store(store);
    let app = api::router(state);
    setup(&app, "/workflows", "developers", &nextflow_register_body()).await;
    let res =
        app.oneshot(post("/runs", Some("operators"), &resume_body("r0", json!({})))).await.unwrap();
    assert_eq!(res.status(), StatusCode::CONFLICT);
    // The claim's refusal, not the active-run one.
    assert_eq!(
        text_body(res).await,
        "another submit into s3://test-bucket/biffix/runs/r0/ is in progress; retry once it has \
         finished"
    );
    assert!(cloud.launched.lock().unwrap().is_empty());
    assert!(events.events.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_resume_carries_the_prior_run_s_finished_samples_onto_its_anchor() {
    let mut prior = resumable_prior("r0");
    for (key, state) in [("align:sample=A", "succeeded"), ("align:sample=B", "failed")] {
        let mut t = testing::task_view(key, state, None, None);
        t.group = Some("align".into());
        prior.tasks.push(t);
    }
    let (status, body, _, events) = resume_submit(
        Some(prior),
        nextflow_register_body(),
        resume_body("r0", json!({})),
        "operators",
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let reused = recorded_anchor(&events).samples.unwrap().reused;
    assert_eq!(
        reused,
        [biffix_core::event::ReusedSample {
            id: "A".into(),
            done: false,
            steps: [("align".to_string(), 1)].into(),
        }]
    );
}

#[tokio::test]
async fn a_fresh_submit_carries_no_reused_samples() {
    let (status, body, _, events) = resume_submit(
        None,
        register_body(),
        json!({"workflow_version_id": "abc:hash:0",
               "params": {"reads": "s3://b/in.fastq", "outdir": "s3://b/out/"}}),
        "operators",
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let anchor = recorded_anchor(&events);
    assert!(anchor.samples.is_none_or(|s| s.reused.is_empty()));
}
