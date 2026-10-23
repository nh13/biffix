//! The keys and build subjects an image's import, derivation and build are tracked by, and the
//! architectures and build kinds they distinguish.

use std::fmt;

use sha2::{Digest, Sha256};

use crate::images::DOCKERFILE_BUILD_VERSION;

/// The key an import of `source_digest` is cached under, tenant-wide: the same upstream bytes are
/// imported once whichever version (or tag) names them.
pub fn content_key(source_digest: &str) -> String {
    hex::encode(Sha256::digest(format!("import:{source_digest}")))
}

/// The build subject of an import.
pub fn subject(content_key: &str) -> String {
    format!("image:{content_key}")
}

/// The key a derived image is cached under: the imported image's digest with the runtime layer of
/// `runtime_key` appended. Independent of the coordinator base, so a base bump derives nothing anew.
pub fn derived_key(imported_digest: &str, runtime_key: &str) -> String {
    hex::encode(Sha256::digest(format!("derived:{imported_digest}:{runtime_key}")))
}

/// The build subject of a derive.
pub fn derived_subject(derived_key: &str) -> String {
    format!("image:derived:{derived_key}")
}

/// A CPU architecture biffix builds images for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Arch {
    Amd64,
    Arm64,
}

impl Arch {
    /// Both architectures, in build order.
    pub const ALL: [Arch; 2] = [Arch::Amd64, Arch::Arm64];

    /// The OCI name (`amd64`, `arm64`).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Amd64 => "amd64",
            Self::Arm64 => "arm64",
        }
    }

    /// The inverse of [`Arch::as_str`].
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "amd64" => Some(Self::Amd64),
            "arm64" => Some(Self::Arm64),
            _ => None,
        }
    }
}

impl fmt::Display for Arch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What a built image is built from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BuildKind {
    /// A Dockerfile the workflow ships.
    Dockerfile,
    /// A pixi environment (`pixi.toml` + `pixi.lock`), built with biffix's generated Dockerfile.
    Pixi,
}

impl BuildKind {
    /// The name the API and the build recipe record (`dockerfile`, `pixi`); [`str::parse`] reads
    /// it back.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Dockerfile => "dockerfile",
            Self::Pixi => "pixi",
        }
    }

    /// The version of the recipe biffix builds this kind with; part of the build's key.
    pub(crate) fn generator_version(self) -> u32 {
        match self {
            Self::Dockerfile => DOCKERFILE_BUILD_VERSION,
            Self::Pixi => crate::pixi::PIXI_GENERATOR_VERSION,
        }
    }
}

/// A build kind name that is not `dockerfile` or `pixi`.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("unknown build kind `{0}` (expected `dockerfile` or `pixi`)")]
pub struct UnknownBuildKind(pub String);

impl std::str::FromStr for BuildKind {
    type Err = UnknownBuildKind;

    /// The inverse of [`BuildKind::as_str`].
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "dockerfile" => Ok(Self::Dockerfile),
            "pixi" => Ok(Self::Pixi),
            other => Err(UnknownBuildKind(other.to_string())),
        }
    }
}

/// The key a built image is cached under, tenant-wide: what it is built from (the kind and biffix's
/// recipe version, and for `pixi` the sha256 of the Dockerfile biffix generates, so a change to its
/// pinned images rebuilds even without a version bump), the sha256 of the staged context tarball,
/// the Dockerfile's path within the context, and the architectures.
///
/// The context is named by its tarball's sha256, which the build checks against the bytes it
/// downloads before it builds (`codebuild/image/build.sh`), not by the client's claimed tree hash,
/// which nothing checks: a key then only ever names an image built from the content it spells, so
/// no registration can seed the tenant-wide cache with an image built from other content. The
/// tarball also carries each file's mode, so `chmod +x` is a new build. The CLI's tarball is
/// deterministic, so the same context files still build once. The `build2:` scheme keeps these keys
/// apart from the earlier tree-hash keys (which an old registration could have claimed for any
/// content); moving to it rebuilds each built image once.
///
/// `context_sha256` must be sha256 hex: the key is injective only because that part is fixed-width
/// hex and the arch list holds no `:`.
pub fn build_content_key(
    kind: BuildKind,
    context_sha256: &str,
    dockerfile: Option<&str>,
    archs: &[Arch],
) -> String {
    let mut archs = archs.to_vec();
    archs.sort();
    archs.dedup();
    let archs: Vec<&str> = archs.iter().map(|a| a.as_str()).collect();
    let recipe = match kind {
        BuildKind::Pixi => format!("{}:", hex::encode(Sha256::digest(crate::pixi::dockerfile()))),
        BuildKind::Dockerfile => String::new(),
    };
    hex::encode(Sha256::digest(format!(
        "build2:{}:{}:{recipe}{context_sha256}:{}:{}",
        kind.as_str(),
        kind.generator_version(),
        dockerfile.unwrap_or(""),
        archs.join(",")
    )))
}

/// The build subject of a built image: one per architecture's build, and one (no arch) for the
/// index that joins them.
pub fn build_subject(content_key: &str, arch: Option<Arch>) -> String {
    match arch {
        Some(arch) => format!("image:build:{content_key}:{arch}"),
        None => format!("image:build:{content_key}"),
    }
}
