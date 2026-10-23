//! The AWS [`crate::aws::CloudProvider`] (behind `feature = "aws"`), over the AWS SDK for Rust,
//! one file per service: [`batch`] (the run session, coordinator launch, variant job definitions,
//! cancel, job owners), [`builds`] (CodeBuild runtime builds and image imports/builds),
//! [`registry`] (ECR and public registries: image digests), [`objects`] and [`uploads`] (S3:
//! inputs, outputs, the data browser, presigned and multipart uploads), [`logs`] (CloudWatch Logs:
//! task logs) and [`alerts`] (SNS: alert recipients).
//!
//! - `mint_run_session` records the run's project's base role (the run's blast-radius boundary)
//!   and mints a per-run capability token. No STS AssumeRole: the coordinator and its children
//!   receive that role via Batch, not via forwarded credentials.
//! - `launch_coordinator` is Batch `SubmitJob` for the coordinator, with the env contract
//!   `docker/coordinator/entrypoint.sh` documents ([`crate::aws::coordinator_env`]).
//!
//! Every SDK failure goes through [`errors::classify`], so an outage, a missing grant and an absent
//! object read the same from every operation. The exception is an ECR tag lookup (an image
//! reference's resolution), which classifies its own errors in `aws::ecr`: a missing repository is
//! the reference's mistake, a throttle or outage is unavailable, and anything else, a missing pull
//! grant included, is an ECR failure (502). The pure pieces (error classification, variant
//! naming, polled build events) are unit-tested beside them; nothing exercises the SDK calls
//! themselves.

mod alerts;
mod batch;
mod builds;
mod errors;
mod logs;
mod objects;
mod registry;
mod uploads;

use biffix_core::ids::RunId;

use crate::aws::ecr::EcrImageResolver;
use crate::config::env::ProviderConfig;

pub(crate) struct AwsCloudProvider {
    batch: aws_sdk_batch::Client,
    codebuild: aws_sdk_codebuild::Client,
    /// Talks to public image registries (tag → digest at registration).
    http: crate::aws::oci::RegistryClient,
    /// The base coordinator image, resolved once: it only changes with a deploy, which replaces
    /// the Lambda's environment and so this process.
    base: tokio::sync::OnceCell<crate::aws::registry::CoordinatorBase>,
    s3: aws_sdk_s3::Client,
    logs: aws_sdk_cloudwatchlogs::Client,
    sns: aws_sdk_sns::Client,
    /// The ECR tag→digest resolver used by registration image pinning.
    ecr: EcrImageResolver,
    cfg: ProviderConfig,
    /// Mints the per-run capability token. Injected so KMS-signing is pluggable/testable.
    token_minter: Box<dyn Fn(&RunId) -> String + Send + Sync>,
}

impl AwsCloudProvider {
    /// Every client comes from `sdk_config` except S3, which the caller configures (presigned PUTs
    /// need its request checksums set to `WhenRequired`; see `compose.rs`).
    pub(crate) fn new(
        s3: aws_sdk_s3::Client,
        sdk_config: aws_config::SdkConfig,
        cfg: ProviderConfig,
        token_minter: Box<dyn Fn(&RunId) -> String + Send + Sync>,
    ) -> Self {
        Self {
            batch: aws_sdk_batch::Client::new(&sdk_config),
            codebuild: aws_sdk_codebuild::Client::new(&sdk_config),
            http: crate::aws::oci::RegistryClient::new(),
            base: tokio::sync::OnceCell::new(),
            s3,
            logs: aws_sdk_cloudwatchlogs::Client::new(&sdk_config),
            sns: aws_sdk_sns::Client::new(&sdk_config),
            ecr: EcrImageResolver::new(sdk_config),
            cfg,
            token_minter,
        }
    }
}
