//! Image pinning tests.

use std::sync::Mutex;

use async_trait::async_trait;
use biffix_backend::aws::ecr::{
    resolve_image, EcrImageLookup, EcrLookupError, ImageResolutionError,
};
use biffix_workflow::digest::ImageDigestError;
use biffix_workflow::images::{EcrCoords, EcrRefError, ImageRef};

fn digest(fill: char) -> String {
    format!("sha256:{}", fill.to_string().repeat(64))
}

fn image(reference: &str) -> ImageRef {
    ImageRef::parse(reference).unwrap()
}

#[derive(Debug)]
struct FakeEcr {
    response: Result<Option<String>, EcrLookupError>,
    calls: Mutex<Vec<(EcrCoords, String)>>,
}

impl FakeEcr {
    fn returning(response: Option<String>) -> Self {
        Self { response: Ok(response), calls: Mutex::new(Vec::new()) }
    }

    fn failing(message: &str) -> Self {
        Self { response: Err(EcrLookupError::new(message)), calls: Mutex::new(Vec::new()) }
    }

    fn calls(&self) -> Vec<(EcrCoords, String)> {
        self.calls.lock().unwrap().clone()
    }
}

#[async_trait]
impl EcrImageLookup for FakeEcr {
    async fn digest_for_tag(
        &self,
        coords: &EcrCoords,
        tag: &str,
    ) -> Result<Option<String>, EcrLookupError> {
        self.calls.lock().unwrap().push((coords.clone(), tag.to_string()));
        self.response.clone()
    }
}

#[tokio::test]
async fn accepts_an_ecr_reference_already_pinned_by_digest_without_a_lookup() {
    let ecr = FakeEcr::failing("must not be called");
    let reference =
        format!("123456789012.dkr.ecr.cn-north-1.amazonaws.com.cn/team/workflow@{}", digest('b'));

    let resolved = resolve_image(&ecr, &image(&reference)).await.unwrap();

    assert_eq!(resolved.as_str(), digest('b'));
    assert!(ecr.calls().is_empty());
}

#[tokio::test]
async fn resolves_ecr_tag_to_validated_digest_with_exact_coordinates() {
    let expected = digest('c');
    let ecr = FakeEcr::returning(Some(expected.clone()));

    let resolved = resolve_image(
        &ecr,
        &image("123456789012.dkr.ecr.us-gov-west-1.amazonaws.com/team/workflow:release-7"),
    )
    .await
    .unwrap();

    assert_eq!(resolved.as_str(), expected);
    assert_eq!(
        ecr.calls(),
        [(
            EcrCoords {
                registry_id: "123456789012".into(),
                region: "us-gov-west-1".into(),
                repository: "team/workflow".into(),
            },
            "release-7".to_string()
        )]
    );
}

#[tokio::test]
async fn an_untagged_ecr_reference_looks_up_latest() {
    let ecr = FakeEcr::returning(Some(digest('d')));

    resolve_image(&ecr, &image("123456789012.dkr.ecr.us-east-1.amazonaws.com/workflow"))
        .await
        .unwrap();

    // One lookup, of the reference's own coordinates, for `latest`.
    assert_eq!(
        ecr.calls(),
        [(
            EcrCoords {
                registry_id: "123456789012".into(),
                region: "us-east-1".into(),
                repository: "workflow".into(),
            },
            "latest".to_string()
        )]
    );
}

#[tokio::test]
async fn reports_lookup_missing_and_invalid_digest_results() {
    let reference = image("123456789012.dkr.ecr.us-east-1.amazonaws.com/team/workflow:v1");

    let missing = resolve_image(&FakeEcr::returning(None), &reference).await.unwrap_err();
    assert_eq!(
        missing,
        ImageResolutionError::DigestMissing {
            repository: "team/workflow".into(),
            tag: "v1".into(),
        }
    );

    let invalid = resolve_image(&FakeEcr::returning(Some("sha256:short".into())), &reference)
        .await
        .unwrap_err();
    assert_eq!(
        invalid,
        ImageResolutionError::InvalidEcrDigest {
            repository: "team/workflow".into(),
            tag: "v1".into(),
            source: ImageDigestError::WrongLength(5),
        }
    );

    let failed = resolve_image(&FakeEcr::failing("access denied"), &reference).await.unwrap_err();
    assert_eq!(
        failed,
        ImageResolutionError::Ecr {
            repository: "team/workflow".into(),
            tag: "v1".into(),
            source: EcrLookupError::new("access denied"),
        }
    );
}

/// Only an image ECR could hold is looked up, or accepted pinned.
#[tokio::test]
async fn rejects_images_outside_a_valid_private_ecr_repository() {
    let ecr = FakeEcr::failing("must not be called");
    for (reference, error) in [
        (format!("quay.io/biocontainers/fastqc@{}", digest('a')), EcrRefError::NotPrivateEcr),
        ("ubuntu:latest".into(), EcrRefError::NotPrivateEcr),
        (
            "123.dkr.ecr.us-east-1.amazonaws.com/workflow:v1".into(),
            EcrRefError::InvalidRegistryId("123".into()),
        ),
        (
            "123456789012.dkr.ecr.us-east-1.amazonaws.com/workflow--bad:v1".into(),
            EcrRefError::InvalidRepository("workflow--bad".into()),
        ),
    ] {
        assert_eq!(
            resolve_image(&ecr, &image(&reference)).await,
            Err(ImageResolutionError::InvalidReference(error)),
            "{reference}"
        );
    }
    assert!(ecr.calls().is_empty());
}
