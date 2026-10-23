//! HTTP tests for the QC previews of a run's outputs (in-memory doubles, auth disabled, no network):
//! a FastQC result as data (`/outputs/{id}/fastqc`), what a genome browser needs to show an
//! alignment (`/outputs/{id}/alignment-view`), and the FastQC results `/reports` finds undeclared.
//! The FastQC zip and the BAM/BAI/CSI/CRAI/FASTA fixtures are real tools' output
//! (`tests/fixtures/README.md`); a CRAM's header is built here.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::http::StatusCode;
use serde_json::Value;

use biffix_api::ReferenceBundle;
use biffix_backend::api;
use biffix_backend::config::data_sources::{DataSource, DataSourceKind};
use biffix_backend::store::outputs::OutputRow;
use biffix_backend::testing::{self, get_json, FakeCloud, MemStore, TestState};
use biffix_backend::AppState;
use biffix_core::reducer::output_id;

const RUN: &str = "s3://proj/biffix/runs/r1/";
const FASTQC_ZIP: &[u8] = include_bytes!("../fixtures/fastqc/S1_R1_fastqc.zip");
const BAM: &[u8] = include_bytes!("../fixtures/igv/tiny.bam");
const BAI: &[u8] = include_bytes!("../fixtures/igv/tiny.bam.bai");
const CSI: &[u8] = include_bytes!("../fixtures/igv/tiny.bam.csi");
const CRAI: &[u8] = include_bytes!("../fixtures/igv/tiny.cram.crai");
const FA: &[u8] = include_bytes!("../fixtures/igv/tiny.fa");
const FAI: &[u8] = include_bytes!("../fixtures/igv/tiny.fa.fai");

fn output(rel: &str) -> OutputRow {
    testing::output_row("r1", &format!("{RUN}{rel}"), None, "output")
}

fn id_of(rel: &str) -> String {
    output_id("r1", &format!("{RUN}{rel}"))
}

/// Run `r1` in project bucket `proj` with the outputs `registered` (relative to its prefix), S3
/// holding `objects` (full URIs), and `refs` (bucket `refs`, prefix `genomes/`) as a data source.
fn state(
    registered: &[&str],
    objects: &[(&str, &[u8])],
    reference: Option<biffix_core::event::ReferenceUse>,
) -> (AppState, Arc<FakeCloud>) {
    let store = MemStore::new();
    let mut r1 = testing::run_detail("r1", "succeeded");
    r1.output_prefix = Some(RUN.into());
    r1.reference_bundle = reference.map(|r| serde_json::to_value(r).unwrap());
    store.insert(r1);
    store.insert_outputs("r1", registered.iter().map(|r| output(r)).collect());
    let cloud = Arc::new(FakeCloud::new());
    {
        let mut held = cloud.objects.lock().unwrap();
        for (uri, bytes) in objects {
            held.insert(uri.to_string(), bytes.to_vec());
        }
    }
    let state = TestState::new()
        .with_store(Arc::new(store))
        .with_cloud(cloud.clone())
        .with_projects(qc_project(false, false))
        .build();
    (state, cloud)
}

/// The one project (bucket `proj`, with a `refs` data source), with the deploy's viewer CORS flag
/// on its bucket (`project_viewer`) and on the source (`source_viewer`).
fn qc_project(
    project_viewer: bool,
    source_viewer: bool,
) -> Arc<biffix_backend::config::projects::ProjectsHandle> {
    testing::one_project(biffix_backend::config::projects::ProjectContext {
        viewer_cors: project_viewer,
        data_sources: vec![DataSource {
            id: "refs".into(),
            name: "References".into(),
            bucket: "refs".into(),
            prefix: "genomes/".into(),
            description: None,
            kind: DataSourceKind::Data,
            viewer_cors: source_viewer,
        }],
        ..testing::project("demo", "proj")
    })
}

// ── FastQC ──────────────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn a_fastqc_zip_is_read_as_its_modules_with_its_html_beside_it() {
    let zip = format!("{RUN}qc/S1_R1_fastqc.zip");
    let (state, _) =
        state(&["qc/S1_R1_fastqc.zip", "qc/S1_R1_fastqc.html"], &[(&zip, FASTQC_ZIP)], None);
    let app = || api::router(state.clone());
    for asked in ["qc/S1_R1_fastqc.zip", "qc/S1_R1_fastqc.html"] {
        let (status, v) =
            get_json(app(), &format!("/runs/r1/outputs/{}/fastqc", id_of(asked))).await;
        assert_eq!(status, StatusCode::OK, "{asked}: {v}");
        assert_eq!(v["output_id"], id_of("qc/S1_R1_fastqc.zip"));
        assert_eq!(v["html_output_id"], id_of("qc/S1_R1_fastqc.html"));
        assert_eq!(v["version"], "0.12.1");
        let modules = v["modules"].as_array().unwrap();
        assert_eq!(modules.len(), 10);
        assert_eq!(modules[0]["name"], "Basic Statistics");
        assert_eq!(modules[0]["status"], "pass");
        assert_eq!(modules[0]["columns"], serde_json::json!(["Measure", "Value"]));
        let statuses: Vec<&str> = modules.iter().map(|m| m["status"].as_str().unwrap()).collect();
        assert!(statuses.contains(&"fail") && statuses.contains(&"warn"));
    }
}

#[tokio::test]
async fn a_fastq_finds_the_zip_fastqc_named_after_it() {
    let zip = format!("{RUN}qc/fastqc/S1_R1_fastqc.zip");
    let (state, _) =
        state(&["reads/S1_R1.fastq.gz", "qc/fastqc/S1_R1_fastqc.zip"], &[(&zip, FASTQC_ZIP)], None);
    let (status, v) = get_json(
        api::router(state),
        &format!("/runs/r1/outputs/{}/fastqc", id_of("reads/S1_R1.fastq.gz")),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["output_id"], id_of("qc/fastqc/S1_R1_fastqc.zip"));
    assert!(v.get("html_output_id").is_none(), "no HTML registered");
}

