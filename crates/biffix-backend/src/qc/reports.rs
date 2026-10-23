//! A run's reports: the reports its workflow version declared (`reports:` in `biffix.yaml`, frozen
//! in the version's form), resolved against the outputs the run registered, and its MultiQC
//! report, found by name. Only registered outputs are ever offered, so a report
//! opens through the same output endpoints (and the same serving rules) as any other output.

use std::collections::HashSet;

use biffix_api::{
    FastqcStatusReport, FastqcStatusSample, QcGeneralStats, RunReport, RunReportList,
};
use biffix_core::ids::RunId;
use biffix_workflow::formschema::{report_format, FormReport, FormSchema, SAMPLE_PLACEHOLDER};
use biffix_workflow::glob;

use crate::aws::AwsError;
use crate::error::ServiceError;
use crate::qc::fastqc;
use crate::qc::multiqc::{
    fastqc_statuses, fastqc_table_section, general_stats, non_tsv_data_format, non_tsv_message,
    scan_headers, HeadersScan, DATA_JSON_FILE, FASTQC_DATA_FILE, GENERAL_STATS_FILE,
};
use crate::runs::outputs::{output_scope, output_view};
use crate::store::outputs::checkable;
use crate::store::outputs::{OutputRow, OutputScope};
use crate::store::{RunDetailView, StoreError};
use crate::AppState;
use biffix_workflow::text::{basename, parent};

/// Candidate outputs looked at per declaration, and for the MultiQC report: each has its own
/// budget, so a broad glob cannot crowd another declaration, or the MultiQC report, out. A glob
/// that matches more lists the first ones by URI.
const MAX_CANDIDATES: i64 = 500;
/// A per-sample declaration's budget per sample id (at least [`MAX_CANDIDATES`] in all): room for a
/// few files per sample when `{sample}` sits beside a glob (`fastqc/{sample}*.html`). Only when the
/// samples average more matches than this can late ones (by URI) go unseen, which is logged.
const PER_SAMPLE_CANDIDATES: i64 = 16;
/// Files one sample's per-sample report lists (a paired-end sample's R1 and R2, raw and trimmed,
/// say), in path order. A sample with more (a sample sequenced over several lanes) lists its first
/// ones, and the rest are logged: the list is what a person scans, not an inventory (every file is
/// still in the run's outputs).
const PER_SAMPLE_FILES: usize = 4;
/// MultiQC's report file name.
const MULTIQC_REPORT: &str = "multiqc_report.html";
/// The largest MultiQC table read (`multiqc_general_stats.txt`, `multiqc_fastqc.txt`: one row per
/// sample; far below this in practice).
const MULTIQC_TABLE_MAX_BYTES: u64 = 16 << 20;
/// The most FastQC tables read for one report (MultiQC writes one per FastQC section: raw and
/// trimmed reads, say); more are logged and left out.
const MAX_FASTQC_TABLES: usize = 16;
/// The most bytes of FastQC tables read for one report, its sections together.
const FASTQC_TABLES_MAX_BYTES: u64 = 2 * MULTIQC_TABLE_MAX_BYTES;
/// Listing pages read looking for them: MultiQC writes one `multiqc_*` file per module, far below
/// one page.
const DATA_LIST_PAGES: usize = 4;
const DATA_LIST_PAGE: i32 = 1000;
/// `multiqc_data.json` is read in growing chunks from its start until the general-statistics
/// headers are found (near the top in every MultiQC version), never past the cap. The cap is sized
/// to the 512 MB API Lambda: a read holds the accumulated prefix (at most the cap) plus the last
/// chunk twice over (the SDK's body and the provider's copy of it, each at most half the cap),
/// about twice the cap in all, so 64 MiB peaks near 128 MiB.
const DATA_JSON_FIRST_CHUNK: u64 = 4 << 20;
const DATA_JSON_MAX_BYTES: u64 = 64 << 20;

/// The run's reports: the ones its workflow version declared, resolved against its outputs, then
/// its MultiQC report and any FastQC results it did not declare. What `GET /runs/{id}/reports`
/// answers.
pub(crate) async fn run_reports(
    state: &AppState,
    detail: &RunDetailView,
) -> Result<RunReportList, ServiceError> {
    let rid = detail.run.run_id.clone();
    let projects = state.projects.current().await;
    let bucket = &projects.for_run(detail).bucket;
    let output_prefix = detail.output_prefix.clone();
    let declared = match &detail.run.workflow {
        Some(version) => match state.registry.get_version(version.as_str()).await {
            Ok(rec) => match serde_json::from_value::<FormSchema>(rec.version.form_schema) {
                Ok(form) => form.reports,
                Err(e) => {
                    tracing::warn!(
                        version = version.as_str(),
                        error = %e,
                        "list_reports: the version's form schema does not decode; no declared reports"
                    );
                    Vec::new()
                }
            },
            // A version this registry no longer holds declared nothing it can show.
            Err(StoreError::NotFound) => Vec::new(),
            Err(e) => return Err(ServiceError::status_only(e)),
        },
        None => Vec::new(),
    };
    let store_error = |e: StoreError| ServiceError::status_only(e);
    // The run's outputs include those it reused from a run it resumed, so a report the resume did
    // not write again is still the run's.
    let scope = output_scope(state, detail).await.map_err(store_error)?;
    // The lookups that need nothing but the run, together: FastQC results the workflow did not
    // declare (the zips, which the QC viewer reads as data; none without an output prefix to look
    // under), the MultiQC report, and the run's sample ids when a declaration is per-sample. (An
    // existence check would not make the FastQC lookup cheaper: on a run without FastQC it scans
    // the same rows to find none.)
    let per_sample_declared = declared.iter().any(FormReport::is_per_sample);
    let (fastqc_zips, multiqc, ids) = futures::try_join!(
        async {
            match output_prefix.as_deref() {
                Some(prefix) => fastqc_candidates(state, &scope, prefix).await,
                None => Ok(Vec::new()),
            }
        },
        multiqc_report(state, &scope),
        async {
            match per_sample_declared {
                true => state.store.run_sample_ids(&rid).await.map(Some),
                false => Ok(None),
            }
        },
    )
    .map_err(store_error)?;
    // The sample ids that can fill `{sample}`, also read when there are FastQC results to
    // attribute to samples.
    let ids = match ids {
        Some(ids) => ids,
        None if !fastqc_zips.is_empty() => {
            state.store.run_sample_ids(&rid).await.map_err(store_error)?
        }
        None => Vec::new(),
    };
    let sample_ids = fillable_ids(&ids);
    // One capped query per declaration (none without an output prefix to resolve them against). A
    // per-sample declaration asks for each sample's own path (one LIKE pattern per id), so other
    // files cannot crowd a sample out, and its budget grows with the samples.
    let candidates: Vec<Vec<OutputRow>> = match output_prefix.as_deref() {
        Some(prefix) => futures::future::try_join_all(declared.iter().map(|r| {
            let (patterns, limit) = if r.is_per_sample() {
                let patterns: Vec<String> = sample_ids
                    .iter()
                    .map(|id| glob::to_sql_like(prefix, &r.path.replace(SAMPLE_PLACEHOLDER, id)))
                    .collect();
                let limit = MAX_CANDIDATES.max(
                    i64::try_from(sample_ids.len())
                        .unwrap_or(i64::MAX)
                        .saturating_mul(PER_SAMPLE_CANDIDATES),
                );
                (patterns, limit)
            } else {
                (vec![glob::to_sql_like(prefix, &r.path)], MAX_CANDIDATES)
            };
            let rid = &rid;
            let scope = &scope;
            let store = &state.store;
            let name = r.name.as_str();
            async move {
                let rows = store.outputs_like_in(scope, &patterns, limit).await?;
                if i64::try_from(rows.len()).is_ok_and(|n| n >= limit) {
                    tracing::warn!(
                        run_id = rid.as_str(),
                        report = name,
                        limit,
                        "list_reports: a declaration's candidates reached the cap; later ones are not shown"
                    );
                }
                Ok::<_, StoreError>(rows)
            }
        }))
        .await
        .map_err(store_error)?,
        None => Vec::new(),
    };
    let mut resolved =
        resolve(&declared, output_prefix.as_deref(), &candidates, multiqc.as_ref(), &sample_ids);
    if let Some(prefix) = output_prefix.as_deref() {
        let used: HashSet<&str> = resolved.iter().map(|r| r.row.s3_uri.as_str()).collect();
        let detected = detect_fastqc(&fastqc_zips, prefix, &used, &sample_ids);
        resolved.extend(detected);
    }
    let reports = resolved
        .into_iter()
        .map(|r| RunReport {
            name: r.name,
            description: r.description,
            kind: r.kind.to_string(),
            output: output_view(r.row, &rid, output_prefix.as_deref(), bucket),
            format: r.format.to_string(),
            sample: r.sample,
            declaration: r.declaration,
        })
        .collect();
    Ok(RunReportList { reports })
}

