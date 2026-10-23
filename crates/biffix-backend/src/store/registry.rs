//! Workflow registry: persistence for frozen, immutable `WorkflowVersion`s and their workflow-level
//! metadata.
//!
//! A `WorkflowVersion` is provenance the run log references; the registry is *not* event-sourced
//! (it is reference data, not a fold over `RunEvent`s). It is a plain keyed table: registration
//! freezes a version once (immutably) and a run resolves it by id at submit. The read side is a
//! trait so the submit/register HTTP handlers stay unit-testable without a database.

use std::collections::{HashMap, HashSet};

use async_trait::async_trait;
use biffix_core::builds::BuildProjection;

use crate::register::workflow::{WorkflowSource, WorkflowVersion};
use crate::store::StoreError;

/// A frozen `WorkflowVersion` plus the workflow-level metadata a run needs to fetch and attribute
/// it: the human name, the owning project, where the source lives, and who registered it.
#[derive(Clone, Debug, PartialEq)]
pub struct RegisteredVersion {
    /// Display name of the workflow this version belongs to.
    pub name: String,
    /// Owning project (blast-radius scope).
    pub project: String,
    /// Where the immutable source is fetched from (git repo+ref or an S3 uri).
    pub source: WorkflowSource,
    /// The immutable frozen version (id = `{git_sha}:{content_hash}:{build}`).
    pub version: WorkflowVersion,
    /// What the registration was given beyond the source, for re-registering it.
    pub registration_inputs: biffix_api::RegistrationInputs,
    /// Hash of everything that determines the version (see `register::version::inputs_hash`); a
    /// registration with the same hash as an existing build of the source is refused.
    pub registration_inputs_hash: String,
    /// The identity that registered it (`user:<id>`); None when auth is disabled locally.
    pub created_by: Option<String>,
    /// A human label for the registrant (email/name), captured at registration the same way the run
    /// submitter label is — access tokens lack `email`, so a Cognito username is resolved. None
    /// when unresolvable.
    pub created_by_label: Option<String>,
    /// Registration time (epoch micros).
    pub created_at_micros: i64,
    /// The version's declared images, pinned (sorted by name).
    pub images: Vec<VersionImage>,
}

/// One image a version declares (`images:` / `image:` / `--image`), pinned at registration.
#[derive(Clone, Debug, PartialEq)]
pub struct VersionImage {
    /// `default` is the image tasks without their own run on.
    pub name: String,
    /// As declared.
    pub declared_ref: String,
    /// Pinned: `<registry>/<repo>[:tag]@sha256:…`.
    pub source_ref: String,
    /// The import's key (`biffix_workflow::images::content_key` of the source digest).
    pub content_key: String,
    /// `runtime: own` (a Snakemake image that carries its own Snakemake).
    pub own_runtime: bool,
    /// How the image is built, when it is built from a recipe the workflow ships instead of
    /// imported (`None`). Its `content_key` is then the build's key.
    pub build: Option<BuildRecipe>,
}

impl VersionImage {
    /// `import`, `dockerfile` or `pixi`.
    pub fn kind(&self) -> &str {
        self.build.as_ref().map_or("import", |b| b.kind.as_str())
    }
}

/// What an image build needs, recorded on the version at registration.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct BuildRecipe {
    /// `dockerfile` | `pixi`.
    pub kind: String,
    /// Where the context is in the workflow (`docker/align`; `` for the root).
    pub context: String,
    /// `s3://<project bucket>/workflow-sources/…/contexts/<key12>.tar.gz`.
    pub context_uri: String,
    pub context_sha256: String,
    /// [`biffix_workflow::version::content_hash`] of the context's files.
    pub tree_hash: String,
    /// The Dockerfile's path within the context (`dockerfile` builds).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dockerfile: Option<String>,
    /// The Dockerfile biffix generated (`pixi` builds), exactly as built.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dockerfile_text: Option<String>,
    /// The architectures built (`amd64`, `arm64`).
    pub archs: Vec<String>,
}

impl BuildRecipe {
    /// The architectures, parsed (unknown names are skipped: the server wrote them).
    pub(crate) fn archs(&self) -> Vec<biffix_workflow::images::Arch> {
        self.archs.iter().filter_map(|a| biffix_workflow::images::Arch::parse(a)).collect()
    }
}

/// An imported image: the result of an import build (tenant-wide, by content key).
#[derive(Clone, Debug, PartialEq)]
pub struct ImageArtifact {
    pub content_key: String,
    pub source_ref: String,
    /// `<repo>@sha256:…`: the workflow repository for an import (and its derive), the builds
    /// repository for a built image (and its derive).
    pub image: String,
    pub digest: String,
    pub archs: Vec<String>,
    pub capabilities: biffix_workflow::images::Capabilities,
    pub keep_tag: Option<String>,
    pub archived: bool,
    pub archive_bytes: Option<i64>,
    /// Epoch micros.
    pub created_at: i64,
}

/// Read/write access to the immutable workflow-version registry.
#[async_trait]
pub trait RegistryStore: Send + Sync {
    /// Persist a newly frozen version. Immutability is enforced by the caller (duplicate id is
    /// rejected before freezing); the store treats a repeated id, or a second version claiming the
    /// same build of one source (`git_sha`, `source_content_hash`, `build`), as a conflict.
    async fn put_version(&self, rec: &RegisteredVersion) -> Result<(), StoreError>;

    /// Fetch one version by its frozen id, or [`StoreError::NotFound`].
    async fn get_version(&self, id: &str) -> Result<RegisteredVersion, StoreError>;

