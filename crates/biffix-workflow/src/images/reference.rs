//! Image references (`[registry/]repository[:tag][@digest]`): parsing, normalizing and printing
//! them ([`ImageRef`]).

use std::fmt;

use crate::images::registry::ecr_coords;
use crate::images::{EcrCoords, EcrRefError, DOCKER_HUB};

/// A parsed image reference: `[registry/]repository[:tag][@digest]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageRef {
    /// The registry host (with port if any), normalized: `docker.io` for Docker Hub.
    pub registry: String,
    /// The repository path within the registry (`library/ubuntu` on Docker Hub).
    pub repository: String,
    pub tag: Option<String>,
    /// `sha256:<64 hex>`.
    pub digest: Option<String>,
}

/// Why an image reference is not usable.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ImageRefError {
    #[error("image reference is empty")]
    Empty,
    #[error("image reference `{0}` is malformed (a URL, or it has whitespace)")]
    Malformed(String),
    #[error("image reference `{0}` has a registry that is not a host name with an optional port")]
    BadRegistry(String),
    #[error(
        "image reference `{0}` has a repository Docker rejects: each `/`-separated part is \
         lowercase letters and digits, joined by one `.`, one or two `_`, or dashes"
    )]
    BadRepository(String),
    #[error(
        "image reference `{0}` has a tag Docker rejects: 1-128 letters, digits, `_`, `.` and `-`, \
         not starting with `.` or `-`"
    )]
    BadTag(String),
    #[error("image reference `{0}` has a digest that is not sha256:<64 hex>")]
    BadDigest(String),
    #[error(
        "image reference `{0}` is a digest or image id alone; name the image too \
         (`<name>@sha256:…`)"
    )]
    DigestWithoutName(String),
    #[error("image reference `{0}` has a name longer than {NAME_MAX} characters")]
    NameTooLong(String),
}

/// The longest name Docker accepts: the normalized `<registry>/<repository>` ([`ImageRef::name`]),
/// so a bare Docker Hub name counts its implied `docker.io/library/`.
const NAME_MAX: usize = 255;

/// The longest tag Docker accepts.
const TAG_MAX: usize = 128;

/// Whether `h` is a sha256 in hex in either case (a typed reference's spelling, which
/// [`ImageRef::parse`] lowercases).
fn is_any_case_sha256_hex(h: &str) -> bool {
    h.len() == crate::digest::SHA256_HEX_LEN && h.bytes().all(|b| b.is_ascii_hexdigit())
}

fn is_digest(d: &str) -> bool {
    d.strip_prefix("sha256:").is_some_and(is_any_case_sha256_hex)
}

/// A tag: `[A-Za-z0-9_][A-Za-z0-9_.-]{0,127}`.
fn is_tag(t: &str) -> bool {
    let word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    t.len() <= TAG_MAX
        && t.bytes().next().is_some_and(word)
        && t.bytes().all(|b| word(b) || b == b'.' || b == b'-')
}

/// A repository path component: runs of lowercase letters and digits, each pair joined by one
/// separator — `.`, `_`, `__`, or one or more `-`.
fn is_path_component(c: &str) -> bool {
    let alnum = |b: u8| b.is_ascii_lowercase() || b.is_ascii_digit();
    let bytes = c.as_bytes();
    if !bytes.first().is_some_and(|b| alnum(*b)) || !bytes.last().is_some_and(|b| alnum(*b)) {
        return false;
    }
    let mut i = 0;
    while i < bytes.len() {
        if alnum(bytes[i]) {
            i += 1;
            continue;
        }
        let start = i;
        while !alnum(bytes[i]) {
            i += 1;
        }
        let sep = &c[start..i];
        if !(matches!(sep, "." | "_" | "__") || sep.bytes().all(|b| b == b'-')) {
            return false;
        }
    }
    true
}

/// A registry: a host — dot-joined DNS labels, or a bracketed IPv6 address — with an optional
/// numeric port.
fn is_registry(r: &str) -> bool {
    let (host, port) = if r.starts_with('[') {
        match r.find(']') {
            Some(end) => r.split_at(end + 1),
            None => return false,
        }
    } else {
        r.find(':').map_or((r, ""), |at| r.split_at(at))
    };
    let port_ok = port.is_empty()
        || port
            .strip_prefix(':')
            .is_some_and(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()));
    let label = |l: &str| {
        let b = l.as_bytes();
        b.first().is_some_and(u8::is_ascii_alphanumeric)
            && b.last().is_some_and(u8::is_ascii_alphanumeric)
            && b.iter().all(|c| c.is_ascii_alphanumeric() || *c == b'-')
    };
    let host_ok = match host.strip_prefix('[').and_then(|h| h.strip_suffix(']')) {
        Some(ipv6) => {
            !ipv6.is_empty()
                && ipv6.bytes().all(|b| b.is_ascii_hexdigit() || b == b':' || b == b'.')
        }
        None => host.split('.').all(label),
    };
    host_ok && port_ok
}