/// The ids of `sample_ids` that can fill a per-sample path, each once, in order: an id holding a
/// glob character (`*`, `?`) is left out, since its path would match other samples' files.
fn fillable_ids(sample_ids: &[String]) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    sample_ids
        .iter()
        .filter(|id| !id.is_empty() && !id.contains(['*', '?']) && seen.insert(id.as_str()))
        .cloned()
        .collect()
}

/// The ids of `sample_ids` that can own a name (an empty id cannot), each once, in order. Unlike
/// [`fillable_ids`], an id holding `*` or `?` is kept: a name is matched to it literally, not as a
/// glob.
fn distinct_ids(sample_ids: &[String]) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    sample_ids.iter().filter(|id| !id.is_empty() && seen.insert(id.as_str())).cloned().collect()
}

/// The run's MultiQC report: the shallowest output named [`MULTIQC_REPORT`] (the top-level one
/// when a workflow also writes per-group reports). The one report both `/reports` (as its
/// `multiqc` entry) and `/qc/general-stats` use.
async fn multiqc_report(
    state: &AppState,
    scope: &OutputScope,
) -> Result<Option<OutputRow>, StoreError> {
    let pattern = glob::to_sql_like("", &format!("**/{MULTIQC_REPORT}"));
    let rows = state.store.outputs_like_in(scope, &[pattern], MAX_CANDIDATES).await?;
    if i64::try_from(rows.len()).is_ok_and(|n| n >= MAX_CANDIDATES) {
        // The rows come in URI order, so per-group reports can crowd the top-level one out.
        tracing::warn!(
            run_id = scope.run_id().as_str(),
            limit = MAX_CANDIDATES,
            "multiqc: the run's MultiQC reports reached the cap; the one shown may not be its \
             top-level report"
        );
    }
    Ok(shallowest_multiqc(rows, scope.runs()))
}

/// Of `rows` named exactly [`MULTIQC_REPORT`] (the SQL pattern is looser), the nearest run's in
/// `runs` (the run's own first, then the runs it resumed, nearest first), and of that run's the
/// shallowest. A resume that wrote its report somewhere else (another workflow version) shows its
/// own, not the stale one a run it resumed left at another path.
fn shallowest_multiqc(rows: Vec<OutputRow>, runs: &[RunId]) -> Option<OutputRow> {
    rows.into_iter().filter(|r| basename(&r.s3_uri) == MULTIQC_REPORT).min_by_key(|r| {
        (
            runs.iter().position(|run| *run == r.run_id).unwrap_or(usize::MAX),
            r.s3_uri.matches('/').count(),
            r.s3_uri.clone(),
        )
    })
}

/// The run's MultiQC General Statistics as data (`GET /runs/{id}/qc/general-stats`): the values
/// from `multiqc_data/multiqc_general_stats.txt` beside the report, with the columns' metadata from
/// `multiqc_data/multiqc_data.json` (plain column names when that is missing).
pub(crate) async fn general_stats_of(
    state: &AppState,
    detail: &RunDetailView,
) -> Result<QcGeneralStats, ServiceError> {
    let data = multiqc_data(state, detail).await?;
    let tsv = match data
        .read_table(
            state,
            GENERAL_STATS_FILE,
            "the General Statistics table is too large to show",
            "the MultiQC report has no General Statistics",
        )
        .await
    {
        // No TSV table: say so when MultiQC wrote the table in another format.
        Err(ServiceError::NotFound(missing)) => {
            return Err(ServiceError::NotFound(match data.general_stats_format(state).await {
                Some(format) => non_tsv_message(format, "the report's General Statistics"),
                None => missing,
            }))
        }
        read => read?,
    };
    let headers = read_headers(state, &format!("{}{DATA_JSON_FILE}", data.dir)).await;
    let (columns, samples) = general_stats(&tsv, &headers);
    Ok(QcGeneralStats { report_output_id: data.report.id, columns, samples })
}

/// Each FastQC result's module statuses and basic statistics as MultiQC recorded them, each
/// attributed to the run's sample that owns it ([`attribute_to_samples`]); what
/// `GET /runs/{id}/qc/fastqc-status` answers. Read from every FastQC table in `multiqc_data/` beside
/// the report (one per FastQC section, [`fastqc_table_section`]), each result tagged with its
/// section; 404 when the run has no MultiQC report, or the report no FastQC table.
pub(crate) async fn fastqc_status_of(
    state: &AppState,
    detail: &RunDetailView,
) -> Result<FastqcStatusReport, ServiceError> {
    let data = multiqc_data(state, detail).await?;
    let tables = data.fastqc_tables(state).await?;
    let ids = state.store.run_sample_ids(&detail.run.run_id).await.map_err(ServiceError::from)?;
    let ids = distinct_ids(&ids);
    let mut samples = Vec::new();
    for table in tables {
        let tsv = data
            .read_table(
                state,
                &table.name,
                "MultiQC's FastQC table is too large to show",
                "the MultiQC report has no FastQC results",
            )
            .await?;
        let results = fastqc_statuses(&tsv)
            .into_iter()
            .map(|result| FastqcStatusSample { section: table.section.clone(), ..result })
            .collect();
        samples.extend(attribute_to_samples(results, &ids));
    }
    Ok(FastqcStatusReport {
        source: "multiqc".to_string(),
        report_output_id: data.report.id,
        samples,
    })
}

/// `samples` each with the sample of `ids` that owns it, ordered by that sample's place in `ids`
/// (no owner last), then by MultiQC name. The owner is the one [`owning_sample`] finds for the
/// MultiQC name, else for the stem of the FASTQ FastQC read ([`fastq_stem`] of `filename`): MultiQC
/// may have renamed (`--replace-names`), prefixed (`-d`: `raw | S1_R1`) or cut (its `fn_clean_exts`
/// truncate at the first match) the name, while FastQC's own record of its input is untouched.
fn attribute_to_samples(
    samples: Vec<FastqcStatusSample>,
    ids: &[String],
) -> Vec<FastqcStatusSample> {
    let mut owned: Vec<(usize, FastqcStatusSample)> = samples
        .into_iter()
        .map(|mut s| {
            let owner = owning_sample(&s.fastqc_name, ids)
                .or_else(|| s.filename.as_deref().and_then(|f| owning_sample(fastq_stem(f), ids)));
            s.sample = owner.map(|i| ids[i].clone());
            (owner.unwrap_or(usize::MAX), s)
        })
        .collect();
    owned.sort_by(|(a, x), (b, y)| a.cmp(b).then_with(|| x.fastqc_name.cmp(&y.fastqc_name)));
    owned.into_iter().map(|(_, s)| s).collect()
}

