//! The file picker's S3 endpoints: data sources, folder listings, name search, existence checks and
//! browser uploads, with their page and size bounds, against an in-memory S3 double.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{json, Value};

use biffix_backend::api;
use biffix_backend::config::data_sources::parse_sources;
use biffix_backend::config::projects::ProjectContext;
use biffix_backend::testing::{self, send, FakeCloud, TestState};

fn app() -> axum::Router {
    app_with(Arc::new(FakeCloud::new()), true)
}

/// The app over `cloud`, with browser uploads to the project bucket on or off.
fn app_with(cloud: Arc<FakeCloud>, uploads_enabled: bool) -> axum::Router {
    {
        let mut objects = cloud.objects.lock().unwrap();
        for uri in [
            "s3://proj/uploads/a.fastq.gz",
            "s3://proj/biffix/runs/run-1/results/x.txt",
            "s3://proj/biffix/runs/run-1/events.jsonl",
            "s3://proj/biffix/runs/run-1/work/ab/cdef/.command.log",
            "s3://proj/workflow-sources/v/source.tar.gz",
            "s3://proj/top.csv",
            "s3://seq-core/deliveries/2026/batch-12/P01_R1.fastq.gz",
            "s3://seq-core/deliveries/2026/batch-12/P01_R2.fastq.gz",
            "s3://seq-core/deliveries/2026/batch-12/notes.txt",
            "s3://seq-core/deliveries/2026/batch-13/P02_R1.fastq.gz",
            "s3://seq-core/private/secret.txt",
        ] {
            objects.insert(uri.to_string(), b"data".to_vec());
        }
    }
    let data_sources = parse_sources(
        r#"[{"id":"core","name":"Sequencing core","bucket":"seq-core","prefix":"deliveries"},
            {"id":"locked","name":"Locked","bucket":"denied"}]"#,
    )
    .unwrap();
    api::router(
        TestState::new()
            .with_cloud(cloud)
            .with_projects(testing::one_project(ProjectContext {
                data_sources,
                uploads: uploads_enabled,
                ..testing::project("demo", "proj")
            }))
            .build(),
    )
}

async fn get(uri: &str) -> (StatusCode, Value) {
    send(app(), Request::get(uri).body(Body::empty()).unwrap()).await
}

async fn post(uri: &str, body: Value) -> (StatusCode, Value) {
    send(app(), json_req("POST", uri, body)).await
}

fn names(v: &Value, key: &str) -> Vec<String> {
    v[key].as_array().unwrap().iter().map(|x| x["name"].as_str().unwrap().to_string()).collect()
}

