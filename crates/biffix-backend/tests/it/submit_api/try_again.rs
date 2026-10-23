//! Trying a run again marks its notification read.

use super::*;

/// Register a Nextflow version and submit `submit` (as an operator, sub `u`) against a store
/// holding failed runs `r0` and `r9` of `u`'s, each with an unread `failed` notification; the
/// submit's status and body, the store, and what reached the cloud.
async fn try_again_submit(submit: Value) -> (StatusCode, String, Arc<MemStore>, Arc<FakeCloud>) {
    try_again_submit_after_dismissing(&[], submit).await
}

/// [`try_again_submit`], with `dismissed` dismissed by `u` (`POST /me/attention/dismiss`) first.
async fn try_again_submit_after_dismissing(
    dismissed: &[&str],
    submit: Value,
) -> (StatusCode, String, Arc<MemStore>, Arc<FakeCloud>) {
    let store = Arc::new(MemStore::new());
    for run in ["r0", "r9"] {
        store.insert(resumable_prior(run));
        store.insert_notification(run, "u", biffix_core::state::RunState::Failed, 1);
    }
    let (mut state, _, cloud) = state();
    state.store = store.clone();
    let app = api::router(state);
    setup(&app, "/workflows", "developers", &nextflow_register_body()).await;
    if !dismissed.is_empty() {
        let res = app
            .clone()
            .oneshot(post(
                "/me/attention/dismiss",
                Some("viewers"),
                &json!({ "run_ids": dismissed }),
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK, "dismiss");
    }
    let res = app.oneshot(post("/runs", Some("operators"), &submit)).await.unwrap();
    let status = res.status();
    (status, text_body(res).await, store, cloud)
}

/// The notifications `u` has read.
fn read_by_u(store: &MemStore) -> Vec<String> {
    store
        .notification_reads
        .lock()
        .unwrap()
        .iter()
        .filter(|(user, _)| user == "u")
        .map(|(_, id)| id.clone())
        .collect()
}

/// The notifications `u` has dismissed from *Needs attention*, with how.
fn dismissed_by_u(store: &MemStore) -> Vec<(String, &'static str)> {
    store
        .notification_dismissals
        .lock()
        .unwrap()
        .iter()
        .filter(|((user, _), _)| user == "u")
        .map(|((_, id), via)| (id.clone(), *via))
        .collect()
}

#[tokio::test]
async fn retry_of_marks_the_retried_run_s_notification_read() {
    let mut body = resume_body("r0", json!({"retry_of": "r0"}));
    body.as_object_mut().unwrap().remove("resume_from");
    let (status, text, store, cloud) = try_again_submit(body).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{text}");
    assert_eq!(cloud.launched.lock().unwrap().len(), 1);
    // Only the retried run leaves *Needs attention*: read, and dismissed by the try-again.
    assert_eq!(read_by_u(&store), ["r0:failed"]);
    assert_eq!(dismissed_by_u(&store), [("r0:failed".into(), "try_again")]);
}

#[tokio::test]
async fn a_resume_marks_the_resumed_run_s_notification_read() {
    let (status, text, store, _) = try_again_submit(resume_body("r0", json!({}))).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{text}");
    assert_eq!(read_by_u(&store), ["r0:failed"]);
    assert_eq!(dismissed_by_u(&store), [("r0:failed".into(), "try_again")]);
}

#[tokio::test]
async fn trying_again_a_dismissed_run_keeps_its_first_dismissal() {
    // Dismissed from My work, then retried: the dismissal on record stays the first (`dismiss`),
    // so how it left Needs attention can still be answered.
    let mut body = resume_body("r0", json!({"retry_of": "r0"}));
    body.as_object_mut().unwrap().remove("resume_from");
    let (status, text, store, _) = try_again_submit_after_dismissing(&["r0"], body).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{text}");
    assert_eq!(read_by_u(&store), ["r0:failed"]);
    assert_eq!(dismissed_by_u(&store), [("r0:failed".into(), "dismiss")]);
}

#[tokio::test]
async fn retry_of_an_unknown_run_is_404_and_nothing_is_launched_or_read() {
    let mut body = resume_body("r0", json!({"retry_of": "nope"}));
    body.as_object_mut().unwrap().remove("resume_from");
    let (status, text, store, cloud) = try_again_submit(body).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{text}");
    assert!(text.contains("unknown run nope"), "{text}");
    assert!(cloud.launched.lock().unwrap().is_empty());
    assert!(read_by_u(&store).is_empty());
    assert!(dismissed_by_u(&store).is_empty());
}

#[tokio::test]
async fn a_refused_submit_marks_nothing_read() {
    // A retry whose submit is refused (here: a prefix outside the bucket) leaves the run needing
    // attention: nothing was tried again.
    let mut body =
        resume_body("r0", json!({"retry_of": "r0", "output_prefix": "s3://elsewhere/out/"}));
    body.as_object_mut().unwrap().remove("resume_from");
    let (status, text, store, _) = try_again_submit(body).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{text}");
    assert!(text.contains("within the project bucket"), "the prefix refusal, not another: {text}");
    assert!(read_by_u(&store).is_empty());
    assert!(dismissed_by_u(&store).is_empty());
}

#[tokio::test]
async fn a_resume_that_also_names_retry_of_marks_both_runs_read() {
    let (status, text, store, _) =
        try_again_submit(resume_body("r0", json!({"retry_of": "r9"}))).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{text}");
    assert_eq!(read_by_u(&store), ["r0:failed", "r9:failed"]);
    assert_eq!(
        dismissed_by_u(&store),
        [("r0:failed".into(), "try_again"), ("r9:failed".into(), "try_again")]
    );
}

#[tokio::test]
async fn a_blank_retry_of_is_ignored_and_marks_nothing_read() {
    let mut body = resume_body("r0", json!({"retry_of": "  "}));
    body.as_object_mut().unwrap().remove("resume_from");
    let (status, text, store, cloud) = try_again_submit(body).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{text}");
    assert_eq!(cloud.launched.lock().unwrap().len(), 1);
    assert!(read_by_u(&store).is_empty());
    assert!(dismissed_by_u(&store).is_empty());
}

#[tokio::test]
async fn a_retry_parked_for_its_images_marks_the_retried_run_read() {
    // The retry's submit-time image must still be imported, so it parks as `preparing_images`
    // rather than launching: the retried run is tried again all the same.
    let store = MemStore::new();
    for run in ["r0", "r9"] {
        store.insert(testing::run_detail(run, "failed"));
        store.insert_notification(run, "u", biffix_core::state::RunState::Failed, 1);
    }
    let (state, app, _, _, cloud) = images_app_with(store, register_body()).await;
    let mut body = images_submit(json!([SAMTOOLS]));
    body["retry_of"] = json!("r0");
    let res = app.oneshot(post("/runs", Some("developers"), &body)).await.unwrap();
    assert_eq!(res.status(), StatusCode::ACCEPTED);
    let out = json_body(res).await;
    assert_eq!(out["state"], "preparing_images", "{out}");
    assert!(cloud.launched.lock().unwrap().is_empty());
    let list = state.store.list_notifications("u", 20).await.unwrap();
    let read: Vec<&str> = list.items.iter().filter(|n| n.read).map(|n| n.id.as_str()).collect();
    assert_eq!(read, ["r0:failed"]);
    assert_eq!(list.unread_failed, 1);
}
