//! The Lab's pipeline catalog: listing, hiding and releasing versions.

use super::*;

fn put(uri: &str, group: Option<&str>, body: &Value) -> Request<Body> {
    request("PUT", uri, group, Some(body))
}

#[tokio::test]
async fn the_workflow_list_carries_each_versions_catalog_last_run_and_hidden_flag() {
    let store = MemStore::new();
    for (run, version, at) in [
        ("r1", "abc:hash:0", Some(100.5)),
        ("r2", "abc:hash:0", Some(250.25)),
        ("r3", "abc:hash:0", None),
        ("r4", "def:hash:0", None),
    ] {
        let mut d = testing::run_detail(run, "succeeded");
        d.run.workflow = Some(biffix_core::ids::WorkflowVersionId::new(version));
        d.run.submitted_at = at;
        store.insert(d);
    }
    // rnaseq's past successful runs (any version): its typical run, from the one grouped read.
    store.past_walls.lock().unwrap().insert(
        ("genomics".into(), "rnaseq".into()),
        vec![
            biffix_backend::runs::estimate::PastWall { wall_secs: 3600.0, usd: Some(2.0) },
            biffix_backend::runs::estimate::PastWall { wall_secs: 7200.0, usd: Some(4.0) },
        ],
    );
    let (state, _, _) = state_with_store(store);
    let app = api::router(state);
    let bodies = [
        // A frozen catalog: its own description wins over the form's. Its title is frozen untrimmed
        // here (as an older form may be) and read trimmed.
        catalog_body(
            "rnaseq",
            "abc",
            json!({"description": "The form's.", "title": "  Bulk RNA-seq \n", "catalog": {
                "description": "Bulk RNA-seq.", "tags": ["RNA-seq", "Bulk"], "verified": true}}),
            None,
        ),
        // No catalog: the form's description. A blank title is none.
        catalog_body(
            "team/qc",
            "def",
            json!({"description": "Checks reads.", "title": " \t "}),
            None,
        ),
        // Tags only: the description falls back to the About page's first sentence.
        catalog_body(
            "align",
            "fed",
            json!({"catalog": {"tags": ["DNA"]}}),
            Some("# Align\n\nAligns reads. Fast."),
        ),
        // Nothing declared: a catalog of what its form takes alone.
        catalog_body("bare", "cab", json!({}), None),
    ];
    for body in &bodies {
        let res = app.clone().oneshot(post("/workflows", Some("developers"), body)).await.unwrap();
        assert_eq!(res.status(), StatusCode::CREATED, "{}", text_body(res).await);
    }
    let res = app
        .clone()
        .oneshot(put(
            "/workflows/listing",
            Some("developers"),
            &json!({"project": "genomics", "name": "team/qc", "hidden": true}),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let list = json_body(app.oneshot(get("/workflows", "viewers")).await.unwrap()).await;
    let by_name = |name: &str| {
        list.as_array()
            .unwrap()
            .iter()
            .find(|v| v["name"] == name)
            .unwrap_or_else(|| panic!("{name} in {list}"))
            .clone()
    };
    let rnaseq = by_name("rnaseq");
    assert_eq!(
        rnaseq["catalog"],
        // Its form takes reads: what a run takes, on its card.
        json!({"description": "Bulk RNA-seq.", "tags": ["RNA-seq", "Bulk"], "verified": true, "inputs": ["Reads"]})
    );
    assert_eq!(rnaseq["run_count"], 3);
    assert_eq!(rnaseq["last_run_at"], 250.25);
    assert_eq!(rnaseq["hidden"], false);
    assert_eq!(rnaseq["title"], "Bulk RNA-seq");
    assert_eq!(rnaseq["typical"], json!({"hours_median": 1.5, "usd_median": 3.0, "runs": 2}));

    let qc = by_name("team/qc");
    assert_eq!(
        qc["catalog"],
        json!({"description": "Checks reads.", "tags": [], "verified": false, "inputs": ["Reads"]})
    );
    assert_eq!(qc["run_count"], 1);
    assert!(qc.get("last_run_at").is_none(), "{qc}");
    assert!(qc.get("title").is_none(), "{qc}");
    // No past runs: no typical run.
    assert!(qc.get("typical").is_none(), "{qc}");
    assert_eq!(qc["hidden"], true);

    let align = by_name("align");
    assert_eq!(
        align["catalog"],
        json!({"description": "Aligns reads.", "tags": ["DNA"], "verified": false, "inputs": ["Reads"]})
    );
    assert_eq!(align["run_count"], 0);
    assert!(align.get("last_run_at").is_none(), "{align}");

    let bare = by_name("bare");
    // Nothing declared: only what its form takes.
    assert_eq!(bare["catalog"], json!({"tags": [], "verified": false, "inputs": ["Reads"]}));
    assert_eq!(bare["hidden"], false);
}

#[tokio::test]
async fn hiding_a_workflow_needs_the_register_capability_and_a_registered_version() {
    let (state, _, _) = state();
    let app = api::router(state);
    let hide = json!({"project": "lab", "name": "team/align", "hidden": true});
    let version = |git_sha: &str| {
        let mut body = register_body();
        body["project"] = json!("lab");
        body["name"] = json!("team/align");
        body["git_sha"] = json!(git_sha);
        body
    };

    // Nothing registered under that (project, name) yet.
    let res =
        app.clone().oneshot(put("/workflows/listing", Some("developers"), &hide)).await.unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    assert_eq!(text_body(res).await, "no workflow `team/align` is registered in project `lab`");

    let res =
        app.clone().oneshot(post("/workflows", Some("developers"), &version("abc"))).await.unwrap();
    assert_eq!(res.status(), StatusCode::CREATED);
    assert_eq!(json_body(res).await["hidden"], false);

    // A viewer (and an operator) may not set the listing.
    for group in ["viewers", "operators"] {
        let res = app.clone().oneshot(put("/workflows/listing", Some(group), &hide)).await.unwrap();
        assert_eq!(res.status(), StatusCode::FORBIDDEN, "{group}");
        assert!(text_body(res).await.contains("RegisterWorkflow"));
    }
    let list = json_body(app.clone().oneshot(get("/workflows", "viewers")).await.unwrap()).await;
    assert_eq!(list[0]["hidden"], false);

    let res =
        app.clone().oneshot(put("/workflows/listing", Some("developers"), &hide)).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let entry = json_body(res).await;
    assert_eq!(entry["project"], "lab");
    assert_eq!(entry["name"], "team/align");
    assert_eq!(entry["hidden"], true);
    assert_eq!(entry["updated_by"], "u");
    assert!(entry["updated_at_micros"].as_i64().unwrap() > 0);
    let list = json_body(app.clone().oneshot(get("/workflows", "viewers")).await.unwrap()).await;
    assert_eq!(list[0]["hidden"], true);

    // A new version of a hidden workflow says so in its registration's response.
    let res =
        app.clone().oneshot(post("/workflows", Some("developers"), &version("def"))).await.unwrap();
    assert_eq!(res.status(), StatusCode::CREATED);
    assert_eq!(json_body(res).await["hidden"], true);

    // And shown again.
    let mut show = hide.clone();
    show["hidden"] = json!(false);
    let res = app.clone().oneshot(put("/workflows/listing", Some("admins"), &show)).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let list = json_body(app.oneshot(get("/workflows", "viewers")).await.unwrap()).await;
    assert_eq!(list.as_array().unwrap().len(), 2);
    assert!(list.as_array().unwrap().iter().all(|v| v["hidden"] == false), "{list}");
}

#[tokio::test]
async fn hiding_a_workflow_hides_all_its_versions_and_not_the_same_name_in_another_project() {
    let (state, _, _) = state();
    let app = api::router(state);
    for (project, git_sha) in [("lab", "a1"), ("lab", "a2"), ("core", "b1"), ("core", "b2")] {
        let mut body = register_body();
        body["project"] = json!(project);
        body["name"] = json!("rnaseq");
        body["git_sha"] = json!(git_sha);
        let res = app.clone().oneshot(post("/workflows", Some("developers"), &body)).await.unwrap();
        assert_eq!(res.status(), StatusCode::CREATED, "{}", text_body(res).await);
    }
    let res = app
        .clone()
        .oneshot(put(
            "/workflows/listing",
            Some("developers"),
            &json!({"project": "lab", "name": "rnaseq", "hidden": true}),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let list = json_body(app.oneshot(get("/workflows", "viewers")).await.unwrap()).await;
    let mut seen: Vec<(String, bool)> = list
        .as_array()
        .unwrap()
        .iter()
        .map(|v| (v["project"].as_str().unwrap().to_string(), v["hidden"].as_bool().unwrap()))
        .collect();
    seen.sort();
    assert_eq!(
        seen,
        [
            ("core".to_string(), false),
            ("core".to_string(), false),
            ("lab".to_string(), true),
            ("lab".to_string(), true),
        ]
    );
}

#[tokio::test]
async fn releasing_a_version_needs_the_register_capability_and_is_recorded_with_who_and_when() {
    let (state, _, _) = state();
    let app = api::router(state);
    for git_sha in ["abc", "def"] {
        let mut body = register_body();
        body["git_sha"] = json!(git_sha);
        let res = app.clone().oneshot(post("/workflows", Some("developers"), &body)).await.unwrap();
        assert_eq!(res.status(), StatusCode::CREATED);
        // A version just registered is a developer version.
        assert_eq!(json_body(res).await["release"], json!({"released": false}));
    }
    let release = |released: bool| json!({ "released": released });

    // Not released yet; a viewer may read it but not change it.
    let res = app.clone().oneshot(get("/workflows/abc:hash:0", "viewers")).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let read = json_body(res).await;
    assert_eq!(read["release"]["released"], false);
    assert_eq!(read["can_release"], false);

    // A viewer (and an operator) may not release.
    for group in ["viewers", "operators"] {
        let res = app
            .clone()
            .oneshot(put("/workflows/abc:hash:0/release", Some(group), &release(true)))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::FORBIDDEN, "{group}");
        assert!(text_body(res).await.contains("RegisterWorkflow"));
    }

    // A developer releases it: who and when are recorded.
    let res = app
        .clone()
        .oneshot(put("/workflows/abc:hash:0/release", Some("developers"), &release(true)))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let released = json_body(res).await;
    assert_eq!(released["release"]["released"], true);
    assert_eq!(released["release"]["by"], "u");
    assert!(released["release"]["at_micros"].as_i64().unwrap() > 0);
    assert_eq!(released["history"].as_array().unwrap().len(), 1);
    assert_eq!(released["can_change"], true);
    // Releasing it again changes (and records) nothing.
    let again = json_body(
        app.clone()
            .oneshot(put("/workflows/abc:hash:0/release", Some("developers"), &release(true)))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(again["history"], released["history"]);

    // The list says which version is released, and filters on it.
    let list = json_body(app.clone().oneshot(get("/workflows", "viewers")).await.unwrap()).await;
    let by_id = |list: &Value, id: &str| {
        list.as_array()
            .unwrap()
            .iter()
            .find(|v| v["id"] == id)
            .cloned()
            .unwrap_or_else(|| panic!("{id} in {list}"))
    };
    let abc = by_id(&list, "abc:hash:0");
    assert_eq!(abc["release"]["released"], true);
    assert_eq!(abc["release"]["by"], "u");
    assert_eq!(abc["release"]["at_micros"], released["release"]["at_micros"]);
    let def = by_id(&list, "def:hash:0");
    assert_eq!(def["release"]["released"], false);
    assert!(def["release"].get("at_micros").is_none(), "{def}");
    let ids = |list: Value| -> Vec<String> {
        list.as_array().unwrap().iter().map(|v| v["id"].as_str().unwrap().to_string()).collect()
    };
    let only_released =
        json_body(app.clone().oneshot(get("/workflows?released=true", "viewers")).await.unwrap())
            .await;
    assert_eq!(ids(only_released), ["abc:hash:0"]);
    let only_developer =
        json_body(app.clone().oneshot(get("/workflows?released=false", "viewers")).await.unwrap())
            .await;
    assert_eq!(ids(only_developer), ["def:hash:0"]);
    // So does the version's own page.
    let detail =
        json_body(app.clone().oneshot(get("/workflows/abc:hash:0", "viewers")).await.unwrap())
            .await;
    assert_eq!(detail["release"]["released"], true);
    assert_eq!(detail["release"]["by"], "u");
    assert_eq!(detail["release"]["at_micros"], released["release"]["at_micros"]);
    // A viewer is told they may not release it; a developer that they may.
    assert_eq!(detail["can_release"], false);
    let as_developer =
        json_body(app.clone().oneshot(get("/workflows/abc:hash:0", "developers")).await.unwrap())
            .await;
    assert_eq!(as_developer["can_release"], true);

    // An admin takes the release back: a developer version again, both changes kept.
    let res = app
        .clone()
        .oneshot(put("/workflows/abc:hash:0/release", Some("admins"), &release(false)))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let unreleased = json_body(res).await;
    assert_eq!(unreleased["release"]["released"], false);
    assert!(unreleased["release"].get("by").is_none(), "{unreleased}");
    let history = unreleased["history"].as_array().unwrap();
    assert_eq!(history.len(), 2);
    assert_eq!(history[0]["released"], false);
    assert_eq!(history[0]["by"], "u");
    assert_eq!(history[1]["released"], true);
    let only_released =
        json_body(app.clone().oneshot(get("/workflows?released=true", "viewers")).await.unwrap())
            .await;
    assert_eq!(only_released, json!([]));

    // No such version: a 404 naming it.
    let res = app
        .clone()
        .oneshot(put("/workflows/nope:hash:0/release", Some("developers"), &release(true)))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    assert_eq!(text_body(res).await, "no workflow version `nope:hash:0` is registered");
}

#[tokio::test]
async fn a_release_state_that_cannot_be_read_is_a_503_not_a_developer_version() {
    let (_state, app, registry, _, _) = images_app().await;
    registry.fail_releases.store(true, std::sync::atomic::Ordering::SeqCst);
    // Filtered or not, the list cannot say which versions are released, so it says nothing
    // rather than call every version a developer version.
    for uri in [
        "/workflows",
        "/workflows?released=true",
        "/workflows?released=false",
        "/workflows/abc:hash:0",
    ] {
        let res = app.clone().oneshot(get(uri, "viewers")).await.unwrap();
        assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE, "{uri}");
    }
    // Once it can be read again, so is everything.
    registry.fail_releases.store(false, std::sync::atomic::Ordering::SeqCst);
    let res = app.clone().oneshot(get("/workflows", "viewers")).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    // A `released` that is not a boolean is refused.
    let res = app.oneshot(get("/workflows?released=yes", "viewers")).await.unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}
