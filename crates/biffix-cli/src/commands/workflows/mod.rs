//! `biffix workflows …` and `biffix engines`, a module per command family:
//! `register` (with its `images`, and the `wait` until a version can run), `reregister`, `catalog`
//! (list, release, show, engines) and `form` (the launch-form preview).

mod catalog;
mod form;
mod images;
mod register;
mod reregister;
mod wait;

use std::path::Path;

pub(crate) use catalog::engines;
pub use catalog::{list, release, show};
pub(crate) use form::{form, FormOpts};
pub use register::{register, RegisterOpts};
pub use reregister::{reregister, ReregisterOpts};

/// Infer the workflow name from the checkout path's directory name. `Path::file_name()` is `None`
/// for `.`, `./`, and `..`, so for those (only) fall back to the canonicalized path — but never
/// for a path that already resolves a name, so a working symlinked directory keeps its own name.
fn infer_name(root: &Path) -> Option<String> {
    root.file_name().map(|n| n.to_string_lossy().into_owned()).or_else(|| {
        root.canonicalize()
            .ok()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
    })
}

/// A registered version of workflow `name` in `project`, created at `at`, for the tests.
#[cfg(test)]
fn version(id: &str, project: &str, name: &str, at: i64) -> biffix_api::WorkflowVersionSummary {
    biffix_api::WorkflowVersionSummary {
        id: biffix_core::ids::WorkflowVersionId::new(id),
        name: name.into(),
        project: project.into(),
        image_digest: String::new(),
        image_repository: None,
        image_registry_id: None,
        image_region: None,
        engine: "snakemake".into(),
        engine_version: "9.27.0".into(),
        git_sha: "abc".into(),
        queue_classes: vec![],
        created_by: None,
        created_by_label: None,
        created_at_micros: at,
        run_count: 0,
        title: None,
        summary: None,
        state: None,
        catalog: None,
        last_run_at: None,
        hidden: false,
        release: Default::default(),
        typical: None,
    }
}
