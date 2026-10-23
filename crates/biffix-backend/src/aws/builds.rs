//! CodeBuild: runtime builds (coordinator variants) and workflow image imports, builds, indexes
//! and derives ([`BuildService`]).

use std::collections::BTreeMap;

use async_trait::async_trait;

use super::AwsError;

/// An image import to start: `source` (pinned) into the workflow repository, as `subject`.
#[derive(Clone, Debug, PartialEq)]
pub struct ImageImportJob {
    /// The CodeBuild idempotency token (`crate::images::build_follow::start_token`): a repeated
    /// start of the subject from the same state returns the first start's build.
    pub(crate) idempotency_token: String,
    pub subject: String,
    /// `<registry>/<repo>[:tag]@sha256:…`
    pub source: String,
    pub content_key: String,
}

/// One architecture's build of an image from its recipe (the image-build project of `arch`), as
/// `subject` (`codebuild/image/build.sh`).
#[derive(Clone, Debug, PartialEq)]
pub struct ImageBuildJob {
    /// The CodeBuild idempotency token (`crate::images::build_follow::start_token`): a repeated
    /// start of the subject from the same state returns the first start's build.
    pub(crate) idempotency_token: String,
    pub subject: String,
    pub arch: biffix_workflow::images::Arch,
    pub(crate) content_key: String,
    /// `s3://…/contexts/<key12>.tar.gz`: the build context's tracked files.
    pub context_uri: String,
    pub context_sha256: String,
    /// The Dockerfile's path within the context (a workflow's own Dockerfile).
    pub dockerfile: Option<String>,
    /// The Dockerfile's text (one biffix generated, e.g. for a pixi environment).
    pub dockerfile_text: Option<String>,
}

/// The index joining a built image's architectures (the image-import project,
/// `BIFFIX_IMAGE_STEP=index`), as `subject`; it then keep-tags, probes and archives it.
#[derive(Clone, Debug, PartialEq)]
pub struct ImageIndexJob {
    /// The CodeBuild idempotency token (`crate::images::build_follow::start_token`): a repeated
    /// start of the subject from the same state returns the first start's build.
    pub(crate) idempotency_token: String,
    pub subject: String,
    pub(crate) content_key: String,
    /// Each architecture's built image, `<workflow repo>@sha256:…`.
    pub manifests: Vec<String>,
}

/// A derive to start (the image-import project, `BIFFIX_IMAGE_STEP=derive`): the imported `base`
/// plus the runtime's stored layer images, as `subject`.
#[derive(Clone, Debug, PartialEq)]
pub struct ImageDeriveJob {
    /// The CodeBuild idempotency token (`crate::images::build_follow::start_token`): a repeated
    /// start of the subject from the same state returns the first start's build.
    pub(crate) idempotency_token: String,
    pub subject: String,
    /// The imported image, `<workflow repo>@sha256:…`.
    pub base: String,
    /// The runtime's layer image per architecture, `<coordinator repo>@sha256:…`.
    pub layer_amd64: String,
    pub layer_arm64: String,
    pub derived_key: String,
    /// The Snakemake the layer carries (the build checks the derived image has it).
    pub snakemake_version: String,
}

/// A runtime build to start: the coordinator variant `subject` names, built from `base_image` with
/// the runtime recipe's environment (`biffix_workflow::runtime::RuntimeRecipe::build_env`).
#[derive(Clone, Debug, PartialEq)]
pub struct RuntimeBuildJob {
    /// The CodeBuild idempotency token (`crate::images::build_follow::start_token`): a repeated
    /// start of the subject from the same state returns the first start's build.
    pub idempotency_token: String,
    pub subject: String,
    pub base_image: String,
    pub env: BTreeMap<String, String>,
}

/// The builds the backend starts and follows (see the module docs).
#[async_trait]
pub trait BuildService: Send + Sync {
    /// Start a runtime build (CodeBuild); returns the build's ARN.
    async fn start_runtime_build(&self, request: &RuntimeBuildJob) -> Result<String, AwsError>;

    /// A build's current state (a runtime build or an image import), as the CodeBuild state-change
    /// event EventBridge delivers (the backstop for one that was lost), or `None` if CodeBuild no
    /// longer knows the build.
    async fn build_event(
        &self,
        build_arn: &str,
    ) -> Result<Option<biffix_core::wire::aws_codebuild::CodeBuildEvent>, AwsError>;

    /// Start an image import (CodeBuild); returns the build's ARN.
    async fn start_image_import(&self, request: &ImageImportJob) -> Result<String, AwsError>;

    /// Start a derive (CodeBuild); returns the build's ARN.
    async fn start_image_derive(&self, request: &ImageDeriveJob) -> Result<String, AwsError>;

    /// Start one architecture's image build (CodeBuild); returns the build's ARN.
    async fn start_image_build(&self, request: &ImageBuildJob) -> Result<String, AwsError>;

    /// Start the index of a built image (CodeBuild); returns the build's ARN.
    async fn start_image_index(&self, request: &ImageIndexJob) -> Result<String, AwsError>;
}
