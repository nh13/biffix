//! Samplesheets given as rows or as a file, and the sample ids the anchor declares.

use super::*;

#[tokio::test]
async fn rows_for_a_hidden_samplesheet_are_dropped_not_written() {
    let (state, events, cloud) = state();
    let app = api::router(state);
    let mut register = sheet_register_body();
    register["git_sha"] = json!("gatedsheet");
    let fields = register["form_schema"]["fields"].as_array_mut().unwrap();
    fields[0]["required"] = json!(false);
    fields[0]["show_if"] = json!([{"field": "with_sheet", "values": [true]}]);
    fields.push(json!({"name": "with_sheet", "widget": "enum", "required": false,
        "enum_values": ["true", "false"], "default": false,
        "is_output": false, "is_secret": false, "is_path_input": false}));
    setup(&app, "/workflows", "developers", &register).await;
    // The gate is off: the rows are for a param the form hides.
    let body = json!({
        "workflow_version_id": "gatedsheet:hash:0",
        "params": {},
        "tables": {"input": [{"sample": "P01", "fastq_1": "s3://b/P01_R1.fq.gz"}]}
    });
    let res = app.clone().oneshot(post("/runs", Some("operators"), &body)).await.unwrap();
    assert_eq!(res.status(), StatusCode::ACCEPTED);
    assert!(
        !cloud.objects.lock().unwrap().keys().any(|k| k.ends_with("/inputs/input.csv")),
        "no samplesheet is written for a hidden param"
    );
    assert_eq!(cloud.launched.lock().unwrap()[0].params, json!({}));
    let biffix_core::event::RunSubmitted { dropped_inactive, input_tables, .. } =
        &recorded_anchor(&events);
    assert_eq!(dropped_inactive.as_deref(), Some(&["input".to_string()][..]));
    assert!(input_tables.is_none(), "{input_tables:?}");
}

#[tokio::test]
async fn submitted_rows_become_a_samplesheet_under_the_run() {
    let (state, events, cloud) = state();
    let app = api::router(state);
    setup(&app, "/workflows", "developers", &sheet_register_body()).await;
    let body = json!({
        "workflow_version_id": "sheet:hash:0",
        "params": {"input": "s3://elsewhere/ignored.csv"},
        "tables": {"input": [
            {"sample": "P01", "fastq_1": "s3://b/P01_R1.fq.gz", "fastq_2": "s3://b/P01_R2.fq.gz"},
            {"sample": "P02", "fastq_1": "s3://b/P02_R1.fq.gz"}
        ]}
    });
    let res = app.clone().oneshot(post("/runs", Some("operators"), &body)).await.unwrap();
    assert_eq!(res.status(), StatusCode::ACCEPTED);
    let run_id = json_body(res).await["run_id"].as_str().unwrap().to_string();
    let uri = format!("s3://test-bucket/biffix/runs/{run_id}/inputs/input.csv");
    let written = cloud.objects.lock().unwrap().get(&uri).cloned().unwrap();
    assert_eq!(
        String::from_utf8(written).unwrap(),
        "sample,fastq_1,fastq_2\nP01,s3://b/P01_R1.fq.gz,s3://b/P01_R2.fq.gz\nP02,s3://b/P02_R1.fq.gz,\n"
    );
    let biffix_core::event::RunSubmitted { params, input_uris, input_tables, sample_count, .. } =
        &recorded_anchor(&events);
    assert_eq!(params["input"], json!(uri));
    // The rows' files were existence-checked like any input, each once in first-seen order; the
    // sheet param itself (a file biffix writes) and the path it replaced are not.
    assert_eq!(input_uris, &["s3://b/P01_R1.fq.gz", "s3://b/P01_R2.fq.gz", "s3://b/P02_R1.fq.gz"]);
    // Two samples, counted for per-sample cost estimates.
    assert_eq!(*sample_count, Some(2));
    let table = &input_tables.as_ref().unwrap()["input"];
    assert_eq!(table.uri, uri);
    assert_eq!(table.rows.len(), 2);
}