/// A FASTQ's name without its folder and its sequence-file extensions (`raw/S1_R1.fastq.gz` →
/// `S1_R1`): a compression extension, then a reads-format one, as FastQC's inputs are named.
fn fastq_stem(filename: &str) -> &str {
    const COMPRESSED: [&str; 4] = [".gz", ".bz2", ".xz", ".zst"];
    const READS: [&str; 5] = [".fastq", ".fq", ".bam", ".sam", ".cram"];
    let name = basename(filename);
    let strip = |name: &'_ str, exts: &[&str]| -> usize {
        exts.iter()
            .find(|ext| {
                name.len() > ext.len()
                    && name
                        .get(name.len() - ext.len()..)
                        .is_some_and(|tail| tail.eq_ignore_ascii_case(ext))
            })
            .map_or(name.len(), |ext| name.len() - ext.len())
    };
    let name = &name[..strip(name, &COMPRESSED)];
    &name[..strip(name, &READS)]
}

/// The index of the sample of `ids` that owns a file `name`: the LONGEST id `name` is, or starts
/// with followed by a character that cannot continue an id (`S1_R1_fastqc.zip` and `S1_R1` are
/// S1's, `S10_R1` S10's); on a tie, the first. The rule FastQC results are attributed by, both the
/// zips `/reports` detects and MultiQC's names. It cannot tell a read suffix from an id: with ids
/// `S1` and `S1_1`, `S1_1` (S1's read 1, as nf-core names reads) is `S1_1`'s.
fn owning_sample(name: &str, ids: &[String]) -> Option<usize> {
    ids.iter()
        .enumerate()
        .filter(|(_, id)| {
            !id.is_empty()
                && name.strip_prefix(id.as_str()).is_some_and(|rest| {
                    rest.is_empty() || rest.starts_with(|c: char| !c.is_ascii_alphanumeric())
                })
        })
        .max_by(|(i, a), (j, b)| a.len().cmp(&b.len()).then(j.cmp(i)))
        .map(|(i, _)| i)
}

/// One FastQC table MultiQC wrote in `multiqc_data/`.
#[derive(Debug, PartialEq)]
struct FastqcTable {
    /// Its file name (`multiqc_fastqc_fastqc_raw.txt`).
    name: String,
    /// Its section's anchor; `None` for `multiqc_fastqc.txt` ([`fastqc_table_section`]).
    section: Option<String>,
    size: u64,
}

/// Of the file names (and sizes) of a `multiqc_data/` listing, the FastQC tables in the order they
/// are read (`multiqc_fastqc.txt` first, then by name), else the 404 message saying why there are
/// none (the data written in another format, when the names say so).
fn fastqc_tables_of(files: Vec<(String, u64)>) -> Result<Vec<FastqcTable>, String> {
    let mut format = None;
    let mut tables: Vec<FastqcTable> = files
        .into_iter()
        .filter_map(|(name, size)| {
            if let Some(section) = fastqc_table_section(&name) {
                let section = section.map(str::to_string);
                return Some(FastqcTable { name, section, size });
            }
            format = format.or(non_tsv_data_format(&name));
            None
        })
        .collect();
    if tables.is_empty() {
        return Err(match format {
            Some(format) => non_tsv_message(format, "the report's FastQC results"),
            None => "the MultiQC report has no FastQC results".to_string(),
        });
    }
    tables.sort_by(|a, b| (a.section.is_some(), &a.name).cmp(&(b.section.is_some(), &b.name)));
    Ok(tables)
}

/// A run's MultiQC report and the folder MultiQC wrote its data to, beside it.
struct MultiqcData {
    report: OutputRow,
    /// `<report's folder>/multiqc_data/`.
    dir: String,
    /// The run's project bucket: the only one its data is read from.
    bucket: String,
}

/// The MultiQC data of a run: its report (the one `/reports` lists with kind `multiqc`) and the
/// folder beside it. 404 when the run has no MultiQC report.
async fn multiqc_data(
    state: &AppState,
    detail: &RunDetailView,
) -> Result<MultiqcData, ServiceError> {
    let not_found = || ServiceError::NotFound("this run has no MultiQC report".to_string());
    let bucket = state.projects.current().await.for_run(detail).bucket.clone();
    let scope = output_scope(state, detail).await.map_err(ServiceError::from)?;
    let report =
        multiqc_report(state, &scope).await.map_err(ServiceError::from)?.ok_or_else(not_found)?;
    // MultiQC writes its data next to the report: `<dir>/multiqc_data/`.
    let dir = report
        .s3_uri
        .rsplit_once('/')
        .map(|(dir, _)| format!("{dir}/multiqc_data/"))
        .ok_or_else(not_found)?;
    Ok(MultiqcData { report, dir, bucket })
}

impl MultiqcData {
    /// The FastQC tables beside the report ([`fastqc_tables_of`]), found by listing `multiqc_data/`
    /// in the run's project bucket: up to [`MAX_FASTQC_TABLES`] (more are logged), 404 when there
    /// are none, 422 when together they pass [`FASTQC_TABLES_MAX_BYTES`].
    async fn fastqc_tables(&self, state: &AppState) -> Result<Vec<FastqcTable>, ServiceError> {
        let outside =
            || ServiceError::NotFound("the MultiQC data is outside the project bucket".to_string());
        if !checkable(&format!("{}{FASTQC_DATA_FILE}", self.dir), &self.bucket) {
            return Err(outside());
        }
        let (bucket, dir_key) =
            biffix_core::s3_layout::split_s3_uri(&self.dir).ok_or_else(outside)?;
        let prefix = format!("{dir_key}multiqc_");
        let mut files = Vec::new();
        let mut token: Option<String> = None;
        for _ in 0..DATA_LIST_PAGES {
            let page = state
                .cloud
                .list_objects(bucket, &prefix, true, token.as_deref(), DATA_LIST_PAGE)
                .await
                .map_err(ServiceError::from)?;
            files.extend(page.objects.into_iter().filter_map(|o| {
                let name = o.key.strip_prefix(dir_key)?.to_string();
                Some((name, u64::try_from(o.size).unwrap_or(0)))
            }));
            token = page.next_token;
            if token.is_none() {
                break;
            }
        }
        if token.is_some() {
            tracing::warn!(
                dir = %self.dir,
                pages = DATA_LIST_PAGES,
                "multiqc: the data folder's listing was cut short; later FastQC tables are not read"
            );
        }
        let mut tables = fastqc_tables_of(files).map_err(ServiceError::NotFound)?;
        if tables.len() > MAX_FASTQC_TABLES {
            tracing::warn!(
                dir = %self.dir,
                tables = tables.len(),
                "more FastQC tables than are read: the first {MAX_FASTQC_TABLES} are shown"
            );
            tables.truncate(MAX_FASTQC_TABLES);
        }
        if tables.iter().map(|t| t.size).sum::<u64>() > FASTQC_TABLES_MAX_BYTES {
            return Err(ServiceError::Unprocessable(
                "MultiQC's FastQC tables are too large to show".to_string(),
            ));
        }
        Ok(tables)
    }

