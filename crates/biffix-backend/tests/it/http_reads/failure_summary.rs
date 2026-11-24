//! The failure summary: each failed step's cause, the samples and who to ask, attempts and the step cap.

use super::*;

fn summary_state(
    detail: biffix_backend::store::RunDetailView,
    logs: &[(&str, &[&str])],
) -> AppState {
    let cloud = FakeCloud::new();
    for (stream, lines) in logs {
        cloud.logs.lock().unwrap().insert(
            stream.to_string(),
            lines.iter().map(std::string::ToString::to_string).collect(),
        );
    }
    let mut state = state_with_run_detail(detail);
    state.cloud = Arc::new(cloud);
    state
}

#[tokio::test]
async fn a_failed_runs_summary_names_each_step_s_cause_earliest_first() {
    let mut detail = testing::run_detail("r1", "failed");
    detail.tasks = vec![
        failed_task("REVERSE (1)", "REVERSE", 20.0, 1, "s-rev"),
        failed_task("ALIGN (A)", "ALIGN", 10.0, 137, "s-align"),
        task_view("SAY (1)", "succeeded", None, None),
    ];
    let state = summary_state(
        detail,
        &[("s-rev", &["reversing", "failing on purpose"]), ("s-align", &["Killed"])],
    );
    let (status, v) = get_json(api::router(state), "/runs/r1/failure-summary").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["failed_steps"], 2);
    assert_eq!(v["items"][0]["step"], "ALIGN");
    assert_eq!(v["items"][0]["cause"], "out_of_memory");
    assert_eq!(v["items"][1]["step"], "REVERSE");
    assert_eq!(v["items"][1]["task_key"], "REVERSE (1)");
    assert_eq!(v["items"][1]["cause"], "step_error");
    assert_eq!(v["items"][1]["message"], "the step exited with an error (exit code 1)");
    assert_eq!(v["items"][1]["log_tail"], serde_json::json!(["reversing", "failing on purpose"]));
}

#[tokio::test]
async fn a_snakemake_job_s_tail_is_its_step_s_lines_above_its_snakemake_s_epilogue() {
    // A Snakemake job runs its rule under its own Snakemake, which ends every failed job's log
    // with the same epilogue: the tail shows the step's last lines (the OOM kill) instead.
    let mut detail = testing::run_detail("r1", "failed");
    detail.run.engine = Some("snakemake".into());
    detail.tasks = vec![failed_task("align:sample=B", "align", 10.0, 1, "s-align")];
    let mut log: Vec<String> = (0..30).map(|i| format!("step line {i}")).collect();
    log.push(
        "/usr/bin/bash: line 1:    15 Killed      python3 -c 'x = bytearray(4 * 1024**3)'".into(),
    );
    log.extend(
        [
            "Full Traceback (most recent call last):",
            "  File \"local.py\", line 432, in run_wrapper",
            "[Thu Oct  1 00:32:36 2026]",
            "Error in rule align:",
            "Shutting down, this might take some time.",
            "WorkflowError:",
            "At least one job did not complete successfully.",
        ]
        .map(String::from),
    );
    let lines: Vec<&str> = log.iter().map(String::as_str).collect();
    let state = summary_state(detail, &[("s-align", &lines)]);
    let (status, v) = get_json(api::router(state), "/runs/r1/failure-summary").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let tail: Vec<&str> =
        v["items"][0]["log_tail"].as_array().unwrap().iter().map(|l| l.as_str().unwrap()).collect();
    assert_eq!(tail.len(), 20);
    assert_eq!(tail[0], "step line 11");
    assert!(tail[19].contains("Killed"), "{tail:?}");
    assert!(!tail.iter().any(|l| l.contains("WorkflowError")), "{tail:?}");
}

/// The end of a Snakemake job's log whose shell command exited 3 (its job exited 1).
const EXIT_3_JOB_LOG: &[&str] = &[
    "localrule world:",
    "smoke: failing on purpose",
    "Full Traceback (most recent call last):",
    "    raise sp.CalledProcessError(retcode, cmd)",
    "subprocess.CalledProcessError: Command 'set -euo pipefail;  echo 'x does not exist'; exit 3' \
     returned non-zero exit status 3.",
    "RuleException:",
    "[Fri Oct 23 13:00:50 2026]",
    "Error in rule world:",
    "WorkflowError:",
    "At least one job did not complete successfully.",
];

