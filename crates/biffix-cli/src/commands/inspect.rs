//! `biffix inspect`: find the container images a workflow's tasks name, and (with `--write`) add
//! the ones `biffix.yaml` does not declare yet to its `images:` block — which is all
//! `biffix workflows register` imports. For Nextflow, also the plugins the pipeline declares, which
//! `--write` sets as `biffix.yaml`'s `plugins:` (registration builds them into the runtime).
//!
//! Nextflow: runs `nextflow inspect -format json` (the `nextflow` on your PATH; arguments after
//! `--`, e.g. `-profile` or `-params-file`, go to it) and `nextflow config -output json` (given
//! only a `-profile` from those arguments) for its `plugins` (see [`crate::local::nf_plugins`]); a
//! failure there is a warning, and leaves `plugins:` as it is. Snakemake: reads the workflow's
//! tracked files for `aws_batch_container_image` (nothing is run); values that are not plain
//! strings are listed for you to declare by hand, and rules that would run on AWS Batch with
//! Snakemake's default memory are warned about.

use std::path::Path;

use biffix_workflow::engines::Engine;
use biffix_workflow::images::{new_images, NewImage};
use biffix_workflow::scan::{ImageScan, ScanKind};

use crate::error::{CliError, CliResult};
use crate::local::source;
use crate::local::source::SourceMode;
use crate::render::OutputMode;

/// `biffix inspect`'s arguments.
#[derive(Debug, Clone)]
pub(crate) struct InspectOpts {
    /// The workflow checkout.
    pub(crate) path: String,
    /// Add the images found to `biffix.yaml`.
    pub(crate) write: bool,
    /// Extra arguments for `nextflow inspect` (Nextflow only).
    pub(crate) nextflow_args: Vec<String>,
    /// JSON also prints what was found, what is new and what was written, as one document on
    /// stdout (the report on stderr stays).
    pub(crate) output: OutputMode,
}

pub(crate) fn inspect(opts: InspectOpts) -> CliResult<()> {
    let root = Path::new(&opts.path);
    // The source a register in its default (git) source mode would ship: the engine is detected
    // from it and the entrypoint checked against it, as `register` does.
    let files = source::shipped_files(root, SourceMode::Git, None)?;
    let (manifest, engine) = crate::local::form::manifest_and_engine(root, None, &files)?;
    let scan = match engine {
        Engine::Nextflow => nextflow_inspect(root, &opts.nextflow_args)?,
        Engine::Snakemake => {
            if !opts.nextflow_args.is_empty() {
                return Err(CliError::InvalidInput(
                    "arguments after `--` are for `nextflow inspect`; this is a Snakemake \
                     workflow"
                        .into(),
                ));
            }
            snakemake_scan(root)?
        }
    };
    let added = new_images(&manifest.images, manifest.image.as_deref(), &scan.references());
    report(&scan, &added, opts.write);
    // The pipeline's own plugins, as `plugins:` would list them. Not reading them is only a
    // warning: the images found are still reported and written.
    let detection = (engine == Engine::Nextflow).then(|| {
        crate::local::nf_plugins::detect(
            root,
            &opts.nextflow_args,
            manifest.engine_version.as_deref(),
        )
        .map_err(|e| e.into_cli().to_string())
    });
    let plugins = plugins_found(detection, &manifest.plugins, opts.write);
    if let Some(e) = &plugins.error {
        eprintln!(
            "[biffix] warning: could not read the pipeline's Nextflow plugins; biffix.yaml \
             `plugins:` is left as it is: {e}"
        );
    }
    if let Some(p) = &plugins.detected {
        report_plugins(p, &manifest.plugins, plugins.changed, opts.write);
    }
    if engine == Engine::Snakemake {
        // The entrypoint `register` would record, so both check the Snakefile the run uses. One
        // `register` would refuse is only a warning here: the images found are still reported.
        match crate::local::entrypoint::resolve_entrypoint(None, false, &manifest, engine, &files) {
            Ok(entrypoint) => {
                if let Some(note) = &entrypoint.note {
                    eprintln!("[biffix] note: {note}");
                }
                crate::local::memory_warning::warn_rules_without_memory(
                    root,
                    &files,
                    &entrypoint.file,
                    manifest.engine_version.as_deref(),
                );
            }
            Err(e) => eprintln!("[biffix] warning: {e}"),
        }
    }
    // The launch form `register` would freeze: params it shows with nothing to explain them are
    // warned about. A form that cannot be read (`register` would refuse it) is only a warning here.
    match crate::local::form::derive_form_schema(root, &manifest, engine, None) {
        Ok(form) => crate::local::form::warn_undocumented_params(&form),
        Err(e) => eprintln!("[biffix] warning: cannot read the launch form: {e}"),
    }
    let written = opts.write && !added.is_empty();
    if written || plugins.written {
        let path = root.join("biffix.yaml");
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => {
                return Err(CliError::InvalidInput(format!("cannot read {}: {e}", path.display())))
            }
        };
        let to_set = plugins.detected.as_deref().filter(|_| plugins.written);
        let updated = crate::local::manifest_edit::updated_manifest(
            &text,
            if written { &added } else { &[] },
            to_set,
        )?;
        // Atomically, so a failed write (a full disk, an interrupt) leaves the author's manifest
        // as it was rather than cut short.
        crate::fsutil::write_atomic(&path, updated.as_bytes(), false)
            .map_err(|e| CliError::InvalidInput(format!("cannot write {}: {e}", path.display())))?;
        if written {
            eprintln!(
                "[biffix] biffix.yaml: added {} image{}; review and commit it",
                added.len(),
                if added.len() == 1 { "" } else { "s" }
            );
        }
        if let Some(p) = to_set {
            eprintln!(
                "[biffix] biffix.yaml: set `plugins:` to [{}] (was [{}]); review and commit it",
                p.join(", "),
                manifest.plugins.join(", ")
            );
        }
    }
    if opts.output == OutputMode::Json {
        crate::render::print_json(&inspection_json(engine, &scan, &added, written, &plugins));
    }
    Ok(())
}