#[tokio::test]
async fn the_anchor_carries_the_first_sheet_s_sample_ids_and_the_version_s_hint() {
    let (state, events, _) = state();
    let app = api::router(state);
    let mut register = sheet_register_body();
    register["git_sha"] = json!("hinted");
    register["form_schema"]["samples"] = json!({"wildcard": "donor", "steps": {"joint": "study"}});
    setup(&app, "/workflows", "developers", &register).await;
    // Sheet order, not sorted; a sample on two lanes is one sample.
    let body = json!({
        "workflow_version_id": "hinted:hash:0",
        "params": {},
        "tables": {"input": [
            {"sample": "P2", "fastq_1": "s3://b/P2_L1.fq.gz"},
            {"sample": "P10", "fastq_1": "s3://b/P10.fq.gz"},
            {"sample": "P2", "fastq_1": "s3://b/P2_L2.fq.gz"},
            {"sample": "P1", "fastq_1": "s3://b/P1.fq.gz"}
        ]}
    });
    let res = app.clone().oneshot(post("/runs", Some("operators"), &body)).await.unwrap();
    assert_eq!(res.status(), StatusCode::ACCEPTED);
    let samples = recorded_anchor(&events).samples.expect("a sheet and a hint");
    assert_eq!(samples.ids, ["P2", "P10", "P1"]);
    assert_eq!(samples.hint.wildcard.as_deref(), Some("donor"));
    assert_eq!(samples.hint.steps["joint"], biffix_core::event::StepScope::Study);
}

#[tokio::test]
async fn an_anchor_whose_sheet_cannot_be_read_and_has_no_hint_has_no_samples() {
    let (state, events, _) = state();
    let app = api::router(state);
    let mut register = sheet_register_body();
    register["git_sha"] = json!("nosheet");
    register["form_schema"]["fields"][0]["required"] = json!(false);
    setup(&app, "/workflows", "developers", &register).await;
    // The sheet given as a file is not in the fake's storage, so its ids cannot be read; and the
    // version has no hint.
    let body = json!({
        "workflow_version_id": "nosheet:hash:0",
        "params": {"input": "s3://test-bucket/sheets/given.csv"}
    });
    let res = app.clone().oneshot(post("/runs", Some("operators"), &body)).await.unwrap();
    assert_eq!(res.status(), StatusCode::ACCEPTED, "{}", json_body(res).await);
    assert_eq!(recorded_anchor(&events).samples, None);
}

/// nf-core/demo's test samplesheet: three samples, one of them on two rows.
const DEMO_SHEET: &str = "sample,fastq_1,fastq_2
SAMPLE1_PE,s3://b/sample1_R1.fastq.gz,s3://b/sample1_R2.fastq.gz
SAMPLE2_PE,s3://b/sample2_R1.fastq.gz,s3://b/sample2_R2.fastq.gz
SAMPLE3_SE,s3://b/sample1_R1.fastq.gz,
SAMPLE3_SE,s3://b/sample2_R1.fastq.gz,
";

/// Register `register` (its version id `<git_sha>:hash:0`) and submit it with `params`, `cloud`
/// holding `objects` (URI, bytes); the recorded anchor, and the URIs `get_object_range` read.
async fn submit_with_objects(
    register: Value,
    params: Value,
    objects: &[(&str, &[u8])],
    stall_reads: bool,
) -> (biffix_core::event::RunSubmitted, Vec<String>) {
    let (state, events, cloud) = state();
    for (uri, body) in objects {
        cloud.objects.lock().unwrap().insert(uri.to_string(), body.to_vec());
    }
    cloud.stall_ranges.store(stall_reads, std::sync::atomic::Ordering::SeqCst);
    let app = api::router(state);
    setup(&app, "/workflows", "developers", &register).await;
    let version = format!("{}:hash:0", register["git_sha"].as_str().unwrap());
    let body = json!({"workflow_version_id": version, "params": params});
    let res = app.clone().oneshot(post("/runs", Some("operators"), &body)).await.unwrap();
    assert_eq!(res.status(), StatusCode::ACCEPTED, "{}", json_body(res).await);
    let ranges = cloud.ranges.lock().unwrap().clone();
    (recorded_anchor(&events), ranges)
}

/// Submit `sheet_register_body`'s version with its samplesheet given as the file `uri`, holding
/// `body` (or absent from storage when `None`); the recorded anchor.
async fn submit_sheet_file(uri: &str, body: Option<&[u8]>) -> biffix_core::event::RunSubmitted {
    let objects: Vec<(&str, &[u8])> = body.map(|b| (uri, b)).into_iter().collect();
    submit_with_objects(sheet_register_body(), json!({"input": uri}), &objects, false).await.0
}