#[tokio::test]
async fn a_snakemake_step_s_summary_carries_its_command_s_exit_code_and_error() {
    let mut detail = testing::run_detail("r1", "failed");
    detail.run.engine = Some("snakemake".into());
    detail.tasks = vec![failed_task("world", "world", 10.0, 1, "s-world")];
    let state = summary_state(detail, &[("s-world", EXIT_3_JOB_LOG)]);
    let (status, v) = get_json(api::router(state), "/runs/r1/failure-summary").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let item = &v["items"][0];
    assert_eq!(item["exit_code"], 3);
    // From the step's own lines: the command text the error quotes is not a missing input.
    assert_eq!(item["cause"], "step_error");
    assert_eq!(item["message"], "the step exited with an error (exit code 3)");
    assert_eq!(
        item["log_tail"],
        serde_json::json!(["localrule world:", "smoke: failing on purpose", EXIT_3_JOB_LOG[4]])
    );
}

#[tokio::test]
async fn a_nextflow_step_s_exit_code_is_its_job_s() {
    // A Nextflow task's job exits with its script's own status, and its log has no Snakemake
    // epilogue to read one from.
    let mut detail = testing::run_detail("r1", "failed");
    detail.run.engine = Some("nextflow".into());
    detail.tasks = vec![failed_task("SAY (1)", "SAY", 10.0, 1, "s-say")];
    let state = summary_state(detail, &[("s-say", EXIT_3_JOB_LOG)]);
    let (_, v) = get_json(api::router(state), "/runs/r1/failure-summary").await;
    assert_eq!(v["items"][0]["exit_code"], 1);
    assert_eq!(v["items"][0]["log_tail"].as_array().unwrap().len(), EXIT_3_JOB_LOG.len());
}

#[tokio::test]
async fn an_engine_failure_is_explained_from_the_engine_s_log() {
    let mut detail = testing::run_detail("r1", "failed");
    detail.coordinator_log_stream = Some("s-coord".into());
    let state =
        summary_state(detail, &[("s-coord", &["ERROR ~ Path `s3://b/sheet.csv` does not exist"])]);
    let (_, v) = get_json(api::router(state), "/runs/r1/failure-summary").await;
    assert_eq!(v["failed_steps"], 0);
    assert_eq!(v["items"][0]["step"], "workflow engine");
    assert_eq!(v["items"][0]["cause"], "missing_input");
}

/// A failed Snakemake run of version `abc:hash` that `resumable` accepts (finished, a recorded
/// output prefix), with three failed steps named by their `sample` wildcard (one whole-study).
fn failed_samples_run() -> biffix_backend::store::RunDetailView {
    let mut detail = testing::run_detail("r1", "failed");
    detail.run.engine = Some("snakemake".into());
    detail.run.workflow = Some(biffix_core::ids::WorkflowVersionId::new("abc:hash"));
    detail.output_prefix = Some("s3://b/biffix/runs/r1/".into());
    detail.tasks = vec![
        failed_task("ALIGN:sample=Tumor_4", "ALIGN", 10.0, 137, "s-align"),
        failed_task("TRIM:sample=Normal_1", "TRIM", 20.0, 1, "s-trim"),
        failed_task("JOINT:cohort=all", "JOINT", 30.0, 1, "s-joint"),
    ];
    detail
}

