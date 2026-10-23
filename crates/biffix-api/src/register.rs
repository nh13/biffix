//! `register` — workflow-version registration (request) and its views (responses).

use serde::{Deserialize, Serialize};

use crate::{SupportContact, TypicalRun};

use biffix_core::ids::WorkflowVersionId;

/// `POST /workflows` request: freeze an immutable workflow version. The caller (CI/CLI) supplies
/// the resolved `git_sha` + `source_content_hash` + normalized `form_schema`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct RegisterRequest {
    /// Display name of the workflow.
    pub name: String,
    /// Owning project (blast-radius scope).
    pub project: String,
    /// `WorkflowSource`-shaped: `{"kind":"git","repo":..,"git_ref":..}` or
    /// `{"kind":"s3","uri":..}`.
    pub source: serde_json::Value,
    /// Resolved commit SHA the version pins.
    pub git_sha: String,
    /// Deterministic content hash over the source tree.
    pub source_content_hash: String,
    /// Default worker image: an image reference (pinned to its digest), or a bare `sha256:...`
    /// digest of an image in the deploy's workflow repository.
    /// Optional — omit it when every rule/process declares its own container (Nextflow `container`,
    /// Snakemake's `aws_batch_container_image` resource); the version then carries no default
    /// image.
    #[serde(default)]
    pub image: Option<String>,
    /// Engine (default `snakemake`).
    #[serde(default)]
    pub engine: Option<String>,
    pub engine_version: String,
    pub entrypoint: String,
    #[serde(default)]
    pub target: Option<String>,
    /// The normalized FormSchema (as produced by the schema→form adapter).
    pub form_schema: serde_json::Value,
    /// Worker queue-classes this version's tasks reference (validated against the deploy menu).
    #[serde(default)]
    pub queue_classes: Option<Vec<String>>,
    /// The version's About page: the Markdown file the manifest names with `about:` (at most
    /// [`MAX_ABOUT_BYTES`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub about_markdown: Option<String>,
    /// How the CLI was invoked, recorded so the version can be re-registered from its inputs.
    pub registration_inputs: RegistrationInputs,
    /// The workflow's named images (`images:` in `biffix.yaml`), each imported into (or built in)
    /// the tenant repository. An imported default image is sent in `image`; a default image built
    /// from the workflow is sent here as `images["default"]` (with `image` left unset — sending
    /// both is rejected).
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub images: std::collections::BTreeMap<String, ImageRequest>,
    /// Nextflow only: the plugins the workflow declares (`plugins:` in `biffix.yaml`), each pinned
    /// by the sha256 of its registry zip. They are built into the version's runtime (the
    /// coordinator runs Nextflow offline), so they are part of its runtime key. Empty for a
    /// workflow that declares none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub nextflow_plugins: Vec<NextflowPluginRequest>,
}

/// One Nextflow plugin a workflow declares, at an exact version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct NextflowPluginRequest {
    /// The plugin id (`nf-schema`).
    pub id: String,
    /// The exact version (`2.7.2`).
    pub version: String,
    /// sha256 (64 lowercase hex) of the plugin registry's `<id>-<version>.zip`.
    pub sha256: String,
}

impl From<&biffix_workflow::runtime::WorkflowPlugin> for NextflowPluginRequest {
    fn from(p: &biffix_workflow::runtime::WorkflowPlugin) -> Self {
        Self { id: p.id.clone(), version: p.version.clone(), sha256: p.sha256.clone() }
    }
}

impl From<&NextflowPluginRequest> for biffix_workflow::runtime::WorkflowPlugin {
    fn from(p: &NextflowPluginRequest) -> Self {
        Self { id: p.id.clone(), version: p.version.clone(), sha256: p.sha256.clone() }
    }
}

/// One named image in a registration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ImageRequest {
    /// An image reference to import (`quay.io/biocontainers/fastqc:0.12.1--hdfd78af_0`). Exactly
    /// one of `ref` and `build`.
    #[serde(rename = "ref", default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
    /// A recipe in the workflow to build the image from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build: Option<ImageBuildRequest>,
    /// `own`: a Snakemake image that carries its own Snakemake.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<String>,
}

