//! Image registries: the deployed base coordinator image and the digest an image names
//! ([`ImageRegistry`]).

use async_trait::async_trait;

use super::AwsError;

/// The deployed base coordinator image, resolved to its immutable index digest.
#[derive(Clone, Debug, PartialEq)]
pub struct CoordinatorBase {
    /// `<registry>/<repo>` (no tag).
    pub(crate) repo: String,
    /// `sha256:…`
    pub(crate) digest: String,
}

impl CoordinatorBase {
    /// `<repo>@<digest>`.
    pub(crate) fn image(&self) -> String {
        format!("{}@{}", self.repo, self.digest)
    }
}

/// The image lookups the backend needs (see the module docs).
#[async_trait]
pub trait ImageRegistry: Send + Sync {
    /// The deployed base coordinator image, resolved to its digest.
    async fn coordinator_base(&self) -> Result<CoordinatorBase, AwsError>;

    /// The digest `image` names: a private ECR tag through ECR, any other registry anonymously
    /// through its registry API. A pinned reference is its own digest.
    async fn resolve_image_digest(
        &self,
        image: &biffix_workflow::images::ImageRef,
    ) -> Result<String, AwsError>;
}