/// The output id of the zip `/fastqc` finds for the FASTQ `fastq` among `registered` (all in S3),
/// or the status when it finds none.
async fn zip_for(fastq: &str, registered: &[&str]) -> Result<String, StatusCode> {
    let uris: Vec<String> = registered.iter().map(|r| format!("{RUN}{r}")).collect();
    let objects: Vec<(&str, &[u8])> = uris.iter().map(|u| (u.as_str(), FASTQC_ZIP)).collect();
    let mut all = registered.to_vec();
    all.push(fastq);
    let (state, _) = state(&all, &objects, None);
    let (status, v) =
        get_json(api::router(state), &format!("/runs/r1/outputs/{}/fastqc", id_of(fastq))).await;
    match status {
        StatusCode::OK => Ok(v["output_id"].as_str().unwrap().to_string()),
        status => Err(status),
    }
}

#[tokio::test]
async fn a_fastq_gets_only_a_zip_that_is_unambiguously_its_own() {
    // Another sample's zip whose name merely ends the same is not it.
    assert_eq!(
        zip_for("reads/S1_R1.fastq.gz", &["qc/AS1_R1_fastqc.zip"]).await,
        Err(StatusCode::NOT_FOUND)
    );
    // Under the FASTQ's own folder first, though another sorts before it.
    assert_eq!(
        zip_for("reads/S1_R1.fastq.gz", &["a/S1_R1_fastqc.zip", "reads/fastqc/S1_R1_fastqc.zip"])
            .await,
        Ok(id_of("reads/fastqc/S1_R1_fastqc.zip"))
    );
    // The shallowest under it; two as shallow are ambiguous.
    assert_eq!(
        zip_for("reads/S1_R1.fastq.gz", &["reads/S1_R1_fastqc.zip", "reads/qc/S1_R1_fastqc.zip"])
            .await,
        Ok(id_of("reads/S1_R1_fastqc.zip"))
    );
    assert_eq!(
        zip_for(
            "reads/S1_R1.fastq.gz",
            &["reads/raw/S1_R1_fastqc.zip", "reads/trim/S1_R1_fastqc.zip"]
        )
        .await,
        Err(StatusCode::NOT_FOUND)
    );
    // Files of one name per sample: each FASTQ gets its own sample's zip.
    let per_sample = [
        "results/S1/x.fastq.gz",
        "results/S1/fastqc/x_fastqc.zip",
        "results/S2/fastqc/x_fastqc.zip",
    ];
    assert_eq!(
        zip_for("results/S2/x.fastq.gz", &per_sample).await,
        Ok(id_of("results/S2/fastqc/x_fastqc.zip"))
    );
    // Only another sample's zip of that name: not this FASTQ's.
    assert_eq!(
        zip_for(
            "results/S2/x.fastq.gz",
            &["results/S1/x.fastq.gz", "results/S1/fastqc/x_fastqc.zip"]
        )
        .await,
        Err(StatusCode::NOT_FOUND)
    );
    // Elsewhere, two zips of the name are ambiguous.
    assert_eq!(
        zip_for("reads/S1_R1.fq.gz", &["qc/a/S1_R1_fastqc.zip", "qc/b/S1_R1_fastqc.zip"]).await,
        Err(StatusCode::NOT_FOUND)
    );
    // Any FASTQ FastQC reads, bzip2 too.
    assert_eq!(
        zip_for("reads/S1.fq.bz2", &["qc/S1_fastqc.zip"]).await,
        Ok(id_of("qc/S1_fastqc.zip"))
    );
}

#[tokio::test]
async fn a_fastqc_zip_too_large_to_read_is_422() {
    let zip = format!("{RUN}qc/S1_fastqc.zip");
    let big = vec![0u8; (16 << 20) + 1];
    let (state, _) = state(&["qc/S1_fastqc.zip"], &[(&zip, &big)], None);
    let (status, v) = get_json(
        api::router(state),
        &format!("/runs/r1/outputs/{}/fastqc", id_of("qc/S1_fastqc.zip")),
    )
    .await;
    // The size cap refuses it, not the parse (zeros are not a zip either, also a 422).
    assert_eq!(
        (status, v),
        (StatusCode::UNPROCESSABLE_ENTITY, Value::from("the FastQC zip is too large to read"))
    );
}

#[tokio::test]
async fn no_fastqc_result_is_404_and_a_bad_one_is_422() {
    let bad = format!("{RUN}qc/S2_fastqc.zip");
    let (state, _) = state(
        &[
            "reads/S1.fq",
            "qc/S1_fastqc.html",
            "qc/S2_fastqc.zip",
            "qc/S3_fastqc.zip",
            "results/a.bam",
        ],
        &[(&bad, b"not a zip")],
        None,
    );
    let app = || api::router(state.clone());
    let answer = |rel: &'static str| {
        let app = app();
        async move { get_json(app, &format!("/runs/r1/outputs/{}/fastqc", id_of(rel))).await }
    };
    let no_result = (StatusCode::NOT_FOUND, Value::from("no FastQC result for this output"));
    // A FASTQ with no zip; an HTML report whose zip was not registered; not FastQC at all: no
    // result, each told from an output the lookup never resolved.
    assert_eq!(answer("reads/S1.fq").await, no_result);
    assert_eq!(answer("qc/S1_fastqc.html").await, no_result);
    assert_eq!(answer("results/a.bam").await, no_result);
    assert_eq!(answer("qc/S2_fastqc.zip").await.0, StatusCode::UNPROCESSABLE_ENTITY);
    // Registered but gone from S3: a 404 of its own.
    let (status, v) = answer("qc/S3_fastqc.zip").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_ne!(v, no_result.1, "{v}");
}

