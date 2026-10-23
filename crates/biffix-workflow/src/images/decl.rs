//! The images a version declares (`images:` in `biffix.yaml`): checking each declaration, resolving
//! them into [`DeclaredImage`]s, and finding the ones a tenant has not imported yet.

use std::collections::BTreeMap;

use serde::Deserialize;

use crate::images::{Arch, BuildKind, ImageRef, ImageRefError};

/// One entry of `images:` in `biffix.yaml`. Exactly one of `ref`, `dockerfile`, `pixi`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImageDecl {
    /// An existing image to import (`quay.io/biocontainers/fastqc:0.12.1--hdfd78af_0`).
    #[serde(rename = "ref", default)]
    pub reference: Option<String>,
    /// A Dockerfile to build (a path in the workflow's source).
    #[serde(default)]
    pub dockerfile: Option<String>,
    /// The Dockerfile's build context (with `dockerfile`; default: the Dockerfile's directory).
    /// Only the git-tracked files under it are sent to the build.
    #[serde(default)]
    pub context: Option<String>,
    /// A directory holding a pixi environment (`pixi.toml` + `pixi.lock`) to build an image from.
    #[serde(default)]
    pub pixi: Option<String>,
    /// The architectures to build a `dockerfile` for (`amd64`, `arm64`; default both), for a base
    /// image that has only one. A `pixi` image is built for the platforms its `pixi.toml` lists.
    #[serde(default)]
    pub archs: Option<Vec<String>>,
    /// `own`: a Snakemake image that carries its own Snakemake (no runtime layer is added).
    #[serde(default)]
    pub runtime: Option<String>,
}

/// Why an `images:` entry is not accepted.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ImageDeclError {
    #[error("image `{0}` must set exactly one of `ref`, `dockerfile`, `pixi`")]
    NotExactlyOne(String),
    #[error(
        "image `{name}`: `{path}` must be a relative `/`-separated path inside the workflow (no \
         leading `/`, no `\\`, no `..`)"
    )]
    BadPath { name: String, path: String },
    #[error("image `{0}`: `dockerfile` names no file")]
    NoDockerfile(String),
    #[error("image `{name}`: the Dockerfile `{dockerfile}` is not inside its context `{context}`")]
    DockerfileOutsideContext { name: String, dockerfile: String, context: String },
    #[error("image `{0}`: `context` goes with `dockerfile`")]
    ContextWithoutDockerfile(String),
    #[error("image `{0}`: `archs` goes with `dockerfile`")]
    ArchsWithoutDockerfile(String),
    #[error(
        "image `{0}`: `archs` goes with `dockerfile`; a pixi image is built for the platforms its \
         pixi.toml lists"
    )]
    ArchsWithPixi(String),
    #[error("image `{name}`: `archs` must list `amd64` and/or `arm64`, not `{arch}`")]
    BadArch { name: String, arch: String },
    #[error("image `{0}`: `archs` is empty; list `amd64` and/or `arm64`, or leave it out")]
    NoArchs(String),
    #[error("image `{0}`: `runtime` may only be `own`")]
    BadRuntime(String),
    #[error("image `{name}`: {error}")]
    BadRef { name: String, error: ImageRefError },
    #[error(
        "image name `{0}` is invalid: an image name uses only lowercase letters, digits and `-`"
    )]
    BadName(String),
    #[error("`image:` and `images.default` both name the default image; keep one")]
    DuplicateDefault,
}

/// A validated `images:` entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeclaredImage {
    pub name: String,
    pub source: ImageSource,
    /// `runtime: own`.
    pub own_runtime: bool,
}

impl DeclaredImage {
    /// The image to import, when the entry names one (`ref`).
    pub fn reference(&self) -> Option<&ImageRef> {
        match &self.source {
            ImageSource::Ref(reference) => Some(reference),
            ImageSource::Build(_) => None,
        }
    }
}

/// Where a declared image comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImageSource {
    /// An existing image, imported by digest.
    Ref(ImageRef),
    /// Built from a recipe in the workflow's source.
    Build(BuildSpec),
}

/// A recipe to build an image from, with paths relative to the workflow's root.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BuildSpec {
    pub kind: BuildKind,
    /// The directory whose tracked files are the build context (`""`: the workflow's root). For
    /// `pixi`, the environment's directory.
    pub context: String,
    /// The Dockerfile's path within `context` (`None` for `pixi`: biffix generates it).
    pub dockerfile: Option<String>,
    /// The architectures a `dockerfile` is limited to (`None`: both; always `None` for `pixi`).
    pub archs: Option<Vec<Arch>>,
}

