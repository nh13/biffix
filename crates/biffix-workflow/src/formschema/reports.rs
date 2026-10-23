//! The reports a workflow's runs write (`reports:` in `biffix.yaml`): their kinds and formats, and
//! how the form carries them.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::formschema::{is_false, FormSchema};

/// A report the workflow's runs write.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct FormReport {
    pub name: String,
    /// A path or glob relative to the run's output prefix (see [`crate::glob`]). A `per_sample`
    /// report's path holds `{sample}`.
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// How the report is shown, as the author declared it (one of [`REPORT_KINDS`]); `None` ⇒ from
    /// the file's name ([`report_format`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// One report per sample: `{sample}` in `path` is each of the run's sample ids in turn.
    #[serde(default, skip_serializing_if = "is_false")]
    pub per_sample: bool,
}

impl FormReport {
    /// Whether this is one report per sample: declared `per_sample` AND its path holds `{sample}`.
    /// A form frozen before registration checked that may say `per_sample` of a plain path; that
    /// path is one report.
    pub fn is_per_sample(&self) -> bool {
        self.per_sample && self.path.contains(SAMPLE_PLACEHOLDER)
    }
}

/// The ways a report can be shown (`kind:` in a `reports:` entry).
pub const REPORT_KINDS: [&str; 8] =
    ["html", "table", "image", "pdf", "alignment", "reads", "fastqc", "file"];

/// The endings of the files FastQC writes per input (`{stem}_fastqc.html`, `{stem}_fastqc.zip`),
/// lowercase: a report that is one opens in the FastQC viewer.
pub(crate) const FASTQC_SUFFIXES: [&str; 2] = ["_fastqc.html", "_fastqc.zip"];

/// What a `per_sample` report's path holds in place of the sample id.
pub const SAMPLE_PLACEHOLDER: &str = "{sample}";

/// How to show a report at `path`: its declared `kind` when that is one of [`REPORT_KINDS`], else
/// from the file's name: `fastqc` (FastQC's own `_fastqc.html`/`_fastqc.zip`), `alignment`
/// (BAM/CRAM) and `reads` (FASTQ), then the shared [`crate::viewer::viewer_of`] table, with its
/// `text` (and anything it cannot show) as `file`.
pub fn report_format(kind: Option<&str>, path: &str) -> &'static str {
    if let Some(known) = kind.and_then(|k| REPORT_KINDS.iter().find(|r| **r == k)) {
        return known;
    }
    let name = crate::text::basename(path);
    let lower = name.to_ascii_lowercase();
    let ends = |exts: &[&str]| exts.iter().any(|e| lower.ends_with(e));
    // FastQC's names have a stem: a bare `_fastqc.zip` is not one.
    if FASTQC_SUFFIXES.iter().any(|s| lower.len() > s.len() && lower.ends_with(s)) {
        "fastqc"
    } else if ends(&[".bam", ".cram"]) {
        "alignment"
    } else if ends(&[".fastq", ".fq", ".fastq.gz", ".fq.gz"]) {
        "reads"
    } else {
        // Plain text has no report viewer of its own: it is a file to open or download.
        match crate::viewer::viewer_of(name) {
            Some("text") | None => "file",
            Some(viewer) => viewer,
        }
    }
}

/// Attach the manifest's `reports:` to `form`, normalized by [`normalize_reports`].
pub fn attach_reports(form: &mut FormSchema, decls: &[crate::manifest::ReportDecl]) {
    let reports: Vec<FormReport> = decls
        .iter()
        .map(|d| FormReport {
            name: d.name.clone(),
            path: d.path.clone(),
            description: d.description.clone(),
            kind: d.kind.clone(),
            per_sample: d.per_sample,
        })
        .collect();
    let normalized = normalize_reports(&reports, &mut form.warnings);
    form.reports.extend(normalized);
}

/// `reports` as they are frozen with a version, trimmed, each problem pushed onto `warnings`: a
/// report without a name or with a path that could leave the run's folder is skipped; an unknown
/// `kind` is dropped (the file's name decides); `per_sample` without `{sample}` in the path is
/// ignored (the path is then one report). The CLI applies it to the manifest's `reports:` and the
/// server again to a form registered through the API; idempotent, so a CLI-derived form passes the
/// second time unchanged and unwarned.
pub fn normalize_reports(reports: &[FormReport], warnings: &mut Vec<String>) -> Vec<FormReport> {
    let mut out = Vec::new();
    for d in reports {
        let name = d.name.trim();
        let path = d.path.trim();
        if name.is_empty() {
            warnings.push(format!("reports: `{path}` has no name; skipped"));
            continue;
        }
        if !crate::glob::is_safe_relative(path) {
            warnings.push(format!(
                "reports: `{name}` path `{path}` must be relative to the run's folder; skipped"
            ));
            continue;
        }
        let kind = d.kind.as_deref().map(str::trim).filter(|k| !k.is_empty());
        let kind = match kind {
            Some(k) if REPORT_KINDS.contains(&k) => Some(k.to_string()),
            Some(k) => {
                warnings.push(format!(
                    "reports: `{name}` kind `{k}` is not one of {}; shown by its extension",
                    REPORT_KINDS.join(", ")
                ));
                None
            }
            None => None,
        };
        let per_sample = d.per_sample && path.contains(SAMPLE_PLACEHOLDER);
        if d.per_sample && !per_sample {
            warnings.push(format!(
                "reports: `{name}` is per_sample but its path has no `{SAMPLE_PLACEHOLDER}`; \
                 shown as one report"
            ));
        }
        out.push(FormReport {
            name: name.to_string(),
            path: path.to_string(),
            description: crate::formschema::fields::nonblank(d.description.as_deref()),
            kind,
            per_sample,
        });
    }
    out
}
