//! My runs and my preferences, for one user and for two signed-in users.

use super::*;

#[tokio::test]
async fn mine_lists_only_the_callers_runs() {
    // Auth is off here, so the caller is `local`.
    let store = MemStore::new();
    for (id, actor) in [("r1", "local"), ("r2", "someone-else"), ("r3", "local")] {
        let mut d = testing::run_detail(id, "running");
        d.run.submitter = Some(biffix_core::event::user_submitter(actor));
        store.insert(d);
    }
    store.insert(testing::run_detail("r4", "running"));
    let app = app_with(store);
    let (status, v) = get_json(app.clone(), "/runs?mine=true").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let mut ids: Vec<_> = v["runs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["run_id"].as_str().unwrap().to_string())
        .collect();
    ids.sort();
    assert_eq!(ids, ["r1", "r3"]);
    assert_eq!(v["total"], 2);
    let (_, v) = get_json(app, "/runs?mine=false").await;
    assert_eq!(v["total"], 4);
}

#[tokio::test]
async fn prefs_default_then_round_trip_and_an_omitted_field_is_kept() {
    let app = app_with(MemStore::new());
    let (status, v) = get_json(app.clone(), "/me/prefs").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v, json!({"pinned_workflows": []}));

    let body = json!({"view": "expert", "pin": " rnaseq "});
    let (status, v) = call(app.clone(), Method::PUT, "/me/prefs", Some(body)).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v, json!({"view": "expert", "pinned_workflows": ["rnaseq"]}));

    // A view-only update keeps the pins; the tour flag stamps a time and can be cleared.
    let body = json!({"view": "lab", "tour_done": true});
    let before = biffix_core::clock::epoch_secs();
    let (_, v) = call(app.clone(), Method::PUT, "/me/prefs", Some(body)).await;
    let after = biffix_core::clock::epoch_secs();
    assert_eq!(v["view"], "lab");
    assert_eq!(v["pinned_workflows"], json!(["rnaseq"]));
    // Stamped with the request's time.
    let done_at = v["tour_done_at"].as_f64().unwrap();
    assert!((before.floor()..=after.ceil()).contains(&done_at), "{before} ≤ {done_at} ≤ {after}");
    let (_, v) =
        call(app.clone(), Method::PUT, "/me/prefs", Some(json!({"tour_done": false}))).await;
    assert!(v.get("tour_done_at").is_none(), "{v}");
    let (_, v) = get_json(app, "/me/prefs").await;
    assert_eq!(v, json!({"view": "lab", "pinned_workflows": ["rnaseq"]}));
}

#[tokio::test]
async fn prefs_refuse_an_unknown_view_and_keep_what_was_stored() {
    let app = app_with(MemStore::new());
    call(app.clone(), Method::PUT, "/me/prefs", Some(json!({"view": "expert"}))).await;
    let (status, v) =
        call(app.clone(), Method::PUT, "/me/prefs", Some(json!({"view": "simple"}))).await;
    // The view is typed: an unknown one is a body the API cannot read.
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{v}");
    let (_, v) = get_json(app, "/me/prefs").await;
    assert_eq!(v["view"], "expert");
}