#[tokio::test]
async fn a_failure_names_the_sample_and_what_try_again_does() {
    let logs: &[(&str, &[&str])] = &[
        ("s-align", &["Killed"]),
        ("s-trim", &["gzip: stdin: not in gzip format"]),
        ("s-joint", &["failing on purpose"]),
    ];
    let state = summary_state(failed_samples_run(), logs);
    let (status, v) = get_json(api::router(state), "/runs/r1/failure-summary").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let got: Vec<(&str, &str, Option<&str>, &str)> = v["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| {
            (
                i["step"].as_str().unwrap(),
                i["cause"].as_str().unwrap(),
                i["sample"].as_str(),
                i["try_again"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        got,
        [
            ("ALIGN", "out_of_memory", Some("Tumor_4"), "resume"),
            ("TRIM", "bad_input", Some("Normal_1"), "fix_samples"),
            ("JOINT", "step_error", None, "resume"),
        ]
    );
    // A run whose intermediate files were deleted cannot be resumed: resume becomes a re-run, and
    // a fix still opens the form.
    let mut deleted = failed_samples_run();
    deleted.work_deleted_at_micros = Some(1);
    let state = summary_state(deleted, logs);
    let (_, v) = get_json(api::router(state), "/runs/r1/failure-summary").await;
    assert_eq!(v["items"][0]["try_again"], "rerun");
    assert_eq!(v["items"][1]["try_again"], "fix_samples");
    assert_eq!(v["items"][2]["try_again"], "rerun");
}

#[tokio::test]
async fn a_failure_names_who_to_ask_the_pipeline_s_contact_before_the_site_s() {
    let site = biffix_api::SupportContact {
        name: "Bioinformatics core".into(),
        email: "core@lab.example.org".into(),
        url: None,
    };
    let with_site_support = |mut state: AppState| {
        state.settings = Arc::new(testing::MemSettings {
            inner: std::sync::Mutex::new(biffix_api::SiteSettings {
                support: Some(site.clone()),
                ..Default::default()
            }),
            user_prefs: Default::default(),
        });
        state
    };
    // No version contact: the site's.
    let mut state = with_site_support(summary_state(failed_samples_run(), &[]));
    state.registry = state_with_one_version().await.registry;
    let (_, v) = get_json(api::router(state.clone()), "/runs/r1/failure-summary").await;
    assert_eq!(v["support"], serde_json::to_value(&site).unwrap());
    // The version's frozen `catalog.contact` wins.
    let mut rec = state.registry.get_version("abc:hash").await.unwrap();
    rec.version.form_schema["catalog"] = serde_json::json!({
        "contact": {"name": "RNA-seq team", "email": "rna@lab.example.org", "url": "https://lab.example.org/rna"}
    });
    let registry = MemRegistry::new();
    registry.put_version(&rec).await.unwrap();
    state.registry = Arc::new(registry);
    let (_, v) = get_json(api::router(state.clone()), "/runs/r1/failure-summary").await;
    assert_eq!(
        v["support"],
        serde_json::json!({"name": "RNA-seq team", "email": "rna@lab.example.org", "url": "https://lab.example.org/rna"})
    );
    // Neither: none.
    let (_, v) =
        get_json(api::router(summary_state(failed_samples_run(), &[])), "/runs/r1/failure-summary")
            .await;
    assert!(v.get("support").is_none(), "{v}");
}

#[tokio::test]
async fn a_run_that_did_not_fail_has_no_summary() {
    let state = summary_state(testing::run_detail("r1", "succeeded"), &[]);
    let (status, v) = get_json(api::router(state), "/runs/r1/failure-summary").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["items"], serde_json::json!([]));
}

#[tokio::test]
async fn a_failure_summary_reads_the_recorded_attempts_and_shows_only_the_first_steps() {
    let mut detail = testing::run_detail("r1", "failed");
    detail.tasks = vec![
        failed_task("D", "D", 40.0, 1, "s-d"),
        failed_task("C", "C", 30.0, 1, "s-c"),
        failed_task("B", "B", 20.0, 1, "s-b"),
        // Its own exit code differs from its last attempt's, so the item's can be told apart.
        failed_task("A", "A", 10.0, 137, "s-a"),
    ];
    let store = MemStore::new();
    store.insert(detail);
    let mut reclaimed = attempt_view("job-A0", 1, "failed", "s-a0");
    reclaimed.spot_warned_at = Some(5.0);
    reclaimed.status_reason = Some("Host EC2 (instance i-1) terminated.".into());
    let mut last = attempt_view("job-A1", 2, "failed", "s-a1");
    last.exit_code = Some(1);
    last.status_reason = Some("Essential container in task exited".into());
    store.insert_attempts("r1", "A", vec![reclaimed, last]);
    let mut state = summary_state(
        testing::run_detail("unused", "failed"),
        &[("s-a", &["the task's own stream"]), ("s-a1", &["the last attempt's stream"])],
    );
    state.store = Arc::new(store);

    let (status, v) = get_json(api::router(state), "/runs/r1/failure-summary").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["failed_steps"], 4);
    let items = v["items"].as_array().unwrap();
    // Capped at the first three, earliest first: D (the latest) is left out.
    let steps: Vec<&str> = items.iter().map(|i| i["step"].as_str().unwrap()).collect();
    assert_eq!(steps, vec!["A", "B", "C"]);
    // The last attempt's stream wins over the task's, its reason is the step's, and both
    // attempts are counted.
    assert_eq!(items[0]["log_tail"], serde_json::json!(["the last attempt's stream"]));
    assert_eq!(items[0]["reason"], "Essential container in task exited");
    assert_eq!(items[0]["attempts"], 2);
    // The last attempt's exit code and Batch job, for the support report.
    assert_eq!(items[0]["exit_code"], 1);
    assert_eq!(items[0]["batch_job_id"], "job-A1");
    // A step with no recorded attempt: the task's own exit code, and no job.
    assert_eq!(items[1]["exit_code"], 1);
    assert!(items[1].get("batch_job_id").is_none(), "{}", items[1]);
}

