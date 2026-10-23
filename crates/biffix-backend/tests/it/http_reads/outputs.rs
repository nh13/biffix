//! Outputs (the objects a run registered): listing, paging, downloads, sizes from S3, previews and view URLs.

use super::*;

#[tokio::test]
async fn outputs_list_hides_intermediates_and_shapes_paths() {
    let (status, v) = post_json(
        api::router(state_with_outputs()),
        "/runs/r1/outputs/query",
        serde_json::json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["total"], 6, "the intermediate is hidden by default");
    assert_eq!(v["output_prefix"], "s3://proj/biffix/runs/r1/");
    let outputs = v["outputs"].as_array().unwrap();
    let by_uri = |uri: &str| {
        outputs
            .iter()
            .find(|o| o["s3_uri"] == uri)
            .unwrap_or_else(|| panic!("{uri} listed"))
            .clone()
    };
    let bam = by_uri("s3://proj/biffix/runs/r1/results/a.bam");
    assert_eq!(bam["path"], "results/a.bam");
    assert_eq!(bam["name"], "a.bam");
    assert_eq!(bam["task_key"], "align");
    assert_eq!(bam["servable"], true);
    assert_eq!(by_uri("s3://proj/runs/sha/summary.tsv")["path"], "runs/sha/summary.tsv");
    assert_eq!(by_uri("s3://other/x.txt")["servable"], false);
    assert_eq!(by_uri("s3://proj/workflow-sources/v1/source.tar.gz")["servable"], false);
    assert!(v.get("next_cursor").is_none(), "one page");
}

