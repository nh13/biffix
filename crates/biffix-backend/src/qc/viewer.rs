//! What the QC viewer shows of one output a run's caller may be served: the pipeline's own FastQC
//! result as data ([`fastqc_report`]), and what a genome browser needs to show an alignment
//! ([`alignment_view_of`]). Every S3 object they touch is derived server-side from the output,
//! from the run's other registered outputs, or from a reference bundle and checked against the
//! data sources: a client never names a key.

use std::collections::BTreeMap;

use biffix_api::{
    AlignmentLinks, AlignmentReference, AlignmentTrack, AnnotationTrack, FastqcReport, OutputUrl,
};
use biffix_core::event::ReferenceUse;
use biffix_workflow::glob::to_sql_like;
use biffix_workflow::text::{basename, parent};

use crate::aws::{objects::ObjectHead, AwsError};
use crate::error::ServiceError;
use crate::object_view::{presigned_link, Link, LinkError};
use crate::qc::fastqc::{self, FastqcError};
use crate::qc::locus::{self, IndexKind, LocusQuery};
use crate::runs::outputs::aws_error;
use crate::store::outputs::checkable;
use crate::store::outputs::{OutputRow, OutputScope};
use crate::store::RunDetailView;
use crate::AppState;

/// The largest `_fastqc.zip` read (a few hundred KB with its images; the data file is far less).
const MAX_FASTQC_ZIP_BYTES: u64 = 16 << 20;

/// Registered outputs looked at when finding a FASTQ's FastQC zip by name; a lookup that reaches
/// it cannot tell which is the FASTQ's, so it finds none.
const FASTQC_CANDIDATES: i64 = 50;

/// The largest annotation loaded whole when it has no index.
const MAX_UNINDEXED_ANNOTATION_BYTES: i64 = 16 << 20;

/// The registered, servable `_fastqc.zip` that holds `row`'s FastQC data, if any: `row` itself, the
/// zip beside a FastQC HTML report, or a FASTQ's zip ([`fastq_zip`]). `bucket` is the run's
/// project bucket.
async fn fastqc_zip_for(
    state: &AppState,
    scope: &OutputScope,
    row: &OutputRow,
    bucket: &str,
) -> Result<Option<OutputRow>, ServiceError> {
    let name = basename(&row.s3_uri);
    if fastqc::is_zip(name) {
        return Ok(Some(row.clone()));
    }
    if let Some(zip) = fastqc::zip_beside_html(name) {
        return registered_sibling(state, scope, &row.s3_uri, &zip, bucket).await;
    }
    if !fastqc::is_fastq(name) {
        return Ok(None);
    }
    fastq_zip(state, scope, row, bucket).await
}

