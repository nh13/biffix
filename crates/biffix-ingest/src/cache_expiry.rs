//! Snakemake between-workflow cache expiry: which cache entries the reconcile tick may delete. The
//! cache holds *copies* of cached rules' outputs (a run that reuses an entry copies it into its own
//! output location), so deleting an old entry only means a later run rebuilds it; a run's results
//! are never under this prefix. Pure; the listing and deleting live in the reconcile tick's
//! `cache` step.

use biffix_core::s3_layout::SNAKEMAKE_CACHE_PREFIX;

/// Per tick, at most this many `ListObjectsV2` pages (1,000 keys each, each with at most one
/// `DeleteObjects`) are examined across every bucket, each bucket taking a fair share
/// ([`crate::budget::fair_share`]), and only while the invocation has time left
/// ([`crate::budget::Deadline`]); a backlog drains over ticks.
pub const LIST_PAGES_PER_TICK: usize = 20;

/// Whether the object at `key`, last written at `last_modified_micros`, is an expired cache entry:
/// under the cache prefix and written at or before `cutoff_micros`. An object with no known write
/// time is kept.
pub fn is_expired(key: &str, last_modified_micros: Option<i64>, cutoff_micros: i64) -> bool {
    key.starts_with(SNAKEMAKE_CACHE_PREFIX)
        && key.len() > SNAKEMAKE_CACHE_PREFIX.len()
        && last_modified_micros.is_some_and(|t| t <= cutoff_micros)
}

/// The `reconcile_cursor` name of `bucket`'s cache sweep resume point.
pub fn cursor_name(bucket: &str) -> String {
    format!("snakemake_cache:{bucket}")
}

/// Where the next tick resumes `bucket`'s listing after one that stopped at `last_key` (the last key
/// examined): after it while the listing was truncated there, from the start again (`None`) once a
/// pass reached the end. Keys are hashes, so a sweep that always restarted from the first key would
/// re-read the same leading pages and never reach an expired entry behind them.
pub fn next_cursor(last_key: Option<&str>, truncated: bool) -> Option<String> {
    last_key.filter(|_| truncated).map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sweep_resumes_after_its_last_key_and_wraps_at_the_end() {
        assert_eq!(cursor_name("b"), "snakemake_cache:b");
        assert_eq!(
            next_cursor(Some("biffix/cache/snakemake/k"), true).as_deref(),
            Some("biffix/cache/snakemake/k")
        );
        assert_eq!(next_cursor(Some("biffix/cache/snakemake/k"), false), None);
        assert_eq!(next_cursor(None, true), None);
    }

    const DAY: i64 = 86_400_000_000;

    #[test]
    fn only_old_entries_under_the_cache_prefix_expire() {
        let cutoff = 10 * DAY;
        assert!(is_expired("biffix/cache/snakemake/abc123", Some(9 * DAY), cutoff));
        assert!(is_expired("biffix/cache/snakemake/abc123", Some(10 * DAY), cutoff));
        assert!(!is_expired("biffix/cache/snakemake/abc123", Some(11 * DAY), cutoff));
        assert!(!is_expired("biffix/cache/snakemake/abc123", None, cutoff));
        // Never a run's results, nor the prefix itself.
        assert!(!is_expired("biffix/runs/run-1/index.txt", Some(0), cutoff));
        assert!(!is_expired("runs/abc/aligned.bam", Some(0), cutoff));
        assert!(!is_expired("biffix/cache/snakemake/", Some(0), cutoff));
        assert!(!is_expired("biffix/cache/snakemakeX/abc", Some(0), cutoff));
    }
}