/// An image to build from a recipe the workflow ships; its context is staged first
/// (`POST /workflows/source-upload` with `context_tree_hash`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ImageBuildRequest {
    /// `dockerfile` | `pixi`.
    pub kind: String,
    /// The context directory in the workflow (`docker/align`; an empty string for the root). For
    /// `pixi`, the environment's directory.
    pub context: String,
    /// The Dockerfile's path within the context (`dockerfile`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dockerfile: Option<String>,
    /// The staged context tarball: `s3://<project bucket>/workflow-sources/<source id>/contexts/
    /// <tree_hash>.tar.gz`.
    pub context_uri: String,
    /// The tarball's sha256 (hex).
    pub context_sha256: String,
    /// The content hash of the context's files (paths within the context).
    pub tree_hash: String,
    /// The architectures to build (`amd64`, `arm64`).
    pub archs: Vec<String>,
}

/// One of a version's images and where its import is.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct WorkflowImage {
    pub name: String,
    /// `import` (an existing image, imported), `dockerfile` or `pixi` (built from the workflow).
    #[serde(default = "import_kind")]
    pub kind: String,
    /// As declared (a built image: its name).
    pub declared_ref: String,
    /// Pinned at registration: `<registry>/<repo>[:tag]@sha256:…`; a built image: what it is built
    /// from (`dockerfile:docker/align/Dockerfile`, `pixi:envs/worker`).
    pub source_ref: String,
    /// `unbuilt` | `building` | `failed` | `ready`.
    pub state: String,
    /// The imported image (`<tenant workflow repo>@sha256:…`), once imported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    #[serde(default)]
    pub archs: Vec<String>,
    /// What the import found in it (`bash`, `libc`, `snakemake`).
    // Deliberately `Value`, not `biffix_workflow::images::Capabilities`: it is `null` until the
    // import finishes, and `Capabilities` has required fields and drops keys it does not know, so a
    // CLI built against it would fail on (or silently strip) a capability a newer server reports.
    // The OpenAPI shape is an open object either way.
    #[cfg_attr(feature = "openapi", schema(value_type = Object))]
    #[serde(default)]
    pub capabilities: serde_json::Value,
    #[serde(default)]
    pub archived: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keep_tag: Option<String>,
    /// The image tasks run when it is derived (a Snakemake image with the version's engine runtime
    /// added: `<tenant workflow repo>@sha256:…`), once derived.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub derived_image: Option<String>,
    /// The derive's state (`unbuilt` | `building` | `failed` | `ready`), for a derived image whose
    /// import is done.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub derived_state: Option<String>,
    /// Why it cannot be used (a failed import or derive, or an image unsuited to the engine).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logs_url: Option<String>,
    /// The Dockerfile biffix generated to build it (a `pixi` image).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generated_dockerfile: Option<String>,
    /// A built image's build per architecture.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub arch_builds: Vec<ImageArchBuild>,
}

fn import_kind() -> String {
    "import".into()
}

/// One architecture's build of a built image.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ImageArchBuild {
    /// `amd64` | `arm64`.
    pub arch: String,
    /// `unbuilt` | `requested` | `running` | `succeeded` | `failed`.
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logs_url: Option<String>,
}

/// What a registration was given beyond the source itself (whose `biffix.yaml` travels with it):
/// enough, with the name, project and source, to register the same version again.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct RegistrationInputs {
    /// How the coordinator fetches the source: `git` or `s3`.
    pub source_mode: String,
    /// The CLI release that registered it.
    pub cli_version: String,
    #[serde(default)]
    pub overrides: RegistrationOverrides,
    /// The version this one was re-registered from (`POST /workflows/{id}/reregister`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(value_type = Option<String>))]
    pub reregistered_from: Option<WorkflowVersionId>,
}

/// Command-line values that override or add to the source's `biffix.yaml`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct RegistrationOverrides {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entrypoint: Option<String>,
    /// The default worker image as given (a tag or digest), before it was pinned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub queue_classes: Vec<String>,
}

/// `POST /workflows/{id}/reregister` — register a version again from what it recorded, on this
/// deploy's engine runtime, re-pinning its images.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ReregisterRequest {
    /// An exact engine release to use instead of the recorded one (needed for a version that
    /// recorded only a major version, e.g. `9`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine_version: Option<String>,
    /// Report what re-registering would do without registering anything.
    #[serde(default)]
    pub dry_run: bool,
    /// The project to register the new version in, instead of the one the version was registered
    /// in: how a version labelled with a project the deploy does not configure is moved into one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
}

