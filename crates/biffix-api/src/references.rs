//! `references` — reference bundles: named sets of reference-file URIs a launch form fills params
//! from.

use serde::{Deserialize, Serialize};

/// A reference bundle (`GET /references`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ReferenceBundle {
    /// The id a form, an example or a submit names it by (`grch38-gatk`).
    pub id: String,
    /// `assembly` (a genome) or `assay_kit` (a panel's files).
    pub kind: String,
    pub display_name: String,
    /// Named entries → `s3://` URIs (`fasta`, `fasta_index`, `gtf`, `star_index`, …): a file, a
    /// folder (an index directory, its trailing `/` optional), or an index named by its base path
    /// (`s3://refs/hg38/bwa/genome` for `genome.amb`, `genome.bwt`, …), which a submit accepts for
    /// the params the bundle fills.
    pub uris: std::collections::BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    /// A genome the alignment viewer hosts (`hg38`, `mm39`) to show this bundle's alignments
    /// against instead of its own FASTA: its sequence and annotation then come from the viewer's
    /// public servers, not from biffix (so set it only for public assemblies).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub igv_genome: Option<String>,
    pub created_at_micros: i64,
    pub updated_at_micros: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_by: Option<String>,
}

/// `GET /references` query: which bundles a form can use.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::IntoParams))]
#[cfg_attr(feature = "openapi", into_params(parameter_in = Query))]
pub struct ReferenceBundleListQuery {
    /// Only bundles of this kind.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// Only bundles with all these keys (comma-separated).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keys: Option<String>,
}

/// `POST /references` and `PUT /references/{id}` request (on `PUT`, `id` must match the path).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ReferenceBundleRequest {
    pub id: String,
    pub kind: String,
    pub display_name: String,
    pub uris: std::collections::BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    /// See [`ReferenceBundle::igv_genome`]; empty or absent ⇒ none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub igv_genome: Option<String>,
}

/// The saved bundle, with a warning per URI that could not be found (saved anyway: the files may be
/// on their way).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct SavedReferenceBundle {
    pub bundle: ReferenceBundle,
    pub warnings: Vec<String>,
}

/// The reference bundle a run was launched with and the URIs it filled, by bundle key.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ReferenceUse {
    pub id: String,
    pub uris: std::collections::BTreeMap<String, String>,
}

/// Destructured in full, so a field added to the event's type fails to compile here until the API
/// carries it (or says why not).
impl From<biffix_core::event::ReferenceUse> for ReferenceUse {
    fn from(used: biffix_core::event::ReferenceUse) -> Self {
        let biffix_core::event::ReferenceUse { id, uris } = used;
        Self { id, uris }
    }
}

/// What a run ran with, as recorded when it was submitted (its `run_submitted` anchor).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct RunProvenance {
    /// The workflow source commit the run ran.
    pub git_sha: String,
    /// The default image the run's tasks ran, pinned by digest. Empty when the version has none.
    pub image_digest: String,
    /// The default image as imported, when the tasks ran an image derived from it (a Snakemake
    /// image with the engine runtime added).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_source_digest: Option<String>,
    /// The engine runtime the coordinator ran with. Absent when the run recorded none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<RuntimeUse>,
    /// Each image reference the workflow declares (as written and normalized) → the tenant image
    /// its tasks ran. Absent when the run recorded none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_map: Option<std::collections::BTreeMap<String, String>>,
    /// The images given at submit (`--image`), as pinned, from the run's anchor. Absent when the
    /// submit named none (and, like the rest of provenance, for a run that never launched: see
    /// `RunDetail.submit_images`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub submit_images: Option<Vec<SubmittedImage>>,
}

/// An image given at submit: the reference as written and the digest-pinned source it resolved to.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct SubmittedImage {
    pub declared_ref: String,
    pub source_ref: String,
}

