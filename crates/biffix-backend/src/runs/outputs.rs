//! A run's outputs as a reader sees them: whose rows they are ([`output_scope`], the run's own and
//! those of the runs it resumed) and how each is shown ([`output_view`]).

use biffix_api::RunOutput;
use biffix_core::ids::{RunId, TaskKey};
use biffix_workflow::text::basename;

use crate::aws::AwsError;
use crate::error::ServiceError;
use crate::store::outputs::{checkable, OutputRow, OutputScope};
use crate::store::{RunDetailView, StoreError};
use crate::AppState;

/// How many runs back along its `resumed_from` chain a resumed run's outputs are looked for. The
/// walk stops there: outputs only a run farther back registered are left out (and logged).
const MAX_RESUME_HOPS: usize = 20;

/// Whose rows are run `detail`'s outputs ([`OutputScope`]): its own, then those of the runs it
/// resumed (nearest first), whose files it reused rather than wrote again: an engine skips a
/// finished task on a resume, so the run registers no output for it.
///
/// - `detail` is the run, already loaded and visible to the caller.
/// - A resume shares the output prefix of the run it resumes (submit holds it to that), so a
///   resumed run's rows count only when their URI is under that shared prefix. The run's own rows
///   count wherever they are.
/// - The walk stops at the first run whose prefix differs (none of a run that resumed into another
///   prefix can be in this one's), even if a run farther back shares the prefix again, and after
///   [`MAX_RESUME_HOPS`] runs. Without an output prefix to share there is no walk.
///
/// "Reused" is inferred from the chain: an origin's file under the shared prefix that no nearer
/// run registered counts, whether or not this run's DAG still wanted it. The outputs list's
/// `modified_since_produced` flag (S3's etag against the one the producer reported, when it
/// reported one) marks such a file a later run overwrote.
pub(crate) async fn output_scope(
    state: &AppState,
    detail: &RunDetailView,
) -> Result<OutputScope, StoreError> {
    let rid = &detail.run.run_id;
    let (Some(_), Some(own)) = (&detail.resumed_from, &detail.output_prefix) else {
        return Ok(OutputScope::run(rid));
    };
    // One more than the cap, to tell a chain the cap cut short from one that ends there.
    let found = state.store.resume_origins(rid, MAX_RESUME_HOPS + 1).await?;
    let mut origins = Vec::new();
    for origin in found {
        let prefix = origin.output_prefix.as_deref();
        if !prefix.is_some_and(|p| crate::runs::output_prefix::same_output_prefix(p, own)) {
            tracing::info!(
                run_id = rid.as_str(),
                origin = origin.run_id.as_str(),
                "outputs: the resume chain leaves the run's output prefix here; outputs of this \
                 run and those before it are not the run's"
            );
            break;
        }
        if origins.len() == MAX_RESUME_HOPS {
            tracing::warn!(
                run_id = rid.as_str(),
                origin = origin.run_id.as_str(),
                hops = MAX_RESUME_HOPS,
                "outputs: the resume chain is longer than the hop cap; outputs only runs past it \
                 registered are not the run's"
            );
            break;
        }
        origins.push(origin.run_id);
    }
    // The prefix as recorded, as the coordinator writes under it (it appends `work`, `results`, …
    // to it directly, so one recorded without its trailing `/` is not a folder).
    Ok(OutputScope::resumed(rid, origins, own))
}

/// An S3 failure reading or presigning an output: 404 for a missing object, else as any AWS
/// failure ([`ServiceError`]'s `From`: 403 for a missing grant, 503 for an outage, 502 otherwise).
pub(crate) fn aws_error(what: &str, e: AwsError) -> ServiceError {
    if !matches!(e, AwsError::NotFound) {
        tracing::warn!(error = %e, "{what} failed");
    }
    ServiceError::reworded(e, |e| match e {
        AwsError::NotFound => "the output's object is not in S3".to_string(),
        e => e.to_string(),
    })
}

/// Shape a stored output row of run `rid`'s outputs for the API.
pub(crate) fn output_view(
    row: &OutputRow,
    rid: &RunId,
    output_prefix: Option<&str>,
    bucket: &str,
) -> RunOutput {
    RunOutput {
        id: row.id.clone(),
        s3_uri: row.s3_uri.clone(),
        path: display_path(&row.s3_uri, output_prefix, row.path.as_deref()),
        name: basename(&row.s3_uri).to_string(),
        task_key: row.task_key.clone().map(TaskKey::new),
        attempt: row.attempt,
        role: row.role.clone(),
        reused: row.reused,
        reused_from: (row.run_id != *rid).then(|| row.run_id.clone()),
        size: row.meta.as_ref().and_then(|m| m.size).or(row.size),
        etag: row.meta.as_ref().and_then(|m| m.etag.clone()).or(row.etag.clone()),
        last_modified: row
            .meta
            .as_ref()
            .and_then(|m| m.last_modified)
            .map(|us| us as f64 / 1_000_000.0),
        exists: row.meta.as_ref().map(|m| m.present),
        modified_since_produced: modified_since_produced(row),
        servable: checkable(&row.s3_uri, bucket),
        viewer: biffix_workflow::viewer::viewer_of(basename(&row.s3_uri)).map(str::to_string),
    }
}

/// Whether S3's etag for a present object differs from the one its producer reported. Unknowable
/// (false) when either side has none — Snakemake never reports one.
fn modified_since_produced(row: &OutputRow) -> bool {
    match (&row.etag, &row.meta) {
        (Some(produced), Some(meta)) if meta.present => meta
            .etag
            .as_deref()
            .is_some_and(|now| now.trim_matches('"') != produced.trim_matches('"')),
        _ => false,
    }
}

/// The path an output is shown under: relative to the run's output prefix when inside it (the tree
/// the UI draws), else the engine's own name for it, else its bucket-relative key. Only the first
/// kind is a path the list's `prefix` filter selects by.
pub(crate) fn display_path(
    uri: &str,
    output_prefix: Option<&str>,
    engine_path: Option<&str>,
) -> String {
    if let Some(rel) = output_prefix.and_then(|p| uri.strip_prefix(p)).filter(|r| !r.is_empty()) {
        return rel.trim_start_matches('/').to_string();
    }
    if let Some(p) = engine_path.filter(|p| !p.is_empty()) {
        return p.to_string();
    }
    biffix_core::s3_layout::split_s3_uri(uri).map_or(uri, |(_, key)| key).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_path_prefers_the_run_prefix_then_the_engine_path_then_the_key() {
        let prefix = Some("s3://b/biffix/runs/r/");
        assert_eq!(
            display_path("s3://b/biffix/runs/r/results/a.tsv", prefix, Some("x")),
            "results/a.tsv"
        );
        assert_eq!(
            display_path("s3://b/elsewhere/a.tsv", prefix, Some("published/a.tsv")),
            "published/a.tsv"
        );
        assert_eq!(display_path("s3://b/elsewhere/a.tsv", prefix, None), "elsewhere/a.tsv");
        // The prefix itself (an odd registration) is not an empty path.
        assert_eq!(display_path("s3://b/biffix/runs/r/", prefix, None), "biffix/runs/r/");
    }
}