#[tokio::test]
async fn a_failure_summary_whose_samples_cannot_be_read_still_explains_the_steps() {
    let mut detail = testing::run_detail("r1", "failed");
    detail.tasks = vec![failed_task("A", "A", 10.0, 1, "s-a")];
    let store = MemStore::new();
    store.insert(detail);
    store.fail_task_samples.store(true, std::sync::atomic::Ordering::SeqCst);
    let mut state = summary_state(testing::run_detail("unused", "failed"), &[]);
    state.store = Arc::new(store);
    let (status, v) = get_json(api::router(state), "/runs/r1/failure-summary").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["items"][0]["cause"], "step_error");
    assert!(v["items"][0].get("sample").is_none(), "{v}");
}

#[tokio::test]
async fn an_engine_failure_without_a_readable_log_is_unknown() {
    // No coordinator log stream recorded: nothing says the options are at fault.
    let state = summary_state(testing::run_detail("r1", "failed"), &[]);
    let (status, v) = get_json(api::router(state), "/runs/r1/failure-summary").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["items"][0]["step"], "workflow engine");
    assert_eq!(v["items"][0]["cause"], "unknown");
    // Unknown is resumed, but a run that recorded no prefix cannot be: run again.
    assert_eq!(v["items"][0]["try_again"], "rerun");
}

#[tokio::test]
async fn a_coordinator_that_never_started_is_explained_from_batch_s_reason() {
    // No log (the container never ran), but Batch said why: the reason, not "unknown".
    let mut detail = testing::run_detail("r1", "failed");
    let reason = "Task failed to start: CannotPullContainerError: ref/runtime:sm-9 not found";
    detail.coordinator_stop.status_reason = Some(reason.into());
    let state = summary_state(detail, &[]);
    let (status, v) = get_json(api::router(state), "/runs/r1/failure-summary").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["items"][0]["step"], "workflow engine");
    assert_eq!(v["items"][0]["cause"], "could_not_start");
    assert_eq!(v["items"][0]["reason"], reason);
    assert_eq!(v["items"][0]["exit_code"], serde_json::Value::Null);

    // One Batch killed for memory: its exit code and the out-of-memory cause.
    let mut detail = testing::run_detail("r1", "failed");
    detail.coordinator_stop.exit_code = Some(137);
    detail.coordinator_stop.termination_category = Some("oom".into());
    let state = summary_state(detail, &[]);
    let (_, v) = get_json(api::router(state), "/runs/r1/failure-summary").await;
    assert_eq!(v["items"][0]["cause"], "out_of_memory");
    assert_eq!(v["items"][0]["exit_code"], 137);
}

