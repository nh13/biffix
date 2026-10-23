//! Freeze an immutable WorkflowVersion.
//!
//! The version id folds the git SHA + a directory content-hash + a build number: the first
//! registration of a source is build 0, and registering the same source again with different
//! inputs (image, engine version, …) freezes the next build. Registering it again with the same
//! inputs is refused as identical. Registration validates referenced queue-classes against the
//! deploy menu (menu is the source of truth).

use serde::Serialize;

use crate::register::workflow::{WorkflowSource, WorkflowVersion};
use crate::store::registry::SourceBuild;
use biffix_workflow::engines::Engine;

use biffix_workflow::digest::{ImageDigest, ImageDigestError};
use biffix_workflow::formschema::FormSchema;

use biffix_workflow::version::build_version_id;

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum RegisterError {
    #[error("workflow image is not pinned to a valid digest: {0}")]
    InvalidImageDigest(#[from] ImageDigestError),
    #[error("queue-class(es) not in the deploy menu: {}", .0.join(", "))]
    UnknownQueueClasses(Vec<String>),
    #[error(
        "{prefix} {build} ({id}): the same source registered with the same inputs; nothing to \
         register",
        prefix = biffix_api::IDENTICAL_BUILD_PREFIX
    )]
    IdenticalBuild { id: String, build: u32 },
}

/// Bumped whenever registration starts producing a different version from the same inputs (a new
/// image generator, …), or [`inputs_hash`] changes what it covers, so re-registering with
/// unchanged inputs is then a new build rather than "identical to build N". Registration policy,
/// so it lives here, not in the biffix-workflow version module the CLI shares.
pub(crate) const REGISTRATION_PIPELINE_VERSION: u32 = 6;

/// sha256 over what a registration freezes: the name, project, source (its git sha and content
/// hash, and where the coordinator fetches it from: the git repository and ref, or the S3 object
/// and its sha256) and source mode, the effective engine, engine version, entrypoint, target,
/// queue classes, FormSchema and About page, the engine runtime, the pinned images, and the
/// registration pipeline version. So the same commit registered from a corrected location is a
/// new build, not "identical" to one whose location no longer resolves.
///
/// It hashes the effective values, not the overrides the client says it gave, so two
/// registrations are "identical" exactly when they would freeze the same version: re-registering
/// with an explicit engine version equal to the one recorded is identical, and a FormSchema that a
/// newer backend or CLI adapts differently is a new build. The CLI release is left out.
pub fn inputs_hash(
    name: &str,
    project: &str,
    source: &WorkflowSource,
    source_mode: &str,
    runtime_key: &str,
    images: &[String],
    input: &RegisterInput,
) -> String {
    #[derive(Serialize)]
    struct Hashed<'a> {
        pipeline_version: u32,
        name: &'a str,
        project: &'a str,
        source: &'a WorkflowSource,
        source_mode: &'a str,
        git_sha: &'a str,
        content_hash: &'a str,
        // The pin table in force: re-registering after biffix moves an engine's pins is a new
        // build (a new runtime), not "identical".
        runtime_key: &'a str,
        // The images, pinned (`name=<source>@digest`): a tag that moved upstream is a new build.
        images: &'a [String],
        engine: &'a str,
        engine_version: &'a str,
        entrypoint: &'a str,
        target: Option<&'a str>,
        queue_classes: Vec<&'a str>,
        form_schema: &'a FormSchema,
        about_markdown: Option<&'a str>,
    }
    let mut queue_classes: Vec<&str> = input.queue_classes.iter().map(String::as_str).collect();
    queue_classes.sort_unstable();
    queue_classes.dedup();
    let hashed = Hashed {
        pipeline_version: REGISTRATION_PIPELINE_VERSION,
        name,
        project,
        source,
        source_mode,
        git_sha: &input.git_sha,
        content_hash: &input.content_hash,
        runtime_key,
        images,
        engine: input.engine.as_str(),
        engine_version: &input.engine_version,
        entrypoint: &input.entrypoint,
        target: input.target.as_deref(),
        queue_classes,
        form_schema: &input.form_schema,
        about_markdown: input.about_markdown.as_deref(),
    };
    let bytes = serde_json::to_vec(&hashed).expect("a FormSchema and strings always serialize");
    crate::sha256_hex(bytes)
}

