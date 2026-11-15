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

/// [`prepare_form`] for a registration: `biffix.yaml` (or `manifest`, `--manifest` as given,
/// resolved by [`resolve_manifest`]), the parameter schema and every file the form reads must be
/// among `files`, the source runs get, so nothing the version is not made of (an untracked or
/// git-ignored file) shapes its form. A missing `biffix.yaml` is an empty one; a missing
/// `manifest` is an error.
pub(crate) fn prepare_form_shipped(
    root: &Path,
    manifest: Option<&str>,
    files: &SourceFiles,
) -> CliResult<(BiffixManifest, Engine, FormSchema)> {
    let path = match manifest {
        None => {
            shipped(root, "biffix.yaml", Some(files))?;
            None
        }
        Some(given) => Some(resolve_manifest(root, given, Some(files))?.path),
    };
    let (manifest, engine) = manifest_and_engine(root, path.as_deref(), files)?;
    let form_schema = derive_form_schema(root, &manifest, engine, Some(files))?;
    Ok((manifest, engine, form_schema))
}

/// A `--manifest` resolved within the workflow: its root-relative path, cleaned, and the file it
/// names on disk (symlinks resolved).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ResolvedManifest {
    pub(crate) rel: String,
    pub(crate) path: std::path::PathBuf,
}

/// Resolve `--manifest` as given, one way for `workflows form` and `workflows register`: a path
/// relative to `root` (not absolute, no `\`, no `..` out of it; `./a.yaml` is `a.yaml`) naming an
/// existing file that, through any symlinks, stays inside the workflow. With `files` (a
/// registration), it must also be among them, the source runs get; without (a preview of the
/// working tree), an untracked file is read too.
pub(crate) fn resolve_manifest(
    root: &Path,
    given: &str,
    files: Option<&SourceFiles>,
) -> CliResult<ResolvedManifest> {
    let refuse = |why: String| CliError::InvalidInput(format!("--manifest {given}: {why}"));
    let rel = biffix_workflow::images::clean_path(given)
        .filter(|p| !p.is_empty())
        .ok_or_else(|| refuse("must be a relative path to a file inside the workflow".into()))?;
    if let Some(files) = files {
        if !files.is_file(&rel) {
            return Err(CliError::InvalidInput(format!(
                "--manifest {rel} is not part of the source runs get ({}); commit it first",
                files.describe()
            )));
        }
    }
    let canonical_root = root.canonicalize().map_err(|e| refuse(e.to_string()))?;
    let path = match root.join(&rel).canonicalize() {
        Ok(path) => path,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(refuse("no such file".into()))
        }
        Err(e) => return Err(refuse(e.to_string())),
    };
    if !path.starts_with(&canonical_root) {
        return Err(refuse("the file is outside the workflow (through a symlink)".into()));
    }
    if !path.is_file() {
        return Err(refuse("not a file".into()));
    }
    Ok(ResolvedManifest { rel, path })
}

