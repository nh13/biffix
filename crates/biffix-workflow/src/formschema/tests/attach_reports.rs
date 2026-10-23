use super::*;
use crate::manifest::BiffixManifest;

fn reports_of(yaml: &str) -> FormSchema {
    let manifest = BiffixManifest::parse_yaml(yaml).unwrap();
    let mut form = FormSchema::default();
    attach_reports(&mut form, &manifest.reports);
    form
}

#[test]
fn a_kind_and_per_sample_are_frozen_with_the_report() {
    let form = reports_of(
        "reports:\n\
             - { name: QC report, path: multiqc/multiqc_report.html }\n\
             - { name: Gene counts, path: salmon/counts.tsv, kind: table }\n\
             - { name: Alignments, path: 'star/{sample}.bam', per_sample: true, kind: alignment }\n",
    );
    assert!(form.warnings.is_empty(), "{:?}", form.warnings);
    let got: Vec<(Option<&str>, bool)> =
        form.reports.iter().map(|r| (r.kind.as_deref(), r.per_sample)).collect();
    assert_eq!(got, [(None, false), (Some("table"), false), (Some("alignment"), true)]);
    // An existing entry freezes exactly as before.
    let json = serde_json::to_value(&form.reports[0]).unwrap();
    assert_eq!(
        json,
        serde_json::json!({"name": "QC report", "path": "multiqc/multiqc_report.html"})
    );
}

#[test]
fn an_unknown_kind_or_a_per_sample_path_without_the_placeholder_is_warned_about() {
    let form = reports_of(
        "reports:\n\
             - { name: Counts, path: counts.tsv, kind: spreadsheet }\n\
             - { name: QC, path: qc/all.html, per_sample: true }\n",
    );
    assert_eq!(form.reports[0].kind, None);
    assert!(!form.reports[1].per_sample);
    assert_eq!(
        form.warnings,
        [
            "reports: `Counts` kind `spreadsheet` is not one of html, table, image, pdf, \
                 alignment, reads, fastqc, file; shown by its extension",
            "reports: `QC` is per_sample but its path has no `{sample}`; shown as one report",
        ]
    );
}

#[test]
fn normalizing_reports_again_changes_and_warns_nothing() {
    let form = reports_of(
        "reports:\n\
             - { name: Counts, path: counts.tsv, kind: spreadsheet }\n\
             - { name: ' Alignments ', path: 'star/{sample}.bam', per_sample: true, kind: alignment }\n",
    );
    let mut warnings = Vec::new();
    assert_eq!(normalize_reports(&form.reports, &mut warnings), form.reports);
    assert!(warnings.is_empty(), "{warnings:?}");
}

#[test]
fn a_report_registered_through_the_api_is_held_to_the_same_rules() {
    let raw = [
        FormReport {
            per_sample: true,
            kind: Some("spreadsheet".into()),
            ..FormReport { name: "QC".into(), path: "qc/all.html".into(), ..Default::default() }
        },
        FormReport { name: "Escapes".into(), path: "../other/x.html".into(), ..Default::default() },
    ];
    let mut warnings = Vec::new();
    let got = normalize_reports(&raw, &mut warnings);
    assert_eq!(got.len(), 1);
    assert!(!got[0].per_sample && got[0].kind.is_none());
    let expected = [
        "reports: `QC` kind `spreadsheet` is not one of",
        "reports: `QC` is per_sample but its path has no `{sample}`",
        "reports: `Escapes` path `../other/x.html` must be relative to the run's folder",
    ];
    assert_eq!(warnings.len(), expected.len(), "{warnings:?}");
    for (warning, prefix) in warnings.iter().zip(expected) {
        assert!(warning.starts_with(prefix), "{warning}");
    }
    // A frozen form that skipped the check is still read as one report.
    assert!(!raw[0].is_per_sample());
    let per_sample = FormReport { path: "qc/{sample}.html".into(), ..raw[0].clone() };
    assert!(per_sample.is_per_sample());
}

#[test]
fn a_report_s_format_is_its_kind_else_its_extension() {
    assert_eq!(report_format(Some("table"), "x.html"), "table");
    // A kind this build does not know falls back to the extension.
    assert_eq!(report_format(Some("spreadsheet"), "x.html"), "html");
    for (path, format) in [
        ("multiqc/multiqc_report.HTML", "html"),
        ("a/b.htm", "html"),
        ("counts.tsv", "table"),
        ("counts.csv", "table"),
        ("summary.txt", "table"),
        ("plot.png", "image"),
        ("plot.JPEG", "image"),
        ("plot.svg", "image"),
        ("plot.webp", "image"),
        ("report.pdf", "pdf"),
        ("s1.markdup.bam", "alignment"),
        ("s1.cram", "alignment"),
        ("s1_R1.fastq.gz", "reads"),
        ("s1.fq", "reads"),
        ("qc/S1_R1_fastqc.html", "fastqc"),
        ("qc/S1_R1_fastqc.ZIP", "fastqc"),
        ("qc/fastqc.html", "html"),
        ("qc/_fastqc.html", "html"),
        ("s1.vcf.gz", "table"),
        ("calls.bed", "table"),
        ("run.log", "file"),
        ("report.html.gz", "file"),
        ("html", "file"),
    ] {
        assert_eq!(report_format(None, path), format, "{path}");
    }
}
