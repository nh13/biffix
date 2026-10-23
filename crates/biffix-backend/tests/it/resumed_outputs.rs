//! HTTP tests for a resumed run's outputs (in-memory doubles, auth disabled, no network): a resume
//! reuses the files the run it resumes wrote (the engine skips those tasks, so the resumed run
//! registers no output for them), and its outputs, reports, output links and QC previews include
//! them, each naming the nearest run that registered it (`reused_from`).

use std::sync::Arc;

use axum::http::StatusCode;
use serde_json::Value;

use biffix_backend::api;
use biffix_backend::store::outputs::OutputRow;
use biffix_backend::store::registry::RegistryStore;
use biffix_backend::testing::{
    self, get_json, post_json, FakeCloud, MemRegistry, MemStore, TestState,
};
use biffix_backend::AppState;
use biffix_core::ids::RunId;
use biffix_core::reducer::output_id;
use biffix_workflow::engines::Engine;

/// The output prefix every run here shares (a resume writes where the run it resumes did).
const PREFIX: &str = "s3://proj/out/";

fn output(run: &str, rel: &str) -> OutputRow {
    let uri = if rel.starts_with("s3://") { rel.to_string() } else { format!("{PREFIX}{rel}") };
    testing::output_row(run, &uri, None, "output")
}

/// A finished run `id` in [`PREFIX`] (or `prefix`) of version `abc:hash`, resuming `from`.
fn run(store: &MemStore, id: &str, from: Option<&str>, prefix: &str) {
    let mut d = testing::run_detail(id, "succeeded");
    d.output_prefix = Some(prefix.into());
    d.resumed_from = from.map(RunId::new);
    d.run.workflow = Some("abc:hash".to_string().into());
    store.insert(d);
}

/// `r0` wrote the alignments, the read counts, a summary, a MultiQC report and a file outside the
/// output prefix; `r1` resumed it and wrote the summary again, a QC file and a file of its own
/// outside the prefix; `r2` resumed `r1`.
fn chain() -> MemStore {
    let store = MemStore::new();
    run(&store, "r0", None, PREFIX);
    run(&store, "r1", Some("r0"), PREFIX);
    run(&store, "r2", Some("r1"), PREFIX);
    store.insert_outputs(
        "r0",
        vec![
            output("r0", "aligned/S1.bam"),
            output("r0", "counts.tsv"),
            output("r0", "summary.html"),
            output("r0", "multiqc/multiqc_report.html"),
            output("r0", "s3://proj/elsewhere/x.txt"),
        ],
    );
    store.insert_outputs(
        "r1",
        vec![
            output("r1", "summary.html"),
            output("r1", "qc/S1.txt"),
            output("r1", "s3://proj/elsewhere/r1.txt"),
        ],
    );
    store.insert_outputs("r2", vec![output("r2", "final.txt")]);
    store
}

fn state(store: MemStore) -> AppState {
    TestState::new()
        .with_store(Arc::new(store))
        .with_projects(testing::bucket_project("proj"))
        .build()
}

/// The run's outputs as `(path, reused_from)`, in list order.
async fn outputs(state: AppState, run: &str) -> Vec<(String, Option<String>)> {
    let (status, v) =
        post_json(api::router(state), &format!("/runs/{run}/outputs/query"), serde_json::json!({}))
            .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let rows = v["outputs"].as_array().unwrap();
    assert_eq!(v["total"], rows.len(), "{v}");
    rows.iter()
        .map(|o| {
            (o["path"].as_str().unwrap().to_string(), o["reused_from"].as_str().map(str::to_string))
        })
        .collect()
}

fn owned(rows: &[(&str, Option<&str>)]) -> Vec<(String, Option<String>)> {
    rows.iter().map(|(p, r)| (p.to_string(), r.map(str::to_string))).collect()
}

#[tokio::test]
async fn a_resumed_run_lists_the_outputs_it_reused_and_its_own_win() {
    assert_eq!(
        outputs(state(chain()), "r1").await,
        owned(&[
            // The run's own output outside the shared prefix is still its own.
            ("elsewhere/r1.txt", None),
            ("aligned/S1.bam", Some("r0")),
            ("counts.tsv", Some("r0")),
            ("multiqc/multiqc_report.html", Some("r0")),
            ("qc/S1.txt", None),
            // Both wrote it: the run's own row, not the one it resumed.
            ("summary.html", None),
            // r0's file outside the shared prefix is not one r1 reused.
        ])
    );
    // The run it resumed lists only its own, with no `reused_from`.
    let r0 = outputs(state(chain()), "r0").await;
    assert_eq!(r0.len(), 5);
    assert!(r0.iter().all(|(_, from)| from.is_none()), "{r0:?}");
}