#[tokio::test]
async fn a_fastqc_zip_outside_the_project_bucket_is_refused() {
    let store = MemStore::new();
    let mut r1 = testing::run_detail("r1", "succeeded");
    r1.output_prefix = Some(RUN.into());
    store.insert(r1);
    let uri = "s3://other/S1_fastqc.zip";
    let mut row = output("x");
    row.id = output_id("r1", uri);
    row.s3_uri = uri.into();
    store.insert_outputs("r1", vec![row]);
    let state = TestState::new()
        .with_store(Arc::new(store))
        .with_projects(testing::bucket_project("proj"))
        .build();
    let (status, _) =
        get_json(api::router(state), &format!("/runs/r1/outputs/{}/fastqc", output_id("r1", uri)))
            .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

// ── alignments ──────────────────────────────────────────────────────────────────────────────────

fn tiny_use() -> biffix_core::event::ReferenceUse {
    biffix_core::event::ReferenceUse {
        id: "tiny".into(),
        uris: [("fasta".to_string(), "s3://refs/genomes/tiny.fa".to_string())].into(),
    }
}

fn bundle(id: &str, uris: &[(&str, &str)], igv_genome: Option<&str>) -> ReferenceBundle {
    ReferenceBundle {
        id: id.into(),
        kind: "assembly".into(),
        display_name: format!("Bundle {id}"),
        uris: uris.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect::<BTreeMap<_, _>>(),
        notes: None,
        igv_genome: igv_genome.map(str::to_string),
        created_at_micros: 0,
        updated_at_micros: 0,
        created_by: None,
    }
}

/// A run with an indexed BAM and the `tiny` bundle (FASTA + .fai in the `refs` source).
async fn alignment_state() -> (AppState, Arc<FakeCloud>) {
    let bam = format!("{RUN}aligned/S1.bam");
    let bai = format!("{RUN}aligned/S1.bam.bai");
    let (state, cloud) = state(
        &["aligned/S1.bam", "aligned/S1.bam.bai", "aligned/S2.bam"],
        &[
            (&bam, BAM),
            (&bai, BAI),
            (&format!("{RUN}aligned/S2.bam"), BAM),
            ("s3://refs/genomes/tiny.fa", FA),
            ("s3://refs/genomes/tiny.fa.fai", FAI),
            ("s3://refs/genomes/genes.gtf", b"chrB\tx\tgene\t1\t9\t.\t+\t.\tgene_id \"g\";\n"),
        ],
        Some(tiny_use()),
    );
    state
        .references
        .create_bundle(&bundle(
            "tiny",
            &[("fasta", "s3://refs/genomes/tiny.fa"), ("gtf", "s3://refs/genomes/genes.gtf")],
            None,
        ))
        .await
        .unwrap();
    (state, cloud)
}

async fn view(state: &AppState, rel: &str, query: &str) -> (StatusCode, Value) {
    get_json(
        api::router(state.clone()),
        &format!("/runs/r1/outputs/{}/alignment-view{query}", id_of(rel)),
    )
    .await
}

#[tokio::test]
async fn an_alignment_view_links_the_bam_its_index_and_the_run_s_reference() {
    let (state, _) = alignment_state().await;
    let (status, v) = view(&state, "aligned/S1.bam", "").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let a = &v["alignment"];
    assert_eq!(a["name"], "S1.bam");
    assert_eq!(a["format"], "bam");
    // Stream links: an hour, no content type or disposition forced.
    assert_eq!(
        a["url"],
        "https://fake-presigned.example/proj/biffix/runs/r1/aligned/S1.bam?expires=3600&type=#"
    );
    assert_eq!(
        a["index_url"],
        "https://fake-presigned.example/proj/biffix/runs/r1/aligned/S1.bam.bai?expires=3600&type=#"
    );
    let r = &v["reference"];
    assert_eq!(r["source"], "bundle");
    assert_eq!(r["bundle_id"], "tiny");
    assert_eq!(r["display_name"], "Bundle tiny");
    assert!(r["fasta_url"].as_str().unwrap().contains("refs/genomes/tiny.fa?"));
    // Not named by the bundle: the .fai beside the FASTA.
    assert!(r["fai_url"].as_str().unwrap().contains("refs/genomes/tiny.fa.fai?"));
    assert!(r.get("gzi_url").is_none());
    // A small unindexed annotation is loaded whole.
    assert_eq!(r["annotation"]["format"], "gtf");
    assert!(r["annotation"].get("index_url").is_none());
    // The first contig with reads, from the index and the header.
    assert_eq!(v["locus"], "chrB:9001-11000");
    assert_eq!(v["bucket_cors"], "unknown");
    assert!(v["expires_at"].as_f64().unwrap() > 0.0);
}

#[tokio::test]
async fn bucket_cors_is_configured_only_when_every_bucket_has_the_rule() {
    let (mut state, _) = alignment_state().await;
    state.projects = qc_project(true, false);
    let (_, v) = view(&state, "aligned/S1.bam", "").await;
    assert_eq!(v["bucket_cors"], "unknown", "the refs bucket has no flag");
    state.projects = qc_project(true, true);
    let (_, v) = view(&state, "aligned/S1.bam", "").await;
    assert_eq!(v["bucket_cors"], "configured");
    state.projects = qc_project(false, true);
    let (_, v) = view(&state, "aligned/S1.bam", "").await;
    assert_eq!(v["bucket_cors"], "unknown", "the project bucket has no flag");
}

#[tokio::test]
async fn an_unregistered_index_beside_the_bam_is_found_in_s3() {
    let (state, cloud) = alignment_state().await;
    let (status, v) = view(&state, "aligned/S2.bam", "").await;
    assert_eq!(status, StatusCode::CONFLICT, "no index: {v}");
    cloud.objects.lock().unwrap().insert(format!("{RUN}aligned/S2.bai"), BAI.to_vec());
    let (status, v) = view(&state, "aligned/S2.bam", "").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert!(v["alignment"]["index_url"]
        .as_str()
        .unwrap()
        .ends_with("aligned/S2.bai?expires=3600&type=#"));
}

#[tokio::test]
async fn only_a_bam_or_cram_has_an_alignment_view() {
    let (state, _) = state(&["qc/S1_fastqc.html"], &[], None);
    // The extension refuses it, not the missing index (also a 409).
    assert_eq!(
        view(&state, "qc/S1_fastqc.html", "").await,
        (StatusCode::CONFLICT, Value::from("S1_fastqc.html is not a BAM or CRAM file"))
    );
}

#[tokio::test]
async fn a_run_without_a_reference_gets_none_and_may_ask_for_one() {
    let (state, _) = alignment_state().await;
    // The same BAM in a run that recorded no bundle.
    let store = MemStore::new();
    let mut r1 = testing::run_detail("r1", "succeeded");
    r1.output_prefix = Some(RUN.into());
    store.insert(r1);
    store.insert_outputs("r1", vec![output("aligned/S1.bam"), output("aligned/S1.bam.bai")]);
    let state = TestState::from(state).with_store(Arc::new(store)).build();
    let (status, v) = view(&state, "aligned/S1.bam", "").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert!(v.get("reference").is_none());
    // The locus still comes from the BAM itself.
    assert_eq!(v["locus"], "chrB:9001-11000");
    let (status, v) = view(&state, "aligned/S1.bam", "?reference=tiny").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["reference"]["bundle_id"], "tiny");
    let (status, _) = view(&state, "aligned/S1.bam", "?reference=nope").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_bundle_naming_a_hosted_genome_uses_it() {
    let (state, _) = alignment_state().await;
    state
        .references
        .create_bundle(&bundle("hg38", &[("fasta", "s3://refs/genomes/hg38.fa")], Some("hg38")))
        .await
        .unwrap();
    let (status, v) = view(&state, "aligned/S1.bam", "?reference=hg38").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(
        v["reference"],
        serde_json::json!({
            "source": "igv_genome", "genome": "hg38", "bundle_id": "hg38",
            "display_name": "Bundle hg38"
        })
    );
}

#[tokio::test]
async fn reference_files_outside_every_data_source_are_left_out_with_a_note() {
    let (state, _) = alignment_state().await;
    state
        .references
        .create_bundle(&bundle("elsewhere", &[("fasta", "s3://secret/genomes/x.fa")], None))
        .await
        .unwrap();
    let (status, v) = view(&state, "aligned/S1.bam", "?reference=elsewhere").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert!(v.get("reference").is_none());
    let notes: Vec<&str> =
        v["notes"].as_array().unwrap().iter().map(|n| n.as_str().unwrap()).collect();
    assert!(notes.iter().any(|n| n.contains("s3://secret/genomes/x.fa")), "{notes:?}");
}

#[tokio::test]
async fn a_deleted_bundle_still_shows_what_the_run_recorded() {
    let (state, _) = alignment_state().await;
    state.references.delete_bundle("tiny").await.unwrap();
    let (status, v) = view(&state, "aligned/S1.bam", "").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["reference"]["bundle_id"], "tiny");
    assert_eq!(v["reference"]["display_name"], "tiny");
    assert!(v["reference"]["fasta_url"].as_str().unwrap().contains("refs/genomes/tiny.fa?"));
}

