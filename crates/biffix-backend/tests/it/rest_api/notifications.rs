//! Notifications and needs-attention: listing, reading and dismissing.

use super::*;

/// A store holding the caller's (`local`, auth off) runs `ok` (succeeded, at 10 s), `bad`
/// (failed, at 20 s, 3 samples with 1 failed) and `stop` (cancelled, at 30 s), plus `theirs`, a
/// failed run of someone else's, each with the notification the fold gives it.
fn store_with_notifications() -> MemStore {
    use biffix_core::state::RunState;
    let store = MemStore::new();
    for (id, user, state, at, name) in [
        ("ok", "local", RunState::Succeeded, 10, "batch-1"),
        ("bad", "local", RunState::Failed, 20, "batch-2"),
        ("stop", "local", RunState::Cancelled, 30, "batch-3"),
        ("theirs", "other", RunState::Failed, 40, "batch-4"),
    ] {
        let mut d = testing::run_detail(id, state.as_str());
        d.run.submitter = Some(biffix_core::event::user_submitter(user));
        d.run.run_name = Some(name.into());
        d.run.workflow_name = Some("rnaseq".into());
        d.run.submitted_at = Some(at as f64);
        if id == "bad" {
            d.run.samples =
                Some(biffix_api::RunSampleCounts { total: 3, done: 2, failed: 1, declared: true });
        }
        store.insert(d);
        store.insert_notification(id, user, state, at * 1_000_000);
    }
    store
}

#[tokio::test]
async fn notifications_list_the_callers_newest_first_with_their_run_and_unread_counts() {
    let app = app_with(store_with_notifications());
    let (status, v) = get_json(app.clone(), "/me/notifications").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let ids: Vec<&str> =
        v["items"].as_array().unwrap().iter().map(|n| n["id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["stop:cancelled", "bad:failed", "ok:succeeded"]);
    assert_eq!(
        v["items"][1],
        json!({
            "id": "bad:failed", "run_id": "bad", "run_name": "batch-2", "workflow_name": "rnaseq",
            "kind": "failed", "at": 20.0, "read": false, "samples_total": 3, "samples_failed": 1,
            // When it started, for a run without a name of its own to be named by its day.
            "submitted_at": 20.0,
        })
    );
    assert_eq!((v["unread"].as_i64(), v["unread_failed"].as_i64()), (Some(3), Some(1)));

    // The limit is clamped to 1..=100; the counts are over all of the caller's.
    let (_, v) = get_json(app.clone(), "/me/notifications?limit=0").await;
    assert_eq!(v["items"].as_array().unwrap().len(), 1);
    assert_eq!(v["unread"], 3);
    let (status, v) = get_json(app, "/me/notifications?limit=1000").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["items"].as_array().unwrap().len(), 3);
}

#[tokio::test]
async fn reading_notifications_by_id_run_or_all_and_others_ids_are_ignored() {
    let app = app_with(store_with_notifications());
    let read = |body: serde_json::Value| {
        let app = app.clone();
        async move { call(app, Method::POST, "/me/notifications/read", Some(body)).await }
    };
    // Someone else's id (and an unknown one) marks nothing and says nothing about it.
    let (status, v) = read(json!({"ids": ["theirs:failed", "nope"]})).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v, json!({"unread": 3, "unread_failed": 1}));
    let (_, v) = read(json!({"ids": ["ok:succeeded"]})).await;
    assert_eq!(v, json!({"unread": 2, "unread_failed": 1}));
    // Again: idempotent.
    let (_, v) = read(json!({"ids": ["ok:succeeded"]})).await;
    assert_eq!(v, json!({"unread": 2, "unread_failed": 1}));
    let (_, v) = read(json!({"run_ids": ["bad"]})).await;
    assert_eq!(v, json!({"unread": 1, "unread_failed": 0}));
    let (_, list) = get_json(app.clone(), "/me/notifications").await;
    let read_flags: Vec<bool> =
        list["items"].as_array().unwrap().iter().map(|n| n["read"].as_bool().unwrap()).collect();
    assert_eq!(read_flags, [false, true, true]);
    let (_, v) = read(json!({"all": true})).await;
    assert_eq!(v, json!({"unread": 0, "unread_failed": 0}));
}

#[tokio::test]
async fn a_read_must_select_exactly_one_way() {
    let app = app_with(store_with_notifications());
    for body in [
        json!({}),
        json!({"all": false}),
        json!({"ids": ["a"], "run_ids": ["b"]}),
        json!({"ids": ["a"], "all": true}),
    ] {
        let (status, v) =
            call(app.clone(), Method::POST, "/me/notifications/read", Some(body.clone())).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}: {v}");
    }
}

