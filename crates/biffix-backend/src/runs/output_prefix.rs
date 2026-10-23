//! Where a run's outputs go: S3 key-layout rules over [`biffix_core::s3_layout`] (no AWS call),
//! shared by submit, launch, the outputs reads and provenance.

use biffix_core::s3_layout::SNAKEMAKE_CACHE_PREFIX;

/// Where a project's Snakemake between-workflow output cache lives: shared by every run in the
/// project bucket, expired by the bucket's lifecycle rule.
pub(crate) fn snakemake_cache_uri(bucket: &str) -> Option<String> {
    (!bucket.is_empty()).then(|| format!("s3://{bucket}/{SNAKEMAKE_CACHE_PREFIX}"))
}

/// Where a run may write, relative to the project bucket: `biffix/runs/<run_id>/`.
///
/// Namespaced under `biffix/` deliberately. A project bucket is frequently a pre-existing bucket
/// biffix does not own (product non-goal: no data movement), and a bare `runs/` prefix collides
/// with data already there (a results bucket, say, with thousands of objects already under
/// `runs/`). Confining every biffix write to one owned prefix is what lets the cross-account
/// bucket policy grant write/delete on `biffix/*` ALONE, so a buggy run cannot reach anything else
/// in the bucket.
///
/// This is the single source of truth for the coordinator's `BIFFIX_RUN_PREFIX` (the layout itself
/// is `biffix_core::s3_layout`, shared with ingest's sweeps), so this prefix is the write boundary.
pub(crate) fn run_output_prefix(run_id: &str) -> String {
    biffix_core::s3_layout::run_prefix(run_id)
}

/// The effective output prefix a run's workflow outputs land under: a (validated) submit-time
/// override verbatim, else the isolated per-run `s3://{bucket}/biffix/runs/{run_id}/`. The single
/// source of truth used both to stamp the `run_submitted` anchor and to launch the coordinator, so
/// the recorded location and the real one cannot disagree. It must match the coordinator
/// entrypoint's own fallback (`s3://$BIFFIX_PROJECT_BUCKET/$BIFFIX_RUN_PREFIX`). `None` only when
/// there is no override and no project bucket (a local composition with nowhere to write).
pub fn effective_output_prefix(
    bucket: &str,
    run_id: &str,
    output_prefix_override: Option<&str>,
) -> Option<String> {
    match output_prefix_override {
        Some(p) => Some(p.to_string()),
        None if bucket.is_empty() => None,
        None => Some(format!("s3://{bucket}/{}", run_output_prefix(run_id))),
    }
}

/// Whether two output prefixes name the same place: equal but for a trailing `/`. The one notion
/// of "the same output prefix" a resume is held to at submit and its reused outputs are read by.
pub(crate) fn same_output_prefix(a: &str, b: &str) -> bool {
    a.trim_end_matches('/') == b.trim_end_matches('/')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effective_output_prefix_default_matches_the_coordinator_fallback() {
        // The coordinator entrypoint's fallback is `s3://$BIFFIX_PROJECT_BUCKET/$BIFFIX_RUN_PREFIX`
        // with BIFFIX_RUN_PREFIX = run_output_prefix(run_id); the recorded prefix must be
        // identical.
        assert_eq!(
            effective_output_prefix("b", "run-x", None).as_deref(),
            Some("s3://b/biffix/runs/run-x/")
        );
    }

    #[test]
    fn effective_output_prefix_override_wins_verbatim() {
        assert_eq!(
            effective_output_prefix("b", "run-x", Some("s3://b/")).as_deref(),
            Some("s3://b/")
        );
    }

    #[test]
    fn effective_output_prefix_none_without_bucket_or_override() {
        assert_eq!(effective_output_prefix("", "run-x", None), None);
    }

    #[test]
    fn output_prefixes_compare_and_end_as_directories() {
        assert!(same_output_prefix("s3://b/p", "s3://b/p/"));
        assert!(!same_output_prefix("s3://b/p", "s3://b/pq/"));
    }
}