/// The zip FastQC wrote for the FASTQ `row`, found by the name FastQC gives it
/// ([`fastqc::zip_name_for`]) among the run's registered, servable outputs, only when it can be no
/// other FASTQ's: files of one name are often one per sample (`S1/reads_1.fastq.gz`,
/// `S2/reads_1.fastq.gz`), or one per step (`reads/S1.fq.gz` and `reads/trimmed/S1.fq.gz`), so a
/// zip of that name elsewhere may be another FASTQ's. In order:
///
/// 1. one under the FASTQ's own folder (beside it, or in a subfolder such as `fastqc/`) that no
///    nearer FASTQ of the same name owns (one in a subfolder the zip is in): the shallowest, when
///    no other is as shallow;
/// 2. else, anywhere in the run, the only zip of that name, when this FASTQ is the only output of
///    its own name (nothing else could own it).
///
/// Only for a FASTQ of a role the lookup counts ([`REPORT_ROLES`]): one of another role (a task's
/// intermediate) is not among the same-named FASTQs counted, so which zip is its own cannot be
/// told. Otherwise none (a 404, so the client falls back to what else it has). A lookup that
/// reaches [`FASTQC_CANDIDATES`] rows may not have seen every candidate, so it finds none either.
///
/// [`REPORT_ROLES`]: crate::store::outputs::REPORT_ROLES
async fn fastq_zip(
    state: &AppState,
    scope: &OutputScope,
    row: &OutputRow,
    bucket: &str,
) -> Result<Option<OutputRow>, ServiceError> {
    let name = basename(&row.s3_uri);
    if !crate::store::outputs::REPORT_ROLES.contains(&row.role.as_str()) {
        return Ok(None);
    }
    let zip = fastqc::zip_name_for(name);
    let named = |rows: &[OutputRow], want: &str| -> Vec<OutputRow> {
        // The SQL pattern is looser than the name (`**/` matches any prefix of it), so the name
        // is checked exactly.
        rows.iter()
            .filter(|r| basename(&r.s3_uri) == want && checkable(&r.s3_uri, bucket))
            .cloned()
            .collect()
    };
    let capped = |rows: &[OutputRow]| {
        let hit = i64::try_from(rows.len()).is_ok_and(|n| n >= FASTQC_CANDIDATES);
        if hit {
            tracing::warn!(
                run_id = scope.run_id().as_str(),
                fastq = row.s3_uri,
                "fastqc: too many outputs share the FASTQ's zip name to tell which is its own"
            );
        }
        hit
    };
    let dir = parent(&row.s3_uri);
    let found = state
        .store
        .outputs_like_in(
            scope,
            &[to_sql_like(dir, &format!("**/{zip}")), to_sql_like(dir, &format!("**/{name}"))],
            FASTQC_CANDIDATES,
        )
        .await
        .map_err(ServiceError::from)?;
    if capped(&found) {
        return Ok(None);
    }
    // The folders of the other FASTQs of this name under the FASTQ's own: a zip in one of them (or
    // below it) is that nearer FASTQ's.
    let nearer: Vec<&str> = found
        .iter()
        .filter(|r| basename(&r.s3_uri) == name && r.s3_uri != row.s3_uri)
        .map(|r| parent(&r.s3_uri))
        .filter(|f| f.len() > dir.len() && f.starts_with(dir))
        .collect();
    let under: Vec<OutputRow> = named(&found, &zip)
        .into_iter()
        .filter(|z| !nearer.iter().any(|f| z.s3_uri.starts_with(f)))
        .collect();
    if !under.is_empty() {
        let depth = |r: &OutputRow| {
            r.s3_uri.strip_prefix(dir).map_or(usize::MAX, |rest| rest.matches('/').count())
        };
        let shallowest = under.iter().map(depth).min().unwrap_or_default();
        let mut nearest = under.into_iter().filter(|r| depth(r) == shallowest);
        return Ok(match (nearest.next(), nearest.next()) {
            (Some(only), None) => Some(only),
            _ => None,
        });
    }
    let anywhere = state
        .store
        .outputs_like_in(
            scope,
            &[to_sql_like("", &format!("**/{zip}")), to_sql_like("", &format!("**/{name}"))],
            FASTQC_CANDIDATES,
        )
        .await
        .map_err(ServiceError::from)?;
    if capped(&anywhere) {
        return Ok(None);
    }
    let zips = named(&anywhere, &zip);
    // Every output of this name is this FASTQ itself.
    let only_this = {
        let mut same = anywhere.iter().filter(|r| basename(&r.s3_uri) == name);
        matches!((same.next(), same.next()), (Some(r), None) if r.s3_uri == row.s3_uri)
    };
    Ok(match zips.as_slice() {
        [only] if only_this => Some(only.clone()),
        _ => None,
    })
}

/// The output named `name` in the folder of `uri`, when it is one of `scope`'s (the run's own, or
/// one it reused from a run it resumed) and biffix serves it (from the run's project `bucket`).
async fn registered_sibling(
    state: &AppState,
    scope: &OutputScope,
    uri: &str,
    name: &str,
    bucket: &str,
) -> Result<Option<OutputRow>, ServiceError> {
    let sibling = format!("{}{name}", parent(uri));
    if !checkable(&sibling, bucket) {
        return Ok(None);
    }
    state.store.output_at_in(scope, &sibling).await.map_err(ServiceError::from)
}

/// A stream link ([`Link::Stream`]) to `uri`.
async fn stream_link(state: &AppState, uri: &str) -> Result<OutputUrl, ServiceError> {
    presigned_link(state.cloud.as_ref(), uri, Link::Stream).await.map_err(|e| match e {
        LinkError::NoViewer(m) => ServiceError::Conflict(m),
        LinkError::Aws(e) => aws_error("presigning a viewer link", e),
    })
}

