//! Readiness, status and `/auth/config`.

use super::*;

#[tokio::test]
async fn readyz_is_503_while_the_store_is_unreachable() {
    let (status, _) = get_json(app_with(MemStore::new()), "/readyz").await;
    assert_eq!(status, StatusCode::OK);
    let store = MemStore::new();
    store.unhealthy.store(true, Ordering::SeqCst);
    let (status, _) = get_json(app_with(store), "/readyz").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn status_says_when_its_samples_cannot_be_read() {
    let store = MemStore::new();
    store.fail_status.store(true, Ordering::SeqCst);
    let (status, v) = get_json(app_with(store), "/status").await;
    // The page still renders; every sampled component says why it is degraded, and so is the
    // whole.
    assert_eq!(status, StatusCode::OK, "{v}");
    for name in ["event_ingest", "batch_compute", "reconciler", "alarms"] {
        let component =
            v["components"].as_array().unwrap().iter().find(|c| c["name"] == name).unwrap();
        assert_eq!(component["status"], "degraded", "{name}");
        assert_eq!(component["detail"], "status samples unavailable", "{name}");
    }
    assert_eq!(v["status"], "degraded", "{v}");
}

#[tokio::test]
async fn healthz_is_ok() {
    let (status, _) = get_json(app_with_one_run(), "/healthz").await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn status_is_degraded_until_the_probe_first_reports() {
    let (status, v) = get_json(app_with_one_run(), "/status").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    // A `status` tier + per-component tiers. The live components (api, database) are ok; the
    // probe-sampled components are absent for the in-memory double (no probe), which the handler
    // surfaces as "awaiting first status sample" (degraded), so the overall is degraded.
    assert_eq!(v["status"], "degraded");
    let components = v["components"].as_array().unwrap();
    let by = |name: &str| {
        components
            .iter()
            .find(|c| c["name"] == name)
            .unwrap_or_else(|| panic!("missing component {name}"))
    };
    assert_eq!(by("api")["status"], "ok");
    assert_eq!(by("database")["status"], "ok");
    for name in ["event_ingest", "batch_compute", "reconciler", "alarms"] {
        assert_eq!(by(name)["status"], "degraded", "{name}");
        assert_eq!(by(name)["detail"], "awaiting first status sample", "{name}");
    }
}

#[tokio::test]
async fn status_takes_the_api_probe_and_firing_alarms_from_fresh_samples() {
    let sample = |name: &str, status: &str, detail: Option<&str>, age_secs: f64| {
        biffix_backend::store::status::StatusComponentView {
            name: name.into(),
            status: status.into(),
            detail: detail.map(str::to_string),
            sampled_at: Some(biffix_core::clock::epoch_secs() - age_secs),
            next_within_secs: Some(300),
        }
    };
    let status_of = |samples: Vec<biffix_backend::store::status::StatusComponentView>| async move {
        let store = MemStore::new();
        *store.status_samples.lock().unwrap() = samples;
        store.insert(testing::run_detail("r1", "submitted"));
        let (_, v) = get_json(app_with(store), "/status").await;
        v
    };
    let component = |v: &serde_json::Value, name: &str| {
        v["components"].as_array().unwrap().iter().find(|c| c["name"] == name).cloned().unwrap()
    };
    let v = status_of(vec![
        sample("api", "down", Some("API unreachable: timeout"), 60.0),
        sample("alarms", "degraded", Some("1 alarm firing: Events are in the DLQ."), 60.0),
    ])
    .await;
    assert_eq!(component(&v, "api")["status"], "down");
    assert_eq!(component(&v, "api")["detail"], "API unreachable: timeout");
    assert_eq!(component(&v, "alarms")["status"], "degraded");
    assert_eq!(v["status"], "down");

    // A stale API sample does not override the API answering now.
    let v = status_of(vec![sample("api", "down", None, 3600.0)]).await;
    assert_eq!(component(&v, "api")["status"], "ok");
}

#[tokio::test]
async fn status_history_sizes_its_buckets_and_groups_by_component() {
    let store = Arc::new(MemStore::new());
    let row = |bucket: i64, component: &str| StatusBucketView {
        bucket,
        component: component.into(),
        ok: 3,
        degraded: 0,
        down: 0,
    };
    // In the order Postgres returns them (`ORDER BY bucket, component`); the double hands rows
    // back as seeded.
    *store.status_buckets.lock().unwrap() =
        vec![row(0, "alarms"), row(0, "reconciler"), row(960, "reconciler")];
    let mut st = state(MemStore::new(), DeployConfig::default());
    st.store = store.clone();
    let (status, v) = get_json(api::router(st.clone()), "/status/history?window=1d").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(
        (&v["window_secs"], &v["bucket_secs"], &v["expected_per_bucket"]),
        (&json!(86_400), &json!(960), &json!(3))
    );
    // Components in order of first appearance, each with its own buckets in row order.
    let series: Vec<(String, Vec<i64>)> = v["components"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            let buckets = c["buckets"].as_array().unwrap();
            let starts = buckets.iter().map(|b| b["t"].as_i64().unwrap());
            (c["name"].as_str().unwrap().to_string(), starts.collect())
        })
        .collect();
    assert_eq!(
        series,
        [("alarms".to_string(), vec![0]), ("reconciler".to_string(), vec![0, 960])],
        "{v}"
    );
    // No window is the 90-day one; the store is asked for what the response reports.
    let (_, v) = get_json(api::router(st), "/status/history").await;
    assert_eq!(v["window_secs"], 7_776_000);
    assert_eq!(*store.status_history_asked.lock().unwrap(), [(86_400, 960), (7_776_000, 86_400)]);
}

#[tokio::test]
async fn status_history_refuses_an_unknown_window_and_reports_a_store_outage() {
    let (status, v) = get_json(app_with(MemStore::new()), "/status/history?window=1y").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let message = v.as_str().unwrap();
    assert!(message.contains("`1y`") && message.contains("90d"), "{message}");

    let store = MemStore::new();
    store.fail_status.store(true, Ordering::SeqCst);
    let (status, _) = get_json(app_with(store), "/status/history").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "an outage is not an empty history");
}

#[tokio::test]
async fn auth_config_serves_the_composed_client_or_names_what_is_missing() {
    let configured = OAuthClient::from_lookup(|name| {
        Some(match name {
            "BIFFIX_OIDC_AUDIENCE" => "web-client,other".to_string(),
            "BIFFIX_OIDC_ISSUER" => {
                "https://cognito-idp.us-east-1.amazonaws.com/us-east-1_Pool".to_string()
            }
            "BIFFIX_CLI_CLIENT_ID" | "BIFFIX_CLI_REDIRECT_URIS" => return None,
            other => format!("https://example.org/{other}"),
        })
    });
    let deploy = DeployConfig { oauth_client: configured, ..Default::default() };
    let (status, v) = get_json(api::router(state(MemStore::new(), deploy)), "/auth/config").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["client_id"], "web-client");
    assert_eq!(v["user_pool_id"], "us-east-1_Pool");
    assert_eq!(v["scopes"], json!(["openid", "email"]));

    let missing = OAuthClient::from_lookup(|name| {
        (name != "BIFFIX_OAUTH_LOGOUT_URI").then(|| "https://example.org/x".to_string())
    });
    let deploy = DeployConfig { oauth_client: missing, ..Default::default() };
    let (status, v) = get_json(api::router(state(MemStore::new(), deploy)), "/auth/config").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(v.as_str().unwrap().contains("BIFFIX_OAUTH_LOGOUT_URI"), "{v}");
}