    /// The format MultiQC wrote its General Statistics in when it is not TSV (a
    /// `multiqc_general_stats.{csv,json,yaml}` beside where the TSV would be); `None` when there is
    /// none, or the folder cannot be listed. Listed only in the run's project bucket, as
    /// [`Self::read_table`] reads.
    async fn general_stats_format(&self, state: &AppState) -> Option<&'static str> {
        if !checkable(&format!("{}{GENERAL_STATS_FILE}", self.dir), &self.bucket) {
            return None;
        }
        let (bucket, dir_key) = biffix_core::s3_layout::split_s3_uri(&self.dir)?;
        let stem = GENERAL_STATS_FILE.strip_suffix("txt")?;
        let page = state
            .cloud
            .list_objects(bucket, &format!("{dir_key}{stem}"), true, None, 10)
            .await
            .ok()?;
        page.objects.iter().find_map(|o| non_tsv_data_format(o.key.strip_prefix(dir_key)?))
    }

    /// The text of the data file `name` (a MultiQC table: one row per sample, far below the cap in
    /// practice), read only from the run's project bucket: 404 (`missing`) when it isn't there, 422
    /// (`too_large`) past [`MULTIQC_TABLE_MAX_BYTES`].
    async fn read_table(
        &self,
        state: &AppState,
        name: &str,
        too_large: &str,
        missing: &str,
    ) -> Result<String, ServiceError> {
        let uri = format!("{}{name}", self.dir);
        if !checkable(&uri, &self.bucket) {
            return Err(ServiceError::NotFound(
                "the MultiQC data is outside the project bucket".into(),
            ));
        }
        match crate::object_view::read_capped(state.cloud.as_ref(), &uri, MULTIQC_TABLE_MAX_BYTES)
            .await
        {
            Ok(None) => Err(ServiceError::Unprocessable(too_large.to_string())),
            Ok(Some(bytes)) => Ok(String::from_utf8_lossy(&bytes).into_owned()),
            Err(AwsError::NotFound) => Err(ServiceError::NotFound(missing.to_string())),
            Err(e) => Err(ServiceError::from(e)),
        }
    }
}

/// `report_general_stats_headers` from `multiqc_data.json`, read from its start in growing chunks;
/// `Null` when the file is missing, malformed or the key isn't found within the cap.
async fn read_headers(state: &AppState, uri: &str) -> serde_json::Value {
    let mut bytes: Vec<u8> = Vec::new();
    let mut want = DATA_JSON_FIRST_CHUNK;
    loop {
        let start = bytes.len() as u64;
        match state.cloud.get_object_range(uri, start, want - 1).await {
            Ok(body) => {
                let complete =
                    body.total_size.is_some_and(|t| start + body.bytes.len() as u64 >= t)
                        || body.bytes.is_empty();
                // Exactly what is needed: a doubling Vec would briefly hold twice the prefix.
                bytes.reserve_exact(body.bytes.len());
                bytes.extend_from_slice(&body.bytes);
                drop(body);
                match scan_headers(&bytes) {
                    HeadersScan::Found(v) => return v,
                    HeadersScan::NeedMore if !complete && want < DATA_JSON_MAX_BYTES => {
                        want = (want * 2).min(DATA_JSON_MAX_BYTES);
                    }
                    HeadersScan::NeedMore => {
                        if !complete {
                            tracing::warn!(
                                uri,
                                max_bytes = DATA_JSON_MAX_BYTES,
                                "multiqc_data.json: no general-stats headers within the read cap"
                            );
                        }
                        return serde_json::Value::Null;
                    }
                    HeadersScan::Invalid(e) => {
                        tracing::warn!(uri, error = %e, "multiqc_data.json is not valid JSON");
                        return serde_json::Value::Null;
                    }
                }
            }
            Err(e) => {
                if !matches!(e, AwsError::NotFound) {
                    tracing::warn!(uri, error = %e, "reading multiqc_data.json failed");
                }
                return serde_json::Value::Null;
            }
        }
    }
}

// ── undeclared FastQC results ─────────────────────────────────────────────────────────────────

/// The name of the report [`detect_fastqc`] gives the FastQC results it finds.
const DETECTED_FASTQC_NAME: &str = "Per-sample QC";

/// The run's registered FastQC zips under its output `prefix`, the first [`MAX_CANDIDATES`] by URI
/// (a run with more logs that later ones are not listed). The SQL pattern matches the ending as
/// FastQC writes it (lowercase; LIKE is case-sensitive), and is looser than a FastQC name (it
/// allows an empty stem), so each name is checked with [`fastqc::is_zip`].
async fn fastqc_candidates(
    state: &AppState,
    scope: &OutputScope,
    prefix: &str,
) -> Result<Vec<OutputRow>, StoreError> {
    let pattern = glob::to_sql_like(prefix, &format!("**/*{}", fastqc::ZIP_SUFFIX));
    let rows = state.store.outputs_like_in(scope, &[pattern], MAX_CANDIDATES).await?;
    if i64::try_from(rows.len()).is_ok_and(|n| n >= MAX_CANDIDATES) {
        tracing::warn!(
            run_id = scope.run_id().as_str(),
            limit = MAX_CANDIDATES,
            "list_reports: the run's FastQC results reached the cap; later ones are not shown"
        );
    }
    Ok(rows.into_iter().filter(|r| fastqc::is_zip(basename(&r.s3_uri))).collect())
}

/// FastQC results the workflow did not declare, as `detected` reports shown by the FastQC viewer:
/// each `_fastqc.zip` of `rows` under `prefix` that no report already lists, nor lists the
/// `_fastqc.html` beside (a declared FastQC HTML report already shows that result). A zip belongs
/// to the sample of `ids` (fillable ones, [`fillable_ids`]) that [`owning_sample`] finds, up to
/// [`PER_SAMPLE_FILES`] per sample in path order (more are logged), in sample order; a zip no
/// sample owns follows, named by its path.
fn detect_fastqc<'a>(
    rows: &'a [OutputRow],
    prefix: &str,
    used: &HashSet<&str>,
    ids: &[String],
) -> Vec<Resolved<'a>> {
    let mut over = vec![0usize; ids.len()];
    let mut owned: Vec<Vec<&OutputRow>> = vec![Vec::new(); ids.len()];
    let mut unowned: Vec<(&str, &OutputRow)> = Vec::new();
    for row in rows {
        let Some(rel) = row.s3_uri.strip_prefix(prefix) else {
            continue;
        };
        let name = basename(rel);
        let Some(html) = fastqc::html_beside_zip(name) else {
            continue;
        };
        if used.contains(row.s3_uri.as_str())
            || used.contains(format!("{}{html}", parent(&row.s3_uri)).as_str())
        {
            continue;
        }
        match owning_sample(name, ids) {
            Some(i) if owned[i].len() < PER_SAMPLE_FILES => owned[i].push(row),
            Some(i) => over[i] += 1,
            None => unowned.push((rel, row)),
        }
    }
    log_over_cap(DETECTED_FASTQC_NAME, ids, &over);
    let detected = |name: String, sample: Option<String>, row: &'a OutputRow| Resolved {
        name,
        description: Some("FastQC results the pipeline wrote".to_string()),
        kind: "detected",
        format: "fastqc",
        sample,
        declaration: None,
        row,
    };
    let mut out: Vec<Resolved<'a>> = ids
        .iter()
        .zip(owned)
        .flat_map(|(id, rows)| {
            rows.into_iter()
                .map(|row| detected(DETECTED_FASTQC_NAME.to_string(), Some(id.clone()), row))
        })
        .collect();
    out.extend(
        unowned
            .into_iter()
            .map(|(rel, row)| detected(format!("{DETECTED_FASTQC_NAME} · {rel}"), None, row)),
    );
    out
}

/// Log the samples of `ids` that had more files for `report` than [`PER_SAMPLE_FILES`] (`over[i]`
/// more for `ids[i]`), which were left out.
fn log_over_cap(report: &str, ids: &[String], over: &[usize]) {
    for (id, extra) in ids.iter().zip(over).filter(|(_, n)| **n > 0) {
        tracing::warn!(
            report,
            sample = id.as_str(),
            listed = PER_SAMPLE_FILES,
            left_out = extra,
            "list_reports: a sample has more files than a per-sample report lists"
        );
    }
}

/// One report [`resolve`] found.
#[derive(Debug)]
struct Resolved<'a> {
    name: String,
    description: Option<String>,
    /// `declared`, `multiqc` or `detected` (`RunReport.kind`).
    kind: &'static str,
    /// How to show it (`RunReport.format`).
    format: &'static str,
    /// The sample a per-sample report is.
    sample: Option<String>,
    /// The index of the declaration it came from (`RunReport.declaration`); `None` when none did.
    declaration: Option<u32>,
    row: &'a OutputRow,
}

