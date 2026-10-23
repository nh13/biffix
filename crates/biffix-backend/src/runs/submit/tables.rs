//! A submit's samplesheets: the rows given at submit, checked against their param's declared
//! columns and planned to the run's own prefix ([`plan_tables`]), and the sample ids of a sheet
//! given as a file ([`sheet_file_samples`]).

use std::collections::BTreeMap;

use biffix_core::ids::RunId;
use biffix_workflow::formschema::FormSchema;

use super::request::unreadable_in;
use crate::error::ServiceError;

/// Most rows one samplesheet may have.
const MAX_TABLE_ROWS: usize = 10_000;
/// The largest samplesheet file given as a path that submit reads for its sample ids.
const MAX_SHEET_FILE_BYTES: u64 = 1 << 20;
/// How long submit waits to read a samplesheet file for its sample ids before going on without.
const SHEET_FILE_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
/// Most sheet problems a rejection lists.
const MAX_LISTED_PROBLEMS: usize = 20;

/// A samplesheet to write for a run from rows given at submit.
pub(super) struct TablePlan {
    pub(super) param: String,
    pub(super) uri: String,
    pub(super) spec: biffix_workflow::samples::SheetSpec,
    pub(super) rows: Vec<biffix_workflow::samples::SheetRow>,
}

impl TablePlan {
    pub(super) fn content_type(&self) -> &'static str {
        match self.spec.format {
            biffix_workflow::samples::SheetFormat::Csv => "text/csv",
            biffix_workflow::samples::SheetFormat::Tsv => "text/tab-separated-values",
        }
    }
}

/// Where the samplesheet biffix writes for `param` lands:
/// `biffix/runs/<run>/inputs/<param>.<csv|tsv>` in the project bucket.
pub(super) fn sheet_uri(
    project_bucket: &str,
    run_id: &RunId,
    param: &str,
    spec: &biffix_workflow::samples::SheetSpec,
) -> String {
    format!(
        "s3://{project_bucket}/{}inputs/{param}.{}",
        crate::runs::output_prefix::run_output_prefix(run_id.as_str()),
        spec.format.extension()
    )
}

/// Check each submitted samplesheet against its param's declared columns and decide where it is
/// written ([`sheet_uri`]). Any problem is a 400 naming the rows and columns at fault.
pub(super) fn plan_tables(
    form: &FormSchema,
    tables: BTreeMap<String, Vec<biffix_workflow::samples::SheetRow>>,
    project_bucket: &str,
    run_id: &RunId,
) -> Result<Vec<TablePlan>, ServiceError> {
    let bad = ServiceError::Invalid;
    let mut plans = Vec::new();
    for (param, rows) in tables {
        let field = form
            .fields
            .iter()
            .find(|f| f.name == param)
            .ok_or_else(|| bad(format!("`{param}` is not a parameter of this workflow")))?;
        let spec = field.samplesheet.clone().ok_or_else(|| {
            bad(format!(
                "`{param}` declares no samplesheet columns; give it a path instead of rows"
            ))
        })?;
        if rows.len() > MAX_TABLE_ROWS {
            return Err(bad(format!(
                "`{param}` has {} rows; at most {MAX_TABLE_ROWS}",
                rows.len()
            )));
        }
        let problems = spec.validate(&rows);
        if !problems.is_empty() {
            let listed: Vec<String> = problems
                .iter()
                .take(MAX_LISTED_PROBLEMS)
                .map(|p| match (p.row, &p.column) {
                    (Some(r), Some(c)) => format!("row {}, {c}: {}", r + 1, p.message),
                    _ => p.message.clone(),
                })
                .collect();
            return Err(bad(format!("samplesheet `{param}` has problems: {}", listed.join("; "))));
        }
        if project_bucket.is_empty() {
            return Err(ServiceError::Internal(
                "no project bucket to write the samplesheet to".into(),
            ));
        }
        let uri = sheet_uri(project_bucket, run_id, &param, &spec);
        plans.push(TablePlan { param, uri, spec, rows });
    }
    Ok(plans)
}

