//! `biffix workflows form`: preview the launch form a checkout would register with, without
//! registering it.

use std::path::Path;

use biffix_workflow::engines::Engine;
use biffix_workflow::formschema::FormSchema;

use crate::commands::Session;
use crate::error::{CliError, CliResult};
use crate::local::form::prepare_form;
use crate::local::source;
use crate::local::source::SourceMode;
use crate::render::OutputMode;

use super::infer_name;

/// Options for `workflows form`.
pub(crate) struct FormOpts {
    pub(crate) path: String,
    /// A manifest to use instead of the workflow's `biffix.yaml`.
    pub(crate) manifest: Option<String>,
    /// JSON prints the FormSchema exactly as `register` sends it; human, a summary.
    pub(crate) output: OutputMode,
}

/// Preview the launch form a checkout would register with, without registering it: a summary (or
/// the FormSchema as JSON), and with a `session` (`--open`) the form itself opened in that
/// deployment's UI. Nothing is sent to the deployment: the form rides in the URL's fragment.
pub(crate) fn form(session: Option<&Session>, opts: FormOpts) -> CliResult<()> {
    let root = Path::new(&opts.path);
    // Detected from the source a (default, git-mode) register would ship.
    let files = source::shipped_files(root, SourceMode::Git, None)?;
    let (_, engine, form_schema) =
        prepare_form(root, opts.manifest.as_deref().map(Path::new), &files)?;
    match opts.output {
        OutputMode::Json => {
            // What `register` prints, which the human summary lists among its warnings.
            crate::local::form::warn_form(&form_schema);
            crate::render::print_json(&form_schema);
        }
        OutputMode::Human => print!("{}", form_summary(&form_schema, engine)),
    }
    if let Some(session) = session {
        let name = infer_name(root).unwrap_or_else(|| "workflow".into());
        let ui = ui_base(&session.api_base).unwrap_or_else(|| {
            let base = session.api_base.trim_end_matches('/');
            eprintln!(
                "[biffix] warning: the API base URL {base} does not end in /api, so the UI is \
                 taken to be at the same address; the preview may not open there"
            );
            base
        });
        let url = preview_url(ui, &name, engine, &form_schema)?;
        if open::that(&url).is_err() {
            eprintln!("[biffix] open this URL to see the form:\n{url}");
        } else {
            eprintln!("[biffix] opened the form preview in your browser");
        }
    }
    Ok(())
}

/// The UI's preview page for a form: `/workflows/preview#<payload>`, where the payload is the
/// base64url of the zlib-deflated `{name, engine, form_schema}` JSON. A fragment never reaches the
/// server, so an unregistered form stays on the author's machine. `ui` is the UI's origin.
fn preview_url(ui: &str, name: &str, engine: Engine, form: &FormSchema) -> CliResult<String> {
    use base64::Engine as _;
    use std::io::Write as _;
    let json = serde_json::to_vec(&serde_json::json!({
        "name": name, "engine": engine.as_str(), "form_schema": form,
    }))
    .map_err(|e| CliError::Unclassified(format!("cannot encode the form: {e}")))?;
    let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
    let deflated = z
        .write_all(&json)
        .and_then(|()| z.finish())
        .map_err(|e| CliError::Unclassified(format!("cannot compress the form: {e}")))?;
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(deflated);
    Ok(format!("{ui}/workflows/preview#{payload}"))
}

/// The UI's origin for an API base URL (`https://host/api` → `https://host`); `None` for a base
/// that does not end in `/api`, which a deployment's always does.
fn ui_base(api_base: &str) -> Option<&str> {
    api_base.trim_end_matches('/').strip_suffix("/api")
}