#[tokio::test]
async fn outputs_list_filters_by_task_role_and_prefix() {
    let app = || api::router(state_with_outputs());
    let (_, v) =
        post_json(app(), "/runs/r1/outputs/query", serde_json::json!({"task": "align"})).await;
    assert_eq!(v["total"], 2, "align's output + log; its intermediate stays hidden");
    let (_, v) =
        post_json(app(), "/runs/r1/outputs/query", serde_json::json!({"roles": ["intermediate"]}))
            .await;
    assert_eq!(v["total"], 1);
    let (_, v) =
        post_json(app(), "/runs/r1/outputs/query", serde_json::json!({"roles": ["output", "log"]}))
            .await;
    assert_eq!(v["total"], 5);
    // `*` is every role, intermediate (and any role this build does not know) included.
    let (_, default) = post_json(app(), "/runs/r1/outputs/query", serde_json::json!({})).await;
    let (_, all) =
        post_json(app(), "/runs/r1/outputs/query", serde_json::json!({"roles": ["*"]})).await;
    assert_eq!(all["total"].as_i64().unwrap(), default["total"].as_i64().unwrap() + 1);
    let (_, v) =
        post_json(app(), "/runs/r1/outputs/query", serde_json::json!({"prefix": "results/"})).await;
    let paths: Vec<_> = v["outputs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["path"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(paths, ["results/a.bam", "results/b.bam"]);
}

#[tokio::test]
async fn outputs_list_pages_with_a_cursor() {
    let app = || api::router(state_with_outputs());
    let (_, first) =
        post_json(app(), "/runs/r1/outputs/query", serde_json::json!({"limit": 4})).await;
    assert_eq!(first["outputs"].as_array().unwrap().len(), 4);
    let cursor = first["next_cursor"].as_str().unwrap().to_string();
    let (_, second) = post_json(
        app(),
        "/runs/r1/outputs/query",
        serde_json::json!({"limit": 4, "after": cursor}),
    )
    .await;
    assert_eq!(second["outputs"].as_array().unwrap().len(), 2);
    assert!(second.get("next_cursor").is_none());
    assert_eq!(second["total"], 6);
}

#[tokio::test]
async fn outputs_list_404_for_unknown_run() {
    let (status, _) = post_json(
        api::router(state_with_outputs()),
        "/runs/nope/outputs/query",
        serde_json::json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn one_output_is_served_by_id_within_its_run_only() {
    let app = || api::router(state_with_outputs());
    let id = biffix_core::reducer::output_id("r1", "s3://proj/biffix/runs/r1/results/a.bam");
    let (status, v) = get_json(app(), &format!("/runs/r1/outputs/{id}")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["id"], id.as_str());
    assert_eq!(v["path"], "results/a.bam");
    assert_eq!(v["servable"], true);
    // An unservable output is still described (the list shows it too), just flagged.
    let foreign = biffix_core::reducer::output_id("r1", "s3://other/x.txt");
    let (status, v) = get_json(app(), &format!("/runs/r1/outputs/{foreign}")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["servable"], false);
    // Another run's output id does not resolve under this run.
    let theirs = biffix_core::reducer::output_id("r2", "s3://proj/biffix/runs/r2/z.txt");
    let (status, _) = get_json(app(), &format!("/runs/r1/outputs/{theirs}")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = get_json(app(), &format!("/runs/nope/outputs/{id}")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn download_presigns_a_servable_output_as_an_attachment() {
    let id = biffix_core::reducer::output_id("r1", "s3://proj/biffix/runs/r1/results/a.bam");
    let (status, v) =
        get_json(api::router(state_with_outputs()), &format!("/runs/r1/outputs/{id}/download"))
            .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["filename"], "a.bam");
    // The whole link the fake echoes: the key, a 600 s expiry, no type, an attachment.
    assert_eq!(
        v["url"],
        "https://fake-presigned.example/proj/biffix/runs/r1/results/a.bam?expires=600&type=\
         #attachment; filename=\"a.bam\""
    );
    let now = biffix_core::clock::epoch_secs();
    let expires_at = v["expires_at"].as_f64().unwrap();
    assert!((expires_at - (now + 600.0)).abs() < 60.0, "{expires_at} vs {now}");
}

#[tokio::test]
async fn download_refuses_unservable_and_foreign_ids() {
    let app = || api::router(state_with_outputs());
    for uri in ["s3://other/x.txt", "s3://proj/workflow-sources/v1/source.tar.gz"] {
        let id = biffix_core::reducer::output_id("r1", uri);
        let (status, _) = get_json(app(), &format!("/runs/r1/outputs/{id}/download")).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{uri}");
    }
    // r2's output id, asked for under r1: not an output of this run.
    let id = biffix_core::reducer::output_id("r2", "s3://proj/biffix/runs/r2/z.txt");
    let (status, _) = get_json(app(), &format!("/runs/r1/outputs/{id}/download")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = get_json(app(), "/runs/r1/outputs/deadbeef/download").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn outputs_list_fills_in_sizes_from_s3_and_caches_them() {
    let (state, cloud) = state_with_objects();
    let (status, v) =
        post_json(api::router(state.clone()), "/runs/r1/outputs/query", serde_json::json!({}))
            .await;
    assert_eq!(status, StatusCode::OK);
    let by_uri = |v: &serde_json::Value, uri: &str| {
        v["outputs"].as_array().unwrap().iter().find(|o| o["s3_uri"] == uri).unwrap().clone()
    };
    let bam = by_uri(&v, "s3://proj/biffix/runs/r1/results/a.bam");
    assert_eq!(bam["size"], 10);
    assert_eq!(bam["exists"], true);
    assert_eq!(bam["etag"], FakeCloud::etag_for(10));
    assert!(bam["last_modified"].as_f64().unwrap() > 0.0);
    assert_eq!(by_uri(&v, "s3://proj/biffix/runs/r1/results/b.bam")["exists"], false);
    // Objects biffix cannot serve are never HEADed, and are not "pending" forever either.
    assert!(by_uri(&v, "s3://other/x.txt").get("exists").is_none());
    let heads = cloud.heads.lock().unwrap().clone();
    assert_eq!(heads.len(), 4, "the four servable outputs: {heads:?}");
    assert!(!heads.iter().any(|u| u.starts_with("s3://other/")));
    assert!(!heads.iter().any(|u| u.contains("workflow-sources/")));
    assert_eq!(v["enrichment_pending"], 0);

    // A second list reads the cache: no new HEADs.
    let (_, again) =
        post_json(api::router(state), "/runs/r1/outputs/query", serde_json::json!({})).await;
    assert_eq!(cloud.heads.lock().unwrap().len(), 4);
    assert_eq!(by_uri(&again, "s3://proj/biffix/runs/r1/results/a.bam")["size"], 10);
}

#[tokio::test]
async fn outputs_list_heads_a_bounded_number_per_request() {
    let store = MemStore::new();
    let mut run = testing::run_detail("big", "succeeded");
    run.output_prefix = Some("s3://proj/biffix/runs/big/".into());
    store.insert(run);
    let uris: Vec<String> =
        (0..150).map(|i| format!("s3://proj/biffix/runs/big/f{i:03}.txt")).collect();
    store
        .insert_outputs("big", uris.iter().map(|u| output_row("big", u, None, "output")).collect());
    let cloud = Arc::new(FakeCloud::new());
    let mut state = state_with_one_run();
    state.store = Arc::new(store);
    state.cloud = cloud.clone();
    state.projects = testing::bucket_project("proj");

    let (_, v) =
        post_json(api::router(state.clone()), "/runs/big/outputs/query", serde_json::json!({}))
            .await;
    assert_eq!(cloud.heads.lock().unwrap().len(), 64);
    assert_eq!(v["enrichment_pending"], 150 - 64);
    let (_, v) =
        post_json(api::router(state.clone()), "/runs/big/outputs/query", serde_json::json!({}))
            .await;
    assert_eq!(v["enrichment_pending"], 150 - 128);
    let (_, v) =
        post_json(api::router(state), "/runs/big/outputs/query", serde_json::json!({})).await;
    assert_eq!(v["enrichment_pending"], 0, "repeat requests converge");
    assert_eq!(cloud.heads.lock().unwrap().len(), 150, "each object HEADed once");
}

#[tokio::test]
async fn outputs_list_flags_an_object_overwritten_since_it_was_produced() {
    let (state, _) = state_with_objects();
    let store = MemStore::new();
    let mut r1 = testing::run_detail("r1", "succeeded");
    r1.output_prefix = Some("s3://proj/biffix/runs/r1/".into());
    store.insert(r1);
    let mut produced =
        output_row("r1", "s3://proj/biffix/runs/r1/results/a.bam", Some("align"), "output");
    produced.etag = Some("etag-from-the-engine".into());
    let mut same = output_row("r1", "s3://proj/runs/sha/summary.tsv", None, "published");
    same.etag = Some(FakeCloud::etag_for(b"sample\treads\nA\t10\n".len()));
    store.insert_outputs("r1", vec![produced, same]);
    let mut state = state;
    state.store = Arc::new(store);

    let (_, v) =
        post_json(api::router(state), "/runs/r1/outputs/query", serde_json::json!({})).await;
    let outputs = v["outputs"].as_array().unwrap();
    let flag = |uri: &str| {
        outputs.iter().find(|o| o["s3_uri"] == uri).unwrap()["modified_since_produced"].clone()
    };
    assert_eq!(flag("s3://proj/biffix/runs/r1/results/a.bam"), true);
    assert_eq!(
        flag("s3://proj/runs/sha/summary.tsv"),
        serde_json::Value::Null,
        "omitted when the etags agree"
    );
}

#[tokio::test]
async fn outputs_list_names_a_viewer_by_file_name() {
    let (_, v) = post_json(
        api::router(state_with_outputs()),
        "/runs/r1/outputs/query",
        serde_json::json!({}),
    )
    .await;
    let viewer = |uri: &str| {
        v["outputs"].as_array().unwrap().iter().find(|o| o["s3_uri"] == uri).unwrap()["viewer"]
            .clone()
    };
    assert_eq!(viewer("s3://proj/runs/sha/summary.tsv"), "table");
    assert_eq!(viewer("s3://proj/biffix/runs/r1/logs/a.log"), "text");
    assert_eq!(viewer("s3://proj/biffix/runs/r1/results/a.bam"), serde_json::Value::Null);
}

#[tokio::test]
async fn preview_returns_the_start_of_a_text_output() {
    let (state, _) = state_with_objects();
    let id = biffix_core::reducer::output_id("r1", "s3://proj/runs/sha/summary.tsv");
    let (status, v) = get_json(api::router(state), &format!("/runs/r1/outputs/{id}/preview")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["kind"], "text");
    assert_eq!(v["text"], "sample\treads\nA\t10\n");
    assert_eq!(v["truncated"], false);
    assert_eq!(v["decompressed"], false);
}

#[tokio::test]
async fn preview_refuses_unservable_foreign_and_missing_objects() {
    let (state, _) = state_with_objects();
    let app = || api::router(state.clone());
    let id = biffix_core::reducer::output_id("r1", "s3://proj/workflow-sources/v1/source.tar.gz");
    let (status, _) = get_json(app(), &format!("/runs/r1/outputs/{id}/preview")).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let id = biffix_core::reducer::output_id("r2", "s3://proj/biffix/runs/r2/z.txt");
    let (status, _) = get_json(app(), &format!("/runs/r1/outputs/{id}/preview")).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "another run's output");
    let id = biffix_core::reducer::output_id("r1", "s3://proj/biffix/runs/r1/logs/a.log");
    let (status, _) = get_json(app(), &format!("/runs/r1/outputs/{id}/preview")).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "registered but gone from S3");
}

#[tokio::test]
async fn view_url_forces_the_content_type_inline() {
    let store = MemStore::new();
    let mut r1 = testing::run_detail("r1", "succeeded");
    r1.output_prefix = Some("s3://proj/biffix/runs/r1/".into());
    store.insert(r1);
    store.insert_outputs(
        "r1",
        vec![
            output_row("r1", "s3://proj/biffix/runs/r1/multiqc_report.html", None, "published"),
            output_row("r1", "s3://proj/biffix/runs/r1/plot.png", None, "published"),
            output_row("r1", "s3://proj/biffix/runs/r1/metrics.tsv", None, "published"),
        ],
    );
    let mut state = state_with_one_run();
    state.store = Arc::new(store);
    state.projects = testing::bucket_project("proj");
    let app = || api::router(state.clone());

    let id = biffix_core::reducer::output_id("r1", "s3://proj/biffix/runs/r1/multiqc_report.html");
    let (status, v) = get_json(app(), &format!("/runs/r1/outputs/{id}/view-url")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["content_type"], "text/html; charset=utf-8");
    assert_eq!(
        v["url"],
        "https://fake-presigned.example/proj/biffix/runs/r1/multiqc_report.html?expires=600\
         &type=text/html; charset=utf-8#inline; filename=\"multiqc_report.html\""
    );

    let id = biffix_core::reducer::output_id("r1", "s3://proj/biffix/runs/r1/plot.png");
    let (_, v) = get_json(app(), &format!("/runs/r1/outputs/{id}/view-url")).await;
    assert_eq!(v["content_type"], "image/png");

    // A table is previewed through the backend, never served inline from S3.
    let id = biffix_core::reducer::output_id("r1", "s3://proj/biffix/runs/r1/metrics.tsv");
    let (status, v) = get_json(app(), &format!("/runs/r1/outputs/{id}/view-url")).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(
        v, "metrics.tsv cannot be shown in the browser; download it",
        "a 409 says why, as /data/view-url does"
    );
}
