//! Per-sample progress: a run's samples grid.

use super::*;

#[tokio::test]
async fn a_run_s_samples_grid_places_tasks_by_sample_in_pipeline_order() {
    let store = MemStore::new();
    let mut d = testing::run_detail("r1", "running");
    d.dag_nodes = vec!["trim".into(), "align".into(), "multiqc".into()];
    d.dag_edges = vec![("trim".into(), "align".into()), ("align".into(), "multiqc".into())];
    for (key, state) in [
        ("trim:sample=A", "succeeded"),
        ("align:sample=A", "running"),
        ("trim:sample=B", "failed"),
        ("multiqc", "queued"),
    ] {
        let mut t = testing::task_view(key, state, None, None);
        t.group = Some(key.split(':').next().unwrap().to_string());
        d.tasks.push(t);
    }
    store.insert(d);
    store.sample_specs.lock().unwrap().insert(
        "r1".into(),
        biffix_core::event::SampleSpec {
            ids: vec!["A".into(), "B".into(), "C".into()],
            ..Default::default()
        },
    );
    let (status, v) = get_json(app_with(store), "/runs/r1/samples").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let steps: Vec<_> = v["steps"].as_array().unwrap().iter().map(|s| s["group"].clone()).collect();
    assert_eq!(steps, [json!("trim"), json!("align"), json!("multiqc")]);
    assert_eq!(v["steps"][2]["scope"], "study");
    let samples: Vec<_> = v["samples"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| (s["id"].as_str().unwrap().to_string(), s["state"].as_str().unwrap().to_string()))
        .collect();
    assert_eq!(
        samples,
        [
            ("A".to_string(), "running".to_string()),
            ("B".to_string(), "failed".to_string()),
            ("C".to_string(), "waiting".to_string())
        ]
    );
    assert_eq!(v["samples"][0]["cells"]["align"]["state"], "running");
    assert_eq!(v["study"]["multiqc"]["state"], "waiting");
    assert_eq!(v["declared"], true);
}

/// A run in `state` with sheet A, B, C: A finished both steps, B only the first, C none.
async fn samples_of_a_run_that_ended(state: &str) -> serde_json::Value {
    let store = MemStore::new();
    let mut d = testing::run_detail("r1", state);
    d.dag_nodes = vec!["trim".into(), "align".into()];
    d.dag_edges = vec![("trim".into(), "align".into())];
    for key in ["trim:sample=A", "align:sample=A", "trim:sample=B"] {
        let mut t = testing::task_view(key, "succeeded", None, None);
        t.group = Some(key.split(':').next().unwrap().to_string());
        d.tasks.push(t);
    }
    store.insert(d);
    store.sample_specs.lock().unwrap().insert(
        "r1".into(),
        biffix_core::event::SampleSpec {
            ids: vec!["A".into(), "B".into(), "C".into()],
            ..Default::default()
        },
    );
    let (status, v) = get_json(app_with(store), "/runs/r1/samples").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let states: Vec<_> =
        v["samples"].as_array().unwrap().iter().map(|s| s["state"].clone()).collect();
    json!(states)
}

#[tokio::test]
async fn a_succeeded_run_s_samples_are_done_and_a_failed_run_s_unfinished_ones_stopped() {
    // Succeeded: every sample that ran is done (B took a shorter path); C never ran.
    assert_eq!(samples_of_a_run_that_ended("succeeded").await, json!(["done", "done", "stopped"]));
    // Failed: A had finished every step; B had not, and the run's end stopped it.
    assert_eq!(samples_of_a_run_that_ended("failed").await, json!(["done", "stopped", "stopped"]));
}

#[tokio::test]
async fn an_unknown_run_has_no_samples_grid() {
    let (status, _) = get_json(app_with(MemStore::new()), "/runs/nope/samples").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
