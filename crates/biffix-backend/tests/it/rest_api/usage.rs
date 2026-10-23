//! Usage totals over a period, and the task list's parameter checks.

use super::*;

fn cost(instance_type: &str, compute_usd: f64, ebs_usd: f64) -> CostRow {
    CostRow {
        batch_job_id: format!("job-{instance_type}"),
        coordinator: false,
        task_key: Some("align:s=1".into()),
        instance_type: instance_type.into(),
        lifecycle: Some("spot".into()),
        hours: 1.0,
        price_per_hour: compute_usd,
        price_source: "spot-history".into(),
        compute_usd,
        ebs_usd,
    }
}

#[tokio::test]
async fn usage_totals_add_compute_and_storage_over_the_half_open_utc_period() {
    // 2026-09-01T00:00:00Z and 2026-10-01T00:00:00Z.
    const FROM: f64 = 1_788_220_800.0;
    const TO: f64 = 1_790_812_800.0;
    let store = MemStore::new();
    for (run_id, submitted_at, costs) in [
        // The period's first microsecond, a day inside it, and its last microsecond: counted.
        ("first", FROM, vec![cost("m7i.large", 1.5, 0.25)]),
        ("mid", 1_789_000_000.0, vec![cost("c7i.xlarge", 2.0, 0.5)]),
        ("last", TO - 0.000_001, vec![cost("m7i.large", 1.0, 0.25)]),
        // The microsecond before it, and the `to` day itself: not counted.
        ("before", FROM - 0.000_001, vec![cost("m7i.large", 100.0, 1.0)]),
        ("after", TO, vec![cost("c7i.xlarge", 100.0, 1.0)]),
    ] {
        let mut run = testing::run_detail(run_id, "succeeded");
        run.run.submitted_at = Some(submitted_at);
        store.insert(run);
        store.costs.lock().unwrap().insert(run_id.into(), costs);
    }
    // The double filters `from <= submitted < to` as the SQL does, so which runs count is what the
    // handler made of the two dates.
    let (status, v) =
        get_json(app_with(store), "/usage?from=2026-09-01&to=2026-10-01&group_by=instance_type")
            .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(
        (&v["from"], &v["to"], &v["group_by"]),
        (&json!("2026-09-01"), &json!("2026-10-01"), &json!("instance_type"))
    );
    // Each row's total is its compute plus storage; largest first.
    assert_eq!(
        v["rows"],
        json!([
            {"key": "m7i.large", "runs": 2, "compute_usd": 2.5, "ebs_usd": 0.5, "total_usd": 3.0},
            {"key": "c7i.xlarge", "runs": 1, "compute_usd": 2.0, "ebs_usd": 0.5, "total_usd": 2.5},
        ]),
        "{v}"
    );
    assert_eq!(
        v["total"],
        json!({"key": "total", "runs": 3, "compute_usd": 4.5, "ebs_usd": 1.0, "total_usd": 5.5}),
        "{v}"
    );
}

#[tokio::test]
async fn usage_defaults_to_this_month_by_month_and_refuses_bad_input() {
    // This month's `[first day, first day of the next)`, as dates.
    let this_month = || {
        let days = biffix_core::clock::epoch_micros().div_euclid(86_400_000_000);
        let (y, m, _) = biffix_core::time::civil_from_days(days);
        let (ny, nm) = if m == 12 { (y + 1, 1) } else { (y, m + 1) };
        (format!("{y:04}-{m:02}-01"), format!("{ny:04}-{nm:02}-01"))
    };
    let before = this_month();
    let (status, v) = get_json(app_with_one_run(), "/usage").await;
    let after = this_month();
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["group_by"], "month");
    let got = (v["from"].as_str().unwrap().to_string(), v["to"].as_str().unwrap().to_string());
    // Either side of a month boundary the request may have straddled.
    assert!(got == before || got == after, "{v}");
    assert_eq!(v["rows"], serde_json::json!([]));
    assert_eq!(v["total"]["runs"], 0);
    let (status, v) =
        get_json(app_with_one_run(), "/usage?from=2026-01-01&to=2026-04-01&group_by=workflow")
            .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!((v["from"].as_str(), v["to"].as_str()), (Some("2026-01-01"), Some("2026-04-01")));
    for bad in
        ["/usage?group_by=team", "/usage?from=2026-02-30", "/usage?from=2026-04-01&to=2026-01-01"]
    {
        let (status, _) = get_json(app_with_one_run(), bad).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}");
    }
}

#[tokio::test]
async fn task_list_checks_the_run_before_its_parameters_and_names_bad_ones() {
    let store = MemStore::new();
    store.insert(testing::run_detail("r1", "running"));
    let app = app_with(store);
    // An unknown run is a 404 whatever the parameters say.
    let (status, _) =
        post_json(app.clone(), "/runs/nope/tasks/query", serde_json::json!({"state": "bogus"}))
            .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    for (query, named, accepted) in [
        (serde_json::json!({"state": "bogus"}), "state `bogus`", "succeeded"),
        (serde_json::json!({"sort": "size"}), "sort `size`", "duration"),
        (serde_json::json!({"order": "up"}), "order `up`", "desc"),
    ] {
        let (status, v) = post_json(app.clone(), "/runs/r1/tasks/query", query.clone()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{query}");
        let message = v.as_str().unwrap();
        assert!(message.contains(named) && message.contains(accepted), "{message}");
    }
    let (status, _) = post_json(
        app,
        "/runs/r1/tasks/query",
        serde_json::json!({"state": "active", "sort": "state", "order": "desc"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}
