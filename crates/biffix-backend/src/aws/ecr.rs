//! Private ECR images: tag → digest with the AWS SDK.
//!
//! Registration parses every declared image with [`ImageRef`] and pins it to a digest before a
//! `WorkflowVersion` is frozen: a public registry's tag is resolved anonymously by
//! [`crate::aws::oci`], and a private ECR tag (an [`ImageRef::is_private_ecr`] host) by
//! [`EcrImageResolver`] here, which needs the deploy's AWS credentials. The image is then imported
//! (and keep-tagged) by its import build.

use async_trait::async_trait;
use biffix_workflow::digest::{ImageDigest, ImageDigestError};
#[cfg(feature = "aws")]
use biffix_workflow::images::MANIFEST_MEDIA_TYPES;
use biffix_workflow::images::{EcrCoords, EcrRefError, ImageRef};

/// A failure returned by an ECR tag lookup implementation.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error("{message}")]
pub struct EcrLookupError {
    message: String,
    kind: EcrLookupFailure,
}

/// Whose failure an [`EcrLookupError`] is, which decides how it is answered.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EcrLookupFailure {
    /// ECR failed to answer (permissions, an unexpected failure): ECR's, not the reference's.
    Failed,
    /// The reference's mistake: no such repository or tag, or an image ECR cannot serve as named.
    NotFound,
    /// ECR (or the registry a pull-through cache fronts) is shedding load or down: retry.
    Unavailable,
}

impl EcrLookupError {
    /// ECR could not answer (permissions, an unexpected failure).
    pub fn new(message: impl Into<String>) -> Self {
        Self { message: message.into(), kind: EcrLookupFailure::Failed }
    }

    /// ECR answered: it has no such repository or tag, or cannot serve the image as named (the
    /// reference's mistake, not ECR's).
    pub(crate) fn not_found(message: impl Into<String>) -> Self {
        Self { message: message.into(), kind: EcrLookupFailure::NotFound }
    }

    /// ECR is throttling or unavailable: the same lookup may succeed if retried.
    pub(crate) fn unavailable(message: impl Into<String>) -> Self {
        Self { message: message.into(), kind: EcrLookupFailure::Unavailable }
    }

    /// Whether ECR said the repository or tag does not exist, or cannot be served as named.
    pub(crate) fn is_not_found(&self) -> bool {
        self.kind == EcrLookupFailure::NotFound
    }

    /// Whether ECR was throttling or unavailable.
    pub(crate) fn is_unavailable(&self) -> bool {
        self.kind == EcrLookupFailure::Unavailable
    }
}

/// The one ECR operation required by registration.
///
/// Keeping this boundary narrower than the AWS SDK lets registration tests use a deterministic
/// fake and guarantees that accepting an already-pushed digest performs no AWS request.
#[async_trait]
pub trait EcrImageLookup: Send + Sync {
    /// The digest `tag` names in the repository at `coords`; `None` when ECR answers none.
    async fn digest_for_tag(
        &self,
        coords: &EcrCoords,
        tag: &str,
    ) -> Result<Option<String>, EcrLookupError>;
}