#[tokio::test]
async fn a_step_the_engine_failed_although_its_job_succeeded_says_what_the_engine_said() {
    // Nextflow rejected the task (missing outputs) whose Batch job exited 0: the attempt is
    // succeeded in AWS's record, the task carries the engine's verdict.
    let mut detail = testing::run_detail("r1", "failed");
    let mut task = failed_task("A", "A", 10.0, 0, "s-a");
    task.batch_job_id = Some("job-A".into());
    task.termination_category = Some("missing_output".into());
    task.verdict_reason = Some("Missing output file(s) `A.bam`".into());
    detail.tasks = vec![task];
    let store = MemStore::new();
    store.insert(detail);
    let mut ok = attempt_view("job-A", 1, "succeeded", "s-a");
    ok.exit_code = Some(0);
    ok.status_reason = Some("Essential container in task exited".into());
    store.insert_attempts("r1", "A", vec![ok]);
    let mut state = summary_state(testing::run_detail("unused", "failed"), &[]);
    state.store = Arc::new(store);

    let (status, v) = get_json(api::router(state), "/runs/r1/failure-summary").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let item = &v["items"][0];
    assert_eq!(item["cause"], "missing_output");
    assert_eq!(item["reason"], "Missing output file(s) `A.bam`");
}

#[tokio::test]
async fn a_failure_summary_finds_the_earliest_failure_beyond_the_first_page() {
    // More failed tasks than one store page (1000, in key order); the earliest failure has the
    // key that sorts last, so it is only on the second page.
    let mut detail = testing::run_detail("r1", "failed");
    detail.tasks = (0..1001)
        .map(|i| failed_task(&format!("t{i:04}"), "T", 100.0 + i as f64, 1, "s-t"))
        .collect();
    detail.tasks.push(failed_task("zzz", "Z", 1.0, 1, "s-z"));
    let state = summary_state(detail, &[]);
    let (status, v) = get_json(api::router(state), "/runs/r1/failure-summary").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["failed_steps"], 1002);
    let keys: Vec<&str> =
        v["items"].as_array().unwrap().iter().map(|i| i["task_key"].as_str().unwrap()).collect();
    assert_eq!(keys, vec!["zzz", "t0000", "t0001"]);
}

#[test]
fn the_api_doc_names_how_long_a_summary_may_be_pending() {
    // The UI cannot import the window, so it reads it from this text (`failureLoader`'s test).
    use utoipa::OpenApi;
    let doc = serde_json::to_value(biffix_backend::api::ApiDoc::openapi()).unwrap();
    let text = doc["components"]["schemas"]["FailureSummary"]["properties"]["pending"]
        ["description"]
        .as_str()
        .unwrap()
        .to_string();
    let window = format!("at most {} seconds", biffix_api::FAILURE_SUMMARY_SETTLE_SECS);
    assert!(text.contains(&window), "{text}");
}