#[tokio::test]
async fn a_samplesheet_given_as_a_file_declares_its_sample_ids() {
    let anchor =
        submit_sheet_file("s3://test-bucket/sheets/samplesheet.csv", Some(DEMO_SHEET.as_bytes()))
            .await;
    let spec = anchor.samples.expect("the file's sample ids");
    assert_eq!(spec.ids, ["SAMPLE1_PE", "SAMPLE2_PE", "SAMPLE3_SE"]);
    // Cost estimates count the same samples.
    assert_eq!(anchor.sample_count, Some(3));
    // So MultiQC's task, tagged with the pipeline's name, is no sample.
    let multiqc = "NFCORE_DEMO:DEMO:MULTIQC (demo)";
    assert_eq!(biffix_core::sample_progress::attribute(multiqc, Some("MULTIQC"), &spec), None);
}

#[tokio::test]
async fn a_tsv_samplesheet_file_is_split_on_tabs() {
    let tabs = DEMO_SHEET.replace(',', "\t");
    let anchor =
        submit_sheet_file("s3://test-bucket/sheets/samplesheet.tsv", Some(tabs.as_bytes())).await;
    assert_eq!(anchor.samples.unwrap().ids, ["SAMPLE1_PE", "SAMPLE2_PE", "SAMPLE3_SE"]);
}

#[tokio::test]
async fn a_samplesheet_file_s_delimiter_is_the_one_its_header_names_columns_with() {
    // A comma-separated `.txt`, and a tab-separated file named `.csv`: the header decides.
    let tabs = DEMO_SHEET.replace(',', "\t");
    for (uri, body) in [
        ("s3://test-bucket/sheets/samplesheet.txt", DEMO_SHEET),
        ("s3://test-bucket/sheets/samplesheet.csv", tabs.as_str()),
    ] {
        let anchor = submit_sheet_file(uri, Some(body.as_bytes())).await;
        assert_eq!(anchor.sample_count, Some(3), "{uri}");
    }
    // A header naming as many columns either way (here, one): the file's ending, else the declared
    // format. Read as TSV, `A,1` is one sample; read as CSV, the row is wider than the header.
    let one_column = b"sample\nA,1\nB,2\n";
    let tsv_register = || {
        let mut register = sheet_register_body();
        register["git_sha"] = json!("tsvsheet");
        register["form_schema"]["fields"][0]["samplesheet"]["format"] = json!("tsv");
        register
    };
    for (register, uri, ids) in [
        (tsv_register(), "s3://test-bucket/sheets/samplesheet", Some(vec!["A,1", "B,2"])),
        (sheet_register_body(), "s3://test-bucket/sheets/samplesheet", None),
        (
            sheet_register_body(),
            "s3://test-bucket/sheets/samplesheet.tsv",
            Some(vec!["A,1", "B,2"]),
        ),
        (tsv_register(), "s3://test-bucket/sheets/samplesheet.csv", None),
    ] {
        let (anchor, _) =
            submit_with_objects(register, json!({"input": uri}), &[(uri, one_column)], false).await;
        let got = anchor.samples.map(|s| s.ids);
        let want = ids.map(|ids| ids.into_iter().map(String::from).collect::<Vec<_>>());
        assert_eq!(got, want, "{uri}");
    }
}