/// `register --manifest`'s path as recorded: relative to the workflow's root, cleaned (`./a.yaml`
/// is `a.yaml`), and `None` for the default `biffix.yaml` (given or not), so naming the default
/// records nothing.
pub(crate) fn manifest_path(manifest: Option<&str>) -> CliResult<Option<String>> {
    let Some(given) = manifest else {
        return Ok(None);
    };
    let clean =
        biffix_workflow::images::clean_path(given).filter(|p| !p.is_empty()).ok_or_else(|| {
            CliError::InvalidInput(format!(
                "--manifest {given} must be a relative path to a file inside the workflow"
            ))
        })?;
    Ok((clean != "biffix.yaml").then_some(clean))
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
            let err = prepare_form_shipped(dir.path(), None, &files)
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
        let (manifest, _, form) = prepare_form_shipped(dir.path(), None, &files).unwrap();
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
    fn a_registration_reads_another_manifest_only_when_it_is_shipped() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Snakefile"), "rule all:\n    input: []\n").unwrap();
        std::fs::write(dir.path().join("biffix.yaml"), "engine_version: \"9.1.0\"\n").unwrap();
        std::fs::create_dir(dir.path().join("alt")).unwrap();
        std::fs::write(dir.path().join("alt/biffix.yaml"), "engine_version: \"9.2.0\"\n").unwrap();
        let files = SourceFiles::of(&["Snakefile", "biffix.yaml", "alt/biffix.yaml"]);
        let (manifest, ..) =
            prepare_form_shipped(dir.path(), Some("alt/biffix.yaml"), &files).unwrap();
        assert_eq!(manifest.engine_version.as_deref(), Some("9.2.0"));
        let (manifest, ..) = prepare_form_shipped(dir.path(), None, &files).unwrap();
        assert_eq!(manifest.engine_version.as_deref(), Some("9.1.0"));
        // On disk but not shipped (untracked), or not there at all: refused, unlike a missing
        // default `biffix.yaml`.
        for files in
            [SourceFiles::of(&["Snakefile", "biffix.yaml"]), SourceFiles::of(&["Snakefile"])]
        {
            for rel in ["alt/biffix.yaml", "alt/missing.yaml"] {
                let err = prepare_form_shipped(dir.path(), Some(rel), &files).unwrap_err();
                assert!(
                    matches!(&err, CliError::InvalidInput(m)
                        if m.contains(&format!("--manifest {rel} is not part of the source"))),
                    "{rel}: {err:?}"
                );
            }
        }
    }

    #[test]
    fn form_and_register_resolve_a_manifest_to_the_same_file() {
        let outer = tempfile::tempdir().unwrap();
        let root = outer.path().join("wf");
        std::fs::create_dir_all(root.join("alt")).unwrap();
        std::fs::write(root.join("Snakefile"), "rule all:\n    input: []\n").unwrap();
        std::fs::write(root.join("alt/biffix.yaml"), "engine: snakemake\n").unwrap();
        std::fs::write(outer.path().join("x.yaml"), "engine: snakemake\n").unwrap();
        std::os::unix::fs::symlink("../../x.yaml", root.join("alt/escape.yaml")).unwrap();
        let files = SourceFiles::of(&["Snakefile", "alt/biffix.yaml", "alt/escape.yaml"]);
        let preview = |given: &str| resolve_manifest(&root, given, None);
        let register = |given: &str| resolve_manifest(&root, given, Some(&files));
        let message = |r: CliResult<ResolvedManifest>| match r {
            Err(CliError::InvalidInput(m)) => m,
            other => panic!("expected a refusal, got {other:?}"),
        };
        for given in ["alt/biffix.yaml", "./alt/biffix.yaml", "alt/./biffix.yaml"] {
            let (a, b) = (preview(given).unwrap(), register(given).unwrap());
            assert_eq!(a, b, "{given}");
            assert_eq!(a.rel, "alt/biffix.yaml");
            assert_eq!(a.path, root.join("alt/biffix.yaml").canonicalize().unwrap());
        }
        // The same spellings are refused by both: not relative, or climbing out.
        let absolute = root.join("alt/biffix.yaml").display().to_string();
        for bad in ["../x.yaml", absolute.as_str(), ".", "alt\\biffix.yaml"] {
            for got in [preview(bad), register(bad)] {
                assert!(message(got).contains("relative path"), "{bad}");
            }
        }
        // A symlink out of the workflow, even a tracked one.
        for got in [preview("alt/escape.yaml"), register("alt/escape.yaml")] {
            assert!(message(got).contains("outside the workflow"));
        }
        // A missing file is an error for both, not "no manifest".
        assert!(message(preview("alt/missing.yaml")).contains("no such file"));
        assert!(message(register("alt/missing.yaml")).contains("not part of the source"));
        // An untracked file: a preview reads it, a registration refuses it.
        std::fs::write(root.join("alt/draft.yaml"), "engine: snakemake\n").unwrap();
        assert_eq!(preview("alt/draft.yaml").unwrap().rel, "alt/draft.yaml");
        assert!(message(register("alt/draft.yaml")).contains("not part of the source"));
    }

    #[test]
    fn a_manifest_path_is_relative_to_the_root_and_spelled_one_way() {
        assert_eq!(manifest_path(None).unwrap(), None);
        assert_eq!(
            manifest_path(Some("./alt/biffix.yaml")).unwrap().as_deref(),
            Some("alt/biffix.yaml")
        );
        // The default, named, is the default: nothing to record.
        assert_eq!(manifest_path(Some("biffix.yaml")).unwrap(), None);
        for bad in ["../biffix.yaml", "/abs/biffix.yaml", ".", "a\\b.yaml"] {
            let err = manifest_path(Some(bad)).unwrap_err();
            assert!(
                matches!(&err, CliError::InvalidInput(m) if m.contains("relative path")),
                "{bad}: {err:?}"
            );
        }
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