#[tokio::test]
async fn the_notification_limit_is_capped_at_100() {
    let store = MemStore::new();
    for i in 0..105 {
        let id = format!("r{i:03}");
        store.insert(testing::run_detail(&id, "succeeded"));
        store.insert_notification(&id, "local", biffix_core::state::RunState::Succeeded, i);
    }
    let app = app_with(store);
    let (status, v) = get_json(app.clone(), "/me/notifications?limit=1000").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["items"].as_array().unwrap().len(), 100);
    assert_eq!(v["unread"], 105);
    // The default is 20.
    let (_, v) = get_json(app, "/me/notifications").await;
    assert_eq!(v["items"].as_array().unwrap().len(), 20);
}

#[tokio::test]
async fn a_read_names_at_most_500_ids() {
    let app = app_with(store_with_notifications());
    let ids = |n: usize| (0..n).map(|i| format!("x{i}:failed")).collect::<Vec<_>>();
    let (status, v) =
        call(app.clone(), Method::POST, "/me/notifications/read", Some(json!({"ids": ids(500)})))
            .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    for field in ["ids", "run_ids"] {
        let (status, v) = call(
            app.clone(),
            Method::POST,
            "/me/notifications/read",
            Some(json!({ field: ids(501) })),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{field}");
        assert!(v.to_string().contains("at most 500"), "{v}");
    }
}

#[tokio::test]
async fn attention_lists_the_callers_undismissed_failed_runs_with_the_true_total() {
    let store = store_with_notifications();
    // More failed runs of the caller's than a page, to show the total is not the page's size.
    for i in 0..3 {
        let id = format!("f{i}");
        let mut d = testing::run_detail(&id, "failed");
        d.run.submitter = Some(biffix_core::event::user_submitter("local"));
        store.insert(d);
        store.insert_notification(&id, "local", biffix_core::state::RunState::Failed, i);
    }
    let app = app_with(store);
    let ids = |v: &serde_json::Value| {
        let mut ids: Vec<String> = v["runs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["run_id"].as_str().unwrap().to_string())
            .collect();
        ids.sort();
        ids
    };
    let (status, v) = get_json(app.clone(), "/runs?attention=true&limit=2").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["total"], 4, "{v}");
    assert_eq!(v["runs"].as_array().unwrap().len(), 2);
    // Reading a run's notification (opening it) leaves it there; dismissing it takes it out.
    call(
        app.clone(),
        Method::POST,
        "/me/notifications/read",
        Some(json!({"run_ids": ["bad", "f0"]})),
    )
    .await;
    let (_, v) = get_json(app.clone(), "/runs?attention=true").await;
    assert_eq!(v["total"], 4, "{v}");
    let (status, v) = call(
        app.clone(),
        Method::POST,
        "/me/attention/dismiss",
        Some(json!({"run_ids": ["bad", "f0"]})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v, json!({"attention": 2}));
    // The POST form says the same.
    let (status, v) =
        call(app, Method::POST, "/runs/query", Some(json!({"attention": true}))).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(ids(&v), ["f1", "f2"]);
    assert_eq!(v["total"], 2);
}

#[tokio::test]
async fn runs_query_names_runs_by_id() {
    let app = app_with(store_with_notifications());
    let ids = |v: &serde_json::Value| {
        let mut ids: Vec<String> = v["runs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["run_id"].as_str().unwrap().to_string())
            .collect();
        ids.sort();
        ids
    };
    // The runs named (an unknown id matches nothing).
    let (status, v) = call(
        app.clone(),
        Method::POST,
        "/runs/query",
        Some(json!({"ids": ["theirs", "bad", "nope", " "]})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(ids(&v), ["bad", "theirs"]);
    assert_eq!(v["total"], 2);
    // An empty list names no runs in particular: every run.
    let (_, all) = call(app.clone(), Method::POST, "/runs/query", Some(json!({}))).await;
    let (_, empty) = call(app.clone(), Method::POST, "/runs/query", Some(json!({"ids": []}))).await;
    assert_eq!(empty["total"], all["total"]);
    // A list of blank ids still names runs (none), never every run.
    let (status, blank) =
        call(app.clone(), Method::POST, "/runs/query", Some(json!({"ids": [" ", ""]}))).await;
    assert_eq!(status, StatusCode::OK, "{blank}");
    assert_eq!(blank["total"], 0);
    assert_eq!(blank["runs"], json!([]));
    // A page's worth is accepted; more is refused, not cut short.
    let page: Vec<String> = (0..200).map(|i| format!("r{i}")).collect();
    let (status, v) =
        call(app.clone(), Method::POST, "/runs/query", Some(json!({"ids": page}))).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let many: Vec<String> = (0..201).map(|i| format!("r{i}")).collect();
    let (status, _) =
        call(app.clone(), Method::POST, "/runs/query", Some(json!({"ids": many}))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    // `GET /runs` cannot carry a list: `ids` there is refused.
    let (status, _) = get_json(app, "/runs?ids=bad").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

/// The run ids `GET /runs?attention=true` lists, sorted.
async fn attention_ids(app: axum::Router) -> Vec<String> {
    let (status, v) = get_json(app, "/runs?attention=true").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let mut ids: Vec<String> = v["runs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["run_id"].as_str().unwrap().to_string())
        .collect();
    ids.sort();
    ids
}

#[tokio::test]
async fn opening_a_failed_run_reads_it_but_leaves_it_needing_attention_until_dismissed() {
    let app = app_with(store_with_notifications());
    // Opening the run (the run page reads its notifications) and the bell's Mark all read.
    for body in [json!({"run_ids": ["bad"]}), json!({"all": true})] {
        let (status, v) =
            call(app.clone(), Method::POST, "/me/notifications/read", Some(body)).await;
        assert_eq!(status, StatusCode::OK, "{v}");
        let (_, list) = get_json(app.clone(), "/me/notifications").await;
        assert_eq!(list["items"][1]["id"], "bad:failed");
        assert_eq!(list["items"][1]["read"], true, "{list}");
        assert_eq!(attention_ids(app.clone()).await, ["bad"]);
    }
    // Dismissed, it is gone (and stays read); again is a no-op.
    for _ in 0..2 {
        let (status, v) = call(
            app.clone(),
            Method::POST,
            "/me/attention/dismiss",
            Some(json!({"run_ids": ["bad"]})),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{v}");
        assert_eq!(v, json!({"attention": 0}));
    }
    assert!(attention_ids(app.clone()).await.is_empty());
    let (_, list) = get_json(app, "/me/notifications").await;
    assert_eq!(list["items"][1]["read"], true);
}

#[tokio::test]
async fn dismissing_takes_out_only_the_named_runs_and_leaves_them_unread() {
    let store = store_with_notifications();
    for i in 0..3 {
        let id = format!("f{i}");
        let mut d = testing::run_detail(&id, "failed");
        d.run.submitter = Some(biffix_core::event::user_submitter("local"));
        store.insert(d);
        store.insert_notification(&id, "local", biffix_core::state::RunState::Failed, i);
    }
    let app = app_with(store);
    assert_eq!(attention_ids(app.clone()).await, ["bad", "f0", "f1", "f2"]);
    let (_, before) = get_json(app.clone(), "/me/notifications").await;
    // *Dismiss all* names exactly the runs it showed; a run failing after (f2, not named) stays.
    // Another user's run, a succeeded one and an unknown one dismiss nothing.
    let (status, v) = call(
        app.clone(),
        Method::POST,
        "/me/attention/dismiss",
        Some(json!({"run_ids": ["bad", "f0", "f1", "theirs", "ok", "nope", " f1 "]})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v, json!({"attention": 1}));
    assert_eq!(attention_ids(app.clone()).await, ["f2"]);
    // Dismissing is not reading: the bell's unread counts are untouched.
    let (_, list) = get_json(app, "/me/notifications").await;
    assert_eq!(list["unread_failed"], 4, "{list}");
    assert_eq!(
        (&list["unread"], &list["unread_failed"]),
        (&before["unread"], &before["unread_failed"]),
        "{list}"
    );
    assert!(list["items"].as_array().unwrap().iter().all(|n| n["read"] == false));
}

#[tokio::test]
async fn a_dismissal_is_taken_even_when_the_count_after_cannot_be_read() {
    let store = Arc::new(store_with_notifications());
    let state = TestState::new().with_store(store.clone()).build();
    store.fail_list_runs.store(true, std::sync::atomic::Ordering::SeqCst);
    let (status, v) = call(
        api::router(state),
        Method::POST,
        "/me/attention/dismiss",
        Some(json!({"run_ids": ["bad"]})),
    )
    .await;
    // Recorded, so a 200; only the count is missing.
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v, json!({}));
    assert_eq!(
        store
            .notification_dismissals
            .lock()
            .unwrap()
            .get(&("local".to_string(), "bad:failed".to_string())),
        Some(&"dismiss")
    );
}

#[tokio::test]
async fn a_dismissal_names_1_to_500_runs() {
    let app = app_with(store_with_notifications());
    let ids = |n: usize| (0..n).map(|i| format!("x{i}")).collect::<Vec<_>>();
    // Each refusal says which rule refused it.
    for (body, want, says) in [
        (json!({"run_ids": []}), StatusCode::BAD_REQUEST, Some("nothing selected")),
        (json!({"run_ids": ["  "]}), StatusCode::BAD_REQUEST, Some("nothing selected")),
        (json!({"run_ids": ids(501)}), StatusCode::BAD_REQUEST, Some("at most 500")),
        (json!({"run_ids": ids(500)}), StatusCode::OK, None),
    ] {
        let (status, v) =
            call(app.clone(), Method::POST, "/me/attention/dismiss", Some(body)).await;
        assert_eq!(status, want, "{v}");
        if let Some(says) = says {
            assert!(v.to_string().contains(says), "{says}: {v}");
        }
    }
}

#[tokio::test]
async fn one_user_s_dismissal_leaves_another_s_needs_attention_alone() {
    let store = MemStore::new();
    for (run, user) in [("a1", "alice"), ("b1", "bob")] {
        let mut d = testing::run_detail(run, "failed");
        d.run.submitter = Some(biffix_core::event::user_submitter(user));
        store.insert(d);
        store.insert_notification(run, user, biffix_core::state::RunState::Failed, 1);
    }
    let app = authed_app(store);
    let (alice, bob) = (token("alice"), token("bob"));
    // Alice names Bob's run too: only her own is dismissed.
    let (status, v) = testing::call_as(
        app.clone(),
        Method::POST,
        "/me/attention/dismiss",
        Some(json!({"run_ids": ["a1", "b1"]})),
        Some(&alice),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v, json!({"attention": 0}));
    let (_, v) = testing::call_as(app, Method::GET, "/runs?attention=true", None, Some(&bob)).await;
    assert_eq!(v["total"], 1, "alice's dismissal left bob's alone: {v}");
    assert_eq!(v["runs"][0]["run_id"], "b1");
}

#[tokio::test]
async fn each_user_sees_and_reads_only_their_own_notifications() {
    let store = MemStore::new();
    for (run, user) in [("a1", "alice"), ("b1", "bob")] {
        store.insert(testing::run_detail(run, "failed"));
        store.insert_notification(run, user, biffix_core::state::RunState::Failed, 1);
    }
    let app = authed_app(store);
    let (alice, bob) = (token("alice"), token("bob"));
    let (status, v) = testing::call_as(
        app.clone(),
        Method::POST,
        "/me/notifications/read",
        Some(json!({"all": true})),
        Some(&alice),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v, json!({"unread": 0, "unread_failed": 0}));
    let (_, v) = testing::call_as(app, Method::GET, "/me/notifications", None, Some(&bob)).await;
    assert_eq!(v["unread"], 1, "alice's read left bob's alone: {v}");
    // Only bob's: alice's (tied on time) is not among them.
    assert_eq!(v["items"].as_array().unwrap().len(), 1, "{v}");
    assert_eq!(v["items"][0]["id"], "b1:failed");
    assert_eq!(v["items"][0]["read"], false);
}

#[tokio::test]
async fn a_run_s_end_recorded_through_the_sink_notifies_its_submitter() {
    use biffix_core::time::Timestamp;
    let store = Arc::new(MemStore::new());
    let events = Arc::new(testing::MemEventSink::folding_into(store.clone()));
    let run = biffix_core::ids::RunId::new("r1");
    let mut anchor = biffix_core::event::RunSubmitted::new("v1", "sha", "img", json!({}), vec![]);
    anchor.submitter = Some(biffix_core::event::user_submitter("local"));
    use biffix_backend::store::EventSink;
    events
        .record_all(&[
            biffix_core::event::run_submitted(&run, Timestamp::from_epoch_seconds(1.0), anchor),
            biffix_core::event::run_failed(
                &run,
                Timestamp::from_epoch_seconds(2.0),
                biffix_core::event::FAILURE_STAGE_LAUNCH,
                "no capacity",
            ),
        ])
        .await
        .unwrap();
    let app = api::router(TestState::new().with_store(store).with_events(events).build());
    let (_, v) = get_json(app.clone(), "/me/notifications").await;
    assert_eq!(v["items"][0]["id"], "r1:failed", "{v}");
    assert_eq!(v["items"][0]["at"], 2.0);
    assert_eq!(v["unread_failed"], 1);
    let (_, v) = get_json(app, "/runs?attention=true").await;
    assert_eq!(v["total"], 1, "{v}");
}
