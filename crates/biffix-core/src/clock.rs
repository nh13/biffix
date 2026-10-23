//! One definition of "now, since the Unix epoch" for the workspace.
//!
//! The backend and ingest need the current wall-clock time in several units (epoch seconds as `f64`
//! for RunEvent timestamps, epoch micros as `i64` for stored stamps, whole seconds for token TTLs,
//! a `Duration` for window arithmetic). Every one saturates to zero for a clock before the epoch
//! (never in practice), so no caller has an error to handle.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Time since the Unix epoch. Saturates to zero if the clock is before the epoch.
pub fn since_epoch() -> Duration {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default()
}

/// Seconds since the Unix epoch as `f64`.
pub fn epoch_secs() -> f64 {
    since_epoch().as_secs_f64()
}

/// Whole seconds since the Unix epoch as `u64`.
pub fn epoch_secs_u64() -> u64 {
    since_epoch().as_secs()
}

/// Microseconds since the Unix epoch as `i64` (saturating at `i64::MAX`).
pub fn epoch_micros() -> i64 {
    micros(since_epoch())
}

/// `at` (a time since the epoch, or a span) in whole milliseconds, saturating at `i64::MAX`.
pub fn millis(at: Duration) -> i64 {
    i64::try_from(at.as_millis()).unwrap_or(i64::MAX)
}

/// `at` (a time since the epoch, or a span) in whole microseconds, saturating at `i64::MAX`.
pub fn micros(at: Duration) -> i64 {
    i64::try_from(at.as_micros()).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_unit_reads_the_same_clock() {
        let secs = epoch_secs_u64();
        // Past 2020 and consistent across units (allowing the calls a second apart).
        assert!(secs > 1_577_836_800);
        assert!((epoch_micros() / 1_000_000 - secs as i64).abs() <= 1);
        assert!((epoch_secs() - secs as f64).abs() <= 1.0);
    }

    #[test]
    fn a_duration_converts_to_whole_units_and_saturates() {
        let at = Duration::new(3, 2_500_999);
        assert_eq!(millis(at), 3_002);
        assert_eq!(micros(at), 3_002_500);
        assert_eq!(millis(Duration::MAX), i64::MAX);
        assert_eq!(micros(Duration::MAX), i64::MAX);
    }
}