/// Destructured in full, as for [`ReferenceUse`]. The tenant cache key is internal.
impl From<&biffix_core::event::SubmitImage> for SubmittedImage {
    fn from(image: &biffix_core::event::SubmitImage) -> Self {
        let biffix_core::event::SubmitImage { declared_ref, source_ref, content_key: _ } = image;
        Self { declared_ref: declared_ref.clone(), source_ref: source_ref.clone() }
    }
}

impl SubmittedImage {
    /// How a resume or re-run gives this image again: the spelling as given at the digest it ran
    /// (`ubuntu:24.04@sha256:…`), so the same bytes run and the spelling tasks name still maps to
    /// them. A spelling that already carries a digest is given as-is; without a spelling or a digest
    /// (never the case for a pinned image) it is given as its source.
    pub fn pinned_spelling(&self) -> String {
        if self.declared_ref.contains("@sha256:") {
            return self.declared_ref.clone();
        }
        match self.source_ref.rsplit_once('@') {
            Some((_, digest)) if !self.declared_ref.is_empty() && digest.starts_with("sha256:") => {
                format!("{}@{digest}", self.declared_ref)
            }
            _ => self.source_ref.clone(),
        }
    }
}

/// The engine runtime a run's coordinator ran with.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct RuntimeUse {
    pub engine_version: String,
    /// sha256 of the runtime's recipe.
    pub runtime_key: String,
    /// `<repo>@sha256:…` of the coordinator image (base image + this runtime).
    pub coordinator_image: String,
}

/// Destructured in full, as for [`ReferenceUse`].
impl From<&biffix_core::event::RuntimeUse> for RuntimeUse {
    fn from(runtime: &biffix_core::event::RuntimeUse) -> Self {
        let biffix_core::event::RuntimeUse { engine_version, runtime_key, coordinator_image } =
            runtime;
        Self {
            engine_version: engine_version.clone(),
            runtime_key: runtime_key.clone(),
            coordinator_image: coordinator_image.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use biffix_core::event as core;

    /// The cases the UI's `pinnedSpelling` is tested with (`ui/src/lib/launch/form.test.ts`), so
    /// the two cannot diverge.
    #[test]
    fn an_image_is_given_again_as_its_spelling_at_the_digest_it_ran() {
        let pinned = |declared: &str, source: &str| {
            SubmittedImage { declared_ref: declared.into(), source_ref: source.into() }
                .pinned_spelling()
        };
        assert_eq!(pinned("quay.io/b/z", "quay.io/b/z:latest@sha256:f"), "quay.io/b/z@sha256:f");
        assert_eq!(pinned("x@sha256:e", "docker.io/library/x@sha256:e"), "x@sha256:e");
        // No digest to pin to (never so for a pinned image): the source as recorded.
        assert_eq!(pinned("x:1", "docker.io/library/x:1"), "docker.io/library/x:1");
    }

    #[test]
    fn the_event_s_types_convert_field_for_field() {
        let runtime = core::RuntimeUse {
            engine_version: "9.1.0".into(),
            runtime_key: "k".into(),
            coordinator_image: "repo@sha256:c".into(),
        };
        assert_eq!(
            RuntimeUse::from(&runtime),
            RuntimeUse {
                engine_version: "9.1.0".into(),
                runtime_key: "k".into(),
                coordinator_image: "repo@sha256:c".into(),
            }
        );
        let uris = std::collections::BTreeMap::from([("fasta".into(), "s3://b/g.fa".into())]);
        let used = core::ReferenceUse { id: "hg38".into(), uris: uris.clone() };
        assert_eq!(ReferenceUse::from(used), ReferenceUse { id: "hg38".into(), uris });
        let image = core::SubmitImage {
            declared_ref: "ubuntu:24.04".into(),
            source_ref: "repo@sha256:a".into(),
            content_key: "ck".into(),
        };
        assert_eq!(
            SubmittedImage::from(&image),
            SubmittedImage {
                declared_ref: "ubuntu:24.04".into(),
                source_ref: "repo@sha256:a".into(),
            }
        );
    }
}