/// The sample count and ids of the samplesheet given as a file, for a run with no sheet written
/// from rows. The sheet is the first field (in the form's field order) with declared columns, one
/// of them the sample id, whose param is one `s3://` path; a later such field is never tried, even
/// when this one's ids cannot be read. Best effort: `None` (logged), so the run's sample set stays
/// open as it was before submit read sheet files, when there is no such field, or its file:
/// - is in storage only another project's runs can read ([`unreadable_in`]). A path-input sheet
///   there has already refused the submit; a sheet field that is not a path input (a
///   `format: samplesheet` hint) is not existence-checked or refused, so this is its only gate;
/// - is larger than [`MAX_SHEET_FILE_BYTES`], or not read within [`SHEET_FILE_READ_TIMEOUT`];
/// - cannot be read, is not UTF-8, does not parse, or names no sample.
///
/// The delimiter is [`biffix_workflow::samples::SheetSpec::file_format`]'s: the one whose header
/// names more of the declared columns (as the launch form's import picks it), else the file's
/// ending (`.csv`, `.tsv`), else the declared format.
pub(super) async fn sheet_file_samples(
    cloud: &dyn crate::aws::objects::ObjectStore,
    projects: &crate::config::projects::Projects,
    project: &crate::config::projects::ProjectContext,
    form: &FormSchema,
    params: &serde_json::Value,
) -> Option<(u32, Vec<String>)> {
    use biffix_workflow::samples::ColumnRole;
    let (field, spec, id_column, uri) = form.fields.iter().find_map(|f| {
        let spec = f.samplesheet.as_ref()?;
        let id = spec.columns.iter().find(|c| c.role == Some(ColumnRole::SampleId))?;
        let uri = params.get(&f.name)?.as_str()?.trim();
        uri.starts_with("s3://").then_some((f.name.as_str(), spec, id.name.as_str(), uri))
    })?;
    let skip = |reason: &str| {
        tracing::warn!(
            field,
            uri,
            id_column,
            reason,
            "submit: the samplesheet file's sample ids are not declared; the run's sample set \
             stays open"
        );
        None
    };
    if let Some(reason) = unreadable_in(projects, project, uri) {
        return skip(&reason);
    }
    // The range is inclusive, so `cap - 1` reads at most `cap` bytes, as every reader of a capped
    // object does. A larger object says so in its total size; one whose size S3 did not give that
    // fills the cap may have been cut short, so it is refused too.
    let read = cloud.get_object_range(uri, 0, MAX_SHEET_FILE_BYTES - 1);
    let body = match tokio::time::timeout(SHEET_FILE_READ_TIMEOUT, read).await {
        Ok(Ok(body)) => body,
        Ok(Err(e)) => return skip(&format!("reading it failed: {e}")),
        Err(_) => {
            return skip(&format!(
                "reading it took more than {}s",
                SHEET_FILE_READ_TIMEOUT.as_secs()
            ))
        }
    };
    let too_large = match body.total_size {
        Some(total) => total > MAX_SHEET_FILE_BYTES,
        None => body.bytes.len() as u64 >= MAX_SHEET_FILE_BYTES,
    };
    if too_large {
        return skip(&format!("it is larger than {MAX_SHEET_FILE_BYTES} bytes"));
    }
    let text = match std::str::from_utf8(&body.bytes) {
        Ok(text) => text,
        Err(e) => return skip(&format!("it is not UTF-8 text: {e}")),
    };
    let rows = match spec.parse(text, spec.file_format(text, uri), MAX_TABLE_ROWS) {
        Ok(rows) => rows,
        Err(e) => return skip(&e.to_string()),
    };
    let ids = spec.sample_ids(&rows);
    if ids.is_empty() {
        return skip("it names no sample");
    }
    Some((spec.sample_count(&rows), ids))
}
