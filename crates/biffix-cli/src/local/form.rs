//! A workflow checkout as `register`, `workflows form` and `inspect` read it: its manifest
//! (`biffix.yaml`), its engine, the launch form derived from its parameter schema, and its About
//! page. Local only — nothing here talks to a deployment.

use std::path::Path;

use biffix_workflow::engines::Engine;
use biffix_workflow::formschema::{
    apply_overlay, attach_catalog, attach_reports, attach_samples, from_nextflow_schema_ordered,
    from_snakemake_config_ordered, help_lint, parse_schema_text, FormSchema,
};
use biffix_workflow::manifest::{
    detect_engine, BiffixManifest, NEXTFLOW_MARKERS, SNAKEFILE_LOCATIONS,
};

use biffix_api::MAX_ABOUT_BYTES;

use crate::error::{CliError, CliResult};
use crate::local::source::SourceFiles;

/// The workflow's manifest (`biffix.yaml`, or `manifest` when given), its engine (detected from
/// `files`, the source the coordinator gets), and its form, all as `register` freezes them, read
/// from the working tree as it is (a preview while authoring).
pub(crate) fn prepare_form(
    root: &Path,
    manifest: Option<&Path>,
    files: &SourceFiles,
) -> CliResult<(BiffixManifest, Engine, FormSchema)> {
    let (manifest, engine) = manifest_and_engine(root, manifest, files)?;
    let form_schema = derive_form_schema(root, &manifest, engine, None)?;
    Ok((manifest, engine, form_schema))
}

/// [`prepare_form`] for a registration: `biffix.yaml`, the parameter schema and every file the
/// form reads must be among `files`, the source runs get, so nothing the version is not made of
/// (an untracked or git-ignored file) shapes its form.
pub(crate) fn prepare_form_shipped(
    root: &Path,
    files: &SourceFiles,
) -> CliResult<(BiffixManifest, Engine, FormSchema)> {
    shipped(root, "biffix.yaml", Some(files))?;
    let (manifest, engine) = manifest_and_engine(root, None, files)?;
    let form_schema = derive_form_schema(root, &manifest, engine, Some(files))?;
    Ok((manifest, engine, form_schema))
}

/// Refuse `rel` when it is on disk under `root` but not among `files` (when given): the form
/// would be shaped by a file the source runs get does not have.
fn shipped(root: &Path, rel: &str, files: Option<&SourceFiles>) -> CliResult<()> {
    match files {
        Some(files) if !files.is_file(rel) && root.join(rel).exists() => {
            Err(CliError::InvalidInput(format!(
                "{rel} is not part of the source runs get ({}); commit it first",
                files.describe()
            )))
        }
        _ => Ok(()),
    }
}

/// Print (to stderr) what `register` warns about a form: its schema warnings, then
/// [`warn_undocumented_params`]. Advisory only: it never fails the command.
pub(crate) fn warn_form(form: &FormSchema) {
    for w in &form.warnings {
        eprintln!("[biffix] schema warning: {w}");
    }
    warn_undocumented_params(form);
}

/// Print (to stderr) the lint for params the launch form shows with no description or help
/// ([`help_lint`]), if any. Advisory only: it never fails the command.
pub(crate) fn warn_undocumented_params(form: &FormSchema) {
    if let Some(lint) = help_lint(form) {
        eprintln!("[biffix] warning: {lint}");
    }
}

/// The workflow's manifest (`biffix.yaml`, or `manifest` when given; empty when absent) and its
/// engine: the manifest's `engine`, else detected from `files` (the source the coordinator gets,
/// so a name is matched case and all, and a Snakefile reached through a tracked symlink counts),
/// so the right schema adapter and defaults are picked.
pub(crate) fn manifest_and_engine(
    root: &Path,
    manifest: Option<&Path>,
    files: &SourceFiles,
) -> CliResult<(BiffixManifest, Engine)> {
    let manifest =
        load_manifest(&manifest.map_or_else(|| root.join("biffix.yaml"), Path::to_path_buf))?;
    let present: Vec<String> = NEXTFLOW_MARKERS
        .into_iter()
        .chain(SNAKEFILE_LOCATIONS)
        .filter(|path| files.is_file(path))
        .map(str::to_string)
        .collect();
    let engine =
        detect_engine(&manifest, &present).map_err(|e| CliError::InvalidInput(e.to_string()))?;
    Ok((manifest, engine))
}

