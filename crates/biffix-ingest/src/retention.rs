//! Intermediate-file retention: which Nextflow work dirs the reconcile tick may
//! delete. It deletes only a run's own `s3://<bucket>/biffix/runs/<run_id>/work/` — never a custom
//! output prefix, and never results. Pure; the query lives in `store::retention::work_due_for_deletion`.

/// Per tick, at most this many `DeleteObjects` calls (1,000 keys each), shared fairly across the
/// due work dirs ([`crate::budget::fair_share`]) and made only while the invocation has time left
/// ([`crate::budget::Deadline`]); a larger work dir is finished by later ticks.
pub const DELETE_BATCHES_PER_TICK: usize = 8;

/// Work dirs considered per tick.
pub const GROUPS_PER_TICK: i64 = 20;

/// A work dir the sweep may delete, and the runs that share it (a resumed run shares the work dir
/// of the run it resumed) whose deletion is not yet recorded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkDue {
    pub output_prefix: String,
    pub run_ids: Vec<String>,
}

/// Where a run's work dir lives: `(bucket, key prefix)` for an output prefix of the default
/// per-run shape `s3://<bucket>/biffix/runs/<id>/`, else `None` (a custom prefix is never swept).
pub fn work_location(output_prefix: &str) -> Option<(String, String)> {
    let (bucket, id) = biffix_core::s3_layout::split_run_prefix(output_prefix)?;
    Some((bucket.to_string(), format!("{}work/", biffix_core::s3_layout::run_prefix(id))))
}

/// The completion cutoff for the deploy's retention period: runs that ended at or before it are
/// due. `None` when the deploy keeps intermediate files forever.
pub fn cutoff_micros(now_micros: i64, retention_days: Option<u64>) -> Option<i64> {
    let days = i64::try_from(retention_days?).ok()?;
    Some(now_micros.saturating_sub(days.saturating_mul(86_400_000_000)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_runs_own_prefix_has_a_work_dir_to_sweep() {
        assert_eq!(
            work_location("s3://bkt/biffix/runs/run-1/"),
            Some(("bkt".into(), "biffix/runs/run-1/work/".into()))
        );
        for custom in [
            "s3://bkt/results/run-1/",
            "s3://bkt/biffix/runs/run-1",
            "s3://bkt/biffix/runs/a/b/",
            "s3://bkt/biffix/runs//",
            "s3://bkt/biffix/runs/../",
            "s3:///biffix/runs/run-1/",
            "gs://bkt/biffix/runs/run-1/",
        ] {
            assert_eq!(work_location(custom), None, "{custom}");
        }
    }

    #[test]
    fn the_cutoff_is_the_retention_period_before_now() {
        let day = 86_400_000_000;
        assert_eq!(cutoff_micros(100 * day, Some(30)), Some(70 * day));
        assert_eq!(cutoff_micros(100 * day, Some(0)), Some(100 * day));
        assert_eq!(cutoff_micros(100 * day, None), None);
    }
}
