//! The data browser, uploads and the project list.

use super::*;

#[tokio::test]
async fn the_projects_are_listed_with_the_default_marked() {
    let (status, v) = harness().get("/projects").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(
        v,
        json!([
            {"id": PROJECT_A, "bucket": BUCKET_A, "uploads": true, "viewer_cors": true, "default": true},
            {"id": PROJECT_B, "bucket": BUCKET_B, "uploads": false, "viewer_cors": false, "default": false},
        ])
    );
}

#[tokio::test]
async fn the_data_sources_are_the_named_project_s() {
    let h = harness();
    let uris = |v: &Value| -> Vec<(String, bool)> {
        v.as_array()
            .unwrap()
            .iter()
            .map(|s| (s["uri"].as_str().unwrap().to_string(), s["writable"].as_bool().unwrap()))
            .collect()
    };
    let (status, v) = h.get(&format!("/data-sources?project={PROJECT_B}")).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(
        uris(&v),
        [(format!("s3://{BUCKET_B}/"), false), ("s3://acme-core/deliveries/".to_string(), false)]
    );
    let (status, v) = h.get("/data-sources").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(uris(&v), [(format!("s3://{BUCKET_A}/"), true)]);
    let (status, _) = h.get("/data-sources?project=consensus").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    // A listing of B's own bucket lists B's bucket, not A's.
    {
        let mut objects = h.cloud.objects.lock().unwrap();
        for uri in [
            format!("s3://{BUCKET_A}/a-only.txt"),
            format!("s3://{BUCKET_B}/b-only.txt"),
            "s3://acme-core/deliveries/r.txt".to_string(),
            "s3://acme-core/deliveries/r.html".to_string(),
        ] {
            objects.insert(uri, b"x".to_vec());
        }
    }
    let files = |v: &Value| -> Vec<String> {
        v["files"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["uri"].as_str().unwrap().to_string())
            .collect()
    };
    let (status, v) =
        h.post("/data/list", json!({"source": "project", "project": PROJECT_B})).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(files(&v), [format!("s3://{BUCKET_B}/b-only.txt")]);
    let (_, v) = h.post("/data/list", json!({"source": "project"})).await;
    assert_eq!(files(&v), [format!("s3://{BUCKET_A}/a-only.txt")]);
    // B's data source is not A's.
    let (status, _) = h.post("/data/list", json!({"source": "core"})).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // A search, too.
    let (status, v) =
        h.post("/data/find", json!({"source": "project", "q": "only", "project": PROJECT_B})).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let found: Vec<&str> =
        v["files"].as_array().unwrap().iter().map(|f| f["uri"].as_str().unwrap()).collect();
    assert_eq!(found, [format!("s3://{BUCKET_B}/b-only.txt")]);
    // An existence check sees B's source only when it names B.
    let core = "s3://acme-core/deliveries/r.txt";
    for (project, exists) in [(Some(PROJECT_B), json!(true)), (None, Value::Null)] {
        let mut body = json!({"uris": [core]});
        if let Some(p) = project {
            body["project"] = json!(p);
        }
        let (status, v) = h.post("/data/head", body).await;
        assert_eq!(status, StatusCode::OK, "{v}");
        assert_eq!(v["objects"][0]["exists"], exists, "{project:?}: {v}");
    }
    // A file of B's source: previewed and linked only through B.
    for (route, path) in
        [("/data/preview", "r.txt"), ("/data/view-url", "r.html"), ("/data/download-url", "r.txt")]
    {
        let (status, v) =
            h.post(route, json!({"source": "core", "path": path, "project": PROJECT_B})).await;
        assert_eq!(status, StatusCode::OK, "{route}: {v}");
        let (status, _) = h.post(route, json!({"source": "core", "path": path})).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{route}");
        let (status, _) =
            h.post(route, json!({"source": "core", "path": path, "project": "consensus"})).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{route}");
    }
}

#[tokio::test]
async fn uploads_follow_the_named_project_s_flag_and_bucket() {
    let h = harness();
    let start = |project: Option<&str>| {
        let mut body = json!({"key": "uploads/a.fastq.gz", "size": 10});
        if let Some(p) = project {
            body["project"] = json!(p);
        }
        body
    };
    let (status, v) = h.post("/data/uploads", start(Some(PROJECT_B))).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{v}");
    let (status, v) = h.post("/data/uploads", start(None)).await;
    assert_eq!(status, StatusCode::CREATED, "{v}");
    assert_eq!(v["uri"], format!("s3://{BUCKET_A}/uploads/a.fastq.gz"));
    // A project this deploy does not configure: 400 on every upload route.
    let key = "uploads/a.fastq.gz";
    for (route, body) in [
        ("/data/uploads".to_string(), start(Some("consensus"))),
        (
            "/data/uploads/u/parts".to_string(),
            json!({"key": key, "part_numbers": [1], "project": "consensus"}),
        ),
        ("/data/uploads/u/status".to_string(), json!({"key": key, "project": "consensus"})),
        (
            "/data/uploads/u/complete".to_string(),
            json!({"key": key, "parts": [{"part_number": 1, "etag": "e"}], "project": "consensus"}),
        ),
        ("/data/uploads/u/abort".to_string(), json!({"key": key, "project": "consensus"})),
    ] {
        let (status, v) = h.post(&route, body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{route}: {v}");
    }
}

#[tokio::test]
async fn an_upload_into_a_non_default_project_lands_in_its_bucket() {
    let mut h = harness();
    // B takes uploads on this deploy.
    let contexts = testing::two_projects()
        .iter()
        .cloned()
        .map(|mut p| {
            p.uploads = true;
            p
        })
        .collect();
    h.state.projects = testing::fixed_projects(
        biffix_backend::config::projects::Projects::new(contexts, PROJECT_A).unwrap(),
    );
    let key = "uploads/b.fastq.gz";
    let uri = format!("s3://{BUCKET_B}/{key}");
    let (status, v) =
        h.post("/data/uploads", json!({"key": key, "size": 10, "project": PROJECT_B})).await;
    assert_eq!(status, StatusCode::CREATED, "{v}");
    assert_eq!(v["uri"], uri);
    let id = v["upload_id"].as_str().unwrap().to_string();
    assert_eq!(h.cloud.uploads.lock().unwrap()[&id].0, uri);
    // Its parts are presigned in B's bucket.
    let (status, v) = h
        .post(
            &format!("/data/uploads/{id}/parts"),
            json!({"key": key, "part_numbers": [1], "project": PROJECT_B}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert!(v["urls"][0]["url"].as_str().unwrap().contains(&format!("{BUCKET_B}/{key}")), "{v}");
    // Its status is read in B; naming no project reads the default's bucket, where it is not.
    let (status, v) = h
        .post(&format!("/data/uploads/{id}/status"), json!({"key": key, "project": PROJECT_B}))
        .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let (status, _) = h.post(&format!("/data/uploads/{id}/status"), json!({"key": key})).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = h
        .post(
            &format!("/data/uploads/{id}/complete"),
            json!({"key": key, "parts": [{"part_number": 1, "etag": "e"}]}),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // Cancelled in B.
    let (status, v) = h
        .post(&format!("/data/uploads/{id}/abort"), json!({"key": key, "project": PROJECT_B}))
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{v}");
    assert!(!h.cloud.uploads.lock().unwrap().contains_key(&id));
}