#[tokio::test]
async fn an_engine_failure_is_pending_until_batch_reports_the_stop() {
    let now = biffix_core::clock::epoch_secs();
    let engine_failed = |completed_at: f64| {
        let mut detail = testing::run_detail("r1", "failed");
        detail.run.completed_at = Some(completed_at);
        detail
    };
    // The engine said the run failed a moment ago, and Batch has said nothing about the stop yet.
    let state = summary_state(engine_failed(now), &[]);
    let (status, v) = get_json(api::router(state), "/runs/r1/failure-summary").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["pending"], true, "{v}");
    // Batch reported the coordinator's exit: settled.
    let mut detail = engine_failed(now);
    detail.coordinator_stop.exit_code = Some(1);
    let (_, v) =
        get_json(api::router(summary_state(detail, &[])), "/runs/r1/failure-summary").await;
    assert!(v.get("pending").is_none(), "{v}");
    // Any one of Batch's stop facts settles it.
    let mut detail = engine_failed(now);
    detail.coordinator_stop.status_reason = Some("Essential container in task exited".into());
    let (_, v) =
        get_json(api::router(summary_state(detail, &[])), "/runs/r1/failure-summary").await;
    assert!(v.get("pending").is_none(), "{v}");
    let mut detail = engine_failed(now);
    detail.coordinator_stop.termination_category = Some("app_error".into());
    let (_, v) =
        get_json(api::router(summary_state(detail, &[])), "/runs/r1/failure-summary").await;
    assert!(v.get("pending").is_none(), "{v}");
    // An end time in the future (a producer's clock ahead of ours) never waits.
    let (_, v) = get_json(
        api::router(summary_state(engine_failed(now + 600.0), &[])),
        "/runs/r1/failure-summary",
    )
    .await;
    assert!(v.get("pending").is_none(), "{v}");
    // Long past the run's end: settled with whatever is there, even with no stop facts.
    let (_, v) = get_json(
        api::router(summary_state(
            engine_failed(now - biffix_api::FAILURE_SUMMARY_SETTLE_SECS as f64 - 1.0),
            &[],
        )),
        "/runs/r1/failure-summary",
    )
    .await;
    assert!(v.get("pending").is_none(), "{v}");
    // A summary of failed steps never waits.
    let mut detail = engine_failed(now);
    detail.tasks = vec![failed_task("A", "A", 10.0, 1, "s-a")];
    let (_, v) =
        get_json(api::router(summary_state(detail, &[])), "/runs/r1/failure-summary").await;
    assert_eq!(v["items"][0]["step"], "A");
    assert!(v.get("pending").is_none(), "{v}");
}

/// Where run `r1`'s coordinator writes its failure.txt: under the run's own prefix in its project's
/// bucket (`b`, [`state_with_run_detail`]'s project).
fn failure_txt_uri() -> String {
    format!("s3://b/{}logs/failure.txt", biffix_core::s3_layout::run_prefix("r1"))
}

/// [`summary_state`] for a failed run `r1` with no failed step, whose coordinator logged
/// `coordinator_log`, and whose failure.txt (when given) is `failure_txt`.
fn engine_failure_state(
    coordinator_log: &[&str],
    failure_txt: Option<&str>,
) -> (AppState, Arc<FakeCloud>) {
    let mut detail = testing::run_detail("r1", "failed");
    detail.coordinator_log_stream = Some("s-coord".into());
    detail.coordinator_stop.exit_code = Some(1);
    let cloud = Arc::new(FakeCloud::new());
    cloud.logs.lock().unwrap().insert(
        "s-coord".into(),
        coordinator_log.iter().map(std::string::ToString::to_string).collect(),
    );
    if let Some(text) = failure_txt {
        cloud.objects.lock().unwrap().insert(failure_txt_uri(), text.as_bytes().to_vec());
    }
    let mut state = state_with_run_detail(detail);
    state.cloud = cloud.clone();
    (state, cloud)
}

/// The coordinator's own lines after the engine exited, which fill the end of its log.
const COORDINATOR_NOISE: &[&str] = &[
    "biffix-coordinator: uploading events.jsonl",
    "biffix-coordinator: uploaded 3 log file(s)",
    "biffix-coordinator: replayed 10/10",
];

