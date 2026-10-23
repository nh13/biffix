//! Every reader of a run reads the bucket it ran in.

use super::*;

/// A run labelled B whose recorded prefix is in `bucket`, with an output in each project's bucket
/// and a task with a log stream.
fn run_in(h: &Harness, run_id: &str, bucket: &str) {
    let mut detail = testing::run_detail(run_id, "succeeded");
    detail.run.project = Some(PROJECT_B.into());
    detail.run.engine = Some("nextflow".into());
    detail.output_prefix = Some(format!("s3://{bucket}/biffix/runs/{run_id}/"));
    detail.tasks = vec![testing::task_view("t", "succeeded", Some("stream"), Some("job"))];
    h.store.insert(detail);
    h.store.insert_outputs(
        run_id,
        vec![
            output_row(run_id, &format!("s3://{BUCKET_A}/biffix/runs/{run_id}/a.txt")),
            output_row(run_id, &format!("s3://{BUCKET_B}/biffix/runs/{run_id}/b.txt")),
        ],
    );
}

#[tokio::test]
async fn a_run_s_outputs_and_logs_are_read_in_the_project_it_ran_in() {
    let h = harness();
    run_in(&h, "rb", BUCKET_B);
    // A run labelled B that ran in A's bucket is read in A.
    run_in(&h, "rlegacy", BUCKET_A);
    for (run, bucket, group) in
        [("rb", BUCKET_B, LOG_GROUP_B), ("rlegacy", BUCKET_A, "/biffix/project-genomics/batch")]
    {
        let (status, v) = h.post(&format!("/runs/{run}/outputs/query"), json!({})).await;
        assert_eq!(status, StatusCode::OK, "{v}");
        // Both rows, each servable only in the bucket the run ran in.
        assert_eq!(
            servable_outputs(&v),
            [
                (format!("s3://{BUCKET_A}/biffix/runs/{run}/a.txt"), bucket == BUCKET_A),
                (format!("s3://{BUCKET_B}/biffix/runs/{run}/b.txt"), bucket == BUCKET_B),
            ],
            "{run}"
        );
        let (status, v) = h.post(&format!("/runs/{run}/tasks/query"), json!({})).await;
        assert_eq!(status, StatusCode::OK, "{v}");
        assert_eq!(v["log_group"], group, "{run}");
    }
}

#[tokio::test]
async fn every_reader_of_a_run_reads_the_bucket_it_ran_in() {
    let h = harness();
    run_in(&h, "rb", BUCKET_B);
    let (a, b) = (
        format!("s3://{BUCKET_A}/biffix/runs/rb/a.html"),
        format!("s3://{BUCKET_B}/biffix/runs/rb/b.html"),
    );
    let zip = |bucket: &str| format!("s3://{bucket}/biffix/runs/rb/qc/S1_fastqc.zip");
    let bam = |bucket: &str| format!("s3://{bucket}/biffix/runs/rb/S1.bam");
    let report = format!("s3://{BUCKET_B}/biffix/runs/rb/multiqc/multiqc_report.html");
    let mut rows: Vec<OutputRow> = [zip(BUCKET_A), zip(BUCKET_B), bam(BUCKET_A), report.clone()]
        .iter()
        .map(|u| output_row("rb", u))
        .collect();
    rows.extend([output_row("rb", &a), output_row("rb", &b)]);
    h.store.insert_outputs("rb", rows);
    {
        let mut objects = h.cloud.objects.lock().unwrap();
        // A's FastQC zip and BAM (with its index) exist too, so only the project-bucket guard can
        // refuse them below, not a missing object or index.
        for uri in [&a, &b, &zip(BUCKET_A), &bam(BUCKET_A)] {
            objects.insert(uri.clone(), b"x".to_vec());
        }
        objects.insert(format!("{}.bai", bam(BUCKET_A)), b"x".to_vec());
        objects.insert(
            format!(
                "s3://{BUCKET_B}/biffix/runs/rb/multiqc/multiqc_data/multiqc_general_stats.txt"
            ),
            b"Sample\tgc\nS1\t40\n".to_vec(),
        );
    }
    // One output: servable only in B.
    for (uri, own) in [(&a, false), (&b, true)] {
        let id = output_id("rb", uri);
        let (status, v) = h.get(&format!("/runs/rb/outputs/{id}")).await;
        assert_eq!(status, StatusCode::OK, "{v}");
        assert_eq!(v["servable"], own, "{uri}");
        for link in ["view-url", "download"] {
            let (status, _) = h.get(&format!("/runs/rb/outputs/{id}/{link}")).await;
            let ok = status.is_success() || status.is_redirection();
            assert_eq!(ok, own, "{link} {uri}: {status}");
        }
    }
    // FastQC and the alignment view refuse A's objects for a run that ran in B.
    let outside = Value::from(
        "biffix does not serve this output (it is outside the project bucket, or a staged \
         workflow source)",
    );
    for (route, uri) in [("fastqc", zip(BUCKET_A)), ("alignment-view", bam(BUCKET_A))] {
        let id = output_id("rb", &uri);
        let got = h.get(&format!("/runs/rb/outputs/{id}/{route}")).await;
        assert_eq!(got, (StatusCode::FORBIDDEN, outside.clone()), "{route}");
    }
    // Reports: the FastQC results found under the run's own (B) prefix.
    let (status, v) = h.get("/runs/rb/reports").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let paths: Vec<&str> = v["reports"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|r| r["output"]["s3_uri"].as_str())
        .collect();
    assert!(paths.contains(&zip(BUCKET_B).as_str()), "{v}");
    assert!(!paths.contains(&zip(BUCKET_A).as_str()), "{v}");
    // MultiQC's General Statistics beside a report in B.
    let (status, v) = h.get("/runs/rb/qc/general-stats").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["samples"][0]["name"], "S1");
    // The same report for a run that ran in A is outside its bucket, and B's folder is not even
    // listed for it: a General Statistics written in another format there goes unmentioned.
    h.cloud.objects.lock().unwrap().insert(
        format!("s3://{BUCKET_B}/biffix/runs/rb/multiqc/multiqc_data/multiqc_general_stats.csv"),
        b"Sample,gc\nS1,40\n".to_vec(),
    );
    run_in(&h, "ra", BUCKET_A);
    h.store.insert_outputs("ra", vec![output_row("ra", &report)]);
    assert_eq!(
        h.get("/runs/ra/qc/general-stats").await,
        (StatusCode::NOT_FOUND, "the MultiQC data is outside the project bucket".into())
    );
    let (status, v) = h.get("/runs/ra/qc/fastqc-status").await;
    assert_eq!(
        (status, v),
        (StatusCode::NOT_FOUND, "the MultiQC data is outside the project bucket".into())
    );
}

#[tokio::test]
async fn a_run_s_provenance_is_written_into_its_project_bucket() {
    let h = harness();
    run_in(&h, "rb", BUCKET_B);
    run_in(&h, "rlegacy", BUCKET_A);
    *h.store.provenance_due.lock().unwrap() =
        vec![(RunId::new("rb"), 1), (RunId::new("rlegacy"), 1)];
    assert_eq!(biffix_backend::runs::provenance::write_due(&h.state).await.unwrap(), 2);
    let objects = h.cloud.objects.lock().unwrap();
    for uri in [
        format!("s3://{BUCKET_B}/biffix/runs/rb/provenance/ro-crate-metadata.json"),
        format!("s3://{BUCKET_A}/biffix/runs/rlegacy/provenance/ro-crate-metadata.json"),
    ] {
        assert!(objects.contains_key(&uri), "{uri}");
    }
}