/// What re-registering a version would do (`dry_run`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ReregisterPlan {
    /// The version re-registered from.
    #[cfg_attr(feature = "openapi", schema(value_type = String))]
    pub from: WorkflowVersionId,
    /// The version it would register; when `identical_to` is set, nothing would be registered.
    #[cfg_attr(feature = "openapi", schema(value_type = String))]
    pub id: WorkflowVersionId,
    pub build: u32,
    /// The existing build with the same inputs, when re-registering would change nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(value_type = Option<String>))]
    pub identical_to: Option<WorkflowVersionId>,
    pub engine_version: String,
    pub runtime_key: String,
    /// The runtime is already built on this deploy.
    pub runtime_cached: bool,
    pub images: Vec<ReregisterImagePlan>,
}

/// One image in a [`ReregisterPlan`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ReregisterImagePlan {
    pub name: String,
    /// `import`, `dockerfile` or `pixi`.
    pub kind: String,
    /// Pinned now.
    pub source_ref: String,
    /// What the version re-registered from pinned, when it differs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_source_ref: Option<String>,
    /// Already imported or built on this deploy, and for a Snakemake image that is not
    /// `runtime: own`, already derived with the version's runtime: nothing to build for it.
    pub cached: bool,
}

/// How a `400` refusing a registration identical to an existing build begins (`identical to build
/// N (id): …`). `biffix workflows reregister` reports that refusal as "up to date", so both sides
/// spell it from here.
pub const IDENTICAL_BUILD_PREFIX: &str = "identical to build";

/// The largest About page a version may carry, in bytes.
pub const MAX_ABOUT_BYTES: usize = 256 * 1024;

/// `GET /engines` — an engine biffix runs and the releases a workflow version may pin as its
/// `engine_version`: an exact release from `min_version` (inclusive) to `below_version`
/// (exclusive), written the way the engine writes its versions — and, when `releases` is given,
/// one of those (the only releases biffix can build; Nextflow's are pinned by sha256).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct EngineSupport {
    pub engine: String,
    pub min_version: String,
    pub below_version: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub releases: Vec<String>,
}

impl From<biffix_workflow::engines::SupportedRange> for EngineSupport {
    fn from(range: biffix_workflow::engines::SupportedRange) -> Self {
        Self {
            engine: range.engine.into(),
            min_version: range.min.into(),
            below_version: range.below.into(),
            releases: range.releases.iter().map(std::string::ToString::to_string).collect(),
        }
    }
}

/// `GET /workflows` query: which versions to list.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::IntoParams))]
#[cfg_attr(feature = "openapi", into_params(parameter_in = Query))]
pub struct WorkflowListQuery {
    /// Only released (`true`) or only developer (`false`) versions; absent: every version. A value
    /// other than `true`/`false` is a 400.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub released: Option<bool>,
}

/// `POST /workflows` / `GET /workflows` summary view (omits `form_schema` for payload size).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct WorkflowVersionSummary {
    #[cfg_attr(feature = "openapi", schema(value_type = String))]
    pub id: WorkflowVersionId,
    pub name: String,
    pub project: String,
    pub image_digest: String,
    /// ECR coordinates of the frozen image (for a console deep-link); absent when unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_repository: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_registry_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_region: Option<String>,
    pub engine: String,
    pub engine_version: String,
    pub git_sha: String,
    pub queue_classes: Vec<String>,
    /// Who registered this version (actor id captured at registration); absent when auth is off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_by: Option<String>,
    /// A human label (email/name) for the registrant, when it could be resolved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_by_label: Option<String>,
    /// When this version was registered, epoch microseconds.
    pub created_at_micros: i64,
    /// Number of runs launched from this exact version (joined from the runs projection).
    pub run_count: i64,
    /// The pipeline's friendly title (its form's `title`, from `biffix.yaml`); absent when the
    /// author gave none, and the name is the title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// A one-line summary: the form's description, else the first sentence of the About page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// After a registration: `ready` | `pending` | `failed` (see `GET /workflows/{id}`). Absent in
    /// lists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    /// How the version is listed in the Lab's pipeline catalog; absent when there is nothing to
    /// say. A form with no `catalog:` block still gets one when the server can say what a run gives
    /// or takes (`results`/`inputs`), so its presence does not mean the author declared a catalog.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catalog: Option<CatalogEntry>,
    /// When the newest run of this exact version was submitted, epoch seconds; absent when it has
    /// none (or in a registration's response).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run_at: Option<f64>,
    /// Hidden from the Lab's pipeline cards (`PUT /workflows/listing`), set for every version of
    /// the workflow's (project, name). The Expert list still shows it. Read from the workflow's
    /// listing in `GET /workflows` and in a registration's (or re-registration's) response alike.
    /// Distinct from `catalog`, which is the author's, frozen with the version.
    #[serde(default)]
    pub hidden: bool,
    /// Whether this exact version is released (`PUT /workflows/{id}/release`): a Developer or
    /// Admin promoted it; every other version is a developer version. Per version, unlike
    /// `hidden`. A registration's (or re-registration's) new version is never released.
    #[serde(default)]
    pub release: VersionRelease,
    /// How long (and how much) one run of the pipeline usually takes, as
    /// `WorkflowEstimate.typical`: from the recent successful runs of its (project, name), any
    /// version. Set in `GET /workflows` for every version of the pipeline alike (so a catalog asks
    /// no more); absent until two such runs, and in a registration's response.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub typical: Option<TypicalRun>,
}