#[tokio::test]
async fn prefs_refuse_a_bad_pin_naming_it_and_accept_the_limits() {
    let app = app_with(MemStore::new());
    let longest = "é".repeat(512);
    let (status, v) =
        call(app.clone(), Method::PUT, "/me/prefs", Some(json!({"pin": longest}))).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let too_long = "é".repeat(513);
    let (status, v) = call(
        app.clone(),
        Method::PUT,
        "/me/prefs",
        Some(json!({"unpin": too_long, "view": "expert"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{v}");
    assert!(v.to_string().contains("unpin:"), "the error names the field: {v}");
    // The refused update stored nothing.
    let (_, v) = get_json(app, "/me/prefs").await;
    assert_eq!(v["pinned_workflows"], json!(["é".repeat(512)]));
}

#[tokio::test]
async fn prefs_pin_and_unpin_one_key_against_the_stored_list() {
    let app = app_with(MemStore::new());
    let put = |body: serde_json::Value| call(app.clone(), Method::PUT, "/me/prefs", Some(body));
    let (a, b) = (r#"["lab","rnaseq"]"#, r#"["lab","team/wgs"]"#);

    // Two different pins in sequence (two tabs, each sending only its own key) both stay.
    let (status, v) = put(json!({"pin": a})).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let (_, v) = put(json!({"pin": format!("  {b} ")})).await;
    assert_eq!(v["pinned_workflows"], json!([a, b]));
    // Pinning again is a no-op that keeps the order.
    let (_, v) = put(json!({"pin": a})).await;
    assert_eq!(v["pinned_workflows"], json!([a, b]));
    // Unpinning removes it; again, or a key never pinned, is a no-op.
    let (_, v) = put(json!({"unpin": a})).await;
    assert_eq!(v["pinned_workflows"], json!([b]));
    let (status, v) = put(json!({"unpin": a})).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["pinned_workflows"], json!([b]));
    let (_, v) = put(json!({"unpin": "never pinned", "view": "expert"})).await;
    assert_eq!(v["pinned_workflows"], json!([b]));
    assert_eq!(v["view"], "expert");

    // Two ops together are refused and store nothing.
    let (status, v) = put(json!({"pin": a, "unpin": b})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{v}");
    // An op's key is checked.
    let (status, v) = put(json!({"pin": "é".repeat(513)})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{v}");
    assert!(v.to_string().contains("pin:"), "{v}");
    let (_, v) = get_json(app.clone(), "/me/prefs").await;
    assert_eq!(v["pinned_workflows"], json!([b]));

    // The cap holds after the op: the 51st pin is refused, naming the cap; re-pinning one of the
    // fifty is still fine.
    put(json!({"unpin": b})).await;
    for i in 0..50 {
        put(json!({"pin": format!("wf{i}")})).await;
    }
    let (status, v) = put(json!({"pin": "wf50"})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{v}");
    assert!(v.to_string().contains("at most 50"), "{v}");
    let (status, v) = put(json!({"pin": "wf0"})).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let (_, v) = get_json(app, "/me/prefs").await;
    assert_eq!(v["pinned_workflows"].as_array().unwrap().len(), 50);
}

#[tokio::test]
async fn each_user_reads_and_writes_only_their_own_prefs() {
    let app = authed_app(MemStore::new());
    let (alice, bob) = (token("alice"), token("bob"));
    let (status, v) = testing::call_as(
        app.clone(),
        Method::PUT,
        "/me/prefs",
        Some(json!({"view": "expert", "pin": "rnaseq"})),
        Some(&alice),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let (_, v) = testing::call_as(app.clone(), Method::GET, "/me/prefs", None, Some(&bob)).await;
    assert_eq!(v, json!({"pinned_workflows": []}), "bob sees none of alice's");
    testing::call_as(
        app.clone(),
        Method::PUT,
        "/me/prefs",
        Some(json!({"view": "lab"})),
        Some(&bob),
    )
    .await;
    let (_, v) = testing::call_as(app, Method::GET, "/me/prefs", None, Some(&alice)).await;
    assert_eq!(
        v,
        json!({"view": "expert", "pinned_workflows": ["rnaseq"]}),
        "bob's write left alice's alone"
    );
}

#[tokio::test]
async fn mine_lists_only_the_signed_in_user_s_runs() {
    let store = MemStore::new();
    for (id, sub) in [("r1", "alice"), ("r2", "bob"), ("r3", "alice")] {
        let mut d = testing::run_detail(id, "running");
        d.run.submitter = Some(biffix_core::event::user_submitter(sub));
        store.insert(d);
    }
    let app = authed_app(store);
    let ids = |v: serde_json::Value| {
        let mut ids: Vec<String> = v["runs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["run_id"].as_str().unwrap().to_string())
            .collect();
        ids.sort();
        ids
    };
    let (status, v) =
        testing::call_as(app.clone(), Method::GET, "/runs?mine=true", None, Some(&token("alice")))
            .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(ids(v), ["r1", "r3"]);
    let (_, v) =
        testing::call_as(app.clone(), Method::GET, "/runs?mine=true", None, Some(&token("bob")))
            .await;
    assert_eq!(ids(v), ["r2"]);
    let (_, v) =
        testing::call_as(app, Method::GET, "/runs?mine=true", None, Some(&token("carol"))).await;
    assert!(ids(v).is_empty());
}