#[tokio::test]
async fn the_first_sheet_field_given_a_file_is_the_one_read() {
    // Two sheet fields: `notes` declares no sample-id column and `other` is given no `s3://` path,
    // so `input`, after both, is read.
    let mut register = sheet_register_body();
    register["git_sha"] = json!("twosheets");
    let fields = register["form_schema"]["fields"].as_array_mut().unwrap();
    let input = fields[0].clone();
    let mut notes = input.clone();
    notes["name"] = json!("notes");
    notes["required"] = json!(false);
    notes["samplesheet"]["columns"] = json!([{"name": "note"}]);
    let mut other = input.clone();
    other["name"] = json!("other");
    other["required"] = json!(false);
    other["is_path_input"] = json!(false);
    *fields = vec![notes, other, input.clone()];
    let (notes_uri, input_uri) =
        ("s3://test-bucket/sheets/notes.csv", "s3://test-bucket/sheets/samplesheet.csv");
    let objects: [(&str, &[u8]); 2] =
        [(notes_uri, b"note\nhello\n"), (input_uri, DEMO_SHEET.as_bytes())];
    let params = json!({"notes": notes_uri, "other": "sheets/local.csv", "input": input_uri});
    let (anchor, ranges) = submit_with_objects(register.clone(), params, &objects, false).await;
    assert_eq!(anchor.sample_count, Some(3));
    assert_eq!(ranges, [input_uri]);
    // The first such field is the one: its file unreadable, a later one is not tried.
    register["git_sha"] = json!("twosheets2");
    let fields = register["form_schema"]["fields"].as_array_mut().unwrap();
    let mut first = input;
    first["name"] = json!("first");
    first["required"] = json!(false);
    fields.insert(0, first);
    let first_uri = "s3://test-bucket/sheets/first.csv";
    let objects: [(&str, &[u8]); 2] = [(first_uri, b"id\nA\n"), (input_uri, DEMO_SHEET.as_bytes())];
    let params = json!({"first": first_uri, "input": input_uri});
    let (anchor, ranges) = submit_with_objects(register, params, &objects, false).await;
    assert_eq!((anchor.samples, anchor.sample_count), (None, None));
    assert_eq!(ranges, [first_uri]);
}

#[tokio::test]
async fn a_samplesheet_file_whose_ids_cannot_be_read_leaves_the_sample_set_open() {
    let uri = "s3://test-bucket/sheets/samplesheet.csv";
    // Larger than submit reads (1 MiB).
    let mut big = DEMO_SHEET.as_bytes().to_vec();
    big.resize((1 << 20) + 1, b'\n');
    // Without the sample-id column; with an unclosed quote; not text; a header alone, naming no
    // samples.
    let unreadable: [&[u8]; 5] = [
        &big,
        b"id,fastq_1\nA,s3://b/a.fastq.gz\n",
        b"sample,fastq_1\n\"A,s3://b/a.fastq.gz\n",
        &[0x1f, 0x8b, 0xff, 0xfe],
        b"sample,fastq_1,fastq_2\n",
    ];
    for (i, body) in unreadable.into_iter().enumerate() {
        let anchor = submit_sheet_file(uri, Some(body)).await;
        assert_eq!((anchor.samples, anchor.sample_count), (None, None), "unreadable sheet {i}");
    }
    // Not in storage: the read fails, and the submit still goes on.
    let anchor = submit_sheet_file(uri, None).await;
    assert_eq!((anchor.samples, anchor.sample_count), (None, None));
}

#[tokio::test]
async fn a_samplesheet_file_of_exactly_the_cap_is_read() {
    let mut at_cap = DEMO_SHEET.as_bytes().to_vec();
    at_cap.resize(1 << 20, b'\n');
    let anchor = submit_sheet_file("s3://test-bucket/sheets/samplesheet.csv", Some(&at_cap)).await;
    assert_eq!(anchor.sample_count, Some(3));
}

#[tokio::test(start_paused = true)]
async fn a_stalled_samplesheet_read_leaves_the_sample_set_open() {
    let uri = "s3://test-bucket/sheets/samplesheet.csv";
    let (anchor, ranges) = submit_with_objects(
        sheet_register_body(),
        json!({"input": uri}),
        &[(uri, DEMO_SHEET.as_bytes())],
        true,
    )
    .await;
    assert_eq!(ranges, [uri]);
    assert_eq!((anchor.samples, anchor.sample_count), (None, None));
}

#[tokio::test]
async fn a_sheet_written_from_rows_wins_over_a_file() {
    let (state, events, cloud) = state();
    let given = "s3://test-bucket/sheets/samplesheet.csv";
    cloud.objects.lock().unwrap().insert(given.into(), DEMO_SHEET.into());
    let app = api::router(state);
    setup(&app, "/workflows", "developers", &sheet_register_body()).await;
    let body = json!({
        "workflow_version_id": "sheet:hash:0",
        "params": {"input": given},
        "tables": {"input": [{"sample": "P01", "fastq_1": "s3://b/P01_R1.fq.gz"}]}
    });
    let res = app.clone().oneshot(post("/runs", Some("operators"), &body)).await.unwrap();
    assert_eq!(res.status(), StatusCode::ACCEPTED);
    let anchor = recorded_anchor(&events);
    assert_eq!(anchor.samples.unwrap().ids, ["P01"]);
    assert_eq!(anchor.sample_count, Some(1));
    assert!(!cloud.ranges.lock().unwrap().iter().any(|u| u == given), "the given file is not read");
}