/// The notes of a view.
fn notes(v: &Value) -> Vec<String> {
    v.get("notes")
        .and_then(Value::as_array)
        .map(|n| n.iter().map(|n| n.as_str().unwrap().to_string()).collect())
        .unwrap_or_default()
}

fn url(v: &Value) -> &str {
    v.as_str().unwrap_or_else(|| panic!("not a URL: {v}"))
}

#[tokio::test]
async fn an_edited_bundle_still_shows_the_genome_the_run_used() {
    let (state, cloud) = alignment_state().await;
    cloud.objects.lock().unwrap().extend([
        ("s3://refs/genomes/other.fa".to_string(), FA.to_vec()),
        ("s3://refs/genomes/other.fa.fai".to_string(), FAI.to_vec()),
    ]);
    // The bundle now holds another genome, its own .fai named.
    state
        .references
        .update_bundle(&bundle(
            "tiny",
            &[
                ("fasta", "s3://refs/genomes/other.fa"),
                ("fasta_index", "s3://refs/genomes/other.fa.fai"),
                ("gtf", "s3://refs/genomes/genes.gtf"),
            ],
            None,
        ))
        .await
        .unwrap();
    let (status, v) = view(&state, "aligned/S1.bam", "").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let r = &v["reference"];
    assert!(url(&r["fasta_url"]).contains("refs/genomes/tiny.fa?"), "{r}");
    // The index beside the FASTA the run used, not the bundle's new one; no annotation of the
    // new genome.
    assert!(url(&r["fai_url"]).contains("refs/genomes/tiny.fa.fai?"), "{r}");
    assert!(r.get("annotation").is_none(), "{r}");
    assert!(notes(&v).iter().any(|n| n.contains("changed since this run")), "{v}");
    // A hosted genome added to it since does not replace the FASTA the run used either.
    let mut hosted = bundle("tiny", &[("fasta", "s3://refs/genomes/other.fa")], Some("hg38"));
    state.references.update_bundle(&hosted).await.unwrap();
    let (_, v) = view(&state, "aligned/S1.bam", "").await;
    assert_eq!(v["reference"]["source"], "bundle", "{v}");
    // With the same FASTA, it does.
    hosted.uris = [("fasta".to_string(), "s3://refs/genomes/tiny.fa".to_string())].into();
    state.references.update_bundle(&hosted).await.unwrap();
    let (_, v) = view(&state, "aligned/S1.bam", "").await;
    assert_eq!(v["reference"]["source"], "igv_genome", "{v}");
}