/// The index of the alignment at `uri`, and its kind: a registered output beside it (`x.bam.bai`,
/// `x.bai`, `x.bam.csi`; `x.cram.crai`, `x.crai`), else one of those names that S3 holds, the first
/// in that order either way. Every candidate is derived from `uri` here and is in the same
/// (servable from the run's project `bucket`) folder; the lookups (and then the HEADs) run
/// together.
async fn alignment_index(
    state: &AppState,
    scope: &OutputScope,
    uri: &str,
    cram: bool,
    bucket: &str,
) -> Result<Option<(String, IndexKind)>, ServiceError> {
    let stem = &uri[..uri.rfind('.').unwrap_or(uri.len())];
    let candidates: Vec<(String, IndexKind)> = if cram {
        vec![(format!("{uri}.crai"), IndexKind::Crai), (format!("{stem}.crai"), IndexKind::Crai)]
    } else {
        vec![
            (format!("{uri}.bai"), IndexKind::Bai),
            (format!("{stem}.bai"), IndexKind::Bai),
            (format!("{uri}.csi"), IndexKind::Csi),
        ]
    };
    let candidates: Vec<(String, IndexKind)> =
        candidates.into_iter().filter(|(c, _)| checkable(c, bucket)).collect();
    let registered = futures::future::try_join_all(
        candidates.iter().map(|(c, _)| state.store.output_at_in(scope, c)),
    )
    .await
    .map_err(ServiceError::from)?;
    if let Some(i) = registered.iter().position(Option::is_some) {
        return Ok(Some(candidates[i].clone()));
    }
    let held =
        futures::future::try_join_all(candidates.iter().map(|(c, _)| head(state, c))).await?;
    Ok(held.iter().position(Option::is_some).map(|i| candidates[i].clone()))
}

/// A HEAD of `uri`: `None` when absent.
async fn head(state: &AppState, uri: &str) -> Result<Option<ObjectHead>, ServiceError> {
    match state.cloud.head_object_meta(uri).await {
        Ok(head) => Ok(head),
        Err(AwsError::NotFound) => Ok(None),
        Err(e) => Err(aws_error("checking a viewer file", e)),
    }
}

/// Whether S3 holds `uri`, which a data source shows (else it is never looked for).
async fn shown_and_held<S: Fn(&str) -> bool>(
    state: &AppState,
    shown: &S,
    uri: &str,
) -> Result<bool, ServiceError> {
    Ok(shown(uri) && head(state, uri).await?.is_some())
}

/// The reference an alignment is shown against, as S3 URIs (presigned by the caller).
enum ResolvedReference {
    Bundle {
        bundle_id: String,
        display_name: String,
        fasta: String,
        fai: String,
        gzi: Option<String>,
        /// The annotation, its index, and its format.
        annotation: Option<(String, Option<String>, String)>,
    },
    IgvGenome {
        genome: String,
        bundle_id: String,
        display_name: String,
    },
}

/// A bundle file and the index that goes with it: one never comes from a different version of the
/// bundle than the other.
const COMPANIONS: [(&str, &str); 4] =
    [("fasta", "fasta_index"), ("gtf", "gtf_index"), ("gff3", "gff3_index"), ("gff", "gff_index")];

/// The bundle URIs to show a run's alignment against, from the bundle as it is now (`current`) and
/// what the run recorded of it (`recorded`, only when it is the same bundle), and whether the
/// bundle's own `igv_genome` may stand in for them.
///
/// The run's record holds only the keys its form filled (often just `fasta`, or just an index), so
/// it is laid over the current bundle, guarded both ways against pairing a file with an index of
/// another version ([`COMPANIONS`]):
///
/// - a recorded `fasta` the bundle no longer has, or a recorded `fasta_index` it no longer has
///   with no `fasta` beside it (the bundle now holds another genome): ONLY the recorded files are
///   used, and not the bundle's hosted genome either;
/// - another recorded file: the bundle's index for it is dropped, so the index is the one found
///   beside the recorded file;
/// - another recorded index, without its file: the bundle's file is another version's, so neither
///   is shown.
fn bundle_uris(
    current: Option<&BTreeMap<String, String>>,
    recorded: Option<&BTreeMap<String, String>>,
) -> (BTreeMap<String, String>, bool) {
    let Some(recorded) = recorded else {
        return (current.cloned().unwrap_or_default(), true);
    };
    let Some(current) = current else {
        return (recorded.clone(), false);
    };
    let changed = |key: &str| recorded.get(key).is_some_and(|uri| current.get(key) != Some(uri));
    if changed("fasta") || (changed("fasta_index") && !recorded.contains_key("fasta")) {
        return (recorded.clone(), false);
    }
    let mut uris = current.clone();
    let mut unpaired = Vec::new();
    for (file, index) in COMPANIONS {
        if changed(file) && !recorded.contains_key(index) {
            uris.remove(index);
        }
        if changed(index) && !recorded.contains_key(file) {
            uris.remove(file);
            uris.remove(index);
            unpaired.push(index);
        }
    }
    uris.extend(
        recorded
            .iter()
            .filter(|(k, _)| !unpaired.contains(&k.as_str()))
            .map(|(k, v)| (k.clone(), v.clone())),
    );
    (uris, true)
}