/// A readable outline of a form: its sections and their fields (widget, required, default,
/// presets, show-if), its examples and its warnings.
fn form_summary(form: &FormSchema, engine: Engine) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let title = form.title.as_deref().unwrap_or("(untitled form)");
    let curated = if form.curated { "curated" } else { "not curated" };
    let _ = writeln!(out, "{title} — {} form, {curated}", engine.as_str());
    if let Some(d) = form.description.as_deref().filter(|d| !d.trim().is_empty()) {
        let _ = writeln!(out, "{}", d.trim());
    }
    if let Some(r) = &form.references {
        let fills: Vec<String> = r.fill.iter().map(|(p, k)| format!("{p} ← {k}")).collect();
        let kind = r.kind.as_deref().map(|k| format!(" ({k})")).unwrap_or_default();
        let own = if r.custom { "; own files allowed" } else { "" };
        let _ = writeln!(
            out,
            "\n{}: reference dropdown{kind} filling {}{own}",
            r.label,
            fills.join(", ")
        );
    }

    // Fields by section, in the form's section order; fields of no (or an unknown) section last.
    let known: std::collections::HashSet<&str> =
        form.sections.iter().map(|s| s.id.as_str()).collect();
    let mut groups: Vec<(String, Vec<&biffix_workflow::formschema::FormField>)> = form
        .sections
        .iter()
        .map(|s| {
            let advanced = if s.advanced { " (advanced)" } else { "" };
            let fields = form
                .fields
                .iter()
                .filter(|f| f.section.as_deref() == Some(s.id.as_str()))
                .collect();
            (format!("{}{advanced}", s.title), fields)
        })
        .collect();
    let loose: Vec<_> = form
        .fields
        .iter()
        .filter(|f| !f.section.as_deref().is_some_and(|s| known.contains(s)))
        .collect();
    if !loose.is_empty() {
        let title = if form.sections.is_empty() { "Parameters" } else { "Other parameters" };
        groups.push((title.to_string(), loose));
    }
    for (title, fields) in groups.iter().filter(|(_, f)| !f.is_empty()) {
        let _ = writeln!(out, "\n{title}");
        for f in fields {
            let _ = writeln!(out, "  {}", field_line(f));
        }
    }

    if !form.examples.is_empty() {
        let names: Vec<&str> = form.examples.iter().map(|e| e.name.as_str()).collect();
        let _ = writeln!(out, "\nExamples: {}", names.join(", "));
    }
    if !form.reports.is_empty() {
        let _ = writeln!(out, "\nReports");
        for r in &form.reports {
            let _ = writeln!(out, "  {} — {}", r.name, r.path);
        }
    }
    if form.cache {
        let _ = writeln!(out, "\nCache: results of `cache: True` rules are reused across runs");
    }
    // The registration lint for params with nothing to explain them, beside the form's own.
    let warnings: Vec<String> =
        form.warnings.iter().cloned().chain(biffix_workflow::formschema::help_lint(form)).collect();
    if warnings.is_empty() {
        let _ = writeln!(out, "\nNo warnings.");
    } else {
        let _ = writeln!(out, "\nWarnings:");
        for w in &warnings {
            let _ = writeln!(out, "  - {w}");
        }
    }
    out
}

