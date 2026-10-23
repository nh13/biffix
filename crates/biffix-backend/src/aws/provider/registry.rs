//! Image registries: the deployed base coordinator image (ECR) and a declared image's digest
//! (ECR for a private ECR tag, else the registry's own API, anonymously).

use async_trait::async_trait;

use super::AwsCloudProvider;
use crate::aws::registry::{CoordinatorBase, ImageRegistry};
use crate::aws::AwsError;

/// A private ECR image that could not be resolved: a reference that is not a valid private ECR
/// tag, a repository or tag ECR says does not exist, or a tag ECR has no digest for, is the
/// caller's mistake ([`AwsError::Image`], a 400 at registration); throttling or an outage is
/// retryable ([`AwsError::Unavailable`], a 503); any other failed lookup (permissions) or a
/// malformed answer is ECR's ([`AwsError::Ecr`], a 502).
fn ecr_resolution_error(e: crate::aws::ecr::ImageResolutionError) -> AwsError {
    use crate::aws::ecr::ImageResolutionError as E;
    match &e {
        E::InvalidReference(_) | E::InvalidDigest(_) | E::DigestMissing { .. } => {
            AwsError::Image(e.to_string())
        }
        E::Ecr { source, .. } if source.is_not_found() => AwsError::Image(e.to_string()),
        E::Ecr { source, .. } if source.is_unavailable() => AwsError::Unavailable(e.to_string()),
        E::Ecr { .. } | E::InvalidEcrDigest { .. } => AwsError::Ecr(e.to_string()),
    }
}

/// A public registry's failure: an outage or rate limit is retryable ([`AwsError::Unavailable`], a
/// 503), an unusable answer the registry's ([`AwsError::Registry`], a 502, never named ECR's), and
/// any other the caller's image ([`AwsError::Image`], a 400).
fn public_registry_error(e: crate::aws::oci::OciError) -> AwsError {
    use crate::aws::oci::OciError as E;
    match e {
        E::Unavailable { .. } => AwsError::Unavailable(e.to_string()),
        E::Registry { .. } => AwsError::Registry(e.to_string()),
        e => AwsError::Image(e.to_string()),
    }
}

/// The deployed coordinator image `image` (`BIFFIX_COORDINATOR_IMAGE`), parsed, and its repository
/// (`<registry>/<repository>`, without tag or digest): pinned by tag (`<repo>:<tag>`), digest
/// (`<repo>@<digest>`) or both. An image naming neither is refused rather than read as `:latest`
/// (as [`biffix_workflow::images::ImageRef`] reads it), which would let the deploy's coordinator
/// float; the tag is after the last `/`, so a registry port (`host:5000/repo`) is not one.
fn coordinator_reference(
    image: &str,
) -> Result<(biffix_workflow::images::ImageRef, String), AwsError> {
    let pinned = image.contains('@') || image.rsplit('/').next().is_some_and(|l| l.contains(':'));
    if !pinned {
        return Err(AwsError::Ecr(format!("coordinator image names no tag or digest: {image}")));
    }
    let reference = biffix_workflow::images::ImageRef::parse(image)
        .map_err(|e| AwsError::Ecr(format!("coordinator image {image}: {e}")))?;
    let repo = reference.name();
    Ok((reference, repo))
}

#[async_trait]
impl ImageRegistry for AwsCloudProvider {
    async fn coordinator_base(&self) -> Result<CoordinatorBase, AwsError> {
        let image = self
            .cfg
            .coordinator_image
            .as_deref()
            .ok_or_else(|| AwsError::Unsupported("BIFFIX_COORDINATOR_IMAGE is unset".into()))?;
        self.base
            .get_or_try_init(|| async {
                let (reference, repo) = coordinator_reference(image)?;
                // Pinned by digest: that is the base. Else the tag's, looked up: the deploy's own
                // image, so a failed lookup is ECR's (or the deploy's), never a caller's; an
                // outage is still one (retryable).
                let digest = match &reference.digest {
                    Some(digest) => digest.clone(),
                    None => self
                        .ecr
                        .resolve(&reference)
                        .await
                        .map(biffix_workflow::digest::ImageDigest::into_string)
                        .map_err(|e| match &e {
                            crate::aws::ecr::ImageResolutionError::Ecr { source, .. }
                                if source.is_unavailable() =>
                            {
                                AwsError::Unavailable(e.to_string())
                            }
                            _ => AwsError::Ecr(e.to_string()),
                        })?,
                };
                Ok(CoordinatorBase { repo, digest })
            })
            .await
            .cloned()
    }