#[tokio::test]
async fn bad_rows_are_rejected_naming_row_and_column() {
    let (state, _, cloud) = state();
    let app = api::router(state);
    setup(&app, "/workflows", "developers", &sheet_register_body()).await;
    let body = json!({
        "workflow_version_id": "sheet:hash:0",
        "params": {},
        "tables": {"input": [{"sample": "", "fastq_1": "local/a.fq.gz"}]}
    });
    let res = app.clone().oneshot(post("/runs", Some("operators"), &body)).await.unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let text = text_body(res).await;
    assert!(text.contains("row 1, sample: required"), "{text}");
    assert!(text.contains("row 1, fastq_1: must be an s3:// path"), "{text}");
    assert!(cloud.objects.lock().unwrap().is_empty(), "nothing written");

    // Rows for a param without declared columns, or for no param at all, are refused too (the
    // params valid, so the tables check is the only thing that can refuse them).
    setup(&app, "/workflows", "developers", &register_body()).await;
    for (param, message) in [
        ("reads", "`reads` declares no samplesheet columns; give it a path instead of rows"),
        ("nope", "`nope` is not a parameter of this workflow"),
    ] {
        let body = json!({
            "workflow_version_id": "abc:hash:0",
            "params": {"reads": "s3://b/in.fastq", "outdir": "s3://b/out/"},
            "tables": {param: [{"sample": "a"}]}
        });
        let res = app.clone().oneshot(post("/runs", Some("operators"), &body)).await.unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST, "{param}");
        assert_eq!(text_body(res).await, message);
    }
    assert!(cloud.objects.lock().unwrap().is_empty(), "nothing written");
    assert!(cloud.launched.lock().unwrap().is_empty());
}

#[tokio::test]
async fn more_rows_than_the_cap_are_refused() {
    let (state, _, cloud) = state();
    let app = api::router(state);
    setup(&app, "/workflows", "developers", &sheet_register_body()).await;
    let rows: Vec<Value> = (0..10_001)
        .map(|i| json!({"sample": format!("P{i}"), "fastq_1": format!("s3://b/P{i}_R1.fq.gz")}))
        .collect();
    let body = json!({
        "workflow_version_id": "sheet:hash:0",
        "params": {},
        "tables": {"input": rows}
    });
    let res = app.oneshot(post("/runs", Some("operators"), &body)).await.unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    assert_eq!(text_body(res).await, "`input` has 10001 rows; at most 10000");
    assert!(cloud.objects.lock().unwrap().is_empty());
    assert!(cloud.launched.lock().unwrap().is_empty());
}

#[tokio::test]
async fn detect_groups_files_and_fills_the_samplesheet() {
    let (state, _, _) = state();
    let body = json!({
        "files": [
            {"uri": "s3://b/run/P01_S1_L001_R1_001.fastq.gz"},
            {"uri": "s3://b/run/P01_S1_L001_R2_001.fastq.gz"},
            {"uri": "s3://b/run/P7_S7_L001_R1_001.fastq.gz"},
            {"uri": "s3://b/run/P07_S7_L001_R2_001.fastq.gz"}
        ],
        "samplesheet": {"columns": [
            {"name": "sample", "role": "sample_id"},
            {"name": "fastq_1", "role": "read1", "file": true},
            {"name": "fastq_2", "role": "read2", "file": true}
        ]}
    });
    let res =
        api::router(state).oneshot(post("/samples/detect", Some("viewers"), &body)).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = json_body(res).await;
    let ids: Vec<&str> = v["detection"]["samples"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["P01", "P07", "P7"]);
    assert_eq!(v["detection"]["suggestions"][0]["into"], json!("P07"));
    assert_eq!(v["rows"].as_array().unwrap().len(), 3);
    assert_eq!(v["tokens"][0]["text"], json!("P01"));
    assert_eq!(v["example"], json!("P01_S1_L001_R1_001.fastq.gz"));
}