/// One field's line in [`form_summary`]: `name "Label" — widget, required, default 3, …`.
fn field_line(f: &biffix_workflow::formschema::FormField) -> String {
    use biffix_workflow::formschema::WidgetHint;
    let value = |v: &serde_json::Value| match v {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    let widget = match f.widget {
        WidgetHint::Text if f.multiline => "text (multi-line)",
        WidgetHint::Text => "text",
        WidgetHint::Number => "number",
        WidgetHint::Enum => "choice",
        WidgetHint::S3File => "file",
        WidgetHint::S3Dir => "folder",
        WidgetHint::ReferenceBundle => "reference",
        WidgetHint::Samplesheet => "samplesheet",
    };
    let mut parts = vec![widget.to_string()];
    for (on, flag) in [
        (f.required, "required"),
        (f.hidden, "hidden"),
        (f.is_output, "output"),
        (f.is_secret, "secret"),
    ] {
        if on {
            parts.push(flag.into());
        }
    }
    if let Some(d) = &f.default {
        parts.push(format!("default {}", value(d)));
    }
    if let Some(values) = f.enum_values.as_ref().filter(|v| !v.is_empty()) {
        parts.push(format!("one of {}", values.join("|")));
    }
    if !f.presets.is_empty() {
        let presets: Vec<String> =
            f.presets.iter().map(|p| format!("{} = {}", p.label, value(&p.value))).collect();
        parts.push(format!("presets {}", presets.join(", ")));
    }
    for cond in &f.show_if {
        let values: Vec<String> = cond.values.iter().map(value).collect();
        parts.push(format!("shown when {} is {}", cond.field, values.join("|")));
    }
    let label = f.label.as_deref().map(|l| format!(" \"{l}\"")).unwrap_or_default();
    format!("{}{label} — {}", f.name, parts.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local::source::SourceFiles;

    /// A small curated Nextflow workflow: a schema with two sections, and a `form:` overlay.
    /// What `form_workflow`'s source holds, for engine detection.
    fn nf_files() -> SourceFiles {
        SourceFiles::of(&["main.nf", "nextflow_schema.json"])
    }

    fn form_workflow() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("main.nf"), "workflow {}\n").unwrap();
        std::fs::write(
            dir.path().join("nextflow_schema.json"),
            r##"{"$defs": {
                "io": {"title": "Input/output", "required": ["outdir"], "properties": {
                    "outdir": {"type": "string", "format": "directory-path"}}},
                "tuning": {"title": "Tuning", "properties": {
                    "mode": {"type": "string", "enum": ["fast", "slow"], "default": "fast"},
                    "depth": {"type": "integer", "default": 3}}}},
               "allOf": [{"$ref": "#/$defs/io"}, {"$ref": "#/$defs/tuning"}]}"##,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("biffix.yaml"),
            "form:\n  title: Hello form\n  params:\n    depth:\n      label: Depth\n      \
             presets: {Deep: 9}\n      show_if: {mode: slow}\n  \
             examples:\n    - name: Quick\n      params: {outdir: s3://b/out}\n",
        )
        .unwrap();
        dir
    }

    #[test]
    fn the_form_summary_outlines_sections_fields_and_examples() {
        let dir = form_workflow();
        let (_, engine, form) = prepare_form(dir.path(), None, &nf_files()).unwrap();
        let summary = form_summary(&form, engine);
        assert!(summary.starts_with("Hello form — nextflow form, curated\n"), "{summary}");
        assert!(
            summary.contains("\nInput/output\n  outdir — folder, required, output\n"),
            "{summary}"
        );
        assert!(
            summary.contains(
                "  depth \"Depth\" — number, default 3, presets Deep = 9, shown when mode is slow\n"
            ),
            "{summary}"
        );
        assert!(summary.contains("  mode — choice, default fast, one of fast|slow\n"), "{summary}");
        assert!(summary.contains("\nExamples: Quick\n"), "{summary}");
    }

    #[test]
    fn cache_is_frozen_for_snakemake_and_refused_for_nextflow() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Snakefile"), "rule all:\n    input: []\n").unwrap();
        let manifest = dir.path().join("biffix.yaml");
        std::fs::write(&manifest, "engine: snakemake\ncache: true\n").unwrap();
        let (_, engine, form) =
            prepare_form(dir.path(), Some(&manifest), &SourceFiles::of(&["Snakefile"])).unwrap();
        assert!(form.cache);
        assert!(form_summary(&form, engine).contains("\nCache: results of `cache: True` rules"));

        let nf = form_workflow();
        let manifest = nf.path().join("with-cache.yaml");
        std::fs::write(&manifest, "cache: true\n").unwrap();
        let err = prepare_form(nf.path(), Some(&manifest), &nf_files()).unwrap_err();
        assert!(
            matches!(err, CliError::InvalidInput(ref m) if m.contains("only to Snakemake")),
            "{err:?}"
        );
    }

    #[test]
    fn declared_reports_are_frozen_with_the_form_and_unsafe_paths_skipped() {
        let dir = form_workflow();
        let manifest = dir.path().join("with-reports.yaml");
        std::fs::write(
            &manifest,
            "reports:\n  - {name: MultiQC, path: multiqc/multiqc_report.html, description: QC}\n  \
             - {name: Escape, path: ../secret.html}\n",
        )
        .unwrap();
        let (_, engine, form) = prepare_form(dir.path(), Some(&manifest), &nf_files()).unwrap();
        assert_eq!(form.reports.len(), 1);
        assert_eq!(form.reports[0].path, "multiqc/multiqc_report.html");
        assert_eq!(form.reports[0].description.as_deref(), Some("QC"));
        assert!(form.warnings.iter().any(|w| w.contains("`Escape`")), "{:?}", form.warnings);
        let summary = form_summary(&form, engine);
        assert!(
            summary.contains("\nReports\n  MultiQC — multiqc/multiqc_report.html\n"),
            "{summary}"
        );
    }

    #[test]
    fn another_manifest_can_stand_in_for_biffix_yaml() {
        let dir = form_workflow();
        let other = dir.path().join("other.yaml");
        std::fs::write(&other, "form:\n  title: Other title\n").unwrap();
        let (_, _, form) = prepare_form(dir.path(), Some(&other), &nf_files()).unwrap();
        assert_eq!(form.title.as_deref(), Some("Other title"));
    }

    #[test]
    fn the_preview_url_carries_the_form_in_its_fragment() {
        use base64::Engine as _;
        use std::io::Read as _;
        let dir = form_workflow();
        let (_, engine, form) = prepare_form(dir.path(), None, &nf_files()).unwrap();
        let ui = ui_base("https://biffix.example.org/api/").unwrap();
        let url = preview_url(ui, "hello", engine, &form).unwrap();
        let payload = url.strip_prefix("https://biffix.example.org/workflows/preview#").unwrap();
        let deflated = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(payload).unwrap();
        let mut json = String::new();
        flate2::read::ZlibDecoder::new(deflated.as_slice()).read_to_string(&mut json).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["name"], "hello");
        assert_eq!(value["engine"], "nextflow");
        assert_eq!(value["form_schema"], serde_json::to_value(&form).unwrap());
        // A base that is not a deployment's `…/api` has no UI origin to derive.
        assert_eq!(ui_base("https://biffix.example.org/api"), Some("https://biffix.example.org"));
        assert_eq!(ui_base("https://biffix.example.org/apis"), None);
        assert_eq!(ui_base("http://localhost:8080"), None);
    }
}
