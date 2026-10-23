//! A workflow's entrypoint: the file (or, for Nextflow, directory) a run starts from, resolved
//! against the source the coordinator gets, as `register` records it and `inspect` checks it.

use std::path::{Component, Path};

use biffix_workflow::engines::Engine;
use biffix_workflow::manifest::{BiffixManifest, SNAKEFILE_LOCATIONS};

use crate::error::{CliError, CliResult};
use crate::local::source::{Named, SourceFiles};

/// The entrypoint a run starts from, as [`resolve_entrypoint`] found it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Entrypoint {
    /// What `register` records and the coordinator runs: root-relative and normalized (no `./`,
    /// empty components or trailing `/`), or `.` for the root (a Nextflow directory).
    pub(crate) path: String,
    /// The shipped file (or, for a Nextflow directory, directory) `path` names, through any
    /// tracked symlinks: what the memory check reads.
    pub(crate) file: String,
    /// What to print when it is not the one asked for (a Snakefile found elsewhere).
    pub(crate) note: Option<String>,
}

/// The entrypoint a run starts from: `given` (`--entrypoint`; `flag` says whether the command has
/// that flag, for the messages), else `biffix.yaml`'s `entrypoint`, else the engine's default. What
/// `register` records, the coordinator runs, and `inspect` checks.
///
/// It must be in `files` (the source the coordinator gets), or the run would fail only at launch;
/// a path through a tracked symlink counts when it stays inside the repository. A path given
/// explicitly that is missing, absolute, outside the workflow, or (for Snakemake) a directory is an
/// error; a Nextflow entrypoint may name a directory, whose main script Nextflow runs. With none
/// given, a Snakemake workflow uses the first of Snakemake's standard locations present, as
/// Snakemake does (with a note when that is not a top-level `Snakefile`); a Nextflow one needs
/// `main.nf`, which is not guessed at.
pub(crate) fn resolve_entrypoint(
    given: Option<String>,
    flag: bool,
    manifest: &BiffixManifest,
    engine: Engine,
    files: &SourceFiles,
) -> CliResult<Entrypoint> {
    let pick = if flag {
        "pass --entrypoint or set `entrypoint` in biffix.yaml"
    } else {
        "set `entrypoint` in biffix.yaml"
    };
    let explicit = match (given, manifest.entrypoint.clone()) {
        (Some(path), _) => Some((path, "--entrypoint")),
        (None, Some(path)) => Some((path, "biffix.yaml `entrypoint`")),
        (None, None) => None,
    };
    if let Some((given, from)) = explicit {
        let refused = |reason: String| {
            CliError::InvalidInput(format!("entrypoint `{given}` (from {from}) {reason}"))
        };
        let Some(path) = normalized(&given) else {
            return Err(refused(
                "must be a path inside the workflow, relative to its root and `/`-separated".into(),
            ));
        };
        let recorded = if path.is_empty() { ".".to_string() } else { path.clone() };
        return match files.resolve(&path) {
            Named::File(file) => Ok(Entrypoint { path: recorded, file, note: None }),
            Named::Dir(dir) if engine == Engine::Nextflow => {
                Ok(Entrypoint { path: recorded, file: dir, note: None })
            }
            Named::Dir(_) => {
                Err(refused("is a directory; a Snakemake entrypoint must be a file".into()))
            }
            Named::Outside { link } => Err(refused(format!(
                "goes through the tracked symlink `{link}`, which points outside the repository"
            ))),
            Named::Loop { link } => Err(refused(format!(
                "goes through the tracked symlink `{link}`, which is part of a symlink loop"
            ))),
            Named::Missing => {
                let hint = did_you_mean(&path, engine, files)
                    .map_or(String::new(), |p| format!("; did you mean `{p}`?"));
                Err(refused(format!(
                    "is not in the source biffix ships ({}){hint}",
                    files.describe()
                )))
            }
        };
    }
    if engine == Engine::Nextflow {
        if let Named::File(file) = files.resolve("main.nf") {
            return Ok(Entrypoint { path: "main.nf".into(), file, note: None });
        }
        return Err(CliError::InvalidInput(format!(
            "no main.nf in the source biffix ships ({}); {pick}",
            files.describe()
        )));
    }
    // The first standard location that is a file, as Snakemake picks it.
    let found = SNAKEFILE_LOCATIONS.into_iter().enumerate().find_map(|(i, path)| {
        match files.resolve(path) {
            Named::File(file) => Some((i, path, file)),
            _ => None,
        }
    });
    match found {
        Some((i, path, file)) => {
            let note = (i != 0).then(|| {
                format!(
                    "no top-level Snakefile; using {path}, the first of Snakemake's standard \
                     locations ({}) present, as Snakemake would ({pick} to pin it)",
                    SNAKEFILE_LOCATIONS.join(", ")
                )
            });
            Ok(Entrypoint { path: path.to_string(), file, note })
        }
        None => Err(CliError::InvalidInput(format!(
            "no Snakefile in the source biffix ships ({}): none of {}; {pick}",
            files.describe(),
            SNAKEFILE_LOCATIONS.join(", ")
        ))),
    }
}

