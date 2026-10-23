//! Where biffix keeps its own objects in a project bucket. Shared by the backend (which launches
//! runs and serves their files) and ingest (whose reconcile sweeps delete under these prefixes), so
//! the two cannot drift apart.
//!
//! Every biffix write is confined to [`BIFFIX_ROOT`]: a project bucket is often a pre-existing
//! bucket biffix does not own, and the cross-account bucket policy grants write/delete on
//! `biffix/*` alone.

/// The one prefix biffix writes under in a project bucket.
pub const BIFFIX_ROOT: &str = "biffix/";

/// The key prefix every run's own files live under: `biffix/runs/<run_id>/`.
pub const RUNS_PREFIX: &str = "biffix/runs/";

/// The key prefix of biffix's caches in a project bucket, which the cache-expiry sweep deletes
/// under.
pub const CACHE_PREFIX: &str = "biffix/cache/";

/// The key prefix of a project bucket's Snakemake between-workflow output cache.
pub const SNAKEMAKE_CACHE_PREFIX: &str = "biffix/cache/snakemake/";

/// A run's own key prefix, relative to the project bucket: `biffix/runs/<run_id>/`.
pub fn run_prefix(run_id: &str) -> String {
    format!("{RUNS_PREFIX}{run_id}/")
}

/// Split `s3://bucket/key` into its bucket and key (the key may be empty: `s3://b/` is the bucket's
/// root). `None` for anything else: another scheme, no `/` after the bucket, or an empty bucket.
pub fn split_s3_uri(uri: &str) -> Option<(&str, &str)> {
    let (bucket, key) = uri.strip_prefix("s3://")?.split_once('/')?;
    (!bucket.is_empty()).then_some((bucket, key))
}

/// The bucket and run id of an output prefix of a run's own per-run shape,
/// `s3://<bucket>/biffix/runs/<id>/`: the only shape whose work dir the retention sweep deletes.
/// `None` for any other prefix (a custom one, or an id that is empty, `.`, `..` or has a `/`).
pub fn split_run_prefix(uri: &str) -> Option<(&str, &str)> {
    let (bucket, key) = split_s3_uri(uri)?;
    let id = key.strip_prefix(RUNS_PREFIX)?.strip_suffix('/')?;
    (!id.is_empty() && !id.contains('/') && id != "." && id != "..").then_some((bucket, id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_biffix_prefix_is_under_the_root() {
        for prefix in [RUNS_PREFIX, CACHE_PREFIX, SNAKEMAKE_CACHE_PREFIX, &run_prefix("r1")] {
            assert!(prefix.starts_with(BIFFIX_ROOT), "{prefix}");
            assert!(prefix.ends_with('/'), "{prefix}");
        }
        assert_eq!(run_prefix("run-1"), "biffix/runs/run-1/");
        assert!(SNAKEMAKE_CACHE_PREFIX.starts_with(CACHE_PREFIX));
    }

    #[test]
    fn an_s3_uri_splits_into_bucket_and_key() {
        assert_eq!(split_s3_uri("s3://b/k/x.txt"), Some(("b", "k/x.txt")));
        assert_eq!(split_s3_uri("s3://b/"), Some(("b", "")));
        for bad in ["s3://b", "b/k", "gs://b/k", "s3:///k", ""] {
            assert_eq!(split_s3_uri(bad), None, "{bad}");
        }
    }

    #[test]
    fn only_a_run_s_own_prefix_is_per_run() {
        assert_eq!(split_run_prefix("s3://bkt/biffix/runs/run-1/"), Some(("bkt", "run-1")));
        for other in [
            "s3://bkt/results/run-1/",
            "s3://bkt/biffix/runs/run-1",
            "s3://bkt/biffix/runs/a/b/",
            "s3://bkt/biffix/runs//",
            "s3://bkt/biffix/runs/../",
            "s3://bkt/biffix/runs/./",
            "s3:///biffix/runs/run-1/",
            "gs://bkt/biffix/runs/run-1/",
        ] {
            assert_eq!(split_run_prefix(other), None, "{other}");
        }
    }
}