fn log_tail_of(v: &serde_json::Value) -> Vec<String> {
    v["items"][0]["log_tail"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l.as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn an_engine_failure_is_explained_from_the_run_s_failure_txt() {
    let output: Vec<String> = (0..60).map(|i| format!("engine line {i}")).collect();
    let text = format!(
        "The engine exited with status 1. The end of its output:\n\n{}\nERROR ~ boom\n",
        output.join("\n")
    );
    let (state, cloud) = engine_failure_state(COORDINATOR_NOISE, Some(&text));
    let (status, v) = get_json(api::router(state), "/runs/r1/failure-summary").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let tail = log_tail_of(&v);
    // The file's last 50 lines, not the coordinator's log.
    assert_eq!(tail.len(), 50);
    assert_eq!(tail[0], "engine line 11");
    assert_eq!(tail[49], "ERROR ~ boom");
    assert!(!tail.iter().any(|l| l.contains("biffix-coordinator")), "{tail:?}");
    assert_eq!(v["items"][0]["cause"], "engine_error");
    assert_eq!(v["items"][0]["exit_code"], 1);
    assert_eq!(*cloud.ranges.lock().unwrap(), [failure_txt_uri()]);
}

#[tokio::test]
async fn a_failure_txt_naming_a_missing_file_is_a_missing_input() {
    let text = "The engine exited with status 1. The end of its output:\n\n\
                ERROR ~ Path `s3://b/sheet.csv` does not exist\n";
    let (state, _) = engine_failure_state(COORDINATOR_NOISE, Some(text));
    let (_, v) = get_json(api::router(state), "/runs/r1/failure-summary").await;
    assert_eq!(v["items"][0]["cause"], "missing_input");
}

#[tokio::test]
async fn a_status_only_failure_txt_shows_its_headline_above_the_log() {
    // The file quotes no output: the coordinator's log is the evidence, under the file's headline.
    let text = "The engine exited with status 1.\n";
    let (state, _) =
        engine_failure_state(&["ERROR ~ Path `s3://b/x` does not exist", "exiting"], Some(text));
    let (_, v) = get_json(api::router(state), "/runs/r1/failure-summary").await;
    assert_eq!(
        log_tail_of(&v),
        ["The engine exited with status 1.", "ERROR ~ Path `s3://b/x` does not exist", "exiting"]
    );
    assert_eq!(v["items"][0]["cause"], "missing_input");
}

/// The headline `run_logs.py --failure-label` writes for a single-job run's image conversion.
const LABELLED: &str = "Converting the task images failed (exit status 1).";

#[tokio::test]
async fn a_labelled_step_s_failure_txt_names_the_step_not_the_engine() {
    // A step of the coordinator's own failed before the engine started: its headline stays in
    // view, above the step's output, and that output is not read as the workflow's inputs.
    let output: Vec<String> = (0..60).map(|i| format!("convert line {i}")).collect();
    let text = format!(
        "{LABELLED} The end of its output:\n\n{}\nFATAL: image.sif: no such file or directory\n",
        output.join("\n")
    );
    let (state, _) = engine_failure_state(COORDINATOR_NOISE, Some(&text));
    let (_, v) = get_json(api::router(state), "/runs/r1/failure-summary").await;
    let tail = log_tail_of(&v);
    assert_eq!(tail.len(), 50);
    assert_eq!(tail[0], LABELLED);
    assert_eq!(tail[1], "convert line 12");
    assert_eq!(tail[49], "FATAL: image.sif: no such file or directory");
    let item = &v["items"][0];
    assert_ne!(item["cause"], "missing_input", "{item}");
    assert_eq!(item["cause"], "unknown", "{item}");
    assert!(
        item["message"].as_str().unwrap().contains("Converting the task images failed"),
        "{item}"
    );
}

#[tokio::test]
async fn a_status_only_labelled_failure_txt_names_the_step_above_the_log() {
    let text = format!("{LABELLED}\n");
    let (state, _) =
        engine_failure_state(&["FATAL: image.sif: no such file or directory"], Some(&text));
    let (_, v) = get_json(api::router(state), "/runs/r1/failure-summary").await;
    assert_eq!(log_tail_of(&v), [LABELLED, "FATAL: image.sif: no such file or directory"]);
    assert_eq!(v["items"][0]["cause"], "unknown");
    assert!(
        v["items"][0]["message"].as_str().unwrap().contains("Converting the task images failed"),
        "{v}"
    );
}

#[tokio::test]
async fn without_a_failure_txt_the_engine_s_log_is_read_as_before() {
    let (state, cloud) = engine_failure_state(&["ERROR ~ Path `s3://b/x` does not exist"], None);
    let (_, v) = get_json(api::router(state), "/runs/r1/failure-summary").await;
    assert_eq!(log_tail_of(&v), ["ERROR ~ Path `s3://b/x` does not exist"]);
    assert_eq!(v["items"][0]["cause"], "missing_input");
    assert_eq!(*cloud.ranges.lock().unwrap(), [failure_txt_uri()]);
}

#[tokio::test(start_paused = true)]
async fn a_stalled_failure_txt_read_falls_back_to_the_log() {
    let (state, cloud) = engine_failure_state(&["the coordinator's last line"], Some("never read"));
    cloud.stall_ranges.store(true, std::sync::atomic::Ordering::SeqCst);
    let (status, v) = get_json(api::router(state), "/runs/r1/failure-summary").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(log_tail_of(&v), ["the coordinator's last line"]);
}

#[tokio::test]
async fn a_blank_failure_txt_falls_back_to_the_log() {
    let (state, _) = engine_failure_state(&["the coordinator's last line"], Some("  \n\n"));
    let (_, v) = get_json(api::router(state), "/runs/r1/failure-summary").await;
    assert_eq!(log_tail_of(&v), ["the coordinator's last line"]);
}

#[tokio::test]
async fn a_refused_failure_txt_read_falls_back_to_the_log() {
    let (state, cloud) = engine_failure_state(&["the coordinator's last line"], Some("never read"));
    cloud.fail_ranges.store(true, std::sync::atomic::Ordering::SeqCst);
    let (status, v) = get_json(api::router(state), "/runs/r1/failure-summary").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(log_tail_of(&v), ["the coordinator's last line"]);
    assert_eq!(*cloud.ranges.lock().unwrap(), [failure_txt_uri()]);
}

#[tokio::test]
async fn a_project_without_a_bucket_reads_the_log_only() {
    let (mut state, cloud) = engine_failure_state(&["the coordinator's last line"], None);
    state.projects = Default::default();
    let (status, v) = get_json(api::router(state), "/runs/r1/failure-summary").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(log_tail_of(&v), ["the coordinator's last line"]);
    assert!(cloud.ranges.lock().unwrap().is_empty());
}

#[tokio::test]
async fn an_out_of_memory_stop_still_shows_the_engine_s_failure_txt() {
    // The kernel killed the engine, not the coordinator, which still wrote this attempt's
    // failure.txt: Batch's account is the cause, the file the evidence.
    let text =
        "The engine exited with status 137. The end of its output:\n\nbuilding DAG\nKilled\n";
    let (mut state, cloud) = engine_failure_state(COORDINATOR_NOISE, Some(text));
    let mut detail = testing::run_detail("r1", "failed");
    detail.coordinator_log_stream = Some("s-coord".into());
    detail.coordinator_stop.exit_code = Some(137);
    detail.coordinator_stop.status_reason =
        Some("OutOfMemoryError: Container killed due to memory usage".into());
    state.store = state_with_run_detail(detail).store;
    let (_, v) = get_json(api::router(state), "/runs/r1/failure-summary").await;
    assert_eq!(v["items"][0]["cause"], "out_of_memory");
    assert_eq!(log_tail_of(&v), ["building DAG", "Killed"]);
    assert_eq!(*cloud.ranges.lock().unwrap(), [failure_txt_uri()]);
}

#[tokio::test]
async fn a_coordinator_batch_stopped_is_never_explained_from_failure_txt() {
    // Batch lost the host: the coordinator never ran its exit steps, so a failure.txt there is
    // another attempt's.
    let stale = "The engine exited with status 1. The end of its output:\n\nan earlier attempt\n";
    let (mut state, cloud) = engine_failure_state(&["the coordinator's last line"], Some(stale));
    let mut detail = testing::run_detail("r1", "failed");
    detail.coordinator_log_stream = Some("s-coord".into());
    detail.coordinator_stop.status_reason = Some("Host EC2 (instance i-1) terminated.".into());
    detail.coordinator_stop.termination_category = Some("host_terminated".into());
    state.store = state_with_run_detail(detail).store;
    let (_, v) = get_json(api::router(state), "/runs/r1/failure-summary").await;
    assert_eq!(v["items"][0]["cause"], "host_lost");
    assert_eq!(log_tail_of(&v), ["the coordinator's last line"]);
    assert!(cloud.ranges.lock().unwrap().is_empty());
}