/// Resolve a private ECR image to the only value allowed in a `WorkflowVersion`: its digest when
/// the reference is pinned (no lookup), else the digest ECR answers for its tag. Either way the
/// image must be in a repository ECR could hold ([`ImageRef::ecr_coords`]).
pub async fn resolve_image(
    lookup: &(impl EcrImageLookup + ?Sized),
    image: &ImageRef,
) -> Result<ImageDigest, ImageResolutionError> {
    let coords = image.ecr_coords()?;
    if let Some(digest) = &image.digest {
        return digest.parse().map_err(ImageResolutionError::InvalidDigest);
    }
    // A parsed reference has a tag or a digest; one with neither means `latest`, as in Docker.
    let tag = image.tag.as_deref().unwrap_or("latest");

    let digest = lookup
        .digest_for_tag(&coords, tag)
        .await
        .map_err(|source| ImageResolutionError::Ecr {
            repository: coords.repository.clone(),
            tag: tag.to_owned(),
            source,
        })?
        .ok_or_else(|| ImageResolutionError::DigestMissing {
            repository: coords.repository.clone(),
            tag: tag.to_owned(),
        })?;

    digest.parse().map_err(|source| ImageResolutionError::InvalidEcrDigest {
        repository: coords.repository,
        tag: tag.to_owned(),
        source,
    })
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ImageResolutionError {
    #[error(transparent)]
    InvalidReference(#[from] EcrRefError),
    #[error("invalid pushed image digest: {0}")]
    InvalidDigest(ImageDigestError),
    #[error("ECR failed to resolve {repository}:{tag}: {source}")]
    Ecr { repository: String, tag: String, source: EcrLookupError },
    #[error("ECR returned no digest for {repository}:{tag}")]
    DigestMissing { repository: String, tag: String },
    #[error("ECR returned an invalid digest for {repository}:{tag}: {source}")]
    InvalidEcrDigest { repository: String, tag: String, source: ImageDigestError },
}

/// Production ECR lookup backed by the AWS SDK for Rust.
#[cfg(feature = "aws")]
#[derive(Clone)]
pub(crate) struct EcrImageResolver {
    sdk_config: aws_config::SdkConfig,
}

#[cfg(feature = "aws")]
impl EcrImageResolver {
    pub(crate) fn new(sdk_config: aws_config::SdkConfig) -> Self {
        Self { sdk_config }
    }

    pub(crate) async fn resolve(
        &self,
        image: &ImageRef,
    ) -> Result<ImageDigest, ImageResolutionError> {
        resolve_image(self, image).await
    }
}

#[cfg(feature = "aws")]
#[async_trait]
impl EcrImageLookup for EcrImageResolver {
    async fn digest_for_tag(
        &self,
        coords: &EcrCoords,
        tag: &str,
    ) -> Result<Option<String>, EcrLookupError> {
        // ECR is regional. Build the client for the region encoded in the registry hostname so a
        // valid cross-region project reference cannot accidentally be queried in the API region.
        let config = aws_sdk_ecr::config::Builder::from(&self.sdk_config)
            .region(aws_sdk_ecr::config::Region::new(coords.region.clone()))
            .build();
        let client = aws_sdk_ecr::Client::from_conf(config);
        let image_id = aws_sdk_ecr::types::ImageIdentifier::builder().image_tag(tag).build();
        // BatchGetImage, not DescribeImages: it is a pull permission, so it works on another
        // account's repository whose policy grants this one pull only. It counts as a pull (it
        // updates the image's last-pulled time).
        let output = client
            .batch_get_image()
            .registry_id(&coords.registry_id)
            .repository_name(&coords.repository)
            .image_ids(image_id)
            .set_accepted_media_types(Some(
                MANIFEST_MEDIA_TYPES.iter().map(ToString::to_string).collect(),
            ))
            .send()
            .await
            .map_err(|error| {
                use aws_sdk_ecr::error::ProvideErrorMetadata;
                // The error's full chain: an SDK error's own text is only "service error".
                let message = aws_sdk_ecr::error::DisplayErrorContext(&error).to_string();
                let status = error.raw_response().map(|r| r.status().as_u16());
                match error.as_service_error() {
                    Some(e) if e.is_repository_not_found_exception() => {
                        EcrLookupError::not_found(message)
                    }
                    // A repository (another account's) whose policy does not let this deploy
                    // pull: the reference's, never fixed by a retry.
                    Some(e) if e.code() == Some("AccessDeniedException") => {
                        EcrLookupError::not_found(format!(
                            "this deploy may not pull from the repository (its policy must grant \
                             it ecr:BatchGetImage): {message}"
                        ))
                    }
                    Some(e)
                        if e.is_server_exception()
                            || e.is_limit_exceeded_exception()
                            || e.code() == Some("ThrottlingException")
                            || status.is_some_and(|s| s == 429 || s >= 500) =>
                    {
                        EcrLookupError::unavailable(message)
                    }
                    Some(_) => EcrLookupError::new(message),
                    // No answer at all (a timeout, a dropped connection): worth a retry.
                    None => EcrLookupError::unavailable(message),
                }
            })?;

        batch_get_digest(&output)
    }
}

/// The digest a BatchGetImage of one tag answered. A tag ECR cannot serve comes back as a
/// per-image failure, not an error: a missing, malformed or inaccessible (e.g. archived) image is
/// the reference's mistake, a pull-through cache's upstream throttling or outage is retryable, and
/// anything else is ECR's.
#[cfg(feature = "aws")]
fn batch_get_digest(
    output: &aws_sdk_ecr::operation::batch_get_image::BatchGetImageOutput,
) -> Result<Option<String>, EcrLookupError> {
    use aws_sdk_ecr::types::ImageFailureCode;
    if let Some(failure) = output.failures().first() {
        let message = format!(
            "{}: {}",
            failure.failure_code().map_or("unknown failure", ImageFailureCode::as_str),
            failure.failure_reason().unwrap_or("no reason given"),
        );
        return Err(match failure.failure_code() {
            Some(
                ImageFailureCode::ImageNotFound
                | ImageFailureCode::InvalidImageTag
                | ImageFailureCode::ImageInaccessible,
            ) => EcrLookupError::not_found(message),
            Some(
                ImageFailureCode::UpstreamTooManyRequests | ImageFailureCode::UpstreamUnavailable,
            ) => EcrLookupError::unavailable(message),
            _ => EcrLookupError::new(message),
        });
    }
    Ok(output
        .images()
        .first()
        .and_then(|image| image.image_id())
        .and_then(|id| id.image_digest())
        .map(str::to_owned))
}

#[cfg(all(test, feature = "aws"))]
mod tests {
    use aws_sdk_ecr::operation::batch_get_image::BatchGetImageOutput;
    use aws_sdk_ecr::types::{Image, ImageFailure, ImageFailureCode, ImageIdentifier};

    use super::*;

    const DIGEST: &str = "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn failure(code: ImageFailureCode, reason: &str) -> BatchGetImageOutput {
        BatchGetImageOutput::builder()
            .failures(ImageFailure::builder().failure_code(code).failure_reason(reason).build())
            .build()
    }

    #[test]
    fn a_served_tag_resolves_to_the_digest_ecr_answered() {
        let output = BatchGetImageOutput::builder()
            .images(
                Image::builder()
                    .image_id(
                        ImageIdentifier::builder().image_tag("v1").image_digest(DIGEST).build(),
                    )
                    .build(),
            )
            .build();
        assert_eq!(batch_get_digest(&output), Ok(Some(DIGEST.to_string())));
    }

    #[test]
    fn an_image_ecr_cannot_serve_as_named_is_the_references_mistake() {
        for (code, reason) in [
            (ImageFailureCode::ImageNotFound, "Requested image not found"),
            (ImageFailureCode::InvalidImageTag, "Invalid image tag"),
            (ImageFailureCode::ImageInaccessible, "The image is archived"),
        ] {
            let error = batch_get_digest(&failure(code.clone(), reason)).unwrap_err();
            assert!(error.is_not_found(), "{code:?}");
            assert!(!error.is_unavailable(), "{code:?}");
            assert_eq!(error.to_string(), format!("{}: {reason}", code.as_str()));
        }
    }

    #[test]
    fn an_upstream_that_is_throttling_or_down_is_worth_a_retry() {
        for code in
            [ImageFailureCode::UpstreamTooManyRequests, ImageFailureCode::UpstreamUnavailable]
        {
            let error = batch_get_digest(&failure(code.clone(), "upstream busy")).unwrap_err();
            assert!(error.is_unavailable(), "{code:?}");
            assert!(!error.is_not_found(), "{code:?}");
        }
    }

    #[test]
    fn any_other_failure_is_ecrs() {
        let kms = batch_get_digest(&failure(
            ImageFailureCode::KmsError,
            "KMS key disabled for the repository",
        ))
        .unwrap_err();
        assert!(!kms.is_not_found() && !kms.is_unavailable());
        assert_eq!(kms.to_string(), "KmsError: KMS key disabled for the repository");

        let bare = BatchGetImageOutput::builder().failures(ImageFailure::builder().build()).build();
        let unknown = batch_get_digest(&bare).unwrap_err();
        assert!(!unknown.is_not_found() && !unknown.is_unavailable());
        assert_eq!(unknown.to_string(), "unknown failure: no reason given");
    }

    #[test]
    fn an_answer_with_no_image_has_no_digest() {
        assert_eq!(batch_get_digest(&BatchGetImageOutput::builder().build()), Ok(None));
    }

    /// The real SDK call: a pull-permission BatchGetImage on the reference's own registry and
    /// region, asking for every manifest type, whose answered digest is returned.
    #[tokio::test]
    async fn a_tag_lookup_is_a_batch_get_image_asking_for_every_manifest_type() {
        let body = format!(
            r#"{{"images":[{{"imageId":{{"imageTag":"v1","imageDigest":"{DIGEST}"}}}}],"failures":[]}}"#
        );
        // Every request sent, each answered with `body`.
        let sent = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorded = sent.clone();
        let http_client = aws_smithy_runtime_api::client::http::http_client_fn(move |_, _| {
            aws_smithy_runtime_api::client::http::SharedHttpConnector::new(Recorder {
                sent: recorded.clone(),
                status: 200,
                body: body.clone(),
            })
        });
        let config = aws_config::SdkConfig::builder()
            .http_client(http_client)
            .credentials_provider(aws_sdk_ecr::config::SharedCredentialsProvider::new(
                aws_sdk_ecr::config::Credentials::new("akid", "secret", None, None, "test"),
            ))
            .region(aws_sdk_ecr::config::Region::new("us-west-2"))
            .behavior_version(aws_sdk_ecr::config::BehaviorVersion::latest())
            .build();
        let image =
            ImageRef::parse("123456789012.dkr.ecr.us-east-1.amazonaws.com/genomics:v1").unwrap();

        let digest = EcrImageResolver::new(config).resolve(&image).await.unwrap();

        assert_eq!(digest.as_str(), DIGEST);
        let sent = sent.lock().unwrap();
        let [(target, uri, body)] = sent.as_slice() else {
            panic!("one request: {sent:?}");
        };
        assert_eq!(target.as_deref(), Some("AmazonEC2ContainerRegistry_V20150921.BatchGetImage"));
        // The registry's region, not the client's.
        assert!(uri.contains("us-east-1"), "{uri}");
        let body: serde_json::Value = serde_json::from_slice(body).unwrap();
        assert_eq!(
            body,
            serde_json::json!({
                "registryId": "123456789012",
                "repositoryName": "genomics",
                "imageIds": [{"imageTag": "v1"}],
                "acceptedMediaTypes": MANIFEST_MEDIA_TYPES,
            })
        );
    }

    /// How a tag lookup ECR answered with an error reads: a missing repository is the
    /// reference's mistake; throttling, a server error or no answer is worth a retry; anything else
    /// (a missing pull grant) is ECR's.
    #[tokio::test]
    async fn a_tag_lookup_error_reads_by_what_ecr_answered() {
        let lookup = |status: u16, error_type: &str| {
            let body = format!(r#"{{"__type":"{error_type}","message":"m"}}"#);
            let http_client = aws_smithy_runtime_api::client::http::http_client_fn(move |_, _| {
                aws_smithy_runtime_api::client::http::SharedHttpConnector::new(Recorder {
                    sent: Default::default(),
                    status,
                    body: body.clone(),
                })
            });
            let config = aws_config::SdkConfig::builder()
                .http_client(http_client)
                .credentials_provider(aws_sdk_ecr::config::SharedCredentialsProvider::new(
                    aws_sdk_ecr::config::Credentials::new("akid", "secret", None, None, "test"),
                ))
                .region(aws_sdk_ecr::config::Region::new("us-west-2"))
                .retry_config(aws_sdk_ecr::config::retry::RetryConfig::disabled())
                .behavior_version(aws_sdk_ecr::config::BehaviorVersion::latest())
                .build();
            let image = ImageRef::parse("123456789012.dkr.ecr.us-east-1.amazonaws.com/genomics:v1")
                .unwrap();
            async move {
                match EcrImageResolver::new(config).resolve(&image).await {
                    Err(ImageResolutionError::Ecr { source, .. }) => source,
                    other => panic!("{other:?}"),
                }
            }
        };
        let missing = lookup(400, "RepositoryNotFoundException").await;
        assert!(missing.is_not_found(), "{missing:?}");
        for (status, error_type) in [
            (400, "ThrottlingException"),
            (400, "LimitExceededException"),
            (500, "ServerException"),
        ] {
            let e = lookup(status, error_type).await;
            assert!(e.is_unavailable() && !e.is_not_found(), "{error_type}: {e:?}");
        }
        // Another account's repository whose policy does not let this deploy pull: the
        // reference's to fix, so a not-found, never a retry.
        let denied = lookup(400, "AccessDeniedException").await;
        assert!(denied.is_not_found() && !denied.is_unavailable(), "{denied:?}");
        assert!(denied.to_string().contains("ecr:BatchGetImage"), "{denied:?}");
    }

    /// One request's `x-amz-target`, URI and body.
    type Sent = (Option<String>, String, Vec<u8>);

    /// Records each request and answers it `status` with `body`.
    #[derive(Debug)]
    struct Recorder {
        sent: std::sync::Arc<std::sync::Mutex<Vec<Sent>>>,
        status: u16,
        body: String,
    }

    impl aws_smithy_runtime_api::client::http::HttpConnector for Recorder {
        fn call(
            &self,
            request: aws_smithy_runtime_api::client::orchestrator::HttpRequest,
        ) -> aws_smithy_runtime_api::client::http::HttpConnectorFuture {
            self.sent.lock().unwrap().push((
                request.headers().get("x-amz-target").map(str::to_owned),
                request.uri().to_owned(),
                request.body().bytes().unwrap_or_default().to_vec(),
            ));
            aws_smithy_runtime_api::client::http::HttpConnectorFuture::ready(Ok(
                aws_smithy_runtime_api::client::orchestrator::HttpResponse::new(
                    self.status.try_into().unwrap(),
                    aws_smithy_types::body::SdkBody::from(self.body.clone()),
                ),
            ))
        }
    }
}