/// A missing manifest is an empty one (every setting falls back to detection).
fn load_manifest(path: &Path) -> CliResult<BiffixManifest> {
    match std::fs::read_to_string(path) {
        Ok(text) => BiffixManifest::parse_yaml(&text).map_err(|e| {
            CliError::InvalidInput(format!("invalid manifest {}: {e}", path.display()))
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(BiffixManifest::default()),
        Err(e) => Err(CliError::InvalidInput(format!("cannot read {}: {e}", path.display()))),
    }
}

/// Derive the FormSchema from the workflow's parameter schema, choosing the adapter by engine: an
/// nf-schema `nextflow_schema.json` for Nextflow (default filename when the manifest omits `schema`),
/// or the manifest's JSON-Schema-shaped config for Snakemake.
///
/// A genuinely **absent** schema ⇒ an empty FormSchema with a warning (a schema-less workflow is
/// legitimate; the adapters are lenient). But a schema that is **present yet unreadable/unparseable**
/// is a hard error: silently degrading it to an empty form would disable ALL client-side param
/// validation for that version with only a stderr warning as the signal. "Explicit but broken" is
/// distinguished from "absent" — a missing manifest-declared `schema` is an error, while a missing
/// *default* filename (the Nextflow convention) is just "no schema".
///
/// The author's `form:` overlay in `biffix.yaml`, if any, is applied over the derived form.
///
/// With `files`, only those files may shape the form (see [`prepare_form_shipped`]).
pub(crate) fn derive_form_schema(
    root: &Path,
    manifest: &BiffixManifest,
    engine: Engine,
    files: Option<&SourceFiles>,
) -> CliResult<FormSchema> {
    let mut derived = derive_baseline(root, manifest, engine, files)?;
    // A key biffix.yaml has that biffix does not know does nothing; say so.
    derived.warnings.extend(manifest.unknown_keys());
    let mut form = match &manifest.form {
        Some(overlay) => apply_overlay(derived, overlay, &|rel| read_shipped(root, rel, files)),
        None => derived,
    };
    attach_reports(&mut form, &manifest.reports);
    attach_samples(&mut form, manifest.samples.as_ref().map(|s| &s.hint));
    attach_catalog(&mut form, manifest.catalog.as_ref());
    // Checked here so every command that reads the workflow refuses a bad `plugins:` list.
    manifest.nextflow_plugins(engine).map_err(|e| CliError::InvalidInput(e.to_string()))?;
    if manifest.cache {
        if engine != Engine::Snakemake {
            return Err(CliError::InvalidInput(
                "`cache: true` applies only to Snakemake workflows (a Nextflow run is resumed \
                 instead)"
                    .into(),
            ));
        }
        form.cache = true;
    }
    Ok(form)
}

/// [`read_repo_file`], refusing a file that is not among `files` (when given).
fn read_shipped(root: &Path, rel: &str, files: Option<&SourceFiles>) -> Result<String, String> {
    shipped(root, rel, files).map_err(|e| e.to_string())?;
    read_repo_file(root, rel)
}

/// Read a file the manifest names, relative to the workflow root; a path that leaves the root is
/// refused (an example's `params_file` is part of the workflow, not the author's machine).
fn read_repo_file(root: &Path, rel: &str) -> Result<String, String> {
    let root = root.canonicalize().map_err(|e| e.to_string())?;
    let path = root.join(rel).canonicalize().map_err(|e| e.to_string())?;
    if !path.starts_with(&root) {
        return Err("path is outside the workflow".into());
    }
    std::fs::read_to_string(path).map_err(|e| e.to_string())
}

/// The About page the manifest names with `about:`, if any. A named file that is missing or outside
/// the workflow (or, with `files`, not among them) is an error (explicit but broken); one over
/// [`MAX_ABOUT_BYTES`] is cut short with a warning.
pub(crate) fn read_about(
    root: &Path,
    manifest: &BiffixManifest,
    files: Option<&SourceFiles>,
) -> CliResult<Option<String>> {
    let Some(rel) = manifest.about.as_deref() else {
        return Ok(None);
    };
    let mut text = read_shipped(root, rel, files)
        .map_err(|e| CliError::InvalidInput(format!("biffix.yaml `about: {rel}`: {e}")))?;
    if text.len() > MAX_ABOUT_BYTES {
        eprintln!(
            "[biffix] warning: `about: {rel}` is larger than {} KiB; keeping only the start",
            MAX_ABOUT_BYTES / 1024
        );
        let mut end = MAX_ABOUT_BYTES;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
    Ok(Some(text))
}

/// The form derived from the engine schema alone (before the author's overlay).
fn derive_baseline(
    root: &Path,
    manifest: &BiffixManifest,
    engine: Engine,
    files: Option<&SourceFiles>,
) -> CliResult<FormSchema> {
    let explicit = manifest.schema.as_deref();
    let default_schema = match engine {
        Engine::Nextflow => Some("nextflow_schema.json"),
        Engine::Snakemake => None,
    };
    let no_schema =
        |warning: String| FormSchema { warnings: vec![warning], ..FormSchema::default() };
    let Some(rel) = explicit.or(default_schema) else {
        return Ok(no_schema(
            "no `schema` in biffix.yaml; registering with no parameter form".into(),
        ));
    };
    shipped(root, rel, files)?;
    let path = root.join(rel);
    // Confined to the workflow as `about:` and the overlay's files are; a missing file is left to
    // the read below, which tells a missing default from a missing declared schema.
    if let Ok(real) = path.canonicalize() {
        let inside = root.canonicalize().is_ok_and(|root| real.starts_with(root));
        if !inside {
            return Err(CliError::InvalidInput(format!(
                "biffix.yaml `schema: {rel}`: path is outside the workflow"
            )));
        }
    }
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        // A missing *default* file just means "no schema"; a missing *explicitly-declared* one is
        // a configuration error the user must fix, not a silent validation bypass.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && explicit.is_none() => {
            return Ok(no_schema(format!("no {rel} found; registering with no parameter form")));
        }
        Err(e) => {
            return Err(CliError::InvalidInput(format!(
                "cannot read schema {}: {e}",
                path.display()
            )))
        }
    };
    // Parsed as YAML (a superset of JSON) so the schema's key order — its sections and the order
    // of params within them — survives into the form.
    let (value, order) = parse_schema_text(&text)
        .map_err(|e| CliError::InvalidInput(format!("invalid schema {}: {e}", path.display())))?;
    let schema = match engine {
        Engine::Nextflow => from_nextflow_schema_ordered(&value, &order, manifest, &|rel| {
            read_shipped(root, rel, files)
        }),
        Engine::Snakemake => from_snakemake_config_ordered(&value, &order, manifest),
    };
    Ok(schema)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snakefile() -> SourceFiles {
        SourceFiles::of(&["Snakefile"])
    }

    fn manifest(about: Option<&str>) -> BiffixManifest {
        BiffixManifest { about: about.map(str::to_string), ..BiffixManifest::default() }
    }

    #[test]
    fn about_is_read_only_when_named() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("docs")).unwrap();
        std::fs::write(dir.path().join("docs/about.md"), "# Hello\n").unwrap();
        std::fs::write(dir.path().join("README.md"), "developer docs").unwrap();
        assert_eq!(read_about(dir.path(), &manifest(None), None).unwrap(), None);
        assert_eq!(
            read_about(dir.path(), &manifest(Some("docs/about.md")), None).unwrap(),
            Some("# Hello\n".to_string())
        );
    }

    #[test]
    fn a_missing_or_outside_about_is_refused() {
        let outer = tempfile::tempdir().unwrap();
        let root = outer.path().join("wf");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(outer.path().join("secret.md"), "no").unwrap();
        assert!(read_about(&root, &manifest(Some("../secret.md")), None).is_err());
        assert!(read_about(&root, &manifest(Some("missing.md")), None).is_err());
    }

    #[test]
    fn a_large_about_is_cut_on_a_character_boundary() {
        let dir = tempfile::tempdir().unwrap();
        // Three-byte characters, so the cap falls inside one.
        let text = "é€".repeat(MAX_ABOUT_BYTES / 5 + 10);
        std::fs::write(dir.path().join("about.md"), &text).unwrap();
        let about = read_about(dir.path(), &manifest(Some("about.md")), None).unwrap().unwrap();
        assert!(about.len() <= MAX_ABOUT_BYTES && about.len() > MAX_ABOUT_BYTES - 4);
        assert!(text.starts_with(&about));
    }

    #[test]
    fn a_manifest_that_does_not_parse_is_named() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Snakefile"), "rule all:\n    input: []\n").unwrap();
        let other = dir.path().join("other.yaml");
        std::fs::write(&other, "engine: [not, an, engine").unwrap();
        let err = prepare_form(dir.path(), Some(&other), &snakefile()).unwrap_err();
        assert!(
            matches!(err, CliError::InvalidInput(ref m)
                if m.starts_with(&format!("invalid manifest {}: ", other.display()))),
            "{err:?}"
        );
    }

    #[test]
    fn a_schema_outside_the_workflow_is_refused() {
        let outer = tempfile::tempdir().unwrap();
        let root = outer.path().join("wf");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("Snakefile"), "rule all:\n    input: []\n").unwrap();
        std::fs::write(outer.path().join("schema.yaml"), "properties:\n  x: {type: string}\n")
            .unwrap();
        for rel in ["../schema.yaml", &outer.path().join("schema.yaml").display().to_string()] {
            std::fs::write(root.join("biffix.yaml"), format!("schema: {rel}\n")).unwrap();
            let err = prepare_form(&root, None, &snakefile()).unwrap_err();
            assert!(
                matches!(err, CliError::InvalidInput(ref m) if m.contains("outside the workflow")),
                "{rel}: {err:?}"
            );
        }
    }

    #[test]
    fn a_declared_schema_that_is_missing_or_broken_is_an_error_but_an_absent_default_is_not() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Snakefile"), "rule all:\n    input: []\n").unwrap();
        std::fs::write(dir.path().join("biffix.yaml"), "schema: nope.json\n").unwrap();
        let err = prepare_form(dir.path(), None, &snakefile()).unwrap_err();
        assert!(
            matches!(err, CliError::InvalidInput(ref m) if m.contains("cannot read schema")),
            "{err:?}"
        );
        std::fs::write(dir.path().join("biffix.yaml"), "schema: config.schema.yaml\n").unwrap();
        std::fs::write(dir.path().join("config.schema.yaml"), "{ not: [valid").unwrap();
        let err = prepare_form(dir.path(), None, &snakefile()).unwrap_err();
        assert!(
            matches!(err, CliError::InvalidInput(ref m) if m.contains("invalid schema")),
            "{err:?}"
        );

        let nf = tempfile::tempdir().unwrap();
        std::fs::write(nf.path().join("main.nf"), "workflow {}\n").unwrap();
        let (_, engine, form) =
            prepare_form(nf.path(), None, &SourceFiles::of(&["main.nf"])).unwrap();
        assert_eq!(engine, Engine::Nextflow);
        assert!(form.fields.is_empty());
        assert!(
            form.warnings.iter().any(|w| w.contains("no nextflow_schema.json found")),
            "{:?}",
            form.warnings
        );
    }

    /// A registration's form is made of the source runs get: a `biffix.yaml`, a schema or an About
    /// page on disk but not shipped (untracked, ignored) is refused, and a file the form only
    /// reads for an example is not read.
    #[test]
    fn a_registration_reads_its_form_from_the_shipped_files_only() {
        let dir = tempfile::tempdir().unwrap();
        let write = |rel: &str, text: &str| std::fs::write(dir.path().join(rel), text).unwrap();
        write("Snakefile", "rule all:\n    input: []\n");
        write(
            "biffix.yaml",
            "schema: schema.json\nabout: about.md\nform:\n  examples:\n    - { name: demo, \
             params_file: demo.yaml }\n",
        );
        write(
            "schema.json",
            r#"{"properties": {"reads": {"type": "string", "description": "Reads"}}}"#,
        );
        write("about.md", "# About\n");
        write("demo.yaml", "reads: s3://b/r.fq\n");
        let refused = |files: &[&str], rel: &str| {
            let files = SourceFiles::of(files);
            let err = prepare_form_shipped(dir.path(), &files)
                .and_then(|(m, ..)| read_about(dir.path(), &m, Some(&files)).map(|_| ()))
                .unwrap_err();
            assert!(
                matches!(&err, CliError::InvalidInput(m)
                    if m.contains(&format!("{rel} is not part of the source runs get"))),
                "{rel}: {err:?}"
            );
        };
        refused(&["Snakefile"], "biffix.yaml");
        refused(&["Snakefile", "biffix.yaml"], "schema.json");
        refused(&["Snakefile", "biffix.yaml", "schema.json"], "about.md");
        let files = SourceFiles::of(&["Snakefile", "biffix.yaml", "schema.json", "about.md"]);
        let (manifest, _, form) = prepare_form_shipped(dir.path(), &files).unwrap();
        assert_eq!(
            read_about(dir.path(), &manifest, Some(&files)).unwrap(),
            Some("# About\n".to_string())
        );
        // The unshipped example params file is not read: the form says so instead.
        assert!(form.warnings.iter().any(|w| w.contains("demo.yaml")), "{:?}", form.warnings);
        // A preview reads the working tree as it is.
        let (_, _, preview) =
            prepare_form(dir.path(), None, &SourceFiles::of(&["Snakefile"])).unwrap();
        assert_eq!(preview.fields.len(), 1);
    }

    #[test]
    fn another_manifest_picks_the_engine_too() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Snakefile"), "rule all:\n    input: []\n").unwrap();
        let other = dir.path().join("other.yaml");
        std::fs::write(&other, "engine: nextflow\n").unwrap();
        let (_, engine) = manifest_and_engine(dir.path(), Some(&other), &snakefile()).unwrap();
        assert_eq!(engine, Engine::Nextflow);
        let (_, engine) = manifest_and_engine(dir.path(), None, &snakefile()).unwrap();
        assert_eq!(engine, Engine::Snakemake);
    }

    #[test]
    fn the_manifest_s_catalog_reaches_the_derived_form() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Snakefile"), "rule all:\n    input: []\n").unwrap();
        std::fs::write(
            dir.path().join("biffix.yaml"),
            "catalog:\n  description: ' Bulk RNA-seq. '\n  tags: [RNA-seq, rna-seq, Bulk]\n  \
             verified: true\n",
        )
        .unwrap();
        let (_, _, form) = prepare_form(dir.path(), None, &snakefile()).unwrap();
        assert_eq!(
            form.catalog,
            Some(biffix_workflow::formschema::FormCatalog {
                description: Some("Bulk RNA-seq.".into()),
                tags: vec!["RNA-seq".into(), "Bulk".into()],
                verified: true,
                contact: None,
            })
        );
    }
}