/// The file a missing entrypoint `path` probably meant: the only other shipped file of the same
/// name, else (Snakemake) the Snakefile Snakemake would run.
fn did_you_mean(path: &str, engine: Engine, files: &SourceFiles) -> Option<String> {
    let name = biffix_workflow::text::basename(path);
    let same_name: Vec<&str> = files
        .paths()
        .into_iter()
        .filter(|p| *p != path && biffix_workflow::text::basename(p) == name)
        .collect();
    if let [only] = same_name.as_slice() {
        return Some(only.to_string());
    }
    if engine != Engine::Snakemake {
        return None;
    }
    SNAKEFILE_LOCATIONS.into_iter().find(|p| files.is_file(p)).map(str::to_string)
}

/// `path` as the root-relative, `/`-separated path it names, without `./`, empty components or a
/// trailing `/` (`""` for the root); `None` when it is absolute, climbs out with `..`, or has a
/// backslash (not a separator on the coordinator).
fn normalized(path: &str) -> Option<String> {
    if path.contains('\\') {
        return None;
    }
    let mut parts = Vec::new();
    for component in Path::new(path).components() {
        match component {
            Component::CurDir => {}
            Component::Normal(part) => parts.push(part.to_str()?),
            Component::RootDir | Component::ParentDir | Component::Prefix(_) => return None,
        }
    }
    Some(parts.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local::form::manifest_and_engine;
    use crate::local::source::ShippedFile;

    fn resolve_in(
        given: Option<&str>,
        in_manifest: Option<&str>,
        engine: Engine,
        files: &SourceFiles,
    ) -> CliResult<Entrypoint> {
        let manifest = BiffixManifest {
            entrypoint: in_manifest.map(str::to_string),
            ..BiffixManifest::default()
        };
        resolve_entrypoint(given.map(str::to_string), true, &manifest, engine, files)
    }

    fn resolve(
        given: Option<&str>,
        in_manifest: Option<&str>,
        engine: Engine,
        files: &[&str],
    ) -> CliResult<Entrypoint> {
        resolve_in(given, in_manifest, engine, &SourceFiles::of(files))
    }

    fn message(result: CliResult<Entrypoint>) -> String {
        match result {
            Err(CliError::InvalidInput(m)) => m,
            other => panic!("expected InvalidInput, got {other:?}"),
        }
    }

    fn at(path: &str) -> Entrypoint {
        Entrypoint { path: path.into(), file: path.into(), note: None }
    }

    #[test]
    fn the_default_entrypoint_is_used_when_present() {
        // Present alongside another Snakefile: the top-level one wins, as Snakemake picks it first.
        let files = ["Snakefile", "workflow/Snakefile"];
        assert_eq!(resolve(None, None, Engine::Snakemake, &files).unwrap(), at("Snakefile"));
        let files = ["main.nf", "nextflow.config"];
        assert_eq!(resolve(None, None, Engine::Nextflow, &files).unwrap(), at("main.nf"));
    }

    #[test]
    fn a_missing_default_snakefile_is_the_first_where_snakemake_looks() {
        let found =
            resolve(None, None, Engine::Snakemake, &["workflow/Snakefile", "a.py"]).unwrap();
        assert_eq!(found.path, "workflow/Snakefile");
        let note = found.note.unwrap();
        assert!(
            note.contains("using workflow/Snakefile") && note.contains("--entrypoint"),
            "{note}"
        );
        let found = resolve(None, None, Engine::Snakemake, &["snakefile"]).unwrap();
        assert_eq!(found.path, "snakefile");
        // Several: the first in Snakemake's order, as Snakemake runs it, named in the note.
        let found = resolve(
            None,
            None,
            Engine::Snakemake,
            &["workflow/snakefile", "snakefile", "workflow/Snakefile"],
        )
        .unwrap();
        assert_eq!(found.path, "snakefile");
        assert!(found.note.unwrap().contains("using snakefile"));
        let found =
            resolve(None, None, Engine::Snakemake, &["workflow/snakefile", "workflow/Snakefile"])
                .unwrap();
        assert_eq!(found.path, "workflow/Snakefile");

        let none = message(resolve(None, None, Engine::Snakemake, &["rules/a.smk"]));
        assert!(
            none.contains("no Snakefile")
                && none.contains("workflow/snakefile")
                && none.contains("--entrypoint"),
            "{none}"
        );
        // Matched case and all: another case is not a Snakefile location.
        let none = message(resolve(None, None, Engine::Snakemake, &["SNAKEFILE"]));
        assert!(none.contains("no Snakefile"), "{none}");
    }

    #[test]
    fn a_missing_main_nf_is_an_error_with_no_guessing() {
        let err = message(resolve(
            None,
            None,
            Engine::Nextflow,
            &["workflow/main.nf", "nextflow.config"],
        ));
        assert!(err.contains("no main.nf") && err.contains("--entrypoint"), "{err}");
    }

    #[test]
    fn an_explicit_entrypoint_must_be_present_and_says_where_it_came_from() {
        for engine in [Engine::Snakemake, Engine::Nextflow] {
            let files = ["Snakefile", "main.nf", "wf/run.smk", "wf/run.nf"];
            assert_eq!(resolve(None, Some("wf/run.nf"), engine, &files).unwrap(), at("wf/run.nf"));
            // The flag overrides biffix.yaml, and a missing one is never replaced by a guess.
            let err = message(resolve(Some("nope.smk"), Some("wf/run.smk"), engine, &files));
            assert!(err.contains("`nope.smk` (from --entrypoint)"), "{err}");
            let err = message(resolve(None, Some("Snakefile.old"), engine, &files));
            assert!(
                err.contains("`Snakefile.old` (from biffix.yaml `entrypoint`)")
                    && err.contains("not in the source biffix ships"),
                "{err}"
            );
            // A valid flag wins over a biffix.yaml `entrypoint` that is missing or outside.
            for in_manifest in ["../gone", "gone.smk"] {
                assert_eq!(
                    resolve(Some("wf/run.smk"), Some(in_manifest), engine, &files).unwrap(),
                    at("wf/run.smk")
                );
            }
        }
    }

    #[test]
    fn an_explicit_entrypoint_is_recorded_normalized() {
        let files = ["Snakefile", "wf/run.smk"];
        for given in ["./wf/run.smk", "wf//run.smk", "wf/./run.smk", "wf/run.smk/", "wf/run.smk/."]
        {
            assert_eq!(
                resolve(Some(given), None, Engine::Snakemake, &files).unwrap(),
                at("wf/run.smk"),
                "{given}"
            );
        }
        assert_eq!(
            resolve(None, Some("Snakefile/"), Engine::Snakemake, &files).unwrap().path,
            "Snakefile"
        );
    }

    #[test]
    fn an_entrypoint_outside_the_workflow_is_refused() {
        let files = ["Snakefile", "main.nf"];
        for path in [
            "/etc/Snakefile",
            "../Snakefile",
            "wf/../../main.nf",
            "wf/../Snakefile",
            "wf\\Snakefile",
        ] {
            for (given, in_manifest) in [(Some(path), None), (None, Some(path))] {
                let err = message(resolve(given, in_manifest, Engine::Snakemake, &files));
                assert!(err.contains("inside the workflow"), "{err}");
            }
        }
    }

    #[test]
    fn an_entrypoint_may_go_through_a_tracked_symlink_that_stays_inside() {
        let link = |path: &str, target: &str| ShippedFile {
            path: path.into(),
            symlink_target: Some(target.into()),
        };
        let file = |path: &str| ShippedFile { path: path.into(), symlink_target: None };
        let files = SourceFiles::new(
            [
                file("pipelines/v2/Snakefile"),
                link("workflow", "pipelines/v2"),
                link("out", "../elsewhere"),
                link("a", "b"),
                link("b", "a"),
            ],
            true,
        );
        // Recorded as named (the checkout has the link), read through it.
        let through = Entrypoint {
            path: "workflow/Snakefile".into(),
            file: "pipelines/v2/Snakefile".into(),
            note: None,
        };
        assert_eq!(
            resolve_in(Some("workflow/Snakefile"), None, Engine::Snakemake, &files).unwrap(),
            through
        );
        // The default search sees it too, as detection does.
        let found = resolve_in(None, None, Engine::Snakemake, &files).unwrap();
        assert_eq!(found.path, "workflow/Snakefile");
        assert_eq!(found.file, "pipelines/v2/Snakefile");
        let err = message(resolve_in(Some("out/Snakefile"), None, Engine::Snakemake, &files));
        assert!(err.contains("symlink `out`") && err.contains("outside the repository"), "{err}");
        let err = message(resolve_in(Some("a/Snakefile"), None, Engine::Snakemake, &files));
        assert!(err.contains("loop"), "{err}");
    }

    #[test]
    fn a_nextflow_entrypoint_may_name_a_directory_but_a_snakemake_one_may_not() {
        let files = ["pipeline/pipeline.nf", "pipeline/nextflow.config", "Snakefile"];
        for (given, recorded) in [(".", "."), ("./", "."), ("pipeline/", "pipeline")] {
            let found = resolve(Some(given), None, Engine::Nextflow, &files).unwrap();
            assert_eq!(found.path, recorded, "{given}");
        }
        let err = message(resolve(Some("pipeline"), None, Engine::Snakemake, &files));
        assert!(err.contains("must be a file"), "{err}");
        let err = message(resolve(Some("."), None, Engine::Snakemake, &files));
        assert!(err.contains("must be a file"), "{err}");
        // A directory with nothing shipped under it is just missing.
        let err = message(resolve(Some("docs"), None, Engine::Nextflow, &files));
        assert!(err.contains("not in the source"), "{err}");
    }

    #[test]
    fn a_missing_explicit_entrypoint_suggests_what_was_probably_meant() {
        // The standard Snakefile, when there is one.
        let err = message(resolve(
            Some("Snakefile"),
            None,
            Engine::Snakemake,
            &["workflow/Snakefile", "workflow/rules/a.smk"],
        ));
        assert!(err.contains("did you mean `workflow/Snakefile`?"), "{err}");
        // The only other file of that name.
        let err = message(resolve(Some("main.nf"), None, Engine::Nextflow, &["pipeline/main.nf"]));
        assert!(err.contains("did you mean `pipeline/main.nf`?"), "{err}");
        // Nothing to suggest.
        let err =
            message(resolve(Some("run.nf"), None, Engine::Nextflow, &["a/main.nf", "b/main.nf"]));
        assert!(!err.contains("did you mean"), "{err}");
        // The message says what the source holds, submodules (git mode) included.
        assert!(err.contains("git-ignored") && err.contains("submodule"), "{err}");
    }

    #[test]
    fn without_the_flag_the_messages_name_only_biffix_yaml() {
        let files = SourceFiles::of(&["workflow/Snakefile", "rules/a.smk"]);
        let found =
            resolve_entrypoint(None, false, &BiffixManifest::default(), Engine::Snakemake, &files)
                .unwrap();
        let note = found.note.unwrap();
        assert!(!note.contains("--entrypoint") && note.contains("biffix.yaml"), "{note}");
        let err = message(resolve_entrypoint(
            None,
            false,
            &BiffixManifest::default(),
            Engine::Snakemake,
            &SourceFiles::of(&["rules/a.smk"]),
        ));
        assert!(!err.contains("--entrypoint") && err.contains("biffix.yaml"), "{err}");
    }

    #[test]
    fn every_standard_snakefile_location_is_detected_from_the_source() {
        let dir = tempfile::tempdir().unwrap();
        for location in SNAKEFILE_LOCATIONS {
            let (_, engine) =
                manifest_and_engine(dir.path(), None, &SourceFiles::of(&[location])).unwrap();
            assert_eq!(engine, Engine::Snakemake, "{location}");
        }
        // Detected from the source's names, not the disk: another case is not detected.
        let err = manifest_and_engine(dir.path(), None, &SourceFiles::of(&["Workflow/Snakefile"]))
            .unwrap_err();
        assert!(
            matches!(err, CliError::InvalidInput(ref m) if m.contains("declare `engine")),
            "{err:?}"
        );
    }
}
