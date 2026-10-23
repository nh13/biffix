//! Reference bundles: named sets of reference-file URIs that a launch form's reference dropdown
//! fills params from. biffix stores only the names and URIs; it never copies or indexes the files.

use std::collections::{BTreeMap, BTreeSet};

use async_trait::async_trait;
use biffix_api::{ReferenceBundle, ReferenceBundleRequest};
use biffix_core::event::ReferenceUse;
use biffix_workflow::formschema::FormReferences;

use crate::config::data_sources::DataSource;
use crate::store::StoreError;

/// The bundle kinds a form can ask for.
pub(crate) const BUNDLE_KINDS: [&str; 2] = ["assembly", "assay_kit"];

/// Read and write reference bundles.
#[async_trait]
pub trait ReferenceStore: Send + Sync {
    /// Every bundle, by id.
    async fn list_bundles(&self) -> Result<Vec<ReferenceBundle>, StoreError>;
    async fn get_bundle(&self, id: &str) -> Result<ReferenceBundle, StoreError>;
    /// Add a new bundle; `Conflict` when the id is taken.
    async fn create_bundle(&self, bundle: &ReferenceBundle) -> Result<(), StoreError>;
    /// Replace an existing bundle's contents, and return the stored bundle. The store keeps the
    /// creation stamp (`created_at_micros`, `created_by`) whatever `bundle` carries: an edit never
    /// changes who made a bundle or when. `NotFound` when absent.
    async fn update_bundle(&self, bundle: &ReferenceBundle) -> Result<ReferenceBundle, StoreError>;
    /// `NotFound` when absent.
    async fn delete_bundle(&self, id: &str) -> Result<(), StoreError>;
}

/// The most URIs one bundle names (a genome's files and indexes are a handful).
const MAX_BUNDLE_URIS: usize = 64;
/// The longest a bundle's URI may be (S3 keys are at most 1024 bytes, plus the bucket).
const MAX_BUNDLE_URI_BYTES: usize = 1100;
/// The longest a bundle's notes may be.
const MAX_BUNDLE_NOTES: usize = 4000;

/// Check a bundle write: a short lowercase id, a known kind, a name, and at least one URI, each an
/// `s3://` URI inside a source biffix can read (the project bucket or a registered data source) —
/// a bundle elsewhere would name files no run can read. Keys are lowercase words (`fasta_index`).
pub(crate) fn validate_request(
    req: &ReferenceBundleRequest,
    sources: &[DataSource],
) -> Result<(), String> {
    let id_ok = !req.id.is_empty()
        && req.id.len() <= 64
        && req.id.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
        && req
            .id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_' | '.'));
    if !id_ok {
        return Err(format!(
            "id `{}` must be 1–64 lowercase letters, digits, `-`, `_` or `.`, starting with a letter \
             or digit",
            req.id
        ));
    }
    if !BUNDLE_KINDS.contains(&req.kind.as_str()) {
        return Err(format!("kind must be one of {}", BUNDLE_KINDS.join(", ")));
    }
    let name = req.display_name.trim();
    if name.is_empty() || name.chars().count() > 200 {
        return Err("display_name must be 1–200 characters".into());
    }
    if req.uris.is_empty() {
        return Err("a bundle needs at least one URI".into());
    }
    // What every viewer's launch form downloads (`GET /references`), so each part is bounded.
    if req.uris.len() > MAX_BUNDLE_URIS {
        return Err(format!(
            "a bundle names at most {MAX_BUNDLE_URIS} URIs ({} given)",
            req.uris.len()
        ));
    }
    if req.notes.as_deref().is_some_and(|n| n.chars().count() > MAX_BUNDLE_NOTES) {
        return Err(format!("notes must be at most {MAX_BUNDLE_NOTES} characters"));
    }
    if let Some(genome) = req.igv_genome.as_deref().map(str::trim) {
        if !genome.is_empty() && !is_igv_genome_id(genome) {
            return Err(format!(
                "igv_genome `{genome}` must be 1–32 letters, digits, `.`, `_` or `-` (a genome id \
                 such as hg38 or mm39)"
            ));
        }
    }
    for (key, uri) in &req.uris {
        let key_ok = !key.is_empty()
            && key.len() <= 64
            && key.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
        if !key_ok {
            return Err(format!("key `{key}` must be 1–64 lowercase letters, digits and `_`"));
        }
        if uri.len() > MAX_BUNDLE_URI_BYTES {
            return Err(format!("{key}: the URI is longer than {MAX_BUNDLE_URI_BYTES} bytes"));
        }
        let Some((bucket, object_key)) =
            biffix_core::s3_layout::split_s3_uri(uri).filter(|(_, k)| !k.is_empty())
        else {
            return Err(format!("{key}: `{uri}` is not an s3://bucket/key URI"));
        };
        if !sources.iter().any(|s| s.holds(uri)) {
            return Err(format!(
                "{key}: `{uri}` is not in the project bucket or a registered data source"
            ));
        }
        // A bundle may name a folder (an index directory), so this is `holds` plus the
        // hidden-prefix rule rather than `shows` (objects only): biffix's own run and source areas
        // are never a reference.
        let hidden = sources.iter().any(|s| {
            bucket == s.bucket && s.path_of(object_key).is_some_and(|path| s.reserves(path))
        });
        if hidden {
            return Err(format!(
                "{key}: `{uri}` is under a prefix biffix reserves for its own files"
            ));
        }
    }
    Ok(())
}