/// A version's entry in the Lab's pipeline catalog (`catalog:` in `biffix.yaml`, frozen with the
/// form).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct CatalogEntry {
    /// The card's description: the catalog's, else the form's, else the About page's first
    /// sentence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Tags to filter on (normalized at registration: trimmed, unique ignoring case).
    #[serde(default)]
    pub tags: Vec<String>,
    /// The author vouches for the workflow.
    #[serde(default)]
    pub verified: bool,
    /// Who to ask about this workflow's runs (the author's `catalog.contact`); wins over the site's
    /// support contact.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contact: Option<SupportContact>,
    /// What a run gives you: the names of the key results the version declares (`reports:` in
    /// `biffix.yaml`), in declaration order, each once.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub results: Vec<String>,
    /// What a run takes: the launch form's main inputs in plain words (its samplesheet and file
    /// inputs a launch must give, and its reference dropdown), in form order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inputs: Vec<String>,
}

/// `PUT /workflows/listing` request: hide (or show again) a workflow on the Lab's pipeline cards.
/// Applies to every version of the (project, name). A curator's setting, unlike the author's
/// `catalog:` block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct WorkflowListingUpdateRequest {
    pub project: String,
    /// The workflow's name (may contain `/`, hence the body rather than the path).
    pub name: String,
    pub hidden: bool,
}

/// A workflow's listing, as stored (`PUT /workflows/listing` response).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct WorkflowListing {
    pub project: String,
    pub name: String,
    pub hidden: bool,
    /// When it was last set, epoch microseconds.
    pub updated_at_micros: i64,
    /// Who last set it (actor id); absent when unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_by: Option<String>,
}

/// `PUT /workflows/{id}/release` request: release a version, or (`false`) take the release back
/// so it is a developer version again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct WorkflowReleaseUpdateRequest {
    pub released: bool,
}

/// One release or un-release of a version, as recorded (never changed afterwards).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct WorkflowReleaseChange {
    /// `true`: released; `false`: un-released.
    pub released: bool,
    /// When, epoch microseconds.
    pub at_micros: i64,
    /// Who (actor id); absent when unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<String>,
    /// A human label (email/name) for who, when it could be resolved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by_label: Option<String>,
}

/// Whether a version is released now, and by whom: the one release shape every workflow read
/// carries (`GET /workflows`, `GET /workflows/{id}`, `PUT /workflows/{id}/release`).
/// `at_micros`, `by` and `by_label` describe the change that released it, so they are set only
/// while `released`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct VersionRelease {
    #[serde(default)]
    pub released: bool,
    /// When it was released, epoch microseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at_micros: Option<i64>,
    /// Who released it (actor id).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<String>,
    /// A human label (email/name) for who released it, when it could be resolved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by_label: Option<String>,
}

impl VersionRelease {
    /// The release a version's newest recorded change gives it: released when that change
    /// released it (`None`, or an un-release, is a developer version).
    pub fn of(newest: Option<&WorkflowReleaseChange>) -> Self {
        match newest.filter(|c| c.released) {
            Some(c) => Self {
                released: true,
                at_micros: Some(c.at_micros),
                by: c.by.clone(),
                by_label: c.by_label.clone(),
            },
            None => Self::default(),
        }
    }
}

/// A version's release in full (the `PUT /workflows/{id}/release` answer): whether it is released
/// now, every release and un-release of it, newest first, and whether the caller may change it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct WorkflowRelease {
    #[cfg_attr(feature = "openapi", schema(value_type = String))]
    pub version_id: WorkflowVersionId,
    /// Whether it is released now (from the newest change in `history`).
    #[serde(default)]
    pub release: VersionRelease,
    /// Every change, newest first; empty for a version never released.
    #[serde(default)]
    pub history: Vec<WorkflowReleaseChange>,
    /// Whether the caller may release or un-release it (a Developer or Admin). A hint for the UI;
    /// the `PUT` is authorized on its own.
    #[serde(default)]
    pub can_change: bool,
}