/// The reference to show the run's alignments against: the bundle `requested`, else the one the run
/// was launched with. A run's own bundle uses the URIs it recorded (what it ran with) with the
/// bundle's current ones as [`bundle_uris`] pairs them, so an edited or deleted bundle still shows
/// the genome the run used. Every URI must be a file a data source shows
/// ([`crate::config::data_sources::DataSource::shows`]); one that is not is left out, with a note.
/// `None` (with a note, when something was missing) when there is no usable reference.
async fn resolve_reference(
    state: &AppState,
    projects: &crate::config::projects::Projects,
    detail: &RunDetailView,
    requested: Option<&str>,
    notes: &mut Vec<String>,
) -> Result<Option<ResolvedReference>, ServiceError> {
    let recorded: Option<ReferenceUse> =
        detail.reference_bundle.clone().and_then(|v| match serde_json::from_value(v) {
            Ok(recorded) => Some(recorded),
            // Read as if the run recorded none, but say so: it did record one.
            Err(e) => {
                tracing::warn!(
                    run_id = detail.run.run_id.as_str(),
                    error = %e,
                    "alignment view: the run's recorded reference bundle does not decode"
                );
                None
            }
        });
    let requested = requested.map(str::trim).filter(|r| !r.is_empty());
    let Some(bundle_id) =
        requested.map(str::to_string).or_else(|| recorded.as_ref().map(|r| r.id.clone()))
    else {
        return Ok(None);
    };
    let current = crate::store::found(state.references.get_bundle(&bundle_id).await)?;
    if current.is_none() && requested.is_some() {
        return Err(ServiceError::NotFound(format!("no reference bundle {bundle_id}")));
    }
    let own = recorded.as_ref().filter(|r| r.id == bundle_id);
    let (uris, current_genome) =
        bundle_uris(current.as_ref().map(|b| &b.uris), own.map(|r| &r.uris));
    if current.is_some() && !current_genome {
        notes.push(format!(
            "reference bundle {bundle_id} has changed since this run; showing the files the run \
             used"
        ));
    }
    let display_name =
        current.as_ref().map_or_else(|| bundle_id.clone(), |b| b.display_name.clone());
    if let Some(genome) =
        current.as_ref().and_then(|b| b.igv_genome.clone()).filter(|_| current_genome)
    {
        return Ok(Some(ResolvedReference::IgvGenome { genome, bundle_id, display_name }));
    }
    let sources = projects.for_run(detail).sources();
    let shown = |uri: &str| sources.iter().any(|s| s.shows(uri));
    let pick = |key: &str, notes: &mut Vec<String>| -> Option<String> {
        let uri = uris.get(key)?;
        if shown(uri) {
            Some(uri.clone())
        } else {
            notes.push(format!(
                "the bundle's {key} ({uri}) is not in a location biffix serves; left out"
            ));
            None
        }
    };
    let Some(fasta) = pick("fasta", notes) else {
        notes.push(format!("reference bundle {bundle_id} has no fasta the viewer can read"));
        return Ok(None);
    };
    let fai_named = pick("fasta_index", notes);
    // The index named, else the one beside the FASTA; the .gzi beside a bgzipped FASTA; and the
    // annotation: independent lookups, made together.
    let fai_beside = format!("{fasta}.fai");
    let gzi_beside = format!("{fasta}.gzi");
    let bgzipped = fasta.to_ascii_lowercase().ends_with(".gz");
    let mut annotation_notes = Vec::new();
    let (fai_held, gzi_held, annotation) = tokio::join!(
        async {
            match fai_named {
                Some(_) => Ok(false),
                None => shown_and_held(state, &shown, &fai_beside).await,
            }
        },
        async {
            match bgzipped {
                true => shown_and_held(state, &shown, &gzi_beside).await,
                false => Ok(false),
            }
        },
        annotation(state, &pick, &shown, &mut annotation_notes)
    );
    let fai = match fai_named {
        Some(fai) => Some(fai),
        None => fai_held?.then_some(fai_beside),
    };
    let Some(fai) = fai else {
        notes.push(format!(
            "reference bundle {bundle_id}'s FASTA has no .fai index; the viewer needs one"
        ));
        return Ok(None);
    };
    let gzi = gzi_held?.then_some(gzi_beside);
    if bgzipped && gzi.is_none() {
        notes.push(
            "the bgzipped FASTA has no .gzi index beside it; the viewer may not load it"
                .to_string(),
        );
    }
    let annotation = annotation?;
    notes.extend(annotation_notes);
    Ok(Some(ResolvedReference::Bundle { bundle_id, display_name, fasta, fai, gzi, annotation }))
}