    async fn resolve_image_digest(
        &self,
        image: &biffix_workflow::images::ImageRef,
    ) -> Result<String, AwsError> {
        if let Some(digest) = &image.digest {
            return Ok(digest.clone());
        }
        if image.is_private_ecr() {
            return self
                .ecr
                .resolve(image)
                .await
                .map(biffix_workflow::digest::ImageDigest::into_string)
                .map_err(ecr_resolution_error);
        }
        crate::aws::oci::resolve_digest(&self.http, image).await.map_err(public_registry_error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_public_registry_s_outage_is_retryable_and_never_named_ecr_s() {
        use crate::aws::oci::OciError as E;
        let image = || "quay.io/b/x:1".to_string();
        let outage = public_registry_error(E::Unavailable {
            image: image(),
            message: "HTTP 503 Service Unavailable".into(),
        });
        assert!(matches!(outage, AwsError::Unavailable(_)), "{outage}");
        let garbled = public_registry_error(E::Registry {
            image: image(),
            message: "the registry reported no digest".into(),
        });
        assert!(matches!(garbled, AwsError::Registry(_)), "{garbled}");
        assert!(!garbled.to_string().contains("ecr"), "{garbled}");
        let missing = public_registry_error(E::NotFound { image: image() });
        assert!(matches!(missing, AwsError::Image(_)), "{missing}");
    }

    #[test]
    fn a_bad_private_ecr_reference_is_the_caller_s_error_and_a_failed_lookup_ecr_s() {
        use crate::aws::ecr::{EcrLookupError, ImageResolutionError as E};
        use biffix_workflow::digest::ImageDigestError;
        use biffix_workflow::images::EcrRefError;
        let (repository, tag) = ("wf".to_string(), "v1".to_string());
        for caller in [
            E::Ecr {
                repository: repository.clone(),
                tag: tag.clone(),
                source: EcrLookupError::not_found("ImageNotFoundException"),
            },
            E::InvalidReference(EcrRefError::InvalidRepository("w".into())),
            E::InvalidReference(EcrRefError::InvalidRegistryId("123".into())),
            E::InvalidDigest(ImageDigestError::NonCanonicalHex),
            E::DigestMissing { repository: repository.clone(), tag: tag.clone() },
        ] {
            assert!(matches!(ecr_resolution_error(caller.clone()), AwsError::Image(_)), "{caller}");
        }
        // A lookup ECR could not answer for now (a throttle, a 5xx, no answer): retryable.
        let throttled = E::Ecr {
            repository: repository.clone(),
            tag: tag.clone(),
            source: EcrLookupError::unavailable("ThrottlingException"),
        };
        assert!(matches!(ecr_resolution_error(throttled), AwsError::Unavailable(_)));
        for fault in [
            // A lookup that failed otherwise (a missing pull grant): ECR's.
            E::Ecr {
                repository: repository.clone(),
                tag: tag.clone(),
                source: EcrLookupError::new("AccessDeniedException"),
            },
            E::InvalidEcrDigest { repository, tag, source: ImageDigestError::UnsupportedAlgorithm },
        ] {
            assert!(matches!(ecr_resolution_error(fault.clone()), AwsError::Ecr(_)), "{fault}");
        }
    }

    #[test]
    fn the_coordinator_image_is_pinned_and_its_repository_has_no_tag_or_digest() {
        let ecr = "123.dkr.ecr.us-east-1.amazonaws.com/coord";
        let digest = format!("sha256:{}", "a".repeat(64));
        let repo = |image: &str| coordinator_reference(image).map(|(_, repo)| repo);
        assert_eq!(repo(&format!("{ecr}:v8")).unwrap(), ecr);
        // By digest, with or without its tag: the repository alone, the digest the reference's.
        for image in [format!("{ecr}:v8@{digest}"), format!("{ecr}@{digest}")] {
            let (reference, repo) = coordinator_reference(&image).unwrap();
            assert_eq!(repo, ecr, "{image}");
            assert_eq!(reference.digest.as_deref(), Some(digest.as_str()), "{image}");
        }
        assert_eq!(repo("host:5000/repo:v1").unwrap(), "host:5000/repo");
        // Neither a tag nor a digest (a registry port is not a tag): refused, never `latest`.
        for unpinned in ["host:5000/repo", ecr] {
            assert!(
                matches!(repo(unpinned), Err(AwsError::Ecr(m)) if m.contains("no tag or digest")),
                "{unpinned}"
            );
        }
        assert!(repo("repo:").is_err());
    }
}