#[derive(Clone)]
pub struct RegisterInput {
    pub git_sha: String,
    pub content_hash: String,
    pub image_digest: String,
    /// ECR coordinates of the deploy's workflow repository, where the imported default image lives
    /// (for a console deep-link): see `WorkflowVersion::image_repository`. `None` when the version
    /// has no imported default, or the deploy configures no workflow repository.
    pub image_repository: Option<String>,
    pub image_registry_id: Option<String>,
    pub image_region: Option<String>,
    pub engine: Engine,
    pub engine_version: String,
    pub entrypoint: String,
    pub target: Option<String>,
    pub form_schema: FormSchema,
    pub queue_classes: Vec<String>,
    /// The version's About page Markdown, when the manifest names one.
    pub about_markdown: Option<String>,
    /// The engine runtime's key and recipe (`biffix_workflow::runtime`).
    pub runtime_key: String,
    pub runtime_recipe: serde_json::Value,
}

/// Freeze a WorkflowVersion, validating queue-classes against the deploy menu. `prior` are the
/// versions already frozen from the same source: one with the same `inputs_hash` makes this an
/// identical registration (refused); otherwise the version is the next build. `menu` and `prior`
/// are supplied by the caller (deploy config + store).
pub fn freeze_version(
    input: &RegisterInput,
    menu: &[String],
    prior: &[SourceBuild],
    inputs_hash: &str,
) -> Result<WorkflowVersion, RegisterError> {
    // An empty digest means the version has NO default worker image (every task declares its own
    // container). Otherwise, defense in depth: the immutable snapshot boundary independently
    // rejects tags and malformed values so no alternate registration path can persist a floating
    // image.
    let image_digest: String = if input.image_digest.is_empty() {
        String::new()
    } else {
        input.image_digest.parse::<ImageDigest>()?.into_string()
    };

    let unknown: Vec<String> =
        input.queue_classes.iter().filter(|qc| !menu.iter().any(|m| m == *qc)).cloned().collect();
    if !unknown.is_empty() {
        return Err(RegisterError::UnknownQueueClasses(unknown));
    }

    if let Some(same) = prior.iter().find(|p| p.registration_inputs_hash == inputs_hash) {
        return Err(RegisterError::IdenticalBuild { id: same.id.clone(), build: same.build });
    }
    let build = prior.iter().map(|p| p.build + 1).max().unwrap_or(0);

    Ok(WorkflowVersion {
        id: build_version_id(&input.git_sha, &input.content_hash, build),
        build,
        git_sha: input.git_sha.clone(),
        source_content_hash: input.content_hash.clone(),
        image_digest,
        image_repository: input.image_repository.clone(),
        image_registry_id: input.image_registry_id.clone(),
        image_region: input.image_region.clone(),
        engine: input.engine,
        engine_version: input.engine_version.clone(),
        entrypoint: input.entrypoint.clone(),
        target: input.target.clone(),
        // As `inputs_hash` serializes it: a FormSchema always serializes, so never a null form.
        form_schema: serde_json::to_value(&input.form_schema)
            .expect("a FormSchema always serializes"),
        queue_classes: input.queue_classes.clone(),
        about_markdown: input.about_markdown.clone(),
        runtime_key: input.runtime_key.clone(),
        runtime_recipe: input.runtime_recipe.clone(),
    })
}

/// A registration the version refuses: the same inputs as an existing build are a conflict, the
/// rest are the caller's errors.
impl From<RegisterError> for crate::error::ServiceError {
    fn from(err: RegisterError) -> Self {
        let message = err.to_string();
        match err {
            RegisterError::IdenticalBuild { .. } => crate::error::ServiceError::Conflict(message),
            _ => crate::error::ServiceError::Invalid(message),
        }
    }
}