/// What `inspect` found of the pipeline's plugins (Nextflow only).
#[derive(Debug, Default, PartialEq)]
struct PluginsFound {
    /// The plugins it declares, as `plugins:` lists them (none when not read).
    detected: Option<Vec<String>>,
    /// Why they could not be read.
    error: Option<String>,
    /// Whether `plugins:` lists another set.
    changed: bool,
    /// Whether `--write` sets `plugins:` (it changed, and was read).
    written: bool,
}

/// [`PluginsFound`] from the detection (`None`: not a Nextflow workflow) and `biffix.yaml`'s
/// `plugins:` (`listed`). A failed detection leaves `plugins:` as it is: a hand-written list is
/// never dropped because `nextflow config` could not run.
fn plugins_found(
    detection: Option<Result<Vec<String>, String>>,
    listed: &[String],
    write: bool,
) -> PluginsFound {
    match detection {
        None => PluginsFound::default(),
        Some(Err(e)) => PluginsFound { error: Some(e), ..PluginsFound::default() },
        Some(Ok(detected)) => {
            let changed = crate::local::nf_plugins::plugins_changed(&detected, listed);
            PluginsFound {
                detected: Some(detected),
                error: None,
                changed,
                written: write && changed,
            }
        }
    }
}

/// The plugins the pipeline declares, and whether `biffix.yaml` lists them already. Nothing when
/// neither has any.
fn report_plugins(plugins: &[String], listed: &[String], changed: bool, write: bool) {
    let n = plugins.len();
    if n == 0 && listed.is_empty() {
        return;
    }
    eprintln!(
        "[biffix] the pipeline declares {n} Nextflow plugin{} for its runtime{}{}",
        if n == 1 { "" } else { "s" },
        if n == 0 { "" } else { ": " },
        plugins.join(", ")
    );
    if changed && !write {
        eprintln!(
            "[biffix] biffix.yaml `plugins:` lists [{}]; run with --write to set it",
            listed.join(", ")
        );
    }
}

