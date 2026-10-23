//! Page-size and read caps.

use super::*;

#[tokio::test]
async fn preview_reads_at_most_1_mib() {
    let (state, cloud) = state_with_objects();
    // A text output larger than the cap.
    cloud
        .objects
        .lock()
        .unwrap()
        .insert("s3://proj/runs/sha/summary.tsv".into(), vec![b'a'; 3 << 20]);
    let id = biffix_core::reducer::output_id("r1", "s3://proj/runs/sha/summary.tsv");
    let (status, v) = get_json(api::router(state), &format!("/runs/r1/outputs/{id}/preview")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["bytes_read"], 1 << 20);
    assert_eq!(v["truncated"], true);
}

#[tokio::test]
async fn the_tasks_page_is_clamped_to_1_through_1000() {
    let store = MemStore::new();
    let mut detail = testing::run_detail("r1", "running");
    detail.tasks =
        (0..1_001).map(|i| task_view(&format!("t{i:04}"), "running", None, None)).collect();
    store.insert(detail);
    let state = TestState::new().with_store(Arc::new(store)).build();
    let len = |v: &serde_json::Value| v["tasks"].as_array().unwrap().len();
    let (status, v) = post_json(
        api::router(state.clone()),
        "/runs/r1/tasks/query",
        serde_json::json!({"limit": 5000}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!((len(&v), v["total"].clone()), (1_000, 1_001.into()));
    let (_, v) =
        post_json(api::router(state), "/runs/r1/tasks/query", serde_json::json!({"limit": 0}))
            .await;
    assert_eq!(len(&v), 1);
}

#[tokio::test]
async fn the_outputs_page_is_clamped_to_2000() {
    let store = MemStore::new();
    store.insert(testing::run_detail("r1", "succeeded"));
    store.insert_outputs(
        "r1",
        (0..2_001)
            .map(|i| output_row("r1", &format!("s3://b/o/{i:05}.txt"), None, "output"))
            .collect(),
    );
    let state = state_with_run_detail(testing::run_detail("r1", "succeeded"));
    let state = TestState::from(state).with_store(Arc::new(store)).build();
    let (status, v) = post_json(
        api::router(state),
        "/runs/r1/outputs/query",
        serde_json::json!({"limit": 100000}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["outputs"].as_array().unwrap().len(), 2_000);
    assert_eq!(v["total"], 2_001);
}

#[tokio::test]
async fn the_runs_page_is_clamped_to_200() {
    let store = MemStore::new();
    for i in 0..201 {
        store.insert(testing::run_detail(&format!("r{i:03}"), "running"));
    }
    let state = TestState::new().with_store(Arc::new(store)).build();
    let (status, v) = get_json(api::router(state), "/runs?limit=1000").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["runs"].as_array().unwrap().len(), 200);
    assert_eq!(v["total"], 201);
}