/// The bundle's gene annotation (`gtf`, else `gff3`/`gff`): with its tabix index (`gtf_index`, else
/// a `.tbi`/`.csi` beside it) when it has one, else whole when it is small enough to load, else
/// left out with a note.
async fn annotation<P, S>(
    state: &AppState,
    pick: &P,
    shown: &S,
    notes: &mut Vec<String>,
) -> Result<Option<(String, Option<String>, String)>, ServiceError>
where
    P: Fn(&str, &mut Vec<String>) -> Option<String>,
    S: Fn(&str) -> bool,
{
    // The format is the bundle key's that named the file, not a guess from its name (a GFF3 under
    // a path holding `.gtf` is still GFF3).
    let Some((uri, index_key, format)) =
        [("gtf", "gtf"), ("gff3", "gff3"), ("gff", "gff3")].iter().find_map(|(k, format)| {
            pick(k, notes).map(|uri| (uri, format!("{k}_index"), format.to_string()))
        })
    else {
        return Ok(None);
    };
    let lower = uri.to_ascii_lowercase();
    let mut index = pick(&index_key, notes);
    if index.is_none() {
        for ext in ["tbi", "csi"] {
            let beside = format!("{uri}.{ext}");
            if shown_and_held(state, shown, &beside).await? {
                index = Some(beside);
                break;
            }
        }
    }
    if index.is_some() {
        return Ok(Some((uri, index, format)));
    }
    let small = !lower.ends_with(".gz")
        && head(state, &uri)
            .await?
            .and_then(|h| h.size)
            .is_some_and(|size| size <= MAX_UNINDEXED_ANNOTATION_BYTES);
    if small {
        return Ok(Some((uri, None, format)));
    }
    notes.push(if lower.ends_with(".gz") {
        // Compressed, whatever its size: the browser reads it only through an index.
        "the gene annotation is compressed without a tabix index (bgzip it and add a .tbi); not \
         shown"
            .to_string()
    } else {
        "the gene annotation is too large to load without a tabix index (bgzip it and add a \
         .tbi); not shown"
            .to_string()
    });
    Ok(None)
}