#[tokio::test]
async fn the_reference_s_indexes_and_annotation_follow_the_bundle() {
    let (state, cloud) = alignment_state().await;
    let put = |uri: &str, bytes: &[u8]| {
        cloud.objects.lock().unwrap().insert(uri.to_string(), bytes.to_vec());
    };
    let make = |id: &'static str, uris: &'static [(&'static str, &'static str)]| {
        let state = state.clone();
        async move {
            state.references.create_bundle(&bundle(id, uris, None)).await.unwrap();
            view(&state, "aligned/S1.bam", &format!("?reference={id}")).await
        }
    };
    // A .fai the bundle names; a bgzipped FASTA with its .gzi; an annotation's own index key.
    put("s3://refs/genomes/named.fai", FAI);
    put("s3://refs/genomes/g.fa.gz", FA);
    put("s3://refs/genomes/g.fa.gz.gzi", b"gzi");
    put("s3://refs/genomes/genes.gtf.gz", b"gtf");
    put("s3://refs/genomes/genes.gtf.gz.tbi", b"tbi");
    let (status, v) = make(
        "indexed",
        &[
            ("fasta", "s3://refs/genomes/g.fa.gz"),
            ("fasta_index", "s3://refs/genomes/named.fai"),
            ("gtf", "s3://refs/genomes/genes.gtf.gz"),
            ("gtf_index", "s3://refs/genomes/genes.gtf.gz.tbi"),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let r = &v["reference"];
    assert!(url(&r["fai_url"]).contains("genomes/named.fai?"));
    assert!(url(&r["gzi_url"]).contains("genomes/g.fa.gz.gzi?"));
    assert!(url(&r["annotation"]["index_url"]).contains("genes.gtf.gz.tbi?"));
    assert_eq!(r["annotation"]["format"], "gtf");
    assert!(notes(&v).is_empty(), "{v}");

    // A bgzipped FASTA without a .gzi (a note); a .tbi found beside a GFF3; no .fai named, so the
    // one beside the FASTA.
    put("s3://refs/genomes/h.fa.gz", FA);
    put("s3://refs/genomes/h.fa.gz.fai", FAI);
    put("s3://refs/genomes/genes.gff3.gz", b"gff");
    put("s3://refs/genomes/genes.gff3.gz.tbi", b"tbi");
    let (status, v) = make(
        "gff",
        &[("fasta", "s3://refs/genomes/h.fa.gz"), ("gff3", "s3://refs/genomes/genes.gff3.gz")],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let r = &v["reference"];
    assert!(url(&r["fai_url"]).contains("genomes/h.fa.gz.fai?"));
    assert!(r.get("gzi_url").is_none());
    assert!(notes(&v).iter().any(|n| n.contains("no .gzi index")), "{v}");
    assert_eq!(r["annotation"]["format"], "gff3");
    assert!(url(&r["annotation"]["index_url"]).contains("genes.gff3.gz.tbi?"));

    // The format is the key's that named the file: a GFF3 under a path holding `.gtf` is GFF3.
    put("s3://refs/genomes/from.gtf_vs_gff/genes.gff3.gz", b"gff");
    put("s3://refs/genomes/from.gtf_vs_gff/genes.gff3.gz.tbi", b"tbi");
    let (status, v) = make(
        "gff-under-gtf",
        &[
            ("fasta", "s3://refs/genomes/h.fa.gz"),
            ("gff3", "s3://refs/genomes/from.gtf_vs_gff/genes.gff3.gz"),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["reference"]["annotation"]["format"], "gff3", "{v}");

    // An unindexed bgzipped annotation cannot be loaded whole, whatever its size (this one is
    // tiny): left out, with a note that says so.
    put("s3://refs/genomes/big.gtf.gz", b"gtf");
    let (_, v) = make(
        "unindexed",
        &[("fasta", "s3://refs/genomes/tiny.fa"), ("gtf", "s3://refs/genomes/big.gtf.gz")],
    )
    .await;
    assert!(v["reference"].get("annotation").is_none(), "{v}");
    assert!(notes(&v).iter().any(|n| n.contains("compressed without a tabix index")), "{v}");

    // No .fai at all: no reference, with a note.
    put("s3://refs/genomes/bare.fa", FA);
    let (status, v) = make("bare", &[("fasta", "s3://refs/genomes/bare.fa")]).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert!(v.get("reference").is_none(), "{v}");
    assert!(notes(&v).iter().any(|n| n.contains("no .fai")), "{v}");
}

#[tokio::test]
async fn a_bam_with_only_a_csi_is_read_through_it() {
    let bam = format!("{RUN}aligned/S1.bam");
    let (state, _) = state(
        &["aligned/S1.bam"],
        &[(&bam, BAM), (&format!("{RUN}aligned/S1.bam.csi"), CSI)],
        None,
    );
    let (status, v) = view(&state, "aligned/S1.bam", "").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert!(url(&v["alignment"]["index_url"]).contains("S1.bam.csi?"));
    assert_eq!(v["locus"], "chrB:9001-11000");
}

/// A CRAM 3.0's first bytes as samtools writes them (the file definition, the header container's
/// header, its gzip SAM header block) with an `@SQ` line per name of `contigs`; nothing after.
fn cram_header(contigs: &[&str]) -> Vec<u8> {
    use std::io::Write;
    let mut text = String::from("@HD\tVN:1.6\tSO:coordinate\n");
    for name in contigs {
        text.push_str(&format!("@SQ\tSN:{name}\tLN:12000\n"));
    }
    let mut payload = (text.len() as i32).to_le_bytes().to_vec();
    payload.extend(text.as_bytes());
    let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    enc.write_all(&payload).unwrap();
    let block = enc.finish().unwrap();
    assert!(block.len() < 0x80 && payload.len() < 0x80, "one-byte ITF8 sizes");
    let mut out = b"CRAM\x03\x00".to_vec();
    out.extend([0u8; 20]); // file id
    out.extend(0i32.to_le_bytes()); // container length
    out.extend([0, 0, 0, 0, 0, 0]); // reference, start, span, records; counter, bases
    out.extend([1, 1, 0]); // one block; one landmark, 0
    out.extend([0, 0, 0, 0]); // CRC32
    out.extend([1, 0, 0, block.len() as u8, payload.len() as u8]); // gzip, file header, id, sizes
    out.extend(block);
    out
}

#[tokio::test]
async fn a_cram_opens_on_the_contig_its_own_header_names() {
    let cram = format!("{RUN}aligned/S1.cram");
    // The CRAM's header lists chrB first: the CRAI's contig 1 is chrA, whatever the reference's
    // .fai order (chrA, chrB).
    let reordered = cram_header(&["chrB", "chrA"]);
    let (reordered_state, _) = state(
        &["aligned/S1.cram", "aligned/S1.cram.crai"],
        &[
            (&cram, reordered.as_slice()),
            (&format!("{RUN}aligned/S1.cram.crai"), CRAI),
            ("s3://refs/genomes/tiny.fa", FA),
            ("s3://refs/genomes/tiny.fa.fai", FAI),
        ],
        Some(tiny_use()),
    );
    let (status, v) = view(&reordered_state, "aligned/S1.cram", "").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["alignment"]["format"], "cram");
    assert!(url(&v["alignment"]["index_url"]).contains("S1.cram.crai?"));
    assert_eq!(v["locus"], "chrA:9001-11000");
    // In the usual order, chrB; and with no reference at all, still (the name is the CRAM's).
    let usual = cram_header(&["chrA", "chrB"]);
    let (plain, _) = state(
        &["aligned/S1.cram", "aligned/S1.cram.crai"],
        &[(&cram, usual.as_slice()), (&format!("{RUN}aligned/S1.cram.crai"), CRAI)],
        None,
    );
    let (status, v) = view(&plain, "aligned/S1.cram", "").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert!(v.get("reference").is_none());
    assert_eq!(v["locus"], "chrB:9001-11000");
}

#[tokio::test]
async fn locus_false_renews_the_links_without_reading_the_files() {
    let (state, cloud) = alignment_state().await;
    let (status, v) = view(&state, "aligned/S1.bam", "?locus=false").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert!(v.get("locus").is_none(), "{v}");
    assert!(url(&v["alignment"]["url"]).contains("aligned/S1.bam?"));
    assert!(url(&v["reference"]["fasta_url"]).contains("tiny.fa?"));
    assert!(
        cloud.ranges.lock().unwrap().is_empty(),
        "nothing read: {:?}",
        cloud.ranges.lock().unwrap()
    );
    let (_, v) = view(&state, "aligned/S1.bam", "?locus=true").await;
    assert_eq!(v["locus"], "chrB:9001-11000");
    assert!(!cloud.ranges.lock().unwrap().is_empty());
}

// ── undeclared FastQC results in /reports ───────────────────────────────────────────────────────

#[tokio::test]
async fn reports_list_undeclared_fastqc_zips_per_sample() {
    // The run's store is replaced below, with its sample sheet.
    let (state, _) = state(&[], &[], None);
    let store = MemStore::new();
    let mut r1 = testing::run_detail("r1", "succeeded");
    r1.output_prefix = Some(RUN.into());
    store.insert(r1);
    store.insert_outputs(
        "r1",
        ["qc/S1_R1_fastqc.zip", "qc/S1_R2_fastqc.zip", "qc/S10_fastqc.zip"]
            .iter()
            .map(|r| output(r))
            .collect(),
    );
    store.sample_specs.lock().unwrap().insert(
        "r1".into(),
        biffix_core::event::SampleSpec {
            ids: vec!["S1".into(), "S10".into()],
            ..Default::default()
        },
    );
    let state = TestState::from(state).with_store(Arc::new(store)).build();
    let (status, v) = get_json(api::router(state), "/runs/r1/reports").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let got: Vec<(&str, &str, &str, &str, &str)> = v["reports"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            assert!(r.get("declaration").is_none(), "no declaration made it: {r}");
            (
                r["name"].as_str().unwrap(),
                r["kind"].as_str().unwrap(),
                r["format"].as_str().unwrap(),
                r["sample"].as_str().unwrap(),
                r["output"]["path"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        got,
        [
            ("Per-sample QC", "detected", "fastqc", "S1", "qc/S1_R1_fastqc.zip"),
            ("Per-sample QC", "detected", "fastqc", "S1", "qc/S1_R2_fastqc.zip"),
            ("Per-sample QC", "detected", "fastqc", "S10", "qc/S10_fastqc.zip"),
        ]
    );
}

// ── FastQC statuses from MultiQC (/qc/fastqc-status) ───────────────────────────────────────────

/// MultiQC 1.35's own `multiqc_fastqc.txt` (FastQC 0.12.1 on `S1_R1`, `S1_R2` and `S10_R1`).
const MULTIQC_FASTQC: &[u8] = include_bytes!("../fixtures/multiqc/multiqc_fastqc.txt");
const MULTIQC_REPORT: &str = "results/multiqc/multiqc_report.html";

/// Run `r1` with its MultiQC report registered, S3 holding `objects`, and the sample ids `ids`; run
/// `r2` with no outputs.
fn multiqc_state(objects: &[(&str, &[u8])], ids: &[&str]) -> AppState {
    let (state, _) = state(&[], objects, None);
    let store = MemStore::new();
    let mut r1 = testing::run_detail("r1", "succeeded");
    r1.output_prefix = Some(RUN.into());
    store.insert(r1);
    store.insert(testing::run_detail("r2", "succeeded"));
    store.insert_outputs("r1", vec![output(MULTIQC_REPORT)]);
    store.sample_specs.lock().unwrap().insert(
        "r1".into(),
        biffix_core::event::SampleSpec {
            ids: ids.iter().map(std::string::ToString::to_string).collect(),
            ..Default::default()
        },
    );
    TestState::from(state).with_store(Arc::new(store)).build()
}

fn fastqc_table_uri() -> String {
    format!("{RUN}results/multiqc/multiqc_data/multiqc_fastqc.txt")
}

#[tokio::test]
async fn fastqc_statuses_come_from_multiqc_each_with_the_sample_that_owns_it() {
    let uri = fastqc_table_uri();
    let state = multiqc_state(&[(&uri, MULTIQC_FASTQC)], &["S1", "S10"]);
    let (status, v) = get_json(api::router(state), "/runs/r1/qc/fastqc-status").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["source"], "multiqc");
    assert_eq!(v["report_output_id"], id_of(MULTIQC_REPORT));
    let samples = v["samples"].as_array().unwrap();
    // In the run's sample order: S1 owns S1_R1 and S1_R2 (not S10_R1, which S10, longer, owns).
    let got: Vec<(&str, &str)> = samples
        .iter()
        .map(|s| (s["sample"].as_str().unwrap(), s["fastqc_name"].as_str().unwrap()))
        .collect();
    assert_eq!(got, [("S1", "S1_R1"), ("S1", "S1_R2"), ("S10", "S10_R1")]);
    let s10 = &samples[2];
    assert_eq!(s10["filename"], "S10_R1.fastq.gz");
    assert_eq!(s10["modules"].as_array().unwrap().len(), 10);
    assert_eq!(
        s10["modules"][1],
        serde_json::json!({"name": "Per base sequence quality", "status": "fail"})
    );
    assert_eq!(
        s10["stats"],
        serde_json::json!({
            "total_sequences": 1000, "total_bases": "100 kbp", "poor_quality": 0,
            "sequence_length": "100", "percent_gc": 30.0, "avg_sequence_length": 100.0,
            "median_sequence_length": 100.0, "percent_duplicates": 0.0
        })
    );
}

#[tokio::test]
async fn a_fastqc_status_no_sample_owns_comes_last_without_one() {
    let uri = fastqc_table_uri();
    let state = multiqc_state(&[(&uri, MULTIQC_FASTQC)], &["S1", "S1*"]);
    let (status, v) = get_json(api::router(state), "/runs/r1/qc/fastqc-status").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let got: Vec<(Option<&str>, &str)> = v["samples"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| (s["sample"].as_str(), s["fastqc_name"].as_str().unwrap()))
        .collect();
    assert_eq!(got, [(Some("S1"), "S1_R1"), (Some("S1"), "S1_R2"), (None, "S10_R1")]);
}

/// The `(status, body)` of the run's `/qc/fastqc-status`.
async fn fastqc_status(state: AppState, run: &str) -> (StatusCode, serde_json::Value) {
    get_json(api::router(state), &format!("/runs/{run}/qc/fastqc-status")).await
}

#[tokio::test]
async fn fastqc_statuses_are_404_without_multiqc_data_and_422_when_too_large() {
    // A run without a MultiQC report; a report without FastQC data beside it.
    let state = multiqc_state(&[], &["S1"]);
    assert_eq!(
        fastqc_status(state.clone(), "r2").await,
        (StatusCode::NOT_FOUND, "this run has no MultiQC report".into())
    );
    assert_eq!(
        fastqc_status(state, "r1").await,
        (StatusCode::NOT_FOUND, "the MultiQC report has no FastQC results".into())
    );
    // Past the read cap.
    let uri = fastqc_table_uri();
    let big = vec![b'x'; (16 << 20) + 1];
    let state = multiqc_state(&[(&uri, &big)], &["S1"]);
    assert_eq!(
        fastqc_status(state, "r1").await,
        (StatusCode::UNPROCESSABLE_ENTITY, "MultiQC's FastQC table is too large to show".into())
    );
    // Sections each under the cap, but past the cap for them all.
    let part = vec![b'x'; 12 << 20];
    let uris: Vec<String> = ["a", "b", "c"]
        .iter()
        .map(|a| format!("{RUN}results/multiqc/multiqc_data/multiqc_fastqc_{a}.txt"))
        .collect();
    let objects: Vec<(&str, &[u8])> = uris.iter().map(|u| (u.as_str(), &part[..])).collect();
    let state = multiqc_state(&objects, &["S1"]);
    assert_eq!(
        fastqc_status(state, "r1").await,
        (StatusCode::UNPROCESSABLE_ENTITY, "MultiQC's FastQC tables are too large to show".into())
    );
    // Not a table: no results, not an error.
    let state = multiqc_state(&[(&uri, b"\x00\xff garbage")], &["S1"]);
    let (status, v) = fastqc_status(state, "r1").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["samples"], serde_json::json!([]));
}

/// MultiQC 1.35's tables for its FastQC module run on raw and on trimmed reads, each section with
/// its own anchor (and no `multiqc_fastqc.txt`).
const MULTIQC_FASTQC_RAW: &[u8] =
    include_bytes!("../fixtures/multiqc/sections/multiqc_fastqc_fastqc_raw.txt");
const MULTIQC_FASTQC_TRIMMED: &[u8] =
    include_bytes!("../fixtures/multiqc/sections/multiqc_fastqc_fastqc_trimmed.txt");

fn data_uri(name: &str) -> String {
    format!("{RUN}results/multiqc/multiqc_data/{name}")
}

/// Each result's `(section, sample, MultiQC name)`.
fn sectioned(v: &serde_json::Value) -> Vec<(Option<&str>, Option<&str>, &str)> {
    v["samples"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| (s["section"].as_str(), s["sample"].as_str(), s["fastqc_name"].as_str().unwrap()))
        .collect()
}

#[tokio::test]
async fn every_fastqc_section_is_read_each_result_tagged_with_its_own() {
    let (raw, trimmed) =
        (data_uri("multiqc_fastqc_fastqc_raw.txt"), data_uri("multiqc_fastqc_fastqc_trimmed.txt"));
    // Not FastQC tables, though their names are close.
    let (screen, odd) =
        (data_uri("multiqc_fastq_screen.txt"), data_uri("multiqc_fastqc_raw/nested.txt"));
    let state = multiqc_state(
        &[
            (&trimmed, MULTIQC_FASTQC_TRIMMED),
            (&raw, MULTIQC_FASTQC_RAW),
            (&screen, b"Sample\tx\nS1_R1\tpass\n"),
            (&odd, MULTIQC_FASTQC),
        ],
        &["S1", "S10"],
    );
    let (status, v) = fastqc_status(state, "r1").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let raw = Some("fastqc_raw");
    let trimmed = Some("fastqc_trimmed");
    assert_eq!(
        sectioned(&v),
        [
            (raw, Some("S1"), "S1_R1"),
            (raw, Some("S1"), "S1_R2"),
            (raw, Some("S10"), "S10_R1"),
            (trimmed, Some("S1"), "S1_R1"),
            (trimmed, Some("S1"), "S1_R2"),
            (trimmed, Some("S10"), "S10_R1"),
        ]
    );
    assert_eq!(v["samples"][0]["stats"]["sequence_length"], "100");
    assert_eq!(v["samples"][3]["stats"]["sequence_length"], "80");
}

#[tokio::test]
async fn the_plain_fastqc_table_comes_before_a_repeated_section_s() {
    let (plain, repeat) = (fastqc_table_uri(), data_uri("multiqc_fastqc_fastqc-1.txt"));
    let state =
        multiqc_state(&[(&repeat, MULTIQC_FASTQC_TRIMMED), (&plain, MULTIQC_FASTQC)], &["S10"]);
    let (status, v) = fastqc_status(state, "r1").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let got = sectioned(&v);
    assert_eq!(
        got.iter().map(|(s, _, _)| *s).collect::<Vec<_>>(),
        [None, None, None, Some("fastqc-1"), Some("fastqc-1"), Some("fastqc-1")]
    );
    assert_eq!(got[0], (None, Some("S10"), "S10_R1"));
    assert!(v["samples"][0].get("section").is_none(), "{v}");
}

#[tokio::test]
async fn fastqc_data_in_another_format_is_a_404_that_says_so() {
    let sources = data_uri("multiqc_sources.json");
    let state = multiqc_state(&[(&sources, b"{}")], &["S1"]);
    let (status, v) = fastqc_status(state, "r1").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(v.as_str().unwrap().starts_with("MultiQC wrote its data as JSON"), "{v}");
}

/// [`zip_for`], with each registered output's role (`(path, role)`) and the FASTQ's own.
async fn zip_for_roles(
    fastq: (&str, &str),
    registered: &[(&str, &str)],
) -> Result<String, StatusCode> {
    let store = MemStore::new();
    let mut r1 = testing::run_detail("r1", "succeeded");
    r1.output_prefix = Some(RUN.into());
    store.insert(r1);
    let mut rows: Vec<OutputRow> = registered
        .iter()
        .map(|(rel, role)| OutputRow { role: role.to_string(), ..output(rel) })
        .collect();
    rows.push(OutputRow { role: fastq.1.to_string(), ..output(fastq.0) });
    store.insert_outputs("r1", rows);
    let cloud = Arc::new(FakeCloud::new());
    for (rel, _) in registered {
        cloud.objects.lock().unwrap().insert(format!("{RUN}{rel}"), FASTQC_ZIP.to_vec());
    }
    let state = TestState::new()
        .with_store(Arc::new(store))
        .with_cloud(cloud)
        .with_projects(qc_project(false, false))
        .build();
    let (status, v) =
        get_json(api::router(state), &format!("/runs/r1/outputs/{}/fastqc", id_of(fastq.0))).await;
    match status {
        StatusCode::OK => Ok(v["output_id"].as_str().unwrap().to_string()),
        status => Err(status),
    }
}

#[tokio::test]
async fn a_fastq_never_gets_the_zip_of_a_nearer_or_uncounted_fastq_of_its_name() {
    // A trimmed copy of the reads, of the same name, in a folder under the raw reads': the zip
    // beside it (or in its own `fastqc/`) is the trimmed reads', so the raw reads get none ...
    for trimmed_zip in ["reads/trimmed/S1_R1_fastqc.zip", "reads/trimmed/fastqc/S1_R1_fastqc.zip"] {
        let registered = ["reads/trimmed/S1_R1.fastq.gz", trimmed_zip, "qc/raw/S1_R1_fastqc.zip"];
        assert_eq!(
            zip_for("reads/S1_R1.fastq.gz", &registered).await,
            Err(StatusCode::NOT_FOUND),
            "{trimmed_zip}"
        );
        // ... and the trimmed reads get theirs.
        let mut with_raw = registered.to_vec();
        with_raw.push("reads/S1_R1.fastq.gz");
        assert_eq!(
            zip_for("reads/trimmed/S1_R1.fastq.gz", &with_raw).await,
            Ok(id_of(trimmed_zip)),
            "{trimmed_zip}"
        );
    }
    // A zip of the raw reads' own beside the trimmed one's is still the raw reads': the trimmed
    // reads' is set aside before the shallowest is picked.
    assert_eq!(
        zip_for(
            "reads/S1.fq.gz",
            &[
                "reads/trimmed/S1.fq.gz",
                "reads/trimmed/S1_fastqc.zip",
                "reads/fastqc/S1_fastqc.zip",
            ]
        )
        .await,
        Ok(id_of("reads/fastqc/S1_fastqc.zip"))
    );
    // A task's intermediate copy is not counted among the FASTQs of its name, so it gets no zip,
    // though the run's one published FASTQ of that name has one.
    let registered = [
        ("results/trimmed/reads_1.fastq.gz", "published"),
        ("results/qc/reads_1_fastqc.zip", "published"),
    ];
    assert_eq!(
        zip_for_roles(("work/ab/cd/reads_1.fastq.gz", "intermediate"), &registered).await,
        Err(StatusCode::NOT_FOUND)
    );
    let registered = [("work/ab/12/S1.fastq.gz", "intermediate"), ("qc/S1_fastqc.zip", "output")];
    assert_eq!(
        zip_for_roles(("results/S1.fastq.gz", "output"), &registered).await,
        Ok(id_of("qc/S1_fastqc.zip"))
    );
    let registered = [("results/S1.fastq.gz", "output"), ("qc/S1_fastqc.zip", "output")];
    assert_eq!(
        zip_for_roles(("work/ab/12/S1.fastq.gz", "intermediate"), &registered).await,
        Err(StatusCode::NOT_FOUND)
    );
}