impl ImageRef {
    /// Parse `value` by Docker's reference grammar (`[registry/]path[:tag][@digest]`, as
    /// `docker pull` reads it) into its normalized form:
    ///
    /// - The first path component is a registry only when it has a `.` or `:` or is `localhost`;
    ///   otherwise the image is on Docker Hub (`docker.io`, which `index.docker.io` and
    ///   `registry-1.docker.io` also name), and a one-part name is under `library/`.
    /// - The registry is lowercased (host names are case-insensitive). The repository is not: an
    ///   uppercase repository is an error, as in Docker.
    /// - No tag and no digest means `latest`.
    ///
    /// It is stricter than Docker in two ways: a first component with uppercase letters but no `.`
    /// or `:` (which Docker would take as a registry host) is not a registry, so it fails as a
    /// repository (a lowercased host would print as a Docker Hub path and not parse back the same;
    /// `Quay.IO` has a `.`, so it is a registry and is lowercased); and only `sha256` digests are
    /// accepted.
    pub fn parse(value: &str) -> Result<Self, ImageRefError> {
        let value = value.trim();
        if value.is_empty() {
            return Err(ImageRefError::Empty);
        }
        if value.contains("://") || value.chars().any(char::is_whitespace) {
            return Err(ImageRefError::Malformed(value.into()));
        }
        // Docker rejects a bare 64-hex name (it reads as an image id). A `sha256:<hex>` value is
        // left as Docker reads it (the image `sha256` at that tag): `--image sha256:<hex>` is
        // the CLI's spelling for an image in the deploy's worker repository, which the backend
        // expands before it parses the reference.
        if is_any_case_sha256_hex(value) {
            return Err(ImageRefError::DigestWithoutName(value.into()));
        }
        let (name, digest) = match value.split_once('@') {
            Some(("", _)) => return Err(ImageRefError::DigestWithoutName(value.into())),
            Some((name, digest)) => {
                if !is_digest(digest) {
                    return Err(ImageRefError::BadDigest(value.into()));
                }
                (name, Some(digest.to_ascii_lowercase()))
            }
            None => (value, None),
        };
        // A tag follows the last `:` after the last `/` (a `:` before it is a registry port).
        let (name, tag) = match name.rsplit_once(':') {
            Some((n, t)) if !t.contains('/') => {
                if !is_tag(t) {
                    return Err(ImageRefError::BadTag(value.into()));
                }
                (n, Some(t.to_string()))
            }
            _ => (name, None),
        };
        let (registry, repository) = match name.split_once('/') {
            Some((first, rest))
                if first.contains('.') || first.contains(':') || first == "localhost" =>
            {
                if !is_registry(first) {
                    return Err(ImageRefError::BadRegistry(value.into()));
                }
                (first.to_ascii_lowercase(), rest.to_string())
            }
            _ => (DOCKER_HUB.to_string(), name.to_string()),
        };
        let registry = match registry.as_str() {
            "index.docker.io" | "registry-1.docker.io" => DOCKER_HUB.to_string(),
            _ => registry,
        };
        let repository = if registry == DOCKER_HUB && !repository.contains('/') {
            format!("library/{repository}")
        } else {
            repository
        };
        if !repository.split('/').all(is_path_component) {
            return Err(ImageRefError::BadRepository(value.into()));
        }
        // On the normalized `<registry>/<repository>`, as Docker checks it: a bare Docker Hub name
        // grows by `docker.io/library/`.
        if registry.len() + 1 + repository.len() > NAME_MAX {
            return Err(ImageRefError::NameTooLong(value.into()));
        }
        Ok(Self {
            registry,
            repository,
            tag: if digest.is_none() && tag.is_none() { Some("latest".into()) } else { tag },
            digest,
        })
    }

    /// `<registry>/<repository>` (no tag or digest).
    pub fn name(&self) -> String {
        format!("{}/{}", self.registry, self.repository)
    }

    /// Whether the image is in a private ECR registry (`<account>.dkr.ecr.<region>.amazonaws.com`,
    /// or `….amazonaws.com.cn` in the China partition). [`ImageRef::ecr_coords`] validates the
    /// host's account and region.
    pub fn is_private_ecr(&self) -> bool {
        self.registry.contains(".dkr.ecr.")
            && (self.registry.ends_with(".amazonaws.com")
                || self.registry.ends_with(".amazonaws.com.cn"))
    }

    /// The private ECR repository this image is in: its account, region and repository, checked
    /// by ECR's rules (which are stricter than Docker's).
    pub fn ecr_coords(&self) -> Result<EcrCoords, EcrRefError> {
        ecr_coords(&self.registry, &self.repository)
    }

    /// This reference pinned to `digest` (what a version records: tag kept for humans).
    pub fn pinned(&self, digest: &str) -> Self {
        Self { digest: Some(digest.to_string()), ..self.clone() }
    }

    /// The spellings a run's image map keys this reference by (after the engine normalizes what
    /// a task names, as [`ImageRef::parse`] does): its normalized form, and for a reference with
    /// both a tag and a digest also the tag alone, which runs the digest it is pinned to. The
    /// first is the reference's own spelling; the rest yield to any image that is spelled so.
    pub fn map_keys(&self) -> Vec<String> {
        let mut keys = vec![self.to_string()];
        if let (Some(tag), Some(_)) = (&self.tag, &self.digest) {
            keys.push(format!("{}:{tag}", self.name()));
        }
        keys
    }
}

impl fmt::Display for ImageRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.registry, self.repository)?;
        if let Some(tag) = &self.tag {
            write!(f, ":{tag}")?;
        }
        if let Some(digest) = &self.digest {
            write!(f, "@{digest}")?;
        }
        Ok(())
    }
}