/// `biffix inspect`'s JSON document: every occurrence found, the images not yet declared, and
/// whether they were written to `biffix.yaml`; for Nextflow also the plugins the pipeline declares
/// (`null` when not read, with `plugins_error` saying why) and whether `plugins:` was set.
fn inspection_json(
    engine: Engine,
    scan: &ImageScan,
    added: &[NewImage],
    written: bool,
    plugins: &PluginsFound,
) -> serde_json::Value {
    serde_json::json!({
        "plugins": plugins.detected,
        "plugins_error": plugins.error,
        "plugins_written": plugins.written,
        "engine": engine.as_str(),
        "found": scan.entries,
        "new": added
            .iter()
            .map(|i| serde_json::json!({
                "name": i.name, "reference": i.reference, "location": i.location,
            }))
            .collect::<Vec<_>>(),
        "written": written,
    })
}

/// Run `nextflow inspect -format json` on the pipeline at `root`.
fn nextflow_inspect(root: &Path, args: &[String]) -> CliResult<ImageScan> {
    let args = [["-format".to_string(), "json".to_string()].as_slice(), args].concat();
    let output = crate::local::nf_plugins::run_nextflow(root, "inspect", &args)?;
    biffix_workflow::scan::from_nextflow_inspect(&output)
        .map_err(|e| CliError::Unclassified(e.to_string()))
}

/// Read a Snakemake workflow's tracked files for its per-rule images.
fn snakemake_scan(root: &Path) -> CliResult<ImageScan> {
    let entries = source::tracked_files(root)?;
    let files = biffix_workflow::version::normalized_files(root, &entries)
        .map_err(|e| CliError::InvalidInput(format!("reading the workflow source: {e}")))?;
    Ok(biffix_workflow::scan::scan_snakemake(&files))
}