    /// List all registered versions, newest first (ties by id).
    async fn list_versions(&self) -> Result<Vec<RegisteredVersion>, StoreError>;

    /// Whether some registered version lives in `project` (the projects biffix knows). The default
    /// scans [`Self::list_versions`], which is exact; Postgres asks with one `EXISTS`.
    async fn has_project(&self, project: &str) -> Result<bool, StoreError> {
        Ok(self.list_versions().await?.iter().any(|v| v.project == project))
    }

    /// The versions already frozen from this exact source (git sha + content hash), for assigning
    /// the next build number and refusing an identical registration. Ordered by build.
    async fn source_builds(
        &self,
        git_sha: &str,
        content_hash: &str,
    ) -> Result<Vec<SourceBuild>, StoreError>;

    /// A build subject's latest build (the build projection), or `None` if it was never built.
    ///
    /// No defaults on this or the methods below: an empty read or a no-op write, silently inherited
    /// by a store that forgot it, loses import and variant records and sends registration into a
    /// re-import loop instead of failing to compile (the rule `Store` states too).
    async fn get_build(&self, subject: &str) -> Result<Option<BuildProjection>, StoreError>;

    /// The coordinator variants registered for a runtime, newest first.
    async fn coordinator_variants(
        &self,
        runtime_key: &str,
    ) -> Result<Vec<CoordinatorVariant>, StoreError>;

    /// Record a variant's job definition. A variant already recorded (same runtime and base) keeps
    /// its image; recorded again with that image, it takes the new job definition (one cloned from
    /// a newer coordinator template, `BatchJobs::variant_is_current`).
    async fn put_coordinator_variant(&self, variant: &CoordinatorVariant)
        -> Result<(), StoreError>;

    /// The imported image of `content_key`, once its import succeeded.
    async fn get_image_artifact(
        &self,
        content_key: &str,
    ) -> Result<Option<ImageArtifact>, StoreError>;

    /// Record an import's result; one already recorded is left as it is.
    async fn put_image_artifact(&self, artifact: &ImageArtifact) -> Result<(), StoreError>;

    /// The (project, name) of every workflow hidden from the Lab's pipeline cards
    /// (`workflow_listing`).
    async fn hidden_workflows(&self) -> Result<HashSet<(String, String)>, StoreError>;

    /// One workflow's listing row (`workflow_listing`); `None` when it was never set (shown).
    async fn workflow_listing(
        &self,
        project: &str,
        name: &str,
    ) -> Result<Option<biffix_api::WorkflowListing>, StoreError>;

    /// Hide (or show) a workflow on the Lab's pipeline cards, recording who did it and when, and
    /// return the stored row. [`StoreError::NotFound`] when no registered version has that
    /// (project, name): a listing is for workflows that exist.
    async fn put_workflow_listing(
        &self,
        project: &str,
        name: &str,
        hidden: bool,
        updated_by: Option<&str>,
    ) -> Result<biffix_api::WorkflowListing, StoreError>;

    /// Which of `version_ids` are released now, each with the change that released it (its newest
    /// `workflow_version_releases` row). A version absent from the map is a developer version.
    /// Reads only each named version's newest row, so its cost follows the versions asked about,
    /// not the length of the release history.
    async fn released_versions(
        &self,
        version_ids: &[String],
    ) -> Result<HashMap<String, biffix_api::WorkflowReleaseChange>, StoreError>;

    /// Every release and un-release of one version, newest first; empty when it was never
    /// released (or no such version is registered).
    async fn release_history(
        &self,
        version_id: &str,
    ) -> Result<Vec<biffix_api::WorkflowReleaseChange>, StoreError>;

    /// Release (or un-release) a version, recording who did it and when, and return its history
    /// after (newest first). Only a change is recorded: releasing a released version (or
    /// un-releasing a developer one) records nothing. [`StoreError::NotFound`] when no version has
    /// that id.
    async fn set_version_released(
        &self,
        version_id: &str,
        released: bool,
        actor: Option<&str>,
        actor_label: Option<&str>,
    ) -> Result<Vec<biffix_api::WorkflowReleaseChange>, StoreError>;

    /// Replace the result recorded under `artifact.content_key` with `artifact`, only while the
    /// recorded one names `stale_image` (a built image recorded outside the builds repository,
    /// `crate::images::build::recorded`, now built again): a no-op when nothing is recorded or
    /// something else already replaced it, so concurrent replacements settle on the first.
    async fn replace_image_artifact(
        &self,
        stale_image: &str,
        artifact: &ImageArtifact,
    ) -> Result<(), StoreError>;
}

/// A coordinator variant a run can launch on: the base coordinator image plus one engine runtime,
/// and the Batch job definition registered for it (Batch cannot override a job's image).
#[derive(Clone, Debug, PartialEq)]
pub struct CoordinatorVariant {
    pub runtime_key: String,
    /// The base coordinator image the variant was built on (`sha256:…`).
    pub base_digest: String,
    /// `<repo>@sha256:…` of the variant.
    pub image: String,
    pub job_definition_arn: String,
    /// Epoch micros.
    pub created_at: i64,
}

/// One version frozen from a given source.
#[derive(Clone, Debug, PartialEq)]
pub struct SourceBuild {
    pub id: String,
    pub build: u32,
    pub registration_inputs_hash: String,
}

impl From<&RegisteredVersion> for SourceBuild {
    fn from(rec: &RegisteredVersion) -> Self {
        Self {
            id: rec.version.id.clone(),
            build: rec.version.build,
            registration_inputs_hash: rec.registration_inputs_hash.clone(),
        }
    }
}
