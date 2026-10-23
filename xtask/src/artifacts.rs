//! The checked-in `schemas/` artifacts: the ONE list of what `cargo xtask emit-schemas` writes, and
//! the drift check that both `emit-schemas --check` (CI) and `checked_in_artifacts_match` (a plain
//! `cargo test`) run against it.

use std::path::{Path, PathBuf};

/// Every artifact under `schemas/`, as `(file name, content)`: the JSON Schemas derived from the
/// contract types, and the cross-language fixture tables the UI and plugin tests read.
pub(crate) fn all() -> Vec<(&'static str, String)> {
    fn gen<T: schemars::JsonSchema>() -> String {
        pretty_json(&schemars::schema_for!(T))
    }
    vec![
        ("runevent.schema.json", gen::<biffix_core::event::RunEvent>()),
        ("formschema.schema.json", gen::<biffix_workflow::formschema::FormSchema>()),
        ("samples.schema.json", gen::<biffix_api::SamplesContract>()),
        ("sample-progress.schema.json", gen::<biffix_core::sample_progress::SampleGrid>()),
        ("wire.engine.schema.json", gen::<biffix_core::wire::engine::EngineEvent>()),
        ("validate-cases.json", crate::cases::validate::cases_json()),
        ("sheet-cases.json", crate::cases::sheet::cases_json()),
        ("image-ref-cases.json", crate::cases::image_ref::cases_json()),
        ("failure-causes.json", failure_causes_json()),
        ("ingest-limits.json", ingest_limits_json()),
    ]
}

/// The ingest route's limits and the plugins' poster policy, which both plugins' tests pin their
/// constants to (`biffix_core::ingest_limits`).
fn ingest_limits_json() -> String {
    pretty_json(&biffix_core::ingest_limits::to_json())
}

/// Every failure cause and *Try again* mode by wire name, in declaration order, so the UI can pin
/// its wording table to the server's list (`FailureItem.cause` / `FailureItem.try_again`).
fn failure_causes_json() -> String {
    use biffix_core::failure::{FailureCause, TryAgain};
    let causes: Vec<&str> = FailureCause::ALL.into_iter().map(FailureCause::as_str).collect();
    let try_again: Vec<&str> = TryAgain::ALL.into_iter().map(TryAgain::as_str).collect();
    pretty_json(&serde_json::json!({
        "causes": causes,
        "try_again": try_again,
    }))
}

/// `value` as pretty JSON with a trailing newline: how every generated artifact is written.
pub(crate) fn pretty_json(value: &impl serde::Serialize) -> String {
    let mut s = serde_json::to_string_pretty(value).expect("serialize an artifact");
    s.push('\n');
    s
}

/// An I/O error's mapper that names `path`: the bare error says only what failed, not where.
pub(crate) fn at(path: &Path) -> impl FnOnce(std::io::Error) -> std::io::Error + '_ {
    move |e| std::io::Error::new(e.kind(), format!("{}: {e}", path.display()))
}

/// The repository's `schemas/` directory.
pub(crate) fn schemas_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives at <repo>/xtask")
        .join("schemas")
}

/// One way the checked-in `schemas/` differs from [`all`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Drift {
    /// An artifact whose file is absent.
    Missing(&'static str),
    /// An artifact whose file content differs from what the types generate.
    OutOfDate(&'static str),
    /// A `*.json` file that no artifact emits any more (its type was removed or renamed).
    Stale(String),
}

impl std::fmt::Display for Drift {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Drift::Missing(name) => {
                write!(f, "{name} is missing (regenerate with cargo xtask emit-schemas)")
            }
            Drift::OutOfDate(name) => {
                write!(f, "{name} is out of date (regenerate with cargo xtask emit-schemas)")
            }
            Drift::Stale(name) => write!(f, "{name} is no longer emitted (delete it)"),
        }
    }
}

/// `*.json` files in `dir` that no artifact in `artifacts` emits, sorted. A missing `dir` has none.
pub(crate) fn stale_files(
    dir: &Path,
    artifacts: &[(&'static str, String)],
) -> std::io::Result<Vec<String>> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(at(dir)(e)),
    };
    let mut stale = Vec::new();
    for entry in entries {
        let name = entry.map_err(at(dir))?.file_name().to_string_lossy().into_owned();
        if name.ends_with(".json") && !artifacts.iter().any(|(a, _)| *a == name) {
            stale.push(name);
        }
    }
    stale.sort();
    Ok(stale)
}

/// Compare `dir` with `artifacts` without writing anything. Empty means no drift.
pub(crate) fn check(
    dir: &Path,
    artifacts: &[(&'static str, String)],
) -> std::io::Result<Vec<Drift>> {
    let mut drift = Vec::new();
    for (name, content) in artifacts {
        let path = dir.join(name);
        match std::fs::read_to_string(&path) {
            Ok(existing) if existing == *content => {}
            Ok(_) => drift.push(Drift::OutOfDate(name)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => drift.push(Drift::Missing(name)),
            Err(e) => return Err(at(&path)(e)),
        }
    }
    drift.extend(stale_files(dir, artifacts)?.into_iter().map(Drift::Stale));
    Ok(drift)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh directory, removed when the guard drops (a failing test included).
    fn scratch() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn check_reports_missing_out_of_date_and_stale() {
        let tmp = scratch();
        let dir = tmp.path();
        let artifacts = vec![
            ("a.schema.json", "A\n".to_string()),
            ("b.schema.json", "B\n".to_string()),
            ("c.schema.json", "C\n".to_string()),
        ];
        std::fs::write(dir.join("a.schema.json"), "A\n").unwrap();
        std::fs::write(dir.join("b.schema.json"), "old\n").unwrap();
        std::fs::write(dir.join("gone.schema.json"), "{}").unwrap();
        std::fs::write(dir.join("README.md"), "not an artifact").unwrap();
        assert_eq!(
            check(dir, &artifacts).unwrap(),
            vec![
                Drift::OutOfDate("b.schema.json"),
                Drift::Missing("c.schema.json"),
                Drift::Stale("gone.schema.json".into()),
            ]
        );
    }

    #[test]
    fn an_unreadable_artifact_is_an_error_naming_its_path() {
        let tmp = scratch();
        let dir = tmp.path();
        std::fs::create_dir(dir.join("a.schema.json")).unwrap();
        let err = check(dir, &[("a.schema.json", "A\n".to_string())]).unwrap_err();
        assert!(
            err.to_string().starts_with(&format!("{}: ", dir.join("a.schema.json").display())),
            "{err}"
        );
    }

    #[test]
    fn check_does_not_create_a_missing_dir() {
        let tmp = scratch();
        let dir = tmp.path().join("missing");
        let artifacts = vec![("a.schema.json", "A\n".to_string())];
        assert_eq!(check(&dir, &artifacts).unwrap(), vec![Drift::Missing("a.schema.json")]);
        assert!(!dir.exists());
    }

    /// The checked-in `schemas/` must match what the types generate, so every artifact, and every
    /// stale file no artifact emits, is covered by `cargo test` as well as by `--check`.
    #[test]
    fn checked_in_artifacts_match() {
        let drift = check(&schemas_dir(), &all()).unwrap();
        let report: Vec<String> = drift.iter().map(ToString::to_string).collect();
        assert!(drift.is_empty(), "schemas/ drifted:\n{}", report.join("\n"));
    }

    #[test]
    fn artifact_names_are_unique() {
        let artifacts = all();
        let mut names: Vec<_> = artifacts.iter().map(|(n, _)| *n).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), artifacts.len());
    }
}
