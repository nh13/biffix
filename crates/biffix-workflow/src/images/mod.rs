//! Workflow images: the container images a workflow version declares (`images:` in `biffix.yaml`),
//! and the keys biffix tracks their import by.
//!
//! Every declared image is imported into the tenant's workflow repository by digest (a CodeBuild
//! build: copy, probe, archive, keep-tag), so a version only ever runs images the tenant holds. An
//! import is identified by its [`content_key`] — the same upstream digest imported for two versions
//! is one import — and followed as the build subject [`subject`].
//!
//! A Snakemake task image is then **derived**: the version's engine runtime layer is appended to the
//! imported image, with a `python` shim first on `PATH` so the executor's `python -m snakemake`
//! runs the pinned Snakemake while the image's own tools keep their own Python. A derived image is
//! identified by [`derived_key`] (imported digest + runtime key) and followed as
//! [`derived_subject`]. Images declared `runtime: own` carry their own Snakemake and are not derived.
//!
//! An image can also be **built** from a recipe the workflow ships (`dockerfile:` or `pixi:`): one
//! native build per architecture ([`build_subject`] with the arch), then an index joining them
//! ([`build_subject`] without), identified by [`build_content_key`] — the same recipe and context
//! tarball build once, tenant-wide. A built image is then used exactly like an imported one.

use serde::Deserialize;

mod decl;
mod keys;
mod reference;
mod registry;
#[cfg(test)]
mod tests;

#[cfg(test)]
use decl::auto_name;
pub use decl::{
    check_image_name, clean_path, declared_images, new_images, own_runtime, BuildSpec,
    DeclaredImage, ImageDecl, ImageDeclError, ImageSource, NewImage,
};
pub use keys::{
    build_content_key, build_subject, content_key, derived_key, derived_subject, subject, Arch,
    BuildKind, UnknownBuildKind,
};
pub use reference::{ImageRef, ImageRefError};
pub use registry::{ecr_coords_from_repo_uri, EcrCoords, EcrRefError, MANIFEST_MEDIA_TYPES};

/// The tag every imported image carries so the repository's lifecycle never expires it:
/// `biffix-keep-<digest12>` (index children add `-<n>`). The build scripts compute and apply it
/// (`keep_tag` in `codebuild/lib/common.sh`), and so does the restore tool
/// (`infra/biffix_infra/image_restore.py`); a test ties both of their prefixes to this one.
pub const KEEP_TAG_PREFIX: &str = "biffix-keep-";

/// Where a Snakemake runtime layer puts its `python`/`python3` shims. A derived image's entrypoint
/// ends with `/opt/biffix/shim-exec`, which puts this first on `PATH` for the task's command.
pub const SHIM_DIR: &str = "/opt/biffix/shim";

/// Tag prefix of a derived image in the workflow repository: `derived-<derived_key12>`.
pub const DERIVED_TAG_PREFIX: &str = "derived-";

/// Tag prefix of a built image in the builds repository: `build-<key12>` (the index) and
/// `build-<key12>-<arch>` (each architecture's image).
pub const BUILD_TAG_PREFIX: &str = "build-";

/// The version of biffix's Dockerfile build (`codebuild/image/build.sh`): part of every Dockerfile
/// build's key, so a change to how biffix builds rebuilds rather than reusing an old image.
pub(crate) const DOCKERFILE_BUILD_VERSION: u32 = 1;

/// Docker Hub's canonical registry host (a bare `ubuntu:24.04` means `docker.io/library/ubuntu`).
pub const DOCKER_HUB: &str = "docker.io";

/// What an import found in an image without running it (`CAPABILITIES=bash=1,libc=glibc,…`).
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, Deserialize)]
pub struct Capabilities {
    /// A `bash` the engines' task scripts can run with.
    pub bash: bool,
    /// `glibc` | `musl` | `none` (static or distroless).
    pub libc: String,
    /// The Snakemake an image carries (its own `snakemake-<v>.dist-info`), if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snakemake: Option<String>,
    /// Its `snakemake-storage-plugin-s3` version, if any (what a `runtime: own` child job loads).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storage_s3: Option<String>,
    /// Its `snakemake-logger-plugin-biffix` version, if any (reported only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logger: Option<String>,
    /// Its `snakemake-executor-plugin-aws-batch` version, if any (reported only: the version string
    /// cannot tell the fork from upstream).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executor: Option<String>,
}

impl Capabilities {
    /// Parse the import build's `k=v,…` output. Unknown keys are ignored; a missing or empty
    /// `libc` is `none`.
    pub fn parse(s: &str) -> Self {
        let mut caps = Self { libc: "none".into(), ..Self::default() };
        for (k, v) in s.split(',').filter_map(|kv| kv.split_once('=')) {
            let v = v.trim();
            let version = (!v.is_empty()).then(|| v.to_string());
            match k.trim() {
                "bash" => caps.bash = v == "1",
                "libc" => caps.libc = if v.is_empty() { "none" } else { v }.to_string(),
                "snakemake" => caps.snakemake = version,
                "storage_s3" => caps.storage_s3 = version,
                "logger" => caps.logger = version,
                "executor" => caps.executor = version,
                _ => {}
            }
        }
        caps
    }
}