#[tokio::test]
async fn a_resume_of_a_resume_reaches_back_through_both() {
    assert_eq!(
        outputs(state(chain()), "r2").await,
        owned(&[
            ("aligned/S1.bam", Some("r0")),
            ("counts.tsv", Some("r0")),
            ("final.txt", None),
            ("multiqc/multiqc_report.html", Some("r0")),
            ("qc/S1.txt", Some("r1")),
            // The nearer run's row wins over the farther one's.
            ("summary.html", Some("r1")),
            // r1's own file outside the shared prefix is not one r2 reused.
        ])
    );
}

#[tokio::test]
async fn the_walk_back_stops_at_a_cycle_and_at_twenty_runs() {
    // A cycle (a hand-edited log; submit cannot make one): each lists the other's, once.
    let store = MemStore::new();
    run(&store, "a", Some("b"), PREFIX);
    run(&store, "b", Some("a"), PREFIX);
    store.insert_outputs("a", vec![output("a", "a.txt")]);
    store.insert_outputs("b", vec![output("b", "b.txt")]);
    assert_eq!(outputs(state(store), "a").await, owned(&[("a.txt", None), ("b.txt", Some("b"))]));

    // A chain of 25 resumes: the newest lists its own and those of the 20 runs before it.
    let store = MemStore::new();
    for i in 0..25 {
        let from = (i > 0).then(|| format!("c{:02}", i - 1));
        run(&store, &format!("c{i:02}"), from.as_deref(), PREFIX);
        store.insert_outputs(
            &format!("c{i:02}"),
            vec![output(&format!("c{i:02}"), &format!("f{i:02}.txt"))],
        );
    }
    let listed = outputs(state(store), "c24").await;
    assert_eq!(listed.len(), 21, "{listed:?}");
    assert_eq!(listed[0], ("f04.txt".to_string(), Some("c04".to_string())));
    assert_eq!(listed[20], ("f24.txt".to_string(), None));
}

#[tokio::test]
async fn a_run_in_another_prefix_shares_none_of_its_outputs() {
    let store = MemStore::new();
    run(&store, "r0", None, "s3://proj/elsewhere/");
    run(&store, "r1", Some("r0"), PREFIX);
    store.insert_outputs(
        "r0",
        vec![output("r0", "s3://proj/elsewhere/a.txt"), output("r0", "b.txt")],
    );
    store.insert_outputs("r1", vec![output("r1", "c.txt")]);
    // Not even r0's row under r1's prefix: r0 did not write where r1 does.
    assert_eq!(outputs(state(store), "r1").await, owned(&[("c.txt", None)]));
}

#[tokio::test]
async fn the_walk_back_stops_at_the_first_run_in_another_prefix() {
    // r2 (P) resumed r1 (Q), which resumed r0 (P): r1 reused none of r0's files, so neither did
    // r2, though r0 wrote where r2 does.
    let store = MemStore::new();
    run(&store, "r0", None, PREFIX);
    run(&store, "r1", Some("r0"), "s3://proj/other/");
    run(&store, "r2", Some("r1"), PREFIX);
    store.insert_outputs("r0", vec![output("r0", "a.txt")]);
    store.insert_outputs("r1", vec![output("r1", "s3://proj/other/b.txt")]);
    store.insert_outputs("r2", vec![output("r2", "c.txt")]);
    assert_eq!(outputs(state(store), "r2").await, owned(&[("c.txt", None)]));
}

async fn register_reports(registry: &MemRegistry, reports: Value) {
    registry
        .put_version(&testing::version_with_reports("smoke", "demo", Engine::Snakemake, reports))
        .await
        .unwrap();
}