/// Whether `id` can name a hosted genome (`hg38`, `mm39`, `GCF_000001405.40`): 1–32 ASCII letters,
/// digits, `.`, `_` or `-`. The id goes into the viewer's configuration, never a URL biffix builds.
pub(crate) fn is_igv_genome_id(id: &str) -> bool {
    (1..=32).contains(&id.len())
        && id.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// What a submit's chosen bundle fills: param → URI, and the record for the run's anchor.
#[derive(Debug, PartialEq)]
pub(crate) struct Resolved {
    pub(crate) params: BTreeMap<String, String>,
    pub(crate) used: ReferenceUse,
}

/// Resolve `bundle` against a form's reference dropdown. Refused when the bundle is of another kind
/// or lacks a key the form requires; a `fill` key the bundle lacks (and the form does not require)
/// leaves that param as given.
pub(crate) fn resolve(bundle: &ReferenceBundle, refs: &FormReferences) -> Result<Resolved, String> {
    if let Some(kind) = &refs.kind {
        if &bundle.kind != kind {
            return Err(format!(
                "reference bundle {} is of kind {}; this form needs {kind}",
                bundle.id, bundle.kind
            ));
        }
    }
    let missing: Vec<&str> = refs
        .required
        .iter()
        .filter(|k| !bundle.uris.contains_key(*k))
        .map(String::as_str)
        .collect();
    if !missing.is_empty() {
        return Err(format!("reference bundle {} has no {}", bundle.id, missing.join(", ")));
    }
    let mut params = BTreeMap::new();
    let mut uris = BTreeMap::new();
    for (param, key) in &refs.fill {
        if let Some(uri) = bundle.uris.get(key) {
            params.insert(param.clone(), uri.clone());
            uris.insert(key.clone(), uri.clone());
        }
    }
    Ok(Resolved { params, used: ReferenceUse { id: bundle.id.clone(), uris } })
}

/// Bundles that have every one of `keys` (and are of `kind`, when given), for `GET /references`.
pub(crate) fn matching<'a>(
    bundles: &'a [ReferenceBundle],
    kind: Option<&str>,
    keys: &BTreeSet<String>,
) -> Vec<&'a ReferenceBundle> {
    bundles
        .iter()
        .filter(|b| kind.is_none_or(|k| b.kind == k))
        .filter(|b| keys.iter().all(|k| b.uris.contains_key(k)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sources() -> Vec<DataSource> {
        crate::config::data_sources::all_sources(
            "proj",
            &crate::config::data_sources::parse_sources(
                r#"[{"id":"refs","name":"Refs","bucket":"shared","prefix":"references","kind":"references"}]"#,
            )
            .unwrap(),
        )
    }

    #[test]
    fn a_bundle_s_uris_and_notes_are_bounded() {
        let many: Vec<(String, String)> =
            (0..=MAX_BUNDLE_URIS).map(|i| (format!("k{i}"), format!("s3://proj/f{i}"))).collect();
        let many: Vec<(&str, &str)> = many.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        assert_eq!(
            validate_request(&request(&many), &sources()),
            Err("a bundle names at most 64 URIs (65 given)".to_string())
        );
        let long = format!("s3://proj/{}", "a".repeat(MAX_BUNDLE_URI_BYTES));
        assert_eq!(
            validate_request(&request(&[("fasta", &long)]), &sources()),
            Err("fasta: the URI is longer than 1100 bytes".to_string())
        );
        let noted = ReferenceBundleRequest {
            notes: Some("n".repeat(MAX_BUNDLE_NOTES + 1)),
            ..request(&[("fasta", "s3://proj/a.fa")])
        };
        assert_eq!(
            validate_request(&noted, &sources()),
            Err("notes must be at most 4000 characters".to_string())
        );
    }

    fn request(uris: &[(&str, &str)]) -> ReferenceBundleRequest {
        ReferenceBundleRequest {
            id: "grch38".into(),
            kind: "assembly".into(),
            display_name: "Human GRCh38".into(),
            uris: uris.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
            notes: None,
            igv_genome: None,
        }
    }

    fn bundle(kind: &str, uris: &[(&str, &str)]) -> ReferenceBundle {
        let r = request(uris);
        ReferenceBundle {
            id: r.id,
            kind: kind.into(),
            display_name: r.display_name,
            uris: r.uris,
            notes: None,
            igv_genome: None,
            created_at_micros: 0,
            updated_at_micros: 0,
            created_by: None,
        }
    }

    #[test]
    fn a_bundle_must_point_inside_a_readable_source() {
        let ok = request(&[
            ("fasta", "s3://shared/references/hg38/genome.fa"),
            ("star_index", "s3://proj/indexes/star/"),
        ]);
        assert_eq!(validate_request(&ok, &sources()), Ok(()));
        let outside = request(&[("fasta", "s3://shared/other/genome.fa")]);
        assert!(validate_request(&outside, &sources())
            .unwrap_err()
            .contains("not in the project bucket"));
        // Refused as malformed, not as outside the sources (it is that too).
        let malformed = request(&[("fasta", "genome.fa")]);
        assert_eq!(
            validate_request(&malformed, &sources()),
            Err("fasta: `genome.fa` is not an s3://bucket/key URI".to_string())
        );
        for reserved in [
            "s3://proj/biffix/runs/r1/genome.fa",
            "s3://proj/workflow-sources/abc.tar.gz",
            // A reserved folder named without its trailing slash.
            "s3://proj/biffix",
            "s3://proj/workflow-sources",
        ] {
            let hidden = request(&[("fasta", reserved)]);
            assert!(
                validate_request(&hidden, &sources()).unwrap_err().contains("reserves"),
                "{reserved}"
            );
        }
    }

    #[test]
    fn a_hosted_genome_id_is_a_short_plain_word() {
        for ok in ["hg38", "mm39", "GCF_000001405.40", "t2t-chm13"] {
            assert!(is_igv_genome_id(ok), "{ok}");
            let req = ReferenceBundleRequest {
                igv_genome: Some(ok.into()),
                ..request(&[("fasta", "s3://proj/a.fa")])
            };
            assert_eq!(validate_request(&req, &sources()), Ok(()));
        }
        for bad in ["", "hg 38", "https://x", "a/b", "é"] {
            assert!(!is_igv_genome_id(bad), "{bad}");
        }
        // Blank is no genome, not an error.
        let blank = ReferenceBundleRequest {
            igv_genome: Some("  ".into()),
            ..request(&[("fasta", "s3://proj/a.fa")])
        };
        assert_eq!(validate_request(&blank, &sources()), Ok(()));
    }

    #[test]
    fn ids_kinds_names_and_keys_are_checked() {
        let good = || request(&[("fasta", "s3://proj/a.fa")]);
        for (bad, what) in [
            (
                ReferenceBundleRequest { id: "Bad Id".into(), ..good() },
                // Not just "id": the igv_genome message says "a genome id" too.
                "id `Bad Id`",
            ),
            (ReferenceBundleRequest { kind: "genome".into(), ..good() }, "kind"),
            (ReferenceBundleRequest { display_name: " ".into(), ..good() }, "display_name"),
            (request(&[]), "at least one"),
            (request(&[("Fasta", "s3://proj/a.fa")]), "key"),
            (
                ReferenceBundleRequest { igv_genome: Some("hg38/../x".into()), ..good() },
                "igv_genome",
            ),
            (ReferenceBundleRequest { igv_genome: Some("a".repeat(33)), ..good() }, "igv_genome"),
        ] {
            let err = validate_request(&bad, &sources()).unwrap_err();
            assert!(err.contains(what), "{what}: {err}");
        }
    }

    fn form_refs(required: &[&str]) -> FormReferences {
        FormReferences {
            kind: Some("assembly".into()),
            fill: [("ref_fasta", "fasta"), ("gtf", "gtf"), ("salmon_index", "salmon_index")]
                .iter()
                .map(|(p, k)| (p.to_string(), k.to_string()))
                .collect(),
            required: required.iter().map(std::string::ToString::to_string).collect(),
            label: "Reference genome".into(),
            section: None,
            custom: true,
            help: None,
        }
    }

    #[test]
    fn a_bundle_fills_the_params_it_has_keys_for() {
        let b = bundle(
            "assembly",
            &[("fasta", "s3://r/g.fa"), ("gtf", "s3://r/g.gtf"), ("fai", "s3://r/g.fa.fai")],
        );
        let r = resolve(&b, &form_refs(&["fasta", "gtf"])).unwrap();
        assert_eq!(
            r.params.into_iter().collect::<Vec<_>>(),
            [
                ("gtf".to_string(), "s3://r/g.gtf".to_string()),
                ("ref_fasta".to_string(), "s3://r/g.fa".to_string())
            ]
        );
        // The anchor keeps the keys used, not the whole bundle.
        assert_eq!(r.used.uris.len(), 2);
        assert_eq!(r.used.id, "grch38");
    }

    #[test]
    fn a_bundle_lacking_a_required_key_or_of_another_kind_is_refused() {
        let b = bundle("assembly", &[("fasta", "s3://r/g.fa")]);
        assert_eq!(
            resolve(&b, &form_refs(&["fasta", "gtf"])).unwrap_err(),
            "reference bundle grch38 has no gtf"
        );
        let kit = bundle("assay_kit", &[("fasta", "s3://r/g.fa")]);
        assert!(resolve(&kit, &form_refs(&["fasta"]))
            .unwrap_err()
            .contains("this form needs assembly"));
    }

    #[test]
    fn matching_filters_by_kind_and_keys() {
        let with_id = |id: &str, b: ReferenceBundle| ReferenceBundle { id: id.into(), ..b };
        let all = [
            with_id("a", bundle("assembly", &[("fasta", "s3://r/a"), ("gtf", "s3://r/b")])),
            with_id("b", bundle("assembly", &[("fasta", "s3://r/a")])),
            with_id("c", bundle("assay_kit", &[("fasta", "s3://r/a"), ("gtf", "s3://r/b")])),
        ];
        let keys: BTreeSet<String> = ["fasta".to_string(), "gtf".to_string()].into();
        // Which bundles, not only how many: an inverted kind filter would keep `c`.
        let ids = |kind: Option<&str>, keys: &BTreeSet<String>| {
            matching(&all, kind, keys).iter().map(|b| b.id.clone()).collect::<Vec<_>>()
        };
        assert_eq!(ids(Some("assembly"), &keys), ["a"]);
        assert_eq!(ids(Some("assay_kit"), &keys), ["c"]);
        assert_eq!(ids(None, &keys), ["a", "c"]);
        assert_eq!(ids(None, &BTreeSet::new()), ["a", "b", "c"]);
        assert_eq!(ids(Some("assembly"), &BTreeSet::new()), ["a", "b"]);
    }
}
