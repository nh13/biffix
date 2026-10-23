//! The launch estimate, from the workflow's past runs.

use super::*;

#[tokio::test]
async fn a_launch_estimate_comes_from_the_workflow_s_past_runs() {
    use biffix_backend::runs::estimate::PastRun;
    let mut state = state_with_one_version().await;
    let (_, v) =
        get_json(api::router(state.clone()), "/workflows/abc:hash/estimate?samples=4").await;
    // No history yet: no estimate.
    assert_eq!(v, serde_json::json!({}));

    let store = MemStore::new();
    store.past_runs.lock().unwrap().insert(
        ("genomics".into(), "genomics".into()),
        vec![
            PastRun { usd: 2.0, samples: 2, wall_secs: 3600.0 },
            PastRun { usd: 6.0, samples: 2, wall_secs: 7200.0 },
        ],
    );
    state.store = Arc::new(store);
    let (status, v) =
        get_json(api::router(state.clone()), "/workflows/abc:hash/estimate?samples=4").await;
    assert_eq!(status, StatusCode::OK);
    // $1 and $3 per sample: median $2 × 4 samples.
    assert_eq!(v["estimate"]["usd_median"], 8.0);
    assert_eq!(v["estimate"]["runs"], 2);
    assert_eq!(v["estimate"]["hours_median"], 1.5);
    let (status, _) = get_json(api::router(state), "/workflows/nope/estimate?samples=4").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_typical_run_comes_from_every_successful_run_with_or_without_samples() {
    use biffix_backend::runs::estimate::PastWall;
    let mut state = state_with_one_version().await;
    let store = MemStore::new();
    store.past_walls.lock().unwrap().insert(
        ("genomics".into(), "genomics".into()),
        vec![
            PastWall { wall_secs: 3600.0, usd: Some(2.0) },
            PastWall { wall_secs: 3.0 * 3600.0, usd: None },
        ],
    );
    state.store = Arc::new(store);
    // Without `samples`: the typical run only (no launch estimate without past sample counts).
    let (status, v) = get_json(api::router(state.clone()), "/workflows/abc:hash/estimate").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v, serde_json::json!({"typical": {"hours_median": 2.0, "runs": 2}}));
    // With samples, the same typical run beside it.
    let (_, v) = get_json(api::router(state), "/workflows/abc:hash/estimate?samples=3").await;
    assert_eq!(v["typical"]["runs"], 2);
    assert!(v.get("estimate").is_none());
}