/// The reports: each declaration's matches among its `candidates` in path order (named by their
/// path when a glob matches several), then the run's `multiqc` report when no declaration listed
/// it. A declared report that IS the run's MultiQC report keeps its declared name but is marked
/// `multiqc`, so the General Statistics (always of that report) show beside it.
///
/// A per-sample declaration ([`FormReport::is_per_sample`]) gives, per id of `sample_ids` (in that
/// order, each once) that has outputs, a report for each of the first [`PER_SAMPLE_FILES`] outputs
/// it owns in path order (a paired-end sample's `_1` and `_2`; more are logged), under the declared
/// name with the sample set: one declaration's reports are listed together, sample by sample, each
/// sample's files back to back. An output belongs to the LONGEST id whose path (`{sample}` filled in) matches it, so
/// with `{sample}*.bam` S10's file is S10's even though S1's path matches it too. An id holding a
/// glob character (`*`, `?`) is skipped, since it would match other samples' files.
fn resolve<'a>(
    declared: &[FormReport],
    output_prefix: Option<&str>,
    candidates: &'a [Vec<OutputRow>],
    multiqc: Option<&'a OutputRow>,
    sample_ids: &[String],
) -> Vec<Resolved<'a>> {
    let mut out = Vec::new();
    let mut used = std::collections::HashSet::new();
    let is_multiqc = |row: &OutputRow| multiqc.is_some_and(|m| m.s3_uri == row.s3_uri);
    let kind_of = |row: &OutputRow| {
        if is_multiqc(row) {
            "multiqc"
        } else {
            "declared"
        }
    };
    if let Some(prefix) = output_prefix {
        for (index, (decl, rows)) in declared.iter().zip(candidates).enumerate() {
            let declaration = u32::try_from(index).ok();
            let relative: Vec<(&str, &OutputRow)> = rows
                .iter()
                .filter_map(|r| r.s3_uri.strip_prefix(prefix).map(|rel| (rel, r)))
                .collect();
            if decl.is_per_sample() {
                let ids = fillable_ids(sample_ids);
                let paths: Vec<String> =
                    ids.iter().map(|id| decl.path.replace(SAMPLE_PLACEHOLDER, id)).collect();
                let mut owned: Vec<Vec<(&str, &OutputRow)>> = vec![Vec::new(); ids.len()];
                let mut over = vec![0usize; ids.len()];
                for &(rel, row) in &relative {
                    if used.contains(row.s3_uri.as_str()) {
                        continue;
                    }
                    // A filled path holds its id literally, so only an id the path contains can
                    // match it. Of those, the longest; on a tie, the first in sample order.
                    let owner = ids
                        .iter()
                        .enumerate()
                        .filter(|(i, id)| {
                            rel.contains(id.as_str()) && glob::matches(&paths[*i], rel)
                        })
                        .max_by(|(i, a), (j, b)| a.len().cmp(&b.len()).then(j.cmp(i)));
                    if let Some((i, _)) = owner {
                        if owned[i].len() < PER_SAMPLE_FILES {
                            owned[i].push((rel, row));
                        } else {
                            over[i] += 1;
                        }
                    }
                }
                log_over_cap(&decl.name, &ids, &over);
                for (id, hits) in ids.iter().zip(owned) {
                    for (rel, row) in hits {
                        used.insert(row.s3_uri.as_str());
                        out.push(Resolved {
                            name: decl.name.clone(),
                            description: decl.description.clone(),
                            kind: kind_of(row),
                            format: report_format(decl.kind.as_deref(), rel),
                            sample: Some(id.clone()),
                            declaration,
                            row,
                        });
                    }
                }
                continue;
            }
            // What an earlier declaration already listed is not this one's, so it does not make this
            // one's name read as one of several.
            let hits: Vec<&(&str, &OutputRow)> = relative
                .iter()
                .filter(|(rel, row)| {
                    glob::matches(&decl.path, rel) && !used.contains(row.s3_uri.as_str())
                })
                .collect();
            let several = hits.len() > 1;
            for (rel, row) in hits {
                if !used.insert(row.s3_uri.as_str()) {
                    continue;
                }
                let name =
                    if several { format!("{} · {rel}", decl.name) } else { decl.name.clone() };
                out.push(Resolved {
                    name,
                    description: decl.description.clone(),
                    kind: kind_of(row),
                    format: report_format(decl.kind.as_deref(), rel),
                    sample: None,
                    declaration,
                    row,
                });
            }
        }
    }
    if let Some(row) = multiqc.filter(|m| !used.contains(m.s3_uri.as_str())) {
        out.push(Resolved {
            name: "MultiQC".to_string(),
            description: Some("Quality control across the run's samples".to_string()),
            kind: "multiqc",
            format: "html",
            sample: None,
            declaration: None,
            row,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(uri: &str) -> OutputRow {
        OutputRow {
            id: uri.to_string(),
            s3_uri: uri.to_string(),
            path: None,
            task_key: None,
            attempt: None,
            role: "published".into(),
            reused: false,
            size: None,
            etag: None,
            meta: None,
            run_id: RunId::new("r1"),
        }
    }

    fn report(name: &str, path: &str) -> FormReport {
        FormReport { name: name.into(), path: path.into(), ..Default::default() }
    }

    const PREFIX: &str = "s3://b/biffix/runs/r1/";

    /// `resolve` over `rows` given to every declaration as its candidates, with the run's MultiQC
    /// report picked from them as the handler picks it.
    fn names(declared: &[FormReport], rows: &[OutputRow]) -> Vec<(String, &'static str)> {
        let candidates = vec![rows.to_vec(); declared.len()];
        let multiqc = shallowest_multiqc(rows.to_vec(), &[RunId::new("r1")]);
        resolve(declared, Some(PREFIX), &candidates, multiqc.as_ref(), &[])
            .into_iter()
            .map(|r| (r.name, r.kind))
            .collect()
    }

    #[test]
    fn declared_reports_come_first_and_multiqc_is_found_by_name() {
        let rows = [
            row("s3://b/biffix/runs/r1/results/multiqc/multiqc_report.html"),
            row("s3://b/biffix/runs/r1/results/summary.pdf"),
            row("s3://b/biffix/runs/r1/results/qc/a.html"),
            row("s3://b/biffix/runs/r1/results/qc/b.html"),
        ];
        let declared = [
            report("Summary", "results/summary.pdf"),
            report("Sample QC", "results/qc/*.html"),
            report("Missing", "results/nope.html"),
        ];
        assert_eq!(
            names(&declared, &rows),
            vec![
                ("Summary".to_string(), "declared"),
                ("Sample QC · results/qc/a.html".to_string(), "declared"),
                ("Sample QC · results/qc/b.html".to_string(), "declared"),
                ("MultiQC".to_string(), "multiqc"),
            ]
        );
    }

    #[test]
    fn a_declaration_left_one_file_by_an_earlier_one_names_it_plainly() {
        let rows =
            [row("s3://b/biffix/runs/r1/summary.html"), row("s3://b/biffix/runs/r1/other.html")];
        let declared = [report("Summary", "summary.html"), report("Pages", "*.html")];
        assert_eq!(
            names(&declared, &rows),
            vec![("Summary".to_string(), "declared"), ("Pages".to_string(), "declared"),]
        );
    }

    #[test]
    fn a_declared_multiqc_report_is_the_multiqc_report_and_is_listed_once() {
        let rows = [
            row("s3://b/biffix/runs/r1/results/multiqc/multiqc_report.html"),
            row("s3://b/biffix/runs/r1/results/multiqc/star/multiqc_report.html"),
        ];
        // The shallowest is the run's MultiQC report (the one General Statistics reads): declared,
        // it keeps its name and is marked `multiqc`; a deeper one is not offered as "MultiQC".
        let declared = [report("QC", "results/multiqc/multiqc_report.html")];
        assert_eq!(names(&declared, &rows), vec![("QC".to_string(), "multiqc")]);
        // Declaring only the deeper one: it is an ordinary report, and the shallowest is MultiQC.
        let declared = [report("STAR QC", "results/multiqc/star/multiqc_report.html")];
        assert_eq!(
            names(&declared, &rows),
            vec![("STAR QC".to_string(), "declared"), ("MultiQC".to_string(), "multiqc")]
        );
        let multiqc = shallowest_multiqc(rows.to_vec(), &[RunId::new("r1")]).unwrap();
        assert!(multiqc.s3_uri.ends_with("results/multiqc/multiqc_report.html"));
        // Only the exact file name counts (the SQL pattern is looser).
        assert!(shallowest_multiqc(
            vec![row("s3://b/x/my_multiqc_report.html")],
            &[RunId::new("r1")]
        )
        .is_none());
    }

    #[test]
    fn the_nearest_runs_multiqc_report_wins_then_the_shallowest_of_its() {
        // A resume (r2) that writes its report deeper than the run it resumed (r0) did shows its
        // own, not the stale one at the shallower path; of one run's, the shallowest.
        let of = |run: &str, uri: &str| OutputRow { run_id: RunId::new(run), ..row(uri) };
        let rows = vec![
            of("r0", "s3://b/p/multiqc_report.html"),
            of("r1", "s3://b/p/qc/multiqc_report.html"),
            of("r2", "s3://b/p/qc/new/multiqc_report.html"),
            of("r2", "s3://b/p/qc/new/group/multiqc_report.html"),
        ];
        let runs = [RunId::new("r2"), RunId::new("r1"), RunId::new("r0")];
        let pick = |rows: Vec<OutputRow>| shallowest_multiqc(rows, &runs).unwrap().s3_uri;
        assert_eq!(pick(rows.clone()), "s3://b/p/qc/new/multiqc_report.html");
        // Without one of its own, the nearest run it resumed.
        assert_eq!(pick(rows[..2].to_vec()), "s3://b/p/qc/multiqc_report.html");
    }

    #[test]
    fn each_declaration_resolves_against_its_own_candidates() {
        // A broad glob's candidates cannot crowd another declaration's out: each has its own.
        let broad: Vec<OutputRow> = (0..600)
            .map(|i| row(&format!("s3://b/biffix/runs/r1/results/fastqc/s{i}.html")))
            .collect();
        let candidates = vec![broad, vec![row("s3://b/biffix/runs/r1/results/final.html")]];
        let declared =
            [report("FastQC", "results/fastqc/*.html"), report("Final", "results/final.html")];
        let got = resolve(&declared, Some(PREFIX), &candidates, None, &[]);
        assert_eq!(got.len(), 601);
        assert_eq!(got.last().unwrap().name, "Final");
    }

    #[test]
    fn declarations_match_only_under_the_output_prefix() {
        let rows = [row("s3://elsewhere/results/summary.pdf")];
        assert!(names(&[report("Summary", "results/summary.pdf")], &rows).is_empty());
        assert_eq!(
            glob::to_sql_like(PREFIX, "results/*.pdf"),
            "s3://b/biffix/runs/r1/results/%.pdf"
        );
        // `**/` is a lone `%`: a MultiQC report at the run's own root is a candidate too.
        assert_eq!(
            glob::to_sql_like("", &format!("**/{MULTIQC_REPORT}")),
            "%multiqc\\_report.html"
        );
        // Without an output prefix no declaration resolves, but MultiQC still does.
        let m = row("s3://b/multiqc_report.html");
        let got = resolve(&[report("Summary", "results/summary.pdf")], None, &[], Some(&m), &[]);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].kind, "multiqc");
        assert_eq!(got[0].format, "html");
    }

    #[test]
    fn a_report_s_format_is_its_declared_kind_else_its_extension() {
        let rows = [
            row("s3://b/biffix/runs/r1/results/multiqc/multiqc_report.html"),
            row("s3://b/biffix/runs/r1/results/counts.tsv"),
            row("s3://b/biffix/runs/r1/results/summary.dat"),
        ];
        let declared = [
            report("Counts", "results/counts.tsv"),
            FormReport { kind: Some("table".into()), ..report("Summary", "results/summary.dat") },
        ];
        let candidates = vec![rows.to_vec(); declared.len()];
        let multiqc = shallowest_multiqc(rows.to_vec(), &[RunId::new("r1")]);
        let got: Vec<(String, &str)> =
            resolve(&declared, Some(PREFIX), &candidates, multiqc.as_ref(), &[])
                .into_iter()
                .map(|r| (r.name, r.format))
                .collect();
        assert_eq!(
            got,
            [
                ("Counts".to_string(), "table"),
                ("Summary".to_string(), "table"),
                ("MultiQC".to_string(), "html"),
            ]
        );
    }

    #[test]
    fn a_per_sample_report_is_one_report_per_sample_with_an_output_in_sample_order() {
        let rows = [
            row("s3://b/biffix/runs/r1/star/S10.markdup.bam"),
            row("s3://b/biffix/runs/r1/star/S1.markdup.bam"),
            row("s3://b/biffix/runs/r1/star/S2.markdup.bam"),
            row("s3://b/biffix/runs/r1/star/other.bam"),
        ];
        let declared = [FormReport {
            per_sample: true,
            kind: Some("alignment".into()),
            ..report("Alignments", "star/{sample}.markdup.bam")
        }];
        let candidates = vec![rows.to_vec()];
        // S3 has no output; `*` is not a sample id to fill in.
        let ids: Vec<String> =
            ["S2", "S1", "S3", "S10", "*"].iter().map(std::string::ToString::to_string).collect();
        let got: Vec<(String, Option<String>, &str, &str)> =
            resolve(&declared, Some(PREFIX), &candidates, None, &ids)
                .into_iter()
                .map(|r| (r.name, r.sample, r.format, r.row.s3_uri.as_str()))
                .collect();
        assert_eq!(
            got,
            [
                (
                    "Alignments".to_string(),
                    Some("S2".to_string()),
                    "alignment",
                    "s3://b/biffix/runs/r1/star/S2.markdup.bam"
                ),
                (
                    "Alignments".to_string(),
                    Some("S1".to_string()),
                    "alignment",
                    "s3://b/biffix/runs/r1/star/S1.markdup.bam"
                ),
                (
                    "Alignments".to_string(),
                    Some("S10".to_string()),
                    "alignment",
                    "s3://b/biffix/runs/r1/star/S10.markdup.bam"
                ),
            ]
        );
        // No samples, no per-sample reports.
        assert!(resolve(&declared, Some(PREFIX), &candidates, None, &[]).is_empty());
    }

    /// `resolve` of one per-sample declaration of `path` over `uris`, as (sample, file name).
    fn per_sample(path: &str, uris: &[&str], ids: &[&str]) -> Vec<(String, String)> {
        let rows: Vec<OutputRow> = uris.iter().map(|u| row(&format!("{PREFIX}{u}"))).collect();
        let declared = [FormReport { per_sample: true, ..report("Alignments", path) }];
        let ids: Vec<String> = ids.iter().map(std::string::ToString::to_string).collect();
        resolve(&declared, Some(PREFIX), &[rows], None, &ids)
            .into_iter()
            .map(|r| (r.sample.unwrap(), basename(&r.row.s3_uri).to_string()))
            .collect()
    }

    fn pairs(want: &[(&str, &str)]) -> Vec<(String, String)> {
        want.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect()
    }

    #[test]
    fn a_file_belongs_to_the_longest_sample_id_that_matches_it() {
        // `S1*` matches S10's file too, which sorts first: it is still S10's.
        let uris = ["star/S10.sorted.bam", "star/S1.sorted.bam"];
        assert_eq!(
            per_sample("star/{sample}*.bam", &uris, &["S1", "S10"]),
            pairs(&[("S1", "S1.sorted.bam"), ("S10", "S10.sorted.bam")])
        );
        // In the sheet's order, whichever id is listed first.
        assert_eq!(
            per_sample("star/{sample}*.bam", &uris, &["S10", "S1"]),
            pairs(&[("S10", "S10.sorted.bam"), ("S1", "S1.sorted.bam")])
        );
        // S1 has no file of its own: S10's is not given to it.
        assert_eq!(
            per_sample("star/{sample}*.bam", &["star/S10.bam"], &["S1", "S10"]),
            pairs(&[("S10", "S10.bam")])
        );
        // When S10 is not one of the run's samples, its file matches only S1's path.
        assert_eq!(
            per_sample("star/{sample}*.bam", &["star/S10.bam"], &["S1"]),
            pairs(&[("S1", "S10.bam")])
        );
    }

    /// [`detect_fastqc`] over `uris` (under [`PREFIX`]) with the outputs at `used` already listed,
    /// as (name, sample, file name).
    fn detected(
        uris: &[&str],
        used: &[&str],
        ids: &[&str],
    ) -> Vec<(String, Option<String>, String)> {
        // In URI order, as the store gives them.
        let mut rows: Vec<OutputRow> = uris.iter().map(|u| row(&format!("{PREFIX}{u}"))).collect();
        rows.sort_by(|a, b| a.s3_uri.cmp(&b.s3_uri));
        let used: Vec<String> = used.iter().map(|u| format!("{PREFIX}{u}")).collect();
        let used: HashSet<&str> = used.iter().map(String::as_str).collect();
        let ids: Vec<String> = ids.iter().map(std::string::ToString::to_string).collect();
        detect_fastqc(&rows, PREFIX, &used, &ids)
            .into_iter()
            .map(|r| {
                assert_eq!((r.kind, r.format), ("detected", "fastqc"));
                (r.name, r.sample, basename(&r.row.s3_uri).to_string())
            })
            .collect()
    }

    #[test]
    fn undeclared_fastqc_zips_are_per_sample_qc_reports() {
        let s = |n: &str, sample: Option<&str>, file: &str| {
            (n.to_string(), sample.map(str::to_string), file.to_string())
        };
        let uris = [
            "qc/S10_R1_fastqc.zip",
            "qc/S1_R1_fastqc.zip",
            "qc/S1_R2_fastqc.zip",
            "qc/S1_fastqc.html",
            "qc/undetermined_fastqc.zip",
            "qc/S2_fastqc.zip",
            "qc/S2_R1_fastqc.zip",
        ];
        assert_eq!(
            detected(&uris, &[], &["S2", "S1", "S10"]),
            [
                s("Per-sample QC", Some("S2"), "S2_R1_fastqc.zip"),
                s("Per-sample QC", Some("S2"), "S2_fastqc.zip"),
                s("Per-sample QC", Some("S1"), "S1_R1_fastqc.zip"),
                s("Per-sample QC", Some("S1"), "S1_R2_fastqc.zip"),
                s("Per-sample QC", Some("S10"), "S10_R1_fastqc.zip"),
                s("Per-sample QC · qc/undetermined_fastqc.zip", None, "undetermined_fastqc.zip"),
            ]
        );
        // A zip already listed, or whose HTML report a declaration lists, is not repeated.
        assert_eq!(
            detected(
                &["qc/S1_R1_fastqc.zip", "qc/S1_R2_fastqc.zip", "qc/S2_fastqc.zip"],
                &["qc/S1_R1_fastqc.html", "qc/S2_fastqc.zip"],
                &["S1", "S2"]
            ),
            [s("Per-sample QC", Some("S1"), "S1_R2_fastqc.zip")]
        );
        // Outside the run's prefix, or not a FastQC zip: nothing.
        let rows = [row("s3://b/elsewhere/S1_fastqc.zip"), row(&format!("{PREFIX}fastqc.zip"))];
        assert!(detect_fastqc(&rows, PREFIX, &HashSet::new(), &["S1".into()]).is_empty());
        // At most PER_SAMPLE_FILES per sample: the first ones by path.
        let many: Vec<String> =
            (1..=PER_SAMPLE_FILES + 2).map(|i| format!("qc/S1_{i}_fastqc.zip")).collect();
        let many: Vec<&str> = many.iter().map(String::as_str).collect();
        let kept: Vec<String> =
            detected(&many, &[], &["S1"]).into_iter().map(|(_, _, file)| file).collect();
        let first: Vec<String> =
            (1..=PER_SAMPLE_FILES).map(|i| format!("S1_{i}_fastqc.zip")).collect();
        assert_eq!(kept, first);
        // A bare `_fastqc.zip` (no stem) is not FastQC's.
        assert!(detected(&["qc/_fastqc.zip"], &[], &["S1"]).is_empty());
    }

    #[test]
    fn a_paired_end_sample_lists_each_of_its_files_up_to_the_cap() {
        // S1's R1 and R2 are both listed, in path order; S10's file stays S10's.
        let uris = ["qc/S10_1_fastqc.zip", "qc/S1_1_fastqc.zip", "qc/S1_2_fastqc.zip"];
        assert_eq!(
            per_sample("qc/{sample}_*_fastqc.zip", &uris, &["S1", "S10"]),
            pairs(&[
                ("S1", "S1_1_fastqc.zip"),
                ("S1", "S1_2_fastqc.zip"),
                ("S10", "S10_1_fastqc.zip"),
            ])
        );
        // At most PER_SAMPLE_FILES per sample, the first ones by path.
        let many: Vec<String> =
            (1..=PER_SAMPLE_FILES + 2).map(|i| format!("qc/S1_{i}_fastqc.zip")).collect();
        let many: Vec<&str> = many.iter().map(String::as_str).collect();
        let got: Vec<String> = per_sample("qc/{sample}_*_fastqc.zip", &many, &["S1"])
            .into_iter()
            .map(|(_, file)| file)
            .collect();
        let first: Vec<String> =
            (1..=PER_SAMPLE_FILES).map(|i| format!("S1_{i}_fastqc.zip")).collect();
        assert_eq!(got, first);
    }

    #[test]
    fn same_named_declarations_whose_samples_touch_keep_their_own_index() {
        // Two "QC" declarations: the first's last sample (S2) is the second's first, so the
        // reports alone (QC/S1, QC/S2, QC/S2, QC/S3) cannot say where one ends.
        let rows: Vec<OutputRow> = ["a/S1.html", "a/S2.html", "b/S2.html", "b/S3.html"]
            .iter()
            .map(|u| row(&format!("{PREFIX}{u}")))
            .collect();
        let declared = [
            FormReport { per_sample: true, ..report("QC", "a/{sample}.html") },
            report("Summary", "a/S1.html"),
            FormReport { per_sample: true, ..report("QC", "b/{sample}.html") },
        ];
        let candidates = vec![rows.clone(); declared.len()];
        let multiqc = row(&format!("{PREFIX}multiqc_report.html"));
        let ids: Vec<String> =
            ["S1", "S2", "S3"].iter().map(std::string::ToString::to_string).collect();
        let got: Vec<(String, Option<String>, Option<u32>)> =
            resolve(&declared, Some(PREFIX), &candidates, Some(&multiqc), &ids)
                .into_iter()
                .map(|r| (r.name, r.sample, r.declaration))
                .collect();
        let s = |n: &str, sample: Option<&str>, d: Option<u32>| {
            (n.to_string(), sample.map(str::to_string), d)
        };
        assert_eq!(
            got,
            [
                s("QC", Some("S1"), Some(0)),
                s("QC", Some("S2"), Some(0)),
                s("QC", Some("S2"), Some(2)),
                s("QC", Some("S3"), Some(2)),
                s("MultiQC", None, None),
            ]
        );
        // Detected FastQC results come from no declaration.
        let zips = [row(&format!("{PREFIX}qc/S1_fastqc.zip"))];
        let found = detect_fastqc(&zips, PREFIX, &HashSet::new(), &ids);
        assert_eq!(found[0].declaration, None);
    }

    #[test]
    fn a_repeated_sample_id_is_one_report() {
        assert_eq!(
            per_sample("star/{sample}.bam", &["star/S1.bam", "star/S2.bam"], &["S1", "S2", "S1"]),
            pairs(&[("S1", "S1.bam"), ("S2", "S2.bam")])
        );
        assert_eq!(
            fillable_ids(&[
                "S1".into(),
                "*".into(),
                "S1".into(),
                "a?".into(),
                "".into(),
                "S2".into()
            ]),
            ["S1", "S2"]
        );
    }

    #[test]
    fn a_per_sample_flag_on_a_path_without_the_placeholder_is_one_report() {
        // A form frozen without registration's check: the path is one plain report.
        let rows = [row(&format!("{PREFIX}qc/all.html"))];
        let declared = [FormReport { per_sample: true, ..report("QC", "qc/all.html") }];
        let ids = vec!["S1".to_string(), "S2".to_string()];
        let candidates = [rows.to_vec()];
        let got = resolve(&declared, Some(PREFIX), &candidates, None, &ids);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].sample, None);
        assert_eq!(got[0].name, "QC");
    }

    #[test]
    fn a_file_belongs_to_the_longest_sample_id_it_starts_with() {
        let ids: Vec<String> = ["S1", "S10", "S1_R", ""].map(String::from).to_vec();
        let owner = |name: &str| owning_sample(name, &ids).map(|i| ids[i].as_str());
        assert_eq!(owner("S1"), Some("S1"), "a name that is the id");
        assert_eq!(owner("S1_R1"), Some("S1"), "`R1` continues no id here");
        assert_eq!(owner("S1_R_1"), Some("S1_R"), "the longest");
        assert_eq!(owner("S10_R1_fastqc.zip"), Some("S10"));
        assert_eq!(owner("S100_R1"), None, "`0` could continue an id");
        assert_eq!(owner("X1"), None, "an empty id owns nothing");
        // A read suffix looks like an id: with S1 and S1_1, S1's read 1 (`S1_1`, as nf-core names
        // reads) is S1_1's, while its read 2 is S1's. Documented on `FastqcStatusSample::sample`.
        let ids: Vec<String> = ["S1", "S1_1"].map(String::from).to_vec();
        let owner = |name: &str| owning_sample(name, &ids).map(|i| ids[i].as_str());
        assert_eq!(owner("S1_1"), Some("S1_1"));
        assert_eq!(owner("S1_2"), Some("S1"));
    }

    fn status_result(fastqc_name: &str, filename: Option<&str>) -> FastqcStatusSample {
        FastqcStatusSample {
            sample: None,
            section: None,
            fastqc_name: fastqc_name.into(),
            filename: filename.map(str::to_string),
            modules: Vec::new(),
            stats: Default::default(),
        }
    }

    #[test]
    fn fastqc_statuses_sort_by_sample_then_name_with_no_owner_last() {
        let ids: Vec<String> = ["S2", "S1"].map(String::from).to_vec();
        let got = attribute_to_samples(
            ["S1_R2", "zz", "S2_R1", "S1_R1", "aa"].map(|n| status_result(n, None)).to_vec(),
            &ids,
        );
        let got: Vec<(Option<&str>, &str)> =
            got.iter().map(|s| (s.sample.as_deref(), s.fastqc_name.as_str())).collect();
        assert_eq!(
            got,
            [
                (Some("S2"), "S2_R1"),
                (Some("S1"), "S1_R1"),
                (Some("S1"), "S1_R2"),
                (None, "aa"),
                (None, "zz")
            ]
        );
    }

    #[test]
    fn a_fastqc_status_falls_back_to_its_fastq_s_name() {
        let ids: Vec<String> = ["S1", "S10", "S1*"].map(String::from).to_vec();
        let got = attribute_to_samples(
            vec![
                // MultiQC's `-d` prefixed the name; FastQC's record of its input is untouched.
                status_result("raw | S1_R1", Some("S1_R1.fastq.gz")),
                // Renamed outright, its FASTQ in a folder.
                status_result("Tumour", Some("reads/S10_R1.FQ.GZ")),
                // The MultiQC name wins when it has an owner.
                status_result("S1_R2", Some("S10_R2.fastq.gz")),
                // An id with a glob character is matched literally.
                status_result("S1*_R1", None),
                status_result("nobody", Some("nobody.fastq.gz")),
                status_result("nothing", None),
            ],
            &ids,
        );
        let got: Vec<(Option<&str>, &str)> =
            got.iter().map(|s| (s.sample.as_deref(), s.fastqc_name.as_str())).collect();
        assert_eq!(
            got,
            [
                (Some("S1"), "S1_R2"),
                (Some("S1"), "raw | S1_R1"),
                (Some("S10"), "Tumour"),
                (Some("S1*"), "S1*_R1"),
                (None, "nobody"),
                (None, "nothing")
            ]
        );
    }

    #[test]
    fn a_fastq_s_stem_drops_its_folder_and_extensions() {
        for (name, stem) in [
            ("S1_R1.fastq.gz", "S1_R1"),
            ("dir/S1_R1.FQ.GZ", "S1_R1"),
            ("S1.merged_R1.fq", "S1.merged_R1"),
            ("S1.bam", "S1"),
            ("S1_R1", "S1_R1"),
            (".fastq.gz", ".fastq"),
            ("", ""),
        ] {
            assert_eq!(fastq_stem(name), stem, "{name}");
        }
    }

    #[test]
    fn distinct_ids_keep_glob_characters_but_not_repeats_or_empties() {
        let ids: Vec<String> = ["S1", "S1*", "", "S1", "a?"].map(String::from).to_vec();
        assert_eq!(distinct_ids(&ids), ["S1", "S1*", "a?"]);
    }

    #[test]
    fn fastqc_tables_are_the_plain_one_first_then_each_section_by_name() {
        let files = |names: &[&str]| -> Vec<(String, u64)> {
            names.iter().map(|n| (n.to_string(), 10)).collect()
        };
        let got = fastqc_tables_of(files(&[
            "multiqc_citations.txt",
            "multiqc_fastqc_fastqc_trimmed.txt",
            "multiqc_fastqc.txt",
            "multiqc_fastqc_fastqc-1.txt",
            "multiqc_fastq_screen.txt",
            "multiqc_sources.json",
        ]))
        .unwrap();
        let got: Vec<(&str, Option<&str>)> =
            got.iter().map(|t| (t.name.as_str(), t.section.as_deref())).collect();
        assert_eq!(
            got,
            [
                ("multiqc_fastqc.txt", None),
                ("multiqc_fastqc_fastqc-1.txt", Some("fastqc-1")),
                ("multiqc_fastqc_fastqc_trimmed.txt", Some("fastqc_trimmed")),
            ]
        );
        // None: why, when the listing says.
        assert_eq!(
            fastqc_tables_of(files(&["multiqc_general_stats.txt"])),
            Err("the MultiQC report has no FastQC results".to_string())
        );
        for (names, format) in [
            (&["multiqc_sources.json", "multiqc_data.json"][..], "JSON"),
            (&["multiqc_fastqc.yaml"][..], "YAML"),
            (&["multiqc_fastqc_fastqc_raw.csv"][..], "CSV"),
        ] {
            let message = fastqc_tables_of(files(names)).unwrap_err();
            assert!(
                message.starts_with(&format!("MultiQC wrote its data as {format} ")),
                "{message}"
            );
        }
    }
}
