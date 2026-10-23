//! Reports: a run's declared reports, its MultiQC report and the QC general stats.

use super::*;

#[tokio::test]
async fn reports_list_the_declared_ones_then_multiqc() {
    let registry = MemRegistry::new();
    registry
        .put_version(&testing::version_with_reports(
            "rnaseq",
            "proj",
            Engine::Nextflow,
            serde_json::json!([
                {"name": "Summary", "path": "results/summary.pdf", "description": "One page"},
                {"name": "Not written", "path": "results/missing.html"}
            ]),
        ))
        .await
        .unwrap();
    let store = MemStore::new();
    let mut r1 = testing::run_detail("r1", "succeeded");
    r1.output_prefix = Some("s3://proj/biffix/runs/r1/".into());
    r1.run.workflow = Some("abc:hash".to_string().into());
    store.insert(r1);
    store.insert_outputs(
        "r1",
        vec![
            output_row("r1", "s3://proj/biffix/runs/r1/results/summary.pdf", None, "published"),
            output_row(
                "r1",
                "s3://proj/biffix/runs/r1/results/multiqc/multiqc_report.html",
                Some("MULTIQC"),
                "output",
            ),
            // A MultiQC report left in the work dir is not a result.
            output_row(
                "r1",
                "s3://proj/biffix/runs/r1/work/ab/multiqc_report.html",
                Some("MULTIQC"),
                "intermediate",
            ),
        ],
    );
    let mut state = state_with_one_run();
    state.store = Arc::new(store);
    state.registry = Arc::new(registry);
    state.projects = testing::bucket_project("proj");

    let (status, v) = get_json(api::router(state), "/runs/r1/reports").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let reports = v["reports"].as_array().unwrap();
    let summary: Vec<(&str, &str, &str)> = reports
        .iter()
        .map(|r| {
            (
                r["name"].as_str().unwrap(),
                r["kind"].as_str().unwrap(),
                r["output"]["path"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        summary,
        vec![
            ("Summary", "declared", "results/summary.pdf"),
            ("MultiQC", "multiqc", "results/multiqc/multiqc_report.html"),
        ]
    );
    assert_eq!(reports[0]["description"], "One page");
    assert_eq!(reports[1]["output"]["viewer"], "html");

    let (status, _) = get_json(api::router(state_with_outputs()), "/runs/nope/reports").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn per_sample_reports_find_every_sample_s_file_however_many_other_files_match() {
    let registry = MemRegistry::new();
    registry
        .put_version(&testing::version_with_reports(
            "rnaseq",
            "proj",
            Engine::Snakemake,
            serde_json::json!([{"name": "FastQC", "path": "fastqc/{sample}*.html", "per_sample": true}]),
        ))
        .await
        .unwrap();
    let store = MemStore::new();
    let mut r1 = testing::run_detail("r1", "succeeded");
    r1.output_prefix = Some("s3://proj/biffix/runs/r1/".into());
    r1.run.workflow = Some("abc:hash".to_string().into());
    store.insert(r1);
    // 200 sheet samples (one listed twice), three files each: 600 matches, past the 500 floor.
    let ids: Vec<String> = (0..200).map(|i| format!("S{i:03}")).collect();
    let mut sheet = ids.clone();
    sheet.push("S000".into());
    store
        .sample_specs
        .lock()
        .unwrap()
        .insert("r1".into(), biffix_core::event::SampleSpec { ids: sheet, ..Default::default() });
    let prefix = "s3://proj/biffix/runs/r1/fastqc/";
    let mut outputs: Vec<_> = ids
        .iter()
        .flat_map(|id| {
            ["R1_fastqc", "R2_fastqc", "x_fastqc"]
                .map(|f| output_row("r1", &format!("{prefix}{id}_{f}.html"), None, "published"))
        })
        .collect();
    // Many files the `{sample}`-as-`*` pattern would match, sorting before every sample's.
    outputs.extend(
        (0..1000).map(|i| output_row("r1", &format!("{prefix}A{i:04}.html"), None, "published")),
    );
    store.insert_outputs("r1", outputs);
    let mut state = state_with_one_run();
    state.store = Arc::new(store);
    state.registry = Arc::new(registry);
    state.projects = testing::bucket_project("proj");

    let (status, v) = get_json(api::router(state), "/runs/r1/reports").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let got: Vec<(&str, &str)> = v["reports"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| (r["sample"].as_str().unwrap(), r["output"]["path"].as_str().unwrap()))
        .collect();
    // Each sample's every file (up to the per-sample cap), in path order.
    let want: Vec<(String, String)> = ids
        .iter()
        .flat_map(|id| {
            ["R1", "R2", "x"].map(|f| (id.clone(), format!("fastqc/{id}_{f}_fastqc.html")))
        })
        .collect();
    let want: Vec<(&str, &str)> = want.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
    assert_eq!(got, want);
}

#[tokio::test]
async fn reports_of_a_version_the_registry_no_longer_holds_are_just_multiqc() {
    let store = MemStore::new();
    let mut r1 = testing::run_detail("r1", "succeeded");
    r1.output_prefix = Some("s3://proj/biffix/runs/r1/".into());
    r1.run.workflow = Some("gone:hash".to_string().into());
    store.insert(r1);
    store.insert_outputs(
        "r1",
        vec![output_row(
            "r1",
            "s3://proj/biffix/runs/r1/multiqc_report.html",
            Some("MULTIQC"),
            "output",
        )],
    );
    let mut state = state_with_one_run();
    state.store = Arc::new(store);
    state.projects = testing::bucket_project("proj");
    let (status, v) = get_json(api::router(state), "/runs/r1/reports").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["reports"].as_array().unwrap().len(), 1, "{v}");
    assert_eq!(v["reports"][0]["kind"], "multiqc");
}

#[tokio::test]
async fn qc_general_stats_join_the_multiqc_table_to_its_columns() {
    let store = MemStore::new();
    let mut r1 = testing::run_detail("r1", "succeeded");
    r1.output_prefix = Some("s3://proj/biffix/runs/r1/".into());
    store.insert(r1);
    store.insert(testing::run_detail("r2", "succeeded"));
    let report = "s3://proj/biffix/runs/r1/results/multiqc/multiqc_report.html";
    store.insert_outputs("r1", vec![output_row("r1", report, Some("MULTIQC"), "output")]);
    let cloud = FakeCloud::new();
    let data = "s3://proj/biffix/runs/r1/results/multiqc/multiqc_data/";
    cloud.objects.lock().unwrap().insert(
        format!("{data}multiqc_general_stats.txt"),
        b"Sample\tfastqc-percent_gc\tother-thing\nS1\t43.5\t7\nS2\t41\t\n".to_vec(),
    );
    cloud.objects.lock().unwrap().insert(
        format!("{data}multiqc_data.json"),
        serde_json::json!({
            "report_general_stats_headers": {"fastqc": {"percent_gc": {
                "title": "GC", "suffix": "%", "rid": "fastqc-percent_gc", "format": "{:,.1f}",
                "cond_formatting_rules": {"fail": [{"lt": 42}]}
            }}},
            "report_plot_data": {}
        })
        .to_string()
        .into_bytes(),
    );
    let mut state = state_with_one_run();
    state.store = Arc::new(store);
    state.cloud = Arc::new(cloud);
    state.projects = testing::bucket_project("proj");
    let app = api::router(state);

    let (status, v) = get_json(app.clone(), "/runs/r1/qc/general-stats").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["report_output_id"], biffix_core::reducer::output_id("r1", report));
    assert_eq!(v["columns"][0]["title"], "GC");
    assert_eq!(v["columns"][0]["decimals"], 1);
    assert_eq!(v["columns"][0]["cond_formatting_rules"]["fail"][0]["lt"], 42);
    assert_eq!(v["columns"][1]["title"], "thing", "no metadata: its own name");
    assert_eq!(
        v["samples"][0],
        serde_json::json!({"name": "S1", "values": {"fastqc-percent_gc": 43.5, "other-thing": 7.0}})
    );
    assert_eq!(v["samples"][1]["values"], serde_json::json!({"fastqc-percent_gc": 41.0}));

    // A run without a MultiQC report.
    let (status, _) = get_json(app, "/runs/r2/qc/general-stats").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn general_stats_written_in_another_format_say_so() {
    let store = Arc::new(MemStore::new());
    let mut r1 = testing::run_detail("r1", "succeeded");
    r1.output_prefix = Some("s3://proj/biffix/runs/r1/".into());
    store.insert(r1);
    let report = "s3://proj/biffix/runs/r1/results/multiqc/multiqc_report.html";
    store.insert_outputs("r1", vec![output_row("r1", report, Some("MULTIQC"), "output")]);
    let data = "s3://proj/biffix/runs/r1/results/multiqc/multiqc_data/";
    let app = |file: Option<&str>| {
        let cloud = FakeCloud::new();
        if let Some(file) = file {
            cloud
                .objects
                .lock()
                .unwrap()
                .insert(format!("{data}{file}"), b"Sample,x\nS1,1\n".to_vec());
        }
        let mut state = state_with_one_run();
        state.store = store.clone();
        state.cloud = Arc::new(cloud);
        state.projects = testing::bucket_project("proj");
        api::router(state)
    };
    let (status, v) =
        get_json(app(Some("multiqc_general_stats.csv")), "/runs/r1/qc/general-stats").await;
    assert_eq!(
        (status, v),
        (
            StatusCode::NOT_FOUND,
            serde_json::json!(
                "MultiQC wrote its data as CSV (its `data_format`); only TSV is read, so the \
                 report's General Statistics cannot be shown"
            )
        )
    );
    // No table in any format: the report has none.
    let (status, v) = get_json(app(None), "/runs/r1/qc/general-stats").await;
    assert_eq!(
        (status, v),
        (StatusCode::NOT_FOUND, serde_json::json!("the MultiQC report has no General Statistics"))
    );
}
