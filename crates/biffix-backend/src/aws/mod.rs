//! The AWS boundary, behind one trait per concern so the backend is unit-testable without AWS:
//!
//! - [`batch::BatchJobs`]: the run's session, its coordinator's launch and variant job
//!   definitions, cancel, and the owners of the jobs a coordinator submits (Batch);
//! - [`builds::BuildService`]: runtime builds and workflow image imports, builds, indexes and
//!   derives (CodeBuild);
//! - [`registry::ImageRegistry`]: the base coordinator image and the digest an image names (ECR,
//!   public registries);
//! - [`objects::ObjectStore`]: submit's input checks, the data browser, outputs and presigned
//!   reads and source uploads (S3);
//! - [`uploads::MultipartUploads`]: a user's file uploads (S3);
//! - [`logs::LogReader`]: task and coordinator logs (CloudWatch Logs);
//! - [`alerts::AlertTopic`]: the Settings page's alert recipients (SNS);
//!
//! and [`CloudProvider`], all of them at once, which [`crate::AppState::cloud`] holds. The real
//! implementation is `provider::AwsCloudProvider` (behind `feature = "aws"`, one file per service
//! under `provider/`); `crate::testing::FakeCloud` drives the tests. The other AWS SDK code lives
//! here too: ECR tag lookups ([`ecr`]), the Cognito identity resolver (`cognito`) and the SSM
//! projects loader (`ssm_projects`); so does the anonymous public-registry client ([`oci`]). The coordinator's environment contract is
//! [`coordinator_env`].
//!
//! Scoping is per-project: the run (coordinator and children) uses its project's base role
//! directly, and blast radius is one project (`docs/architecture.md`, "IAM is the authorization
//! engine"). Which project is the caller's to say: every operation that depends on it takes the
//! project's context ([`batch::LaunchProject`], a bucket, a coordinator template), resolved per
//! request from `crate::config::projects`. A stronger per-run form (a run-scoped IAM role) is not
//! built yet.

pub mod alerts;
pub mod batch;
pub(crate) mod builds;
#[cfg(feature = "aws")]
pub(crate) mod cognito;
pub(crate) mod coordinator_env;
pub mod ecr;
pub mod logs;
pub(crate) mod objects;
pub(crate) mod oci;
#[cfg(feature = "aws")]
pub(crate) mod provider;
pub mod registry;
#[cfg(feature = "aws")]
pub(crate) mod ssm_projects;
pub(crate) mod uploads;

use alerts::AlertTopic;
use batch::BatchJobs;
use builds::BuildService;
use logs::LogReader;
use objects::ObjectStore;
use registry::ImageRegistry;
use uploads::MultipartUploads;

/// Every AWS operation the backend needs: the per-concern traits at once (see the module docs).
/// Anything that implements them all is one.
pub trait CloudProvider:
    BatchJobs + BuildService + ImageRegistry + ObjectStore + MultipartUploads + LogReader + AlertTopic
{
}

impl<T> CloudProvider for T where
    T: BatchJobs
        + BuildService
        + ImageRegistry
        + ObjectStore
        + MultipartUploads
        + LogReader
        + AlertTopic
{
}

#[derive(Debug, thiserror::Error)]
pub enum AwsError {
    #[error("batch error: {0}")]
    Batch(String),
    #[error("ecr error: {0}")]
    Ecr(String),
    #[error("s3 error: {0}")]
    S3(String),
    #[error("cloudwatch logs error: {0}")]
    Logs(String),
    /// The requested resource does not exist (e.g. a CloudWatch log stream that expired, rotated,
    /// or was never created). Distinguished from the opaque errors above so a handler maps it to a
    /// 404 rather than a 500.
    #[error("not found")]
    NotFound,
    /// The deploy's IAM does not let biffix read it (e.g. a data source whose grant is missing).
    #[error("access denied: {0}")]
    Forbidden(String),
    /// AWS could not be reached or is shedding load (a timeout, a dispatch failure, throttling, a
    /// 5xx): transient, so a handler answers 503 rather than blaming the request or the deploy.
    #[error("aws is unavailable: {0}")]
    Unavailable(String),
    /// AWS refused the request as the caller made it (e.g. a multipart part list that does not
    /// match the upload's parts, or run parameters too large to launch with): a 400.
    #[error("{0}")]
    Invalid(String),
    #[error("sns error: {0}")]
    Sns(String),
    /// The deploy has no alerts topic (`BIFFIX_ALERT_TOPIC_ARN` unset).
    #[error("alerts are not configured on this deployment")]
    AlertsNotConfigured,
    #[error("codebuild error: {0}")]
    CodeBuild(String),
    /// A public image registry answered with something unusable (a 502).
    #[error("image registry error: {0}")]
    Registry(String),
    /// A declared image cannot be used: not found, or its registry needs credentials.
    #[error("{0}")]
    Image(String),
    /// The provider cannot do this (a test double, or a deploy missing the configuration).
    #[error("not supported: {0}")]
    Unsupported(String),
    /// A resource the deploy configures (a CodeBuild project, the alerts topic) does not exist: a
    /// deploy fault, not a missing thing the caller named, so a 500 naming it rather than a 404.
    #[error("this deploy is misconfigured: {0}")]
    Misconfigured(String),
}
