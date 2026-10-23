//! A launch's cost and time estimate, from the workflow's recent successful runs: the per-sample
//! cost's median and 25th–75th percentile range, scaled to the samples about to run, and the median
//! wall time. An estimate from history, not a quote.

/// Runs an estimate looks back over, and the fewest it needs.
pub(crate) const BASIS_RUNS: i64 = 20;
pub(crate) const MIN_BASIS_RUNS: usize = 2;

/// One past successful run of the workflow: what it cost, for how many samples, how long it took.
#[derive(Clone, Debug, PartialEq)]
pub struct PastRun {
    pub usd: f64,
    pub samples: u32,
    /// From launch to completion: a run's wait for its submit-time images is not its run time.
    pub wall_secs: f64,
}

/// The `q`-quantile (0–1) of sorted `values`, interpolating between neighbours.
fn quantile(sorted: &[f64], q: f64) -> f64 {
    let pos = q * (sorted.len() - 1) as f64;
    let (lo, hi) = (pos.floor() as usize, pos.ceil() as usize);
    sorted[lo] + (sorted[hi] - sorted[lo]) * (pos - lo as f64)
}

/// The estimate for `samples` samples, or `None` with fewer than two usable past runs (or no
/// samples).
pub(crate) fn estimate(past: &[PastRun], samples: u32) -> Option<biffix_api::LaunchEstimate> {
    let usable: Vec<&PastRun> =
        past.iter().filter(|r| r.samples > 0 && r.usd.is_finite() && r.usd >= 0.0).collect();
    if usable.len() < MIN_BASIS_RUNS || samples == 0 {
        return None;
    }
    let mut per_sample: Vec<f64> = usable.iter().map(|r| r.usd / r.samples as f64).collect();
    per_sample.sort_by(f64::total_cmp);
    // A run with no usable wall time (zero, negative or not finite) has no times, not an instant
    // one, so it is left out of the median hours (as `typical` leaves it out), though its cost
    // still counts.
    let mut wall: Vec<f64> =
        usable.iter().map(|r| r.wall_secs).filter(|w| w.is_finite() && *w > 0.0).collect();
    wall.sort_by(f64::total_cmp);
    let n = samples as f64;
    Some(biffix_api::LaunchEstimate {
        usd_median: quantile(&per_sample, 0.5) * n,
        usd_low: quantile(&per_sample, 0.25) * n,
        usd_high: quantile(&per_sample, 0.75) * n,
        usd_per_sample: quantile(&per_sample, 0.5),
        hours_median: if wall.is_empty() { 0.0 } else { quantile(&wall, 0.5) / 3600.0 },
        runs: usable.len() as i64,
    })
}

/// One past successful run of the workflow, whatever its sample count: how long it took and, once
/// priced, what it cost. The basis of a typical run.
#[derive(Clone, Debug, PartialEq)]
pub struct PastWall {
    /// From launch to completion, as [`PastRun::wall_secs`].
    pub wall_secs: f64,
    pub usd: Option<f64>,
}