#[tokio::test]
async fn lists_the_project_bucket_first_then_the_registered_sources() {
    let (status, v) = get("/data-sources").await;
    assert_eq!(status, StatusCode::OK);
    let ids: Vec<&str> = v.as_array().unwrap().iter().map(|s| s["id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["project", "core", "locked"]);
    assert_eq!(v[0]["writable"], json!(true));
    assert_eq!(v[1]["uri"], json!("s3://seq-core/deliveries/"));
    assert_eq!(v[1]["writable"], json!(false));
}

#[tokio::test]
async fn the_project_root_hides_biffix_prefixes() {
    let (status, v) = post("/data/list", json!({"source": "project"})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(names(&v, "folders"), ["uploads"]);
    assert_eq!(names(&v, "files"), ["top.csv"]);
    let (status, _) =
        post("/data/list", json!({"source": "project", "path": "workflow-sources/"})).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = post("/data/list", json!({"source": "project", "path": "biffix/"})).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_run_s_folder_can_be_opened_though_the_root_hides_biffix() {
    // The run page links a run's results into the Data page: its folder lists and its files open.
    let (status, v) =
        post("/data/list", json!({"source": "project", "path": "biffix/runs/run-1/results/"}))
            .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(names(&v, "files"), ["x.txt"]);
    assert_eq!(v["files"][0]["uri"], json!("s3://proj/biffix/runs/run-1/results/x.txt"));
    let (status, v) =
        post("/data/list", json!({"source": "project", "path": "biffix/runs/"})).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(names(&v, "folders"), ["run-1"]);
    let (status, _) = post(
        "/data/download-url",
        json!({"source": "project", "path": "biffix/runs/run-1/results/x.txt"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    // The rest of biffix's namespace stays hidden.
    let (status, _) =
        post("/data/list", json!({"source": "project", "path": "biffix/cache/"})).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_run_s_whole_folder_is_browsable_not_only_its_outputs() {
    // Deliberate (see `DataSource::hides`): the same `ViewRuns` holder who lists and downloads a
    // run's outputs may open its work directories and its coordinator events, which an Expert
    // debugging the run needs.
    let (status, v) =
        post("/data/list", json!({"source": "project", "path": "biffix/runs/run-1/"})).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(names(&v, "folders"), ["results", "work"]);
    assert_eq!(names(&v, "files"), ["events.jsonl"]);
    let (status, v) =
        post("/data/list", json!({"source": "project", "path": "biffix/runs/run-1/work/ab/cdef/"}))
            .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(names(&v, "files"), [".command.log"]);
    for path in ["biffix/runs/run-1/events.jsonl", "biffix/runs/run-1/work/ab/cdef/.command.log"] {
        let (status, v) =
            post("/data/download-url", json!({"source": "project", "path": path})).await;
        assert_eq!(status, StatusCode::OK, "{path}: {v}");
    }
}

#[tokio::test]
async fn the_project_source_says_which_paths_it_hides() {
    // The UI reads the hide rule from here rather than copying it.
    let (status, v) = get("/data-sources").await;
    assert_eq!(status, StatusCode::OK);
    let project = &v[0];
    assert_eq!(project["id"], "project");
    assert_eq!(project["hidden_prefixes"], json!(["biffix/", "workflow-sources/"]));
    assert_eq!(project["browsable_exceptions"], json!(["biffix/runs/"]));
    // A registered source hides nothing, so it carries neither field.
    assert!(v[1].get("hidden_prefixes").is_none(), "{v}");
    assert!(v[1].get("browsable_exceptions").is_none(), "{v}");
}

#[tokio::test]
async fn a_folder_lists_relative_to_the_source_root() {
    let (status, v) = post("/data/list", json!({"source": "core"})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(names(&v, "folders"), ["2026"]);
    assert_eq!(v["folders"][0]["path"], json!("2026/"));

    let (_, v) = post("/data/list", json!({"source": "core", "path": "2026/batch-12"})).await;
    assert_eq!(v["path"], json!("2026/batch-12/"));
    assert_eq!(names(&v, "files"), ["P01_R1.fastq.gz", "P01_R2.fastq.gz", "notes.txt"]);
    assert_eq!(
        v["files"][0]["uri"],
        json!("s3://seq-core/deliveries/2026/batch-12/P01_R1.fastq.gz")
    );
    assert_eq!(v["files"][0]["size"], json!(4));

    let (_, v) = post(
        "/data/list",
        json!({"source": "core", "path": "2026/batch-12/", "starts_with": "P01"}),
    )
    .await;
    assert_eq!(names(&v, "files"), ["P01_R1.fastq.gz", "P01_R2.fastq.gz"]);
}

#[tokio::test]
async fn a_path_cannot_leave_its_source() {
    // `..` is just characters in an S3 key: the listing stays under the source prefix.
    let (status, v) = post("/data/list", json!({"source": "core", "path": "../private/"})).await;
    assert_eq!(status, StatusCode::OK);
    assert!(names(&v, "files").is_empty());
    let (status, _) = post("/data/list", json!({"source": "nope"})).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn an_unreadable_source_names_the_fix() {
    let (status, v) = post("/data/list", json!({"source": "locked"})).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(v.as_str().unwrap().contains("Locked"), "{v}");
}

#[tokio::test]
async fn find_matches_names_at_any_depth() {
    let (status, v) = post("/data/find", json!({"source": "core", "q": "r1"})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(names(&v, "files"), ["P01_R1.fastq.gz", "P02_R1.fastq.gz"]);
    assert_eq!(v["files"][1]["path"], json!("2026/batch-13/P02_R1.fastq.gz"));
    assert_eq!(v["truncated"], json!(false));
    let (_, v) = post("/data/find", json!({"source": "core", "q": "fastq", "limit": 2})).await;
    assert_eq!(v["files"].as_array().unwrap().len(), 2);
    assert_eq!(v["truncated"], json!(true));
    // Exactly `limit` matches, and nothing left: every match is there.
    let (_, v) = post("/data/find", json!({"source": "core", "q": "r1", "limit": 2})).await;
    assert_eq!(names(&v, "files"), ["P01_R1.fastq.gz", "P02_R1.fastq.gz"]);
    assert_eq!(v["truncated"], json!(false));
    // A case-insensitive match of an upper-case fragment.
    let (_, v) = post("/data/find", json!({"source": "core", "q": "NOTES"})).await;
    assert_eq!(names(&v, "files"), ["notes.txt"]);
}

#[tokio::test]
async fn find_at_the_project_root_never_reads_the_hidden_prefixes() {
    let cloud = Arc::new(FakeCloud::new());
    {
        // More biffix-owned keys than a search reads, all sorting before `uploads/`.
        let mut objects = cloud.objects.lock().unwrap();
        for i in 0..25_000 {
            objects.insert(format!("s3://proj/biffix/runs/r/work/{i:05}.a.fastq.gz"), vec![]);
        }
    }
    let app = app_with(cloud, true);
    let (status, v) = send(
        app.clone(),
        json_req("POST", "/data/find", json!({"source": "project", "q": "a.fastq"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(names(&v, "files"), ["a.fastq.gz"]);
    assert_eq!(v["files"][0]["path"], json!("uploads/a.fastq.gz"));
    assert_eq!(v["truncated"], json!(false));
    // Files at the root are found too, and a hidden folder cannot be searched.
    let (_, v) = send(
        app.clone(),
        json_req("POST", "/data/find", json!({"source": "project", "q": ".csv"})),
    )
    .await;
    assert_eq!(names(&v, "files"), ["top.csv"]);
    let (status, _) = send(
        app,
        json_req("POST", "/data/find", json!({"source": "project", "path": "biffix/", "q": "a"})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn head_reports_exists_absent_and_unknown() {
    let body = json!({"uris": [
        "s3://seq-core/deliveries/2026/batch-12/P01_R1.fastq.gz",
        "s3://seq-core/deliveries/2026/missing.fastq.gz",
        "s3://elsewhere/x.fastq.gz",
        // Under the project bucket's hidden prefixes: unknown, as everywhere else in /data.
        "s3://proj/workflow-sources/v/source.tar.gz",
        // A run's results are not hidden.
        "s3://proj/biffix/runs/run-1/results/x.txt",
        "s3://proj/uploads/a.fastq.gz",
    ]});
    let req = Request::post("/data/head")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let (status, v) = send(app(), req).await;
    assert_eq!(status, StatusCode::OK);
    let objects = v["objects"].as_array().unwrap();
    // One answer per URI, in the request's order.
    let uris: Vec<&Value> = objects.iter().map(|o| &o["uri"]).collect();
    assert_eq!(uris, body["uris"].as_array().unwrap().iter().collect::<Vec<_>>(), "{v}");
    assert_eq!(objects[0]["exists"], json!(true));
    assert_eq!(objects[0]["size"], json!(4));
    assert_eq!(objects[1]["exists"], json!(false));
    assert!(objects[2].get("exists").is_none());
    assert!(objects[3].get("exists").is_none(), "{v}");
    assert_eq!(objects[4]["exists"], json!(true), "{v}");
    assert_eq!(objects[5]["exists"], json!(true));
}

// ── uploads ───────────────────────────────────────────────────────────────────

fn json_req(method: &str, uri: &str, body: Value) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

#[tokio::test]
async fn uploads_are_off_unless_the_deploy_enables_them() {
    let cloud = Arc::new(FakeCloud::new());
    let off = app_with(cloud.clone(), false);
    let (_, v) =
        send(off.clone(), Request::get("/data-sources").body(Body::empty()).unwrap()).await;
    assert_eq!(v[0]["writable"], json!(false));
    let (status, _) =
        send(off, json_req("POST", "/data/uploads", json!({"key": "uploads/a.fq.gz", "size": 10})))
            .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn an_upload_goes_part_by_part_resumes_and_completes() {
    const MIB: usize = 1024 * 1024;
    let cloud = Arc::new(FakeCloud::new());
    let app = app_with(cloud.clone(), true);
    let key = "uploads/2026-09-24/P01_R1.fastq.gz";

    // Outside uploads/: refused.
    let (status, v) =
        send(app.clone(), json_req("POST", "/data/uploads", json!({"key": "biffix/x", "size": 1})))
            .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{v}");

    // 20 MiB: three 8 MiB parts (the last shorter).
    let (status, start) = send(
        app.clone(),
        json_req(
            "POST",
            "/data/uploads",
            json!({"key": key, "size": 20 * MIB, "content_type": "application/gzip"}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(start["part_size"], json!(8 * MIB));
    assert_eq!(start["parts"], json!(3));
    assert_eq!(start["uri"], json!(format!("s3://proj/{key}")));
    let id = start["upload_id"].as_str().unwrap().to_string();

    let (status, urls) = send(
        app.clone(),
        json_req(
            "POST",
            &format!("/data/uploads/{id}/parts"),
            json!({"key": key, "part_numbers": [1, 2, 3]}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(urls["urls"].as_array().unwrap().len(), 3);
    let (status, _) = send(
        app.clone(),
        json_req(
            "POST",
            &format!("/data/uploads/{id}/parts"),
            json!({"key": key, "part_numbers": [0]}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // Two parts sent, then a reload: the status says which.
    let e1 = cloud.put_part(&id, 1, vec![b'a'; 8 * MIB]);
    let e2 = cloud.put_part(&id, 2, vec![b'b'; 8 * MIB]);
    let status_of = || json_req("POST", &format!("/data/uploads/{id}/status"), json!({"key": key}));
    let (_, st) = send(app.clone(), status_of()).await;
    let done: Vec<i64> = st["parts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["part_number"].as_i64().unwrap())
        .collect();
    assert_eq!(done, [1, 2]);
    let e3 = cloud.put_part(&id, 3, vec![b'c'; 4 * MIB]);

    // A gap, or a wrong ETag: refused, and the upload stays open to complete properly.
    let complete = format!("/data/uploads/{id}/complete");
    let (status, _) = send(
        app.clone(),
        json_req(
            "POST",
            &complete,
            json!({"key": key, "parts": [{"part_number": 1, "etag": e1}, {"part_number": 3, "etag": e3}]}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (_, gap) = send(
        app.clone(),
        json_req(
            "POST",
            &complete,
            json!({"key": key, "parts": [{"part_number": 1, "etag": e1}, {"part_number": 3, "etag": e3}]}),
        ),
    )
    .await;
    assert!(
        gap.as_str().unwrap().contains("parts must be numbered 1, 2, 3… with none missing"),
        "{gap}"
    );
    let (status, v) = send(
        app.clone(),
        json_req(
            "POST",
            &complete,
            json!({"key": key, "parts": [
                {"part_number": 1, "etag": e1},
                {"part_number": 2, "etag": "not-the-etag"},
                {"part_number": 3, "etag": e3}
            ]}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{v}");
    assert!(v.as_str().unwrap().contains("InvalidPart"), "{v}");

    let (status, done) = send(
        app.clone(),
        json_req(
            "POST",
            &complete,
            json!({"key": key, "parts": [
                {"part_number": 3, "etag": e3},
                {"part_number": 1, "etag": e1},
                {"part_number": 2, "etag": e2}
            ]}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{done}");
    assert_eq!(done["uri"], json!(format!("s3://proj/{key}")));
    let object = cloud.objects.lock().unwrap()[&format!("s3://proj/{key}")].clone();
    assert_eq!(object.len(), 20 * MIB);
    assert_eq!(&object[8 * MIB..8 * MIB + 1], b"b");

    // It is gone as an upload now.
    let (status, _) = send(app.clone(), status_of()).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Its key is taken now: another upload there is refused before a byte is sent.
    let (status, v) =
        send(app.clone(), json_req("POST", "/data/uploads", json!({"key": key, "size": 10}))).await;
    assert_eq!(status, StatusCode::CONFLICT, "{v}");
    assert_eq!(v, json!(format!("an object already exists at {key}; choose another name")));
}

#[tokio::test]
async fn an_upload_never_replaces_an_object_that_reached_its_key_meanwhile() {
    let cloud = Arc::new(FakeCloud::new());
    let app = app_with(cloud.clone(), true);
    let key = "uploads/2026-09-24/race.fastq.gz";
    let uri = format!("s3://proj/{key}");
    let (status, start) =
        send(app.clone(), json_req("POST", "/data/uploads", json!({"key": key, "size": 5}))).await;
    assert_eq!(status, StatusCode::CREATED);
    let id = start["upload_id"].as_str().unwrap().to_string();
    let etag = cloud.put_part(&id, 1, b"mine!".to_vec());
    // Another upload to the same key finished first.
    cloud.objects.lock().unwrap().insert(uri.clone(), b"theirs".to_vec());
    let (status, v) = send(
        app.clone(),
        json_req(
            "POST",
            &format!("/data/uploads/{id}/complete"),
            json!({"key": key, "parts": [{"part_number": 1, "etag": etag}]}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{v}");
    assert!(v.as_str().unwrap().contains("choose another name"), "{v}");
    assert_eq!(cloud.objects.lock().unwrap()[&uri], b"theirs".to_vec());
    // The upload is cancelled, not left to hold its parts.
    assert!(cloud.uploads.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_cancelled_upload_is_aborted() {
    let cloud = Arc::new(FakeCloud::new());
    let app = app_with(cloud.clone(), true);
    let key = "uploads/x.bam";
    let (_, start) =
        send(app.clone(), json_req("POST", "/data/uploads", json!({"key": key, "size": 100})))
            .await;
    let id = start["upload_id"].as_str().unwrap();
    cloud.put_part(id, 1, b"partial".to_vec());
    let abort = || json_req("POST", &format!("/data/uploads/{id}/abort"), json!({"key": key}));
    let (status, _) = send(app.clone(), abort()).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(cloud.uploads.lock().unwrap().is_empty());
    let (status, _) = send(app.clone(), abort()).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // A key other than the upload's reaches nothing.
    let (_, start) =
        send(app.clone(), json_req("POST", "/data/uploads", json!({"key": key, "size": 100})))
            .await;
    let id = start["upload_id"].as_str().unwrap();
    let (status, _) = send(
        app,
        json_req("POST", &format!("/data/uploads/{id}/abort"), json!({"key": "uploads/other.bam"})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

// ── one file: preview, view, download ─────────────────────────────────────────

#[tokio::test]
async fn a_file_previews_views_and_downloads_by_source_and_path() {
    let cloud = Arc::new(FakeCloud::new());
    {
        let mut objects = cloud.objects.lock().unwrap();
        objects.insert(
            "s3://seq-core/deliveries/2026/stats.tsv".into(),
            b"sample\treads\nA\t10\n".to_vec(),
        );
        objects.insert("s3://seq-core/deliveries/2026/plot.png".into(), vec![0x89, b'P']);
        objects.insert("s3://proj/biffix/cache/snakemake/secret.txt".into(), b"no".to_vec());
    }
    let app = app_with(cloud, true);
    let post = |uri: &str, body: Value| send(app.clone(), json_req("POST", uri, body));

    // The listing says how each file can be shown.
    let (_, v) = post("/data/list", json!({"source": "core", "path": "2026/batch-12"})).await;
    let viewers: Vec<(&str, &str)> = v["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| (f["name"].as_str().unwrap(), f["viewer"].as_str().unwrap()))
        .collect();
    assert_eq!(
        viewers,
        [("P01_R1.fastq.gz", "text"), ("P01_R2.fastq.gz", "text"), ("notes.txt", "table"),],
        "{v}"
    );

    let (status, p) =
        post("/data/preview", json!({"source": "core", "path": "2026/stats.tsv"})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(p["kind"], json!("text"));
    assert_eq!(p["text"], json!("sample\treads\nA\t10\n"));

    let (status, u) =
        post("/data/view-url", json!({"source": "core", "path": "2026/plot.png"})).await;
    assert_eq!(status, StatusCode::OK);
    assert!(u["url"]
        .as_str()
        .unwrap()
        .starts_with("https://fake-presigned.example/seq-core/deliveries/2026/plot.png"));
    assert_eq!(u["content_type"], json!("image/png"));
    let url = u["url"].as_str().unwrap();
    assert!(url.contains("type=image/png"), "{url}");
    assert!(url.contains("inline; filename=\"plot.png\""), "{url}");
    // A table has no inline view; it is previewed instead.
    let (status, v) =
        post("/data/view-url", json!({"source": "core", "path": "2026/stats.tsv"})).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(v, json!("stats.tsv cannot be shown in the browser; download it"));

    let (status, d) = post(
        "/data/download-url",
        json!({"source": "core", "path": "2026/batch-12/P01_R1.fastq.gz"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let url = d["url"].as_str().unwrap();
    assert!(url.contains("attachment; filename=\"P01_R1.fastq.gz\""), "{url}");
    assert!(url.contains("type=#"), "a download sets no content type: {url}");
    assert_eq!(d["filename"], json!("P01_R1.fastq.gz"));

    // Missing, hidden, outside the source, a folder, an unknown source.
    for (uri, source, path, want) in [
        ("/data/preview", "core", "2026/nope.txt", StatusCode::NOT_FOUND),
        ("/data/preview", "project", "biffix/cache/snakemake/secret.txt", StatusCode::NOT_FOUND),
        ("/data/download-url", "core", "../private/secret.txt", StatusCode::BAD_REQUEST),
        ("/data/download-url", "core", "2026/", StatusCode::BAD_REQUEST),
        ("/data/download-url", "nope", "a.txt", StatusCode::NOT_FOUND),
    ] {
        let (status, body) = post(uri, json!({"source": source, "path": path})).await;
        assert_eq!(status, want, "{uri} {source}:{path}: {body}");
    }
}

// ── uploads: the key in the body, never the request URL ─────────────────────────────────────

#[tokio::test]
async fn an_upload_s_status_and_cancel_take_its_key_in_the_body() {
    let cloud = Arc::new(FakeCloud::new());
    let app = app_with(cloud.clone(), true);
    // An upload's status and cancel take its key in the body.
    let key = "uploads/2026-09-28/P01_R1.fastq.gz";
    let (_, start) =
        send(app.clone(), json_req("POST", "/data/uploads", json!({"key": key, "size": 100})))
            .await;
    let id = start["upload_id"].as_str().unwrap().to_string();
    cloud.put_part(&id, 1, b"partial".to_vec());
    let status_of = || json_req("POST", &format!("/data/uploads/{id}/status"), json!({"key": key}));
    let (status, st) = send(app.clone(), status_of()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(st["parts"][0]["part_number"], json!(1));
    // A key other than the upload's reaches nothing.
    let (status, _) = send(
        app.clone(),
        json_req("POST", &format!("/data/uploads/{id}/abort"), json!({"key": "uploads/other.bam"})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = send(
        app.clone(),
        json_req("POST", &format!("/data/uploads/{id}/abort"), json!({"key": key})),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(cloud.uploads.lock().unwrap().is_empty());
    let (status, _) = send(app, status_of()).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

// ── paging and bounds ─────────────────────────────────────────────────────────────────────────

/// An app over the default objects plus `n` files `many/f00000.txt`.. under the project bucket.
fn app_with_many(n: usize) -> axum::Router {
    let cloud = Arc::new(FakeCloud::new());
    {
        let mut objects = cloud.objects.lock().unwrap();
        for i in 0..n {
            objects.insert(format!("s3://proj/many/f{i:05}.txt"), b"x".to_vec());
        }
    }
    app_with(cloud, true)
}

#[tokio::test]
async fn a_folder_pages_through_its_cursor_and_refuses_one_it_did_not_issue() {
    let app = app_with_many(1_500);
    let list = |body: Value| send(app.clone(), json_req("POST", "/data/list", body));
    // S3's page (1000 keys), then the rest from the cursor, then no cursor.
    let (status, first) = list(json!({"source": "project", "path": "many/"})).await;
    assert_eq!(status, StatusCode::OK, "{first}");
    let first_names = names(&first, "files");
    assert_eq!(first_names.len(), 1_000);
    let cursor = first["next_cursor"].as_str().expect("a next cursor");
    let (status, second) =
        list(json!({"source": "project", "path": "many/", "cursor": cursor})).await;
    assert_eq!(status, StatusCode::OK, "{second}");
    let second_names = names(&second, "files");
    assert_eq!(second_names.len(), 500);
    assert!(second["next_cursor"].is_null(), "{}", second["next_cursor"]);
    // The two pages are the folder, each file once, in key order.
    let all: Vec<String> = first_names.into_iter().chain(second_names).collect();
    let want: Vec<String> = (0..1_500).map(|i| format!("f{i:05}.txt")).collect();
    assert_eq!(all, want);
    // A cursor S3 did not issue is the caller's error, not a 5xx: S3 refuses it with
    // `InvalidArgument`, which the provider's listing reads as `AwsError::Invalid`
    // (`aws::provider::errors::classify_invalid`), as the fake answers.
    let (status, v) =
        list(json!({"source": "project", "path": "many/", "cursor": "garbage"})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{v}");
    assert!(v.as_str().unwrap().contains("continuation token"), "{v}");
}

#[tokio::test]
async fn find_caps_its_limit() {
    // More matches than the cap (5000), fewer keys than a search reads (20,000).
    let (status, v) = send(
        app_with_many(5_001),
        json_req(
            "POST",
            "/data/find",
            json!({"source": "project", "path": "many/", "q": ".txt", "limit": 100000}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["files"].as_array().unwrap().len(), 5_000);
    assert_eq!(v["truncated"], json!(true));
}

#[tokio::test]
async fn head_takes_at_most_200_uris() {
    let head = |n: usize| {
        let uris: Vec<String> = (0..n).map(|i| format!("s3://elsewhere/{i}")).collect();
        send(app(), json_req("POST", "/data/head", json!({ "uris": uris })))
    };
    let (status, v) = head(200).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["objects"].as_array().unwrap().len(), 200);
    let (status, v) = head(201).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{v}");
    assert!(v.as_str().unwrap().contains("at most 200"), "{v}");
}

#[tokio::test]
async fn an_upload_s_size_and_part_requests_stay_within_s3_s_limits() {
    const TIB: u64 = 1 << 40;
    let app = app();
    let start = |key: &str, size: u64| {
        send(app.clone(), json_req("POST", "/data/uploads", json!({"key": key, "size": size})))
    };
    // 5 TiB is the largest object S3 stores.
    let (status, v) = start("uploads/too-big.bam", 5 * TIB + 1).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{v}");
    assert!(v.as_str().unwrap().contains("5 TiB"), "{v}");
    let key = "uploads/biggest.bam";
    let (status, started) = start(key, 5 * TIB).await;
    assert_eq!(status, StatusCode::CREATED, "{started}");
    let id = started["upload_id"].as_str().unwrap().to_string();

    let parts = |numbers: Vec<i64>| {
        send(
            app.clone(),
            json_req(
                "POST",
                &format!("/data/uploads/{id}/parts"),
                json!({"key": key, "part_numbers": numbers}),
            ),
        )
    };
    // 1..=100 URLs a request, of part numbers 1..=10,000.
    let (status, v) = parts((1..=100).collect()).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["urls"].as_array().unwrap().len(), 100);
    let (status, v) = parts(vec![10_000]).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    for (numbers, why) in [
        ((1..=101).collect::<Vec<_>>(), "1–100 parts"),
        (vec![], "1–100 parts"),
        (vec![10_001], "part number 10001"),
        (vec![0], "part number 0"),
    ] {
        let (status, v) = parts(numbers).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{why}: {v}");
        assert!(v.as_str().unwrap().contains(why), "{why}: {v}");
    }
}

#[tokio::test]
async fn find_reads_a_root_past_its_first_page_and_stops_at_its_scan_budget() {
    // More top-level folders than one listing page holds, the only match in the last one.
    let cloud = Arc::new(FakeCloud::new());
    {
        let mut objects = cloud.objects.lock().unwrap();
        for i in 0..=1000 {
            objects.insert(format!("s3://proj/d{i:04}/x.txt"), b"x".to_vec());
        }
        objects.insert("s3://proj/d1000/match-me.txt".into(), b"x".to_vec());
    }
    let (status, v) = send(
        app_with(cloud.clone(), true),
        json_req("POST", "/data/find", json!({"source": "project", "q": "match-me"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(names(&v, "files"), ["match-me.txt"]);
    assert_eq!(v["truncated"], json!(false));

    // More keys under one folder than a search reads: a needle that matches none of them is
    // truncated (keys were left unread), never "nothing found".
    {
        let mut objects = cloud.objects.lock().unwrap();
        for i in 0..20_001 {
            objects.insert(format!("s3://proj/big/f{i:05}.dat"), b"x".to_vec());
        }
    }
    let (status, v) = send(
        app_with(cloud, true),
        json_req(
            "POST",
            "/data/find",
            json!({"source": "project", "path": "big/", "q": "nothing-like-it"}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert!(names(&v, "files").is_empty());
    assert_eq!(v["truncated"], json!(true));
}
