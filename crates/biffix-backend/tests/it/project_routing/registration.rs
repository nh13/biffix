//! Registering a version into a project: staged sources and unconfigured projects.

use super::*;

#[tokio::test]
async fn a_source_is_staged_into_the_named_project_s_bucket() {
    let h = harness();
    let upload = |project: &str| json!({"version_id": "abc:hash", "project": project});
    let (status, v) = h.post("/workflows/source-upload", upload(PROJECT_B)).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["uri"], format!("s3://{BUCKET_B}/workflow-sources/abc:hash/source.tar.gz"));
    let (status, v) = h.post("/workflows/source-upload", upload(PROJECT_A)).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert!(v["uri"].as_str().unwrap().starts_with(&format!("s3://{BUCKET_A}/")));
    let (status, v) = h.post("/workflows/source-upload", upload("consensus")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{v}");
    assert_eq!(
        h.cloud
            .source_puts
            .lock()
            .unwrap()
            .iter()
            .map(|(bucket, _)| bucket.as_str())
            .collect::<Vec<_>>(),
        [BUCKET_B, BUCKET_A]
    );
}

#[tokio::test]
async fn a_staged_source_must_be_in_the_version_s_project_bucket() {
    let h = harness();
    let staged = |bucket: &str| {
        let mut body = register_body(PROJECT_B, "consensus");
        body["source"] = json!({
            "kind": "s3",
            "uri": format!("s3://{bucket}/workflow-sources/abc:hash-consensus/source.tar.gz"),
            "sha256": "a".repeat(64),
        });
        body
    };
    // Staged in A's bucket: refused for a B version.
    let (status, v) = h.post("/workflows", staged(BUCKET_A)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{v}");
    assert!(
        v.as_str().unwrap().contains(&format!("must be in the project bucket ({BUCKET_B})")),
        "{v}"
    );
    let (status, v) = h.post("/workflows", staged(BUCKET_B)).await;
    assert!(status.is_success(), "{status} {v}");
}

#[tokio::test]
async fn registering_into_an_unconfigured_project_is_refused_only_with_several() {
    let h = harness();
    let (status, v) = h.post("/workflows", register_body("consensus", "consensus")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{v}");
    assert!(v.as_str().unwrap().contains("is not configured on this deploy"), "{v}");
    // A single-project deploy registers any label into its one project, as before.
    let mut single = harness();
    single.state.projects = testing::bucket_project("proj");
    let (status, v) = single.post("/workflows", register_body("consensus", "consensus")).await;
    assert!(status.is_success(), "{status} {v}");
}

#[tokio::test]
async fn re_registering_with_a_project_moves_a_version_into_it() {
    let h = harness();
    let version = h.register(PROJECT_B, "consensus").await;
    // Registered under a label this deploy does not configure (a project since removed).
    {
        let mut versions = h.registry.versions.lock().unwrap();
        let rec = versions.get_mut(&version).unwrap();
        rec.project = "consensus".into();
        rec.registration_inputs_hash = "hash-in-consensus".into();
    }
    let uri = format!("/workflows/{version}/reregister");
    let (status, v) = h.post(&uri, json!({})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{v}");
    assert!(
        v.as_str()
            .unwrap()
            .contains("is not configured on this deploy (projects: genomics, imported); register it into one of them with --project"),
        "{v}"
    );
    let (status, v) = h.post(&uri, json!({"project": PROJECT_B})).await;
    assert!(status.is_success(), "{status} {v}");
    assert_eq!(v["project"], PROJECT_B);
    assert_ne!(v["id"], version.as_str());
}