impl WorkflowRelease {
    /// The release of `version_id` whose recorded changes are `history` (newest first): released
    /// when the newest change released it.
    pub fn of(
        version_id: &WorkflowVersionId,
        history: Vec<WorkflowReleaseChange>,
        can_change: bool,
    ) -> Self {
        Self {
            version_id: version_id.clone(),
            release: VersionRelease::of(history.first()),
            history,
            can_change,
        }
    }
}

/// `GET /workflows/{id}` detail: the full frozen version, including the `form_schema` the CLI
/// validates params against.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct WorkflowVersionDetail {
    #[cfg_attr(feature = "openapi", schema(value_type = String))]
    pub id: WorkflowVersionId,
    pub name: String,
    pub project: String,
    /// `WorkflowSource`-shaped: `{"kind":"git",..}` or `{"kind":"s3",..}`.
    pub source: serde_json::Value,
    pub git_sha: String,
    pub source_content_hash: String,
    pub image_digest: String,
    /// ECR coordinates of the frozen image (for a console deep-link); absent when unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_repository: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_registry_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_region: Option<String>,
    pub engine: String,
    pub engine_version: String,
    pub entrypoint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    /// The normalized FormSchema (opaque JSON — the CLI/UI deserialize it via `biffix-workflow`).
    pub form_schema: serde_json::Value,
    pub queue_classes: Vec<String>,
    /// The version's About page (Markdown), when it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub about_markdown: Option<String>,
    /// Who registered this version (actor id captured at registration); absent when auth is off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_by: Option<String>,
    /// A human label (email/name) for the registrant, when it could be resolved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_by_label: Option<String>,
    /// When this version was registered, epoch microseconds.
    pub created_at_micros: i64,
    /// The deploy's retention period for a run's intermediate files (a Nextflow run's `work/`), in
    /// days after the run ends, for the launch form's choice. Absent ⇒ they are kept forever
    /// unless a run asks for deletion on success. Filled by `GET /workflows/{id}`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retention_days: Option<u32>,
    /// The build number among versions frozen from this same source (0 for the first).
    pub build: u32,
    /// What the registration was given beyond the source.
    pub registration_inputs: RegistrationInputs,
    /// The engine runtime the version's pinned release is built into (absent when the version's
    /// readiness could not be read).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<VersionRuntime>,
    /// The version's images and where each import is.
    #[serde(default)]
    pub images: Vec<WorkflowImage>,
    /// `ready` (it can run) | `pending` (its runtime or an image is still being built) |
    /// `failed` (one failed; see `runtime` / `images`). Absent when its readiness could not be
    /// read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    /// Why the version cannot run now (empty when ready).
    #[serde(default)]
    pub not_ready: Vec<String>,
    /// Whether this version is released (see [`WorkflowVersionSummary::release`]).
    #[serde(default)]
    pub release: VersionRelease,
    /// Whether the caller may release or un-release it (a Developer or Admin). A hint for the UI,
    /// like [`WorkflowRelease::can_change`]; `PUT /workflows/{id}/release` is authorized on its
    /// own.
    #[serde(default)]
    pub can_release: bool,
}

/// A version's engine runtime and its coordinator variant for the deployed base image.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct VersionRuntime {
    /// sha256 of the recipe.
    pub key: String,
    /// Every pin the runtime is built from (`engine`, `engine_version`, `pins`, …).
    // Deliberately `Value`, not `biffix_workflow::runtime::RuntimeRecipe`: that is a closed,
    // internally-tagged enum with required fields, so a new engine or a new `recipe_version`'s pin
    // layout on a newer server would make an older CLI reject the whole version detail. The recipe
    // is shown verbatim (it is what `key` hashes), which `Value` preserves exactly.
    #[cfg_attr(feature = "openapi", schema(value_type = Object))]
    pub recipe: serde_json::Value,
    /// The variant for the deployed base image: `unbuilt` | `building` | `failed` | `ready`.
    pub state: String,
    /// Whether a run can launch now (on that variant, or on one built on an older base image).
    pub ready: bool,
    /// The variant's latest build.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build_arn: Option<String>,
    /// Why the latest build failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logs_url: Option<String>,
    /// How many builds of the variant there have been.
    #[serde(default)]
    pub builds: u32,
    /// The image a run launches on now (`<repo>@sha256:…`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coordinator_image: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job_definition: Option<String>,
}