#[tokio::test]
async fn a_resumed_run_s_reports_resolve_from_the_outputs_it_reused() {
    let registry = MemRegistry::new();
    register_reports(
        &registry,
        serde_json::json!([
            {"name": "Alignments", "path": "aligned/*.bam"},
            {"name": "Read counts", "path": "counts.tsv"},
            {"name": "Summary", "path": "summary.html"}
        ]),
    )
    .await;
    let state = TestState::from(state(chain())).with_registry(Arc::new(registry)).build();
    let (status, v) = get_json(api::router(state), "/runs/r1/reports").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let reports: Vec<(&str, &str, Option<&str>)> = v["reports"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["name"].as_str().unwrap(),
                r["kind"].as_str().unwrap(),
                r["output"]["reused_from"].as_str(),
            )
        })
        .collect();
    assert_eq!(
        reports,
        vec![
            ("Alignments", "declared", Some("r0")),
            ("Read counts", "declared", Some("r0")),
            ("Summary", "declared", None),
            ("MultiQC", "multiqc", Some("r0")),
        ]
    );
}

#[tokio::test]
async fn a_reused_output_opens_through_the_resumed_run() {
    let cloud = Arc::new(FakeCloud::new());
    cloud
        .objects
        .lock()
        .unwrap()
        .insert(format!("{PREFIX}counts.tsv"), b"sample\tn\nS1\t3\n".to_vec());
    let state = TestState::from(state(chain())).with_cloud(cloud).build();
    let app = || api::router(state.clone());
    let reused = output_id("r0", &format!("{PREFIX}counts.tsv"));
    let (status, v) = get_json(app(), &format!("/runs/r1/outputs/{reused}")).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["reused_from"], "r0");
    assert_eq!(v["path"], "counts.tsv");
    let (status, v) = get_json(app(), &format!("/runs/r1/outputs/{reused}/download")).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    // r0's object, whole (not a longer key that merely starts with it).
    let link = "https://fake-presigned.example/proj/out/counts.tsv?expires=600&type=\
                #attachment; filename=\"counts.tsv\"";
    assert_eq!(v["url"], link);
    let (status, v) = get_json(app(), &format!("/runs/r1/outputs/{reused}/preview")).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["text"], "sample\tn\nS1\t3\n");
    let (status, v) = get_json(app(), &format!("/runs/r2/outputs/{reused}/download")).await;
    assert_eq!(status, StatusCode::OK, "two resumes on");
    assert_eq!(v["url"], link, "two resumes on, still r0's object");

    // Not an output of r1: r0's row r1's own hides, and r0's file outside the prefix.
    for uri in [format!("{PREFIX}summary.html"), "s3://proj/elsewhere/x.txt".to_string()] {
        let id = output_id("r0", &uri);
        let (status, _): (StatusCode, Value) =
            get_json(app(), &format!("/runs/r1/outputs/{id}/download")).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{uri}");
    }
    // A later run's output is never the earlier one's.
    let later = output_id("r1", &format!("{PREFIX}qc/S1.txt"));
    let (status, _) = get_json(app(), &format!("/runs/r0/outputs/{later}")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_resumed_run_s_per_sample_reports_cover_its_own_samples_and_those_it_carried_over() {
    // r0 ran S1-S3; r1 resumed it with S2 dropped from its samplesheet, carrying over S1 (a resume
    // records the finished samples it reuses) and naming S4 itself. r0's S2 file is under the
    // shared prefix, but S2 is not one of r1's samples.
    let store = chain();
    for (run, ids, reused) in
        [("r0", vec!["S1", "S2", "S3"], vec![]), ("r1", vec!["S3", "S4"], vec!["S1"])]
    {
        store.sample_specs.lock().unwrap().insert(
            run.into(),
            biffix_core::event::SampleSpec {
                ids: ids.into_iter().map(String::from).collect(),
                reused: reused
                    .into_iter()
                    .map(|id| biffix_core::event::ReusedSample {
                        id: id.into(),
                        done: true,
                        ..Default::default()
                    })
                    .collect(),
                ..Default::default()
            },
        );
    }
    store.insert_outputs(
        "r0",
        ["S1", "S2", "S3"].map(|s| output("r0", &format!("bam/{s}.bam"))).to_vec(),
    );
    store.insert_outputs("r1", vec![output("r1", "bam/S3.bam"), output("r1", "bam/S4.bam")]);
    let registry = MemRegistry::new();
    register_reports(
        &registry,
        serde_json::json!([
            {"name": "BAM", "path": "bam/{sample}.bam", "per_sample": true}
        ]),
    )
    .await;
    let state = TestState::from(state(store)).with_registry(Arc::new(registry)).build();
    let (status, v) = get_json(api::router(state), "/runs/r1/reports").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let got: Vec<(&str, Option<&str>)> = v["reports"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["name"] == "BAM")
        .map(|r| (r["sample"].as_str().unwrap(), r["output"]["reused_from"].as_str()))
        .collect();
    // The samplesheet's order, then the carried-over sample; no S2.
    assert_eq!(got, [("S3", None), ("S4", None), ("S1", Some("r0"))]);
}

/// The output id of `rel` (under [`PREFIX`], unless a full URI) registered by `run`.
fn id_in(run: &str, rel: &str) -> String {
    output(run, rel).id
}

#[tokio::test]
async fn a_resumed_run_s_alignment_finds_the_index_a_run_it_resumed_registered() {
    // r1 wrote the BAM again but reused r0's index. Nothing is in S3, so only the registered
    // index can be found (the fallback HEADs find none).
    let store = chain();
    store.insert_outputs("r0", vec![output("r0", "aligned/S9.bam.bai")]);
    store.insert_outputs("r1", vec![output("r1", "aligned/S9.bam")]);
    let state = state(store);
    let (status, v) = get_json(
        api::router(state.clone()),
        &format!("/runs/r1/outputs/{}/alignment-view?locus=false", id_in("r1", "aligned/S9.bam")),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert!(
        v["alignment"]["index_url"].as_str().unwrap().contains("proj/out/aligned/S9.bam.bai"),
        "{v}"
    );
    // The run it resumed has no BAM of its own there; nor does r0 see r1's.
    let (status, _) = get_json(
        api::router(state),
        &format!("/runs/r0/outputs/{}/alignment-view?locus=false", id_in("r1", "aligned/S9.bam")),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

const FASTQC_ZIP: &[u8] = include_bytes!("../fixtures/fastqc/S1_R1_fastqc.zip");
const MULTIQC_FASTQC: &[u8] = include_bytes!("../fixtures/multiqc/multiqc_fastqc.txt");

#[tokio::test]
async fn a_resumed_run_s_fastqc_html_pairs_with_the_zip_a_run_it_resumed_registered() {
    let store = chain();
    store.insert_outputs("r0", vec![output("r0", "qc/S1_R1_fastqc.zip")]);
    store.insert_outputs("r1", vec![output("r1", "qc/S1_R1_fastqc.html")]);
    let cloud = Arc::new(FakeCloud::new());
    cloud
        .objects
        .lock()
        .unwrap()
        .insert(format!("{PREFIX}qc/S1_R1_fastqc.zip"), FASTQC_ZIP.to_vec());
    let state = TestState::from(state(store)).with_cloud(cloud).build();
    let zip = id_in("r0", "qc/S1_R1_fastqc.zip");
    let html = id_in("r1", "qc/S1_R1_fastqc.html");
    for asked in [&html, &zip] {
        let (status, v) =
            get_json(api::router(state.clone()), &format!("/runs/r1/outputs/{asked}/fastqc")).await;
        assert_eq!(status, StatusCode::OK, "{v}");
        assert_eq!(v["output_id"], zip.as_str());
        assert_eq!(v["html_output_id"], html.as_str());
    }
}

/// The chain with r0's MultiQC data beside its report (`multiqc/`), and r1's samples: S1 its own,
/// S10 carried over from r0.
fn multiqc_chain() -> AppState {
    let store = chain();
    store.sample_specs.lock().unwrap().insert(
        "r1".into(),
        biffix_core::event::SampleSpec {
            ids: vec!["S1".into()],
            reused: vec![biffix_core::event::ReusedSample {
                id: "S10".into(),
                done: true,
                ..Default::default()
            }],
            ..Default::default()
        },
    );
    let cloud = Arc::new(FakeCloud::new());
    {
        let mut held = cloud.objects.lock().unwrap();
        let data = format!("{PREFIX}multiqc/multiqc_data/");
        held.insert(
            format!("{data}multiqc_general_stats.txt"),
            b"Sample\tfastqc-percent_gc\nS1\t43.5\n".to_vec(),
        );
        held.insert(format!("{data}multiqc_fastqc.txt"), MULTIQC_FASTQC.to_vec());
    }
    TestState::from(state(store)).with_cloud(cloud).build()
}

#[tokio::test]
async fn a_resumed_run_s_qc_reads_the_multiqc_report_a_run_it_resumed_wrote() {
    let state = multiqc_chain();
    let report = id_in("r0", "multiqc/multiqc_report.html");
    let (status, v) = get_json(api::router(state.clone()), "/runs/r1/qc/general-stats").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["report_output_id"], report.as_str());
    assert_eq!(v["samples"][0]["name"], "S1");

    let (status, v) = get_json(api::router(state), "/runs/r1/qc/fastqc-status").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["report_output_id"], report.as_str());
    // The run's own sample, then the one it carried over from r0.
    let got: Vec<(&str, &str)> = v["samples"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| (s["sample"].as_str().unwrap(), s["fastqc_name"].as_str().unwrap()))
        .collect();
    assert_eq!(got, [("S1", "S1_R1"), ("S1", "S1_R2"), ("S10", "S10_R1")]);
}

#[tokio::test]
async fn a_resume_s_own_multiqc_report_wins_over_one_a_run_it_resumed_left_elsewhere() {
    // r1 (another workflow version) wrote its report deeper than r0's: r1's is the run's.
    let store = chain();
    store.insert_outputs("r1", vec![output("r1", "results/qc/multiqc_report.html")]);
    let (status, v) = get_json(api::router(state(store)), "/runs/r1/reports").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let multiqc = v["reports"].as_array().unwrap().iter().find(|r| r["kind"] == "multiqc").unwrap();
    assert_eq!(multiqc["output"]["path"], "results/qc/multiqc_report.html");
    assert!(multiqc["output"]["reused_from"].is_null(), "{multiqc}");
}

#[tokio::test]
async fn a_resumed_run_s_provenance_lists_the_outputs_it_reused_as_reused() {
    let mut store = chain();
    // A task of r0 made the alignments; r1 has a task of the same key, which did not.
    let mut rows = store.outputs.get_mut().unwrap().remove("r0").unwrap();
    rows[0].task_key = Some("align:sample=S1".into());
    store.insert_outputs("r0", rows);
    let (status, c) = get_json(api::router(state(store)), "/runs/r1/provenance").await;
    assert_eq!(status, StatusCode::OK, "{c}");
    let graph = c["@graph"].as_array().unwrap();
    let entity = |id: &str| graph.iter().find(|e| e["@id"] == id).cloned();
    let result: Vec<String> = entity("#run-r1").unwrap()["result"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["@id"].as_str().unwrap().to_string())
        .collect();
    // The results the outputs API lists: its own, and those it reused from r0.
    for uri in [
        "s3://proj/out/aligned/S1.bam",
        "s3://proj/out/counts.tsv",
        "s3://proj/out/qc/S1.txt",
        "s3://proj/out/summary.html",
    ] {
        assert!(result.contains(&uri.to_string()), "{uri}: {result:?}");
    }
    assert!(!result.contains(&"s3://proj/elsewhere/x.txt".to_string()));
    // A reused one says so, and names the run that made it; the run's own does not.
    let reused = entity("s3://proj/out/aligned/S1.bam").unwrap();
    assert_eq!(
        reused["description"],
        "Reused from run r0, which this run resumed: made there, not by this run's tasks."
    );
    assert!(entity("s3://proj/out/qc/S1.txt").unwrap()["description"].is_null());
    // No task of this run claims it as its result.
    let claimed = graph
        .iter()
        .filter(|e| e["@id"].as_str().is_some_and(|id| id.starts_with("#task/")))
        .any(|t| t["result"].to_string().contains("aligned/S1.bam"));
    assert!(!claimed);
}

#[tokio::test]
async fn a_resume_in_a_prefix_recorded_without_its_slash_reuses_what_the_coordinator_wrote_there() {
    // A legacy prefix recorded without its trailing `/`: the coordinator appended its folders to
    // it directly (`…/legacyresults/`).
    let legacy = "s3://proj/legacy";
    let store = MemStore::new();
    run(&store, "r0", None, legacy);
    run(&store, "r1", Some("r0"), legacy);
    store.insert_outputs("r0", vec![output("r0", "s3://proj/legacyresults/x.txt")]);
    assert_eq!(outputs(state(store), "r1").await, owned(&[("results/x.txt", Some("r0"))]));
}
