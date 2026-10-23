//! What biffix asks of a registry: the manifest types a tag lookup accepts, and the ECR
//! coordinates (account, region, repository) a reference names.

/// The manifest media types a tag lookup accepts, for ECR (`BatchGetImage`) and other registries
/// (the `Accept` header) alike: an index / manifest list, or a single-platform manifest. A registry
/// answers with the manifest the tag names whenever its type is listed (order is no preference),
/// so a multi-architecture tag resolves to its index's digest — the digest every client calls the
/// image's. ECR also accepts the two index types although its API reference lists only manifest
/// types (it answers with the same digests `DescribeImages` reports).
/// Schema 1 manifests are left out: none is pushed today, and asking for them can make a registry
/// convert an image and answer a different digest. `infra/biffix_infra/image_retention.py`
/// (`MEDIA_TYPES`) sends the same list; keep them in step.
pub const MANIFEST_MEDIA_TYPES: &[&str] = &[
    "application/vnd.oci.image.index.v1+json",
    "application/vnd.docker.distribution.manifest.list.v2+json",
    "application/vnd.oci.image.manifest.v1+json",
    "application/vnd.docker.distribution.manifest.v2+json",
];

/// A private ECR repository's coordinates: what an ECR API call (a tag lookup) and a console
/// deep-link name it by.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EcrCoords {
    /// The registry's 12-digit AWS account id.
    pub registry_id: String,
    pub region: String,
    pub repository: String,
}

/// Why an image is not in a private ECR repository ECR could hold.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum EcrRefError {
    #[error(
        "image reference is not in a private ECR registry; expected \
         `<account>.dkr.ecr.<region>.amazonaws.com[.cn]/<repository>`"
    )]
    NotPrivateEcr,
    /// The registry id it names.
    #[error("private ECR registry id `{0}` must be a 12-digit AWS account id")]
    InvalidRegistryId(String),
    /// The region it names.
    #[error(
        "private ECR registry region `{0}` is not an AWS region of its partition (`cn-` regions \
         are in `.amazonaws.com.cn`, the others in `.amazonaws.com`)"
    )]
    InvalidRegion(String),
    /// The repository it names.
    #[error(
        "private ECR repository name `{0}` is invalid: 2-256 characters of lowercase letters, \
         digits and single `.`, `_` or `-` between them, in `/`-separated parts"
    )]
    InvalidRepository(String),
}

/// The ECR coordinates of a bare repository URI (`<id>.dkr.ecr.<region>.amazonaws.com/<repo>`, with
/// no tag or digest) — e.g. the deploy's worker-image repo. Lets a version's image be deep-linked
/// to the ECR console even when its registration pinned only a bare digest (which carries no repo
/// of its own). `None` when the URI is not a private ECR repository.
pub fn ecr_coords_from_repo_uri(uri: &str) -> Option<EcrCoords> {
    let (registry, repository) = uri.split_once('/')?;
    ecr_coords(registry, repository).ok()
}

/// The coordinates of `repository` in `registry`, checked by ECR's rules: the host names an
/// account and a region (`cn-` regions in the China partition's `.amazonaws.com.cn`), and the
/// repository is 2-256 characters of lowercase path components joined by single separators.
pub(super) fn ecr_coords(registry: &str, repository: &str) -> Result<EcrCoords, EcrRefError> {
    if !valid_ecr_repository(repository) {
        return Err(EcrRefError::InvalidRepository(repository.to_owned()));
    }

    let parts: Vec<&str> = registry.split('.').collect();
    let (registry_id, region, china_partition) = match parts.as_slice() {
        [registry_id, "dkr", "ecr", region, "amazonaws", "com"] => (*registry_id, *region, false),
        [registry_id, "dkr", "ecr", region, "amazonaws", "com", "cn"] => {
            (*registry_id, *region, true)
        }
        _ => return Err(EcrRefError::NotPrivateEcr),
    };
    if registry_id.len() != 12 || !registry_id.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(EcrRefError::InvalidRegistryId((*registry_id).to_owned()));
    }
    if !valid_aws_region(region) || region.starts_with("cn-") != china_partition {
        return Err(EcrRefError::InvalidRegion((*region).to_owned()));
    }

    Ok(EcrCoords {
        registry_id: registry_id.to_owned(),
        region: region.to_owned(),
        repository: repository.to_owned(),
    })
}

/// An AWS region's shape: two or more lowercase words (the first of two or more letters) and a
/// number, joined by `-` (`us-east-1`, `us-gov-west-1`).
fn valid_aws_region(value: &str) -> bool {
    let parts: Vec<&str> = value.split('-').collect();
    let Some((number, prefix)) = parts.split_last() else {
        return false;
    };

    prefix.len() >= 2
        && prefix.first().is_some_and(|part| part.len() >= 2)
        && prefix.iter().all(|part| {
            !part.is_empty()
                && part.bytes().all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
                && part.bytes().any(|byte| byte.is_ascii_lowercase())
        })
        && !number.is_empty()
        && number.bytes().all(|byte| byte.is_ascii_digit())
}

/// An ECR repository name: stricter than Docker's (one separator between alphanumeric runs, never
/// `__` or `--`), and 2-256 characters.
fn valid_ecr_repository(value: &str) -> bool {
    (2..=256).contains(&value.len()) && value.split('/').all(valid_ecr_repository_component)
}

fn valid_ecr_repository_component(component: &str) -> bool {
    let is_separator = |byte: u8| matches!(byte, b'.' | b'_' | b'-');

    !component.is_empty()
        && component
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || is_separator(byte))
        && component
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        && component
            .bytes()
            .last()
            .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        && component
            .as_bytes()
            .windows(2)
            .all(|pair| !(is_separator(pair[0]) && is_separator(pair[1])))
}