/// A typical run from `past`: the median wall time, and the median cost of the priced runs when
/// there are two. `None` with fewer than two runs that took any time (a zero or negative wall time
/// is a run with no usable times, not an instant one, so it is left out of the median and of
/// `runs`).
pub(crate) fn typical(past: &[PastWall]) -> Option<biffix_api::TypicalRun> {
    let past: Vec<&PastWall> =
        past.iter().filter(|r| r.wall_secs.is_finite() && r.wall_secs > 0.0).collect();
    if past.len() < MIN_BASIS_RUNS {
        return None;
    }
    let mut wall: Vec<f64> = past.iter().map(|r| r.wall_secs).collect();
    wall.sort_by(f64::total_cmp);
    let mut usd: Vec<f64> =
        past.iter().filter_map(|r| r.usd).filter(|u| u.is_finite() && *u >= 0.0).collect();
    usd.sort_by(f64::total_cmp);
    Some(biffix_api::TypicalRun {
        hours_median: quantile(&wall, 0.5) / 3600.0,
        usd_median: (usd.len() >= MIN_BASIS_RUNS).then(|| quantile(&usd, 0.5)),
        runs: past.len() as i64,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(usd: f64, samples: u32, hours: f64) -> PastRun {
        PastRun { usd, samples, wall_secs: hours * 3600.0 }
    }

    #[test]
    fn scales_the_per_sample_spread_to_the_samples_asked_for() {
        // $1, $2, $3, $4 per sample.
        let past = [run(2.0, 2, 1.0), run(8.0, 4, 3.0), run(3.0, 1, 2.0), run(40.0, 10, 5.0)];
        let e = estimate(&past, 10).unwrap();
        assert_eq!(e.runs, 4);
        assert!((e.usd_per_sample - 2.5).abs() < 1e-9);
        assert!((e.usd_median - 25.0).abs() < 1e-9);
        assert!((e.usd_low - 17.5).abs() < 1e-9);
        assert!((e.usd_high - 32.5).abs() < 1e-9);
        assert!((e.hours_median - 2.5).abs() < 1e-9);
    }

    #[test]
    fn a_run_with_no_usable_wall_time_is_left_out_of_the_hours_but_not_the_cost() {
        let mut instant = run(4.0, 2, 0.0);
        instant.wall_secs = 0.0;
        let mut skewed = run(4.0, 2, 0.0);
        skewed.wall_secs = -60.0;
        let e = estimate(&[run(2.0, 2, 1.0), run(6.0, 2, 3.0), instant, skewed], 1).unwrap();
        assert_eq!(e.runs, 4);
        assert!((e.hours_median - 2.0).abs() < 1e-9, "{}", e.hours_median);
        // None with a usable time: no hours, not a panic.
        let unusable = || PastRun { wall_secs: f64::NAN, ..run(2.0, 2, 0.0) };
        let e = estimate(&[unusable(), unusable()], 1).unwrap();
        assert_eq!(e.hours_median, 0.0);
    }

    #[test]
    fn needs_two_runs_and_some_samples() {
        assert_eq!(estimate(&[run(2.0, 2, 1.0)], 5), None);
        assert_eq!(estimate(&[run(2.0, 2, 1.0), run(2.0, 0, 1.0)], 5), None);
        assert_eq!(estimate(&[run(2.0, 2, 1.0), run(4.0, 2, 1.0)], 0), None);
        assert!(estimate(&[run(2.0, 2, 1.0), run(4.0, 2, 1.0)], 1).is_some());
    }

    fn wall(hours: f64, usd: Option<f64>) -> PastWall {
        PastWall { wall_secs: hours * 3600.0, usd }
    }

    #[test]
    fn a_typical_run_is_the_median_time_and_the_median_priced_cost() {
        let t = typical(&[wall(1.0, Some(4.0)), wall(3.0, None), wall(2.0, Some(8.0))]).unwrap();
        assert_eq!(t.runs, 3);
        assert!((t.hours_median - 2.0).abs() < 1e-9);
        assert_eq!(t.usd_median, Some(6.0));
    }

    #[test]
    fn a_typical_run_needs_two_runs_and_two_priced_for_a_cost() {
        assert_eq!(typical(&[]), None);
        assert_eq!(typical(&[wall(1.0, Some(1.0))]), None);
        let t = typical(&[wall(1.0, Some(1.0)), wall(2.0, None)]).unwrap();
        assert!((t.hours_median - 1.5).abs() < 1e-9);
        assert_eq!(t.usd_median, None);
    }

    #[test]
    fn a_run_with_no_time_is_left_out_of_a_typical_run() {
        let t = typical(&[wall(0.0, Some(1.0)), wall(1.0, None), wall(3.0, None)]).unwrap();
        assert_eq!(t.runs, 2);
        assert!((t.hours_median - 2.0).abs() < 1e-9);
        assert_eq!(typical(&[wall(0.0, None), wall(1.0, None)]), None);
    }
}