/// The pipeline's own FastQC result for `row`, an output of `detail` the caller may be served, as
/// data (`GET /runs/{id}/outputs/{output_id}/fastqc`): from the zip [`fastqc_zip_for`] finds. 404
/// when there is none; 422 when the zip is too large or not FastQC's.
pub(crate) async fn fastqc_report(
    state: &AppState,
    detail: &RunDetailView,
    scope: &OutputScope,
    row: &OutputRow,
) -> Result<FastqcReport, ServiceError> {
    // The run's project bucket: the only one its outputs are served from.
    let bucket = state.projects.current().await.for_run(detail).bucket.clone();
    let zip = fastqc_zip_for(state, scope, row, &bucket)
        .await?
        .ok_or(ServiceError::NotFound("no FastQC result for this output".to_string()))?;
    let bytes =
        crate::object_view::read_capped(state.cloud.as_ref(), &zip.s3_uri, MAX_FASTQC_ZIP_BYTES)
            .await
            .map_err(|e| aws_error("reading a FastQC zip", e))?
            .ok_or_else(|| {
                ServiceError::Unprocessable("the FastQC zip is too large to read".into())
            })?;
    let data = fastqc::from_zip(&bytes).map_err(|e| {
        let message = match e {
            FastqcError::NoData | FastqcError::NoModules => {
                format!("{} is not a FastQC result: {e}", basename(&zip.s3_uri))
            }
            e => e.to_string(),
        };
        ServiceError::Unprocessable(message)
    })?;
    if data.truncated {
        tracing::warn!(
            run_id = detail.run.run_id.as_str(),
            uri = zip.s3_uri,
            "fastqc: the data file holds more than is returned; later modules or rows left out"
        );
    }
    let html = match fastqc::html_beside_zip(basename(&zip.s3_uri)) {
        Some(name) => registered_sibling(state, scope, &zip.s3_uri, &name, &bucket).await?,
        None => None,
    };
    Ok(FastqcReport {
        output_id: zip.id,
        html_output_id: html.map(|r| r.id),
        version: data.version,
        modules: data.modules,
    })
}