/// `path` as a clean relative path (`./a/b/` → `a/b`, `.` → ``), or `None` when it is absolute,
/// uses a `\` separator or climbs out with `..`.
pub fn clean_path(path: &str) -> Option<String> {
    let path = path.trim();
    if path.starts_with('/') || path.contains('\\') {
        return None;
    }
    let mut parts = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => return None,
            p => parts.push(p),
        }
    }
    Some(parts.join("/"))
}

/// The build a `dockerfile`/`pixi` entry describes.
fn build_spec(name: &str, decl: &ImageDecl) -> Result<BuildSpec, ImageDeclError> {
    let clean = |path: &str| {
        clean_path(path).ok_or_else(|| ImageDeclError::BadPath {
            name: name.to_string(),
            path: path.to_string(),
        })
    };
    if let Some(dir) = &decl.pixi {
        if decl.context.is_some() {
            return Err(ImageDeclError::ContextWithoutDockerfile(name.into()));
        }
        if decl.archs.is_some() {
            return Err(ImageDeclError::ArchsWithPixi(name.into()));
        }
        return Ok(BuildSpec {
            kind: BuildKind::Pixi,
            context: clean(dir)?,
            dockerfile: None,
            archs: None,
        });
    }
    let dockerfile = clean(decl.dockerfile.as_deref().unwrap_or_default())?;
    if dockerfile.is_empty() {
        return Err(ImageDeclError::NoDockerfile(name.into()));
    }
    let context = match &decl.context {
        Some(context) => clean(context)?,
        None => dockerfile.rsplit_once('/').map(|(dir, _)| dir.to_string()).unwrap_or_default(),
    };
    let within = if context.is_empty() {
        Some(dockerfile.as_str())
    } else {
        dockerfile.strip_prefix(&format!("{context}/"))
    };
    let Some(within) = within else {
        return Err(ImageDeclError::DockerfileOutsideContext {
            name: name.into(),
            dockerfile,
            context,
        });
    };
    let archs = match &decl.archs {
        None => None,
        Some(archs) => {
            let mut parsed = archs
                .iter()
                .map(|a| {
                    Arch::parse(a.trim()).ok_or_else(|| ImageDeclError::BadArch {
                        name: name.into(),
                        arch: a.clone(),
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            parsed.sort();
            parsed.dedup();
            if parsed.is_empty() {
                return Err(ImageDeclError::NoArchs(name.into()));
            }
            Some(parsed)
        }
    };
    Ok(BuildSpec {
        kind: BuildKind::Dockerfile,
        dockerfile: Some(within.to_string()),
        context,
        archs,
    })
}

/// Check an `images:` name: lowercase letters, digits and `-`. The server checks a built image's
/// name with this too, so the CLI and the server agree.
pub fn check_image_name(name: &str) -> Result<(), ImageDeclError> {
    let valid = !name.is_empty()
        && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    if valid {
        Ok(())
    } else {
        Err(ImageDeclError::BadName(name.to_string()))
    }
}

/// Whether image `name`'s `runtime` says it carries its own Snakemake (`own`); any other value is
/// an error.
pub fn own_runtime(name: &str, runtime: Option<&str>) -> Result<bool, ImageDeclError> {
    match runtime {
        None => Ok(false),
        Some("own") => Ok(true),
        Some(_) => Err(ImageDeclError::BadRuntime(name.to_string())),
    }
}

/// Validate a workflow's `images:` (plus `image:`, sugar for `images.default: {ref}`) into the
/// images to import or build, sorted by name.
pub fn declared_images(
    images: &BTreeMap<String, ImageDecl>,
    image: Option<&str>,
) -> Result<Vec<DeclaredImage>, ImageDeclError> {
    let mut all = images.clone();
    if let Some(image) = image.filter(|i| !i.trim().is_empty()) {
        if all.contains_key("default") {
            return Err(ImageDeclError::DuplicateDefault);
        }
        all.insert(
            "default".into(),
            ImageDecl { reference: Some(image.to_string()), ..ImageDecl::default() },
        );
    }
    all.into_iter()
        .map(|(name, decl)| {
            check_image_name(&name)?;
            let kinds = [decl.reference.is_some(), decl.dockerfile.is_some(), decl.pixi.is_some()];
            if kinds.iter().filter(|k| **k).count() != 1 {
                return Err(ImageDeclError::NotExactlyOne(name));
            }
            let own_runtime = own_runtime(&name, decl.runtime.as_deref())?;
            let source = match &decl.reference {
                Some(reference) => ImageSource::Ref(
                    ImageRef::parse(reference)
                        .map_err(|error| ImageDeclError::BadRef { name: name.clone(), error })?,
                ),
                None => ImageSource::Build(build_spec(&name, &decl)?),
            };
            if decl.reference.is_some() && (decl.context.is_some() || decl.archs.is_some()) {
                return Err(if decl.context.is_some() {
                    ImageDeclError::ContextWithoutDockerfile(name)
                } else {
                    ImageDeclError::ArchsWithoutDockerfile(name)
                });
            }
            Ok(DeclaredImage { name, source, own_runtime })
        })
        .collect()
}

/// An `images:` name for a scanned reference: its last repository segment and tag (or digest),
/// lowercased to `[a-z0-9-]` (`quay.io/biocontainers/fastqc:0.12.1--hdfd78af_0` →
/// `fastqc-0-12-1-hdfd78af-0`).
pub(crate) fn auto_name(reference: &ImageRef) -> String {
    let last = crate::text::basename(&reference.repository);
    let suffix = match (&reference.tag, &reference.digest) {
        (Some(tag), _) => tag.clone(),
        (None, Some(digest)) => {
            digest.strip_prefix("sha256:").unwrap_or(digest).chars().take(12).collect()
        }
        (None, None) => String::new(),
    };
    let raw = format!("{last}-{suffix}").to_ascii_lowercase();
    let mut name = String::with_capacity(raw.len());
    for c in raw.chars() {
        let c = if c.is_ascii_lowercase() || c.is_ascii_digit() { c } else { '-' };
        if !(c == '-' && (name.is_empty() || name.ends_with('-'))) {
            name.push(c);
        }
    }
    let name = name.trim_end_matches('-').to_string();
    if name.is_empty() {
        "image".into()
    } else {
        name
    }
}

/// An image to add to `images:`: a reference a workflow names that it does not declare yet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewImage {
    /// Its `auto_name` (`-2`, `-3`, … when the name is taken).
    pub name: String,
    pub reference: String,
    /// Where it was found (`rules/qc.smk:12`, `process FASTQC`).
    pub location: String,
}

/// The references in `found` (`(reference, where)`) that `images` and `default` (the `image:`
/// reference) do not declare yet, each with a free name. A found reference is declared when a
/// run's image map (`build` in `crates/biffix-backend/src/images/map.rs`) would resolve it: it is one of a declared reference's
/// [`ImageRef::map_keys`] (as Docker reads both), or the `images:` name of a declared image (how a
/// task names a built image, `aws_batch_container_image="align"`, and may name an imported one),
/// `default` included when `image:` declares it. A reference that does not parse is skipped.
pub fn new_images(
    images: &BTreeMap<String, ImageDecl>,
    default: Option<&str>,
    found: &[(&str, &str)],
) -> Vec<NewImage> {
    let mut declared: std::collections::BTreeSet<String> = images
        .values()
        .filter_map(|d| d.reference.as_deref())
        .chain(default)
        .filter_map(|r| ImageRef::parse(r).ok())
        .flat_map(|r| r.map_keys())
        .collect();
    let mut names: std::collections::BTreeSet<String> = images.keys().cloned().collect();
    names.insert("default".into());
    let is_name = |reference: &str| {
        images.contains_key(reference)
            || (reference == "default" && default.is_some_and(|d| !d.trim().is_empty()))
    };
    let mut added = Vec::new();
    for (reference, location) in found {
        if is_name(reference.trim()) {
            continue;
        }
        let Ok(parsed) = ImageRef::parse(reference) else {
            continue;
        };
        if declared.contains(&parsed.to_string()) {
            continue;
        }
        declared.extend(parsed.map_keys());
        let base = auto_name(&parsed);
        let mut name = base.clone();
        let mut n = 2;
        while names.contains(&name) {
            name = format!("{base}-{n}");
            n += 1;
        }
        names.insert(name.clone());
        added.push(NewImage {
            name,
            reference: reference.trim().to_string(),
            location: location.to_string(),
        });
    }
    added
}