/// What was found, what is new, and what the author must declare by hand.
fn report(scan: &ImageScan, added: &[NewImage], write: bool) {
    let found = scan.references().len();
    eprintln!(
        "[biffix] found {found} image{}; {} not yet in biffix.yaml",
        if found == 1 { "" } else { "s" },
        added.len()
    );
    for image in added {
        let verb = if write { "adding" } else { "new" };
        eprintln!("  {verb} {}: {} ({})", image.name, image.reference, image.location);
    }
    if !write && !added.is_empty() {
        eprintln!("[biffix] run with --write to add them to biffix.yaml");
    }
    for e in scan.unresolved() {
        match e.kind {
            ScanKind::IgnoredKey => eprintln!(
                "[biffix] warning: {}: `{}` is not read by the AWS Batch executor (the task runs \
                 the default image); use `aws_batch_container_image`",
                e.location, e.key
            ),
            _ => eprintln!(
                "[biffix] declare by hand: {}: `{} {}` is not a plain image reference; add every \
                 image it can produce to `images:` in biffix.yaml",
                e.location, e.key, e.value
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn write_keeps_a_symlinked_biffix_yaml_a_symlink() {
        use crate::test_git::git_ok;
        let outer = tempfile::tempdir().unwrap();
        let root = outer.path().join("wf");
        std::fs::create_dir(&root).unwrap();
        // The author's biffix.yaml lives elsewhere (a shared config tree) and is linked in.
        let shared = outer.path().join("shared.yaml");
        std::fs::write(&shared, "engine: snakemake\nengine_version: \"9.27.0\"\n").unwrap();
        std::os::unix::fs::symlink(&shared, root.join("biffix.yaml")).unwrap();
        std::fs::write(
            root.join("Snakefile"),
            "rule a:\n    output: \"a.txt\"\n    resources: \
             aws_batch_container_image=\"quay.io/b/x:1\", mem_mb=10\n    shell: \"touch \
             {output}\"\n",
        )
        .unwrap();
        git_ok(&root, &["init", "-q"]);
        git_ok(&root, &["add", "."]);
        inspect(InspectOpts {
            path: root.display().to_string(),
            write: true,
            nextflow_args: vec![],
            output: OutputMode::Human,
        })
        .unwrap();
        let link = std::fs::symlink_metadata(root.join("biffix.yaml")).unwrap();
        assert!(link.file_type().is_symlink());
        let text = std::fs::read_to_string(&shared).unwrap();
        assert!(text.contains("ref: quay.io/b/x:1"), "{text}");
    }

    fn image(name: &str, reference: &str) -> NewImage {
        NewImage { name: name.into(), reference: reference.into(), location: "process X".into() }
    }

    fn plugins(list: &[&str]) -> Vec<String> {
        list.iter().map(std::string::ToString::to_string).collect()
    }

    #[test]
    fn a_snakemake_workflow_rejects_nextflow_arguments() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Snakefile"), "rule all:\n    input: []\n").unwrap();
        // The engine is detected from the tracked source.
        crate::test_git::git_ok(dir.path(), &["init", "-q"]);
        crate::test_git::git_ok(dir.path(), &["add", "Snakefile"]);
        let err = inspect(InspectOpts {
            path: dir.path().display().to_string(),
            write: false,
            nextflow_args: vec!["-profile".into(), "docker".into()],
            output: OutputMode::Human,
        })
        .unwrap_err();
        // Not the "`nextflow inspect` failed" error, which shares the command's name.
        assert!(
            matches!(err, CliError::InvalidInput(ref m) if m.contains("this is a Snakemake workflow")),
            "{err:?}"
        );
    }

    #[test]
    fn the_json_document_lists_what_was_found_and_what_is_new() {
        let scan = ImageScan {
            entries: vec![biffix_workflow::scan::ScanEntry {
                location: "Snakefile:3".into(),
                key: "aws_batch_container_image".into(),
                kind: ScanKind::Literal,
                value: "quay.io/b/x:1".into(),
            }],
        };
        let doc = inspection_json(
            Engine::Snakemake,
            &scan,
            &[image("x-1", "quay.io/b/x:1")],
            false,
            &PluginsFound::default(),
        );
        assert_eq!(doc["engine"], "snakemake");
        assert_eq!(doc["found"][0]["kind"], "literal");
        assert_eq!(doc["new"][0]["name"], "x-1");
        assert_eq!(doc["written"], false);
        assert!(doc["plugins"].is_null() && doc["plugins_error"].is_null());
        assert_eq!(doc["plugins_written"], false);
        // Nextflow: the plugins found and whether they were written, or why they were not read.
        let found = plugins_found(Some(Ok(plugins(&["nf-schema@2.7.2"]))), &[], true);
        let doc = inspection_json(Engine::Nextflow, &scan, &[], false, &found);
        assert_eq!(doc["plugins"], serde_json::json!(["nf-schema@2.7.2"]));
        assert_eq!(doc["plugins_written"], true);
        assert!(doc["plugins_error"].is_null());
        let found = plugins_found(Some(Err("`nextflow config` failed".into())), &[], true);
        let doc = inspection_json(Engine::Nextflow, &scan, &[], false, &found);
        assert!(doc["plugins"].is_null());
        assert_eq!(doc["plugins_error"], "`nextflow config` failed");
        assert_eq!(doc["plugins_written"], false);
    }

    #[test]
    fn plugins_are_set_only_when_read_and_changed() {
        let listed = plugins(&["nf-a-b@1", "nf-a@1"]);
        // The same set in another order: nothing to set, so a second `--write` settles.
        let found = plugins_found(Some(Ok(plugins(&["nf-a@1", "nf-a-b@1"]))), &listed, true);
        assert!(!found.changed && !found.written, "{found:?}");
        // Changed: set with --write, only reported without.
        let found = plugins_found(Some(Ok(plugins(&["nf-a@2"]))), &listed, true);
        assert!(found.changed && found.written, "{found:?}");
        let found = plugins_found(Some(Ok(plugins(&["nf-a@2"]))), &listed, false);
        assert!(found.changed && !found.written, "{found:?}");
        // Not read: a hand-written list is never touched.
        let found = plugins_found(Some(Err("boom".into())), &listed, true);
        assert_eq!(found, PluginsFound { error: Some("boom".into()), ..PluginsFound::default() });
        // Snakemake has none.
        assert_eq!(plugins_found(None, &[], true), PluginsFound::default());
    }
}