/// What a genome browser needs to show `row`, a BAM or CRAM output of `detail` the caller may be
/// served (`GET /runs/{id}/outputs/{output_id}/alignment-view`): presigned links to it and its
/// index, the reference to show it against (the run's recorded bundle, or the bundle `reference`
/// names), and, when `find_locus`, where to open. 409 when it is not a BAM/CRAM or has no index.
pub(crate) async fn alignment_view_of(
    state: &AppState,
    detail: &RunDetailView,
    scope: &OutputScope,
    row: &OutputRow,
    reference: Option<&str>,
    find_locus: bool,
) -> Result<AlignmentLinks, ServiceError> {
    let projects = state.projects.current().await;
    let name = basename(&row.s3_uri).to_string();
    let lower = name.to_ascii_lowercase();
    let cram = if lower.ends_with(".bam") {
        false
    } else if lower.ends_with(".cram") {
        true
    } else {
        return Err(ServiceError::Conflict(format!("{name} is not a BAM or CRAM file")));
    };
    let bucket = &projects.for_run(detail).bucket;
    let (index_uri, index_kind) =
        alignment_index(state, scope, &row.s3_uri, cram, bucket).await?.ok_or_else(|| {
            ServiceError::Conflict(format!(
                "{name} has no index ({}) beside it; the viewer needs one",
                if cram { ".crai" } else { ".bai or .csi" }
            ))
        })?;
    let mut notes = Vec::new();
    // The locus needs only the alignment's own files, so it is read while the reference resolves.
    let find_locus = async {
        if !find_locus {
            return None;
        }
        let query =
            LocusQuery { alignment_uri: &row.s3_uri, cram, index_uri: &index_uri, index_kind };
        locus::find(state.cloud.as_ref(), &query).await
    };
    let (reference, locus) = tokio::join!(
        resolve_reference(state, &projects, detail, reference, &mut notes),
        find_locus
    );
    let reference = reference?;

    // Every link, then the view; the buckets they point into decide `bucket_cors`.
    let mut buckets: Vec<String> = Vec::new();
    let mut link = |uri: String| {
        if let Some((bucket, _)) = biffix_core::s3_layout::split_s3_uri(&uri) {
            buckets.push(bucket.to_string());
        }
        let state = state.clone();
        async move { stream_link(&state, &uri).await }
    };
    let alignment_link = link(row.s3_uri.clone()).await?;
    let alignment = AlignmentTrack {
        name,
        format: if cram { "cram" } else { "bam" }.into(),
        url: alignment_link.url,
        index_url: link(index_uri.clone()).await?.url,
    };
    let reference = match reference {
        None => None,
        Some(ResolvedReference::IgvGenome { genome, bundle_id, display_name }) => {
            Some(AlignmentReference::IgvGenome {
                genome,
                bundle_id: Some(bundle_id),
                display_name: Some(display_name),
            })
        }
        Some(ResolvedReference::Bundle {
            bundle_id,
            display_name,
            fasta,
            fai,
            gzi,
            annotation,
        }) => {
            let fasta_url = link(fasta).await?.url;
            let fai_url = link(fai).await?.url;
            let gzi_url = match gzi {
                Some(uri) => Some(link(uri).await?.url),
                None => None,
            };
            let annotation = match annotation {
                Some((uri, index, format)) => Some(AnnotationTrack {
                    url: link(uri).await?.url,
                    index_url: match index {
                        Some(uri) => Some(link(uri).await?.url),
                        None => None,
                    },
                    format,
                }),
                None => None,
            };
            Some(AlignmentReference::Bundle {
                bundle_id,
                display_name,
                fasta_url,
                fai_url,
                gzi_url,
                annotation,
            })
        }
    };
    let bucket_cors = if buckets.iter().all(|b| projects.bucket_allows_viewer(b)) {
        "configured"
    } else {
        "unknown"
    };
    Ok(AlignmentLinks {
        alignment,
        reference,
        locus,
        expires_at: alignment_link.expires_at,
        bucket_cors: bucket_cors.into(),
        notes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn a_run_s_record_is_laid_over_the_bundle_without_mixing_versions() {
        let current = map(&[
            ("fasta", "v2/genome.fa"),
            ("fasta_index", "v2/genome.fa.fai"),
            ("gtf", "v2/genes.gtf.gz"),
            ("gtf_index", "v2/genes.gtf.gz.tbi"),
        ]);
        // The same FASTA: the bundle fills in what the run did not record.
        let (uris, genome) = bundle_uris(Some(&current), Some(&map(&[("fasta", "v2/genome.fa")])));
        assert_eq!(uris, current);
        assert!(genome);
        // Another FASTA: only what the run recorded (its .fai is then found beside it), and not the
        // bundle's hosted genome.
        let (uris, genome) = bundle_uris(Some(&current), Some(&map(&[("fasta", "v1/genome.fa")])));
        assert_eq!(uris, map(&[("fasta", "v1/genome.fa")]));
        assert!(!genome);
        // Another annotation with the same FASTA: the bundle's index for it is not taken.
        let (uris, _) = bundle_uris(
            Some(&current),
            Some(&map(&[("fasta", "v2/genome.fa"), ("gtf", "v1/genes.gtf.gz")])),
        );
        assert_eq!(uris.get("gtf").map(String::as_str), Some("v1/genes.gtf.gz"));
        assert!(!uris.contains_key("gtf_index"));
        assert_eq!(uris.get("fasta_index").map(String::as_str), Some("v2/genome.fa.fai"));
        // A record without a FASTA (a form that filled only the annotation) keeps the bundle's.
        let (uris, genome) = bundle_uris(Some(&current), Some(&map(&[("gtf", "v2/genes.gtf.gz")])));
        assert_eq!(uris, current);
        assert!(genome);
        // A record of only the FASTA's index, another than the bundle's: the bundle holds another
        // genome, so only the record (the run's index is never laid over the bundle's new FASTA).
        let recorded = map(&[("fasta_index", "v1/genome.fa.fai")]);
        assert_eq!(bundle_uris(Some(&current), Some(&recorded)), (recorded, false));
        // The same FASTA's index again, beside the FASTA it records: the run's pair.
        let (uris, genome) = bundle_uris(
            Some(&current),
            Some(&map(&[("fasta", "v2/genome.fa"), ("fasta_index", "v2/genome.fa.fai.v2")])),
        );
        assert_eq!(uris.get("fasta_index").map(String::as_str), Some("v2/genome.fa.fai.v2"));
        assert!(genome);
        // Another annotation index, without its annotation: neither the bundle's annotation nor
        // the run's index is shown; the genome is.
        let (uris, genome) =
            bundle_uris(Some(&current), Some(&map(&[("gtf_index", "v1/genes.gtf.gz.tbi")])));
        assert_eq!(uris, map(&[("fasta", "v2/genome.fa"), ("fasta_index", "v2/genome.fa.fai")]));
        assert!(genome);
        // No record: the bundle as it is; a deleted bundle: the record.
        assert_eq!(bundle_uris(Some(&current), None), (current.clone(), true));
        let recorded = map(&[("fasta", "v1/genome.fa")]);
        assert_eq!(bundle_uris(None, Some(&recorded)), (recorded, false));
    }
}
