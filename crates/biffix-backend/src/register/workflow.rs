//! Workflow + immutable WorkflowVersion.

use serde::{Deserialize, Serialize};

use biffix_workflow::engines::Engine;

/// Where a workflow's immutable source is fetched from: a git repository and ref, or an S3 URI.
/// A version pins the exact commit via [`WorkflowVersion::git_sha`]; this records *where* to get
/// it so a run is reproducible from provenance alone.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WorkflowSource {
    /// A git repository at a branch/tag/ref (resolved to `git_sha` at registration).
    Git { repo: String, git_ref: String },
    /// A workflow tarball or directory staged in S3.
    S3 {
        uri: String,
        /// The staged tarball's sha256 (hex), computed by the CLI from the bytes it uploads. The
        /// coordinator checks the object it downloads against it before running anything, since
        /// the key is only a name and its object can be replaced after registration. `None` only
        /// for a prefix (directory) source, which has no one object to check.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sha256: Option<String>,
    },
}

/// An immutable, frozen workflow version — the provenance anchor a run references.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WorkflowVersion {
    /// `{git_sha}:{content_hash}:{build}` — the source identity plus which registration of that
    /// source this is.
    pub id: String,
    /// Which registration of this exact source (git sha + content hash) this version is: 0, 1, 2…
    pub build: u32,
    pub git_sha: String,
    pub source_content_hash: String,
    pub image_digest: String,
    /// ECR coordinates (repository, registry/account id, region) of the deploy's workflow
    /// (worker-image) repository, where the version's imported default image lives
    /// (`register::pipeline::workflow_repo_coords`), recorded whenever the version has an imported
    /// default, whatever registry it was imported from. They let the UI deep-link the image to its
    /// ECR console page. `None` when the version has no imported default image, or the deploy
    /// configures no workflow repository.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub image_repository: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub image_registry_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub image_region: Option<String>,
    pub engine: Engine,
    pub engine_version: String,
    pub entrypoint: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub target: Option<String>,
    /// The normalized FormSchema (opaque JSON here; the adapter lives in the registration path).
    pub form_schema: serde_json::Value,
    /// Queue-classes this version's tasks reference (validated against the deploy menu).
    #[serde(default)]
    pub queue_classes: Vec<String>,
    /// The version's About page (Markdown from the manifest's `about:`), when it has one.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub about_markdown: Option<String>,
    /// The engine runtime the pinned `engine_version` is built into: the key of `runtime_recipe`
    /// (`biffix_workflow::runtime::runtime_key`).
    pub runtime_key: String,
    /// The runtime's recipe (`biffix_workflow::runtime::RuntimeRecipe`) as frozen at registration,
    /// so the version keeps its exact pins when biffix's pin tables move on.
    pub runtime_recipe: serde_json::Value,
}
