//! One definition of "now, since the Unix epoch" for the workspace.
//!
//! The backend and ingest need the current wall-clock time in several units (epoch seconds as `f64`
//! for RunEvent timestamps, epoch micros as `i64` for stored stamps, whole seconds for token TTLs,
//! a `Duration` for window arithmetic). Every one saturates to zero for a clock before the epoch
//! (never in practice), so no caller has an error to handle.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Time since the Unix epoch. Saturates to zero if the clock is before the epoch.
#[cfg_attr(
    feature = "test-support",
    doc = "\nWith the `test-support` feature, a thread's clock can be [`testing::freeze`]n and \
           [`testing::advance`]d."
)]
pub fn since_epoch() -> Duration {
    #[cfg(feature = "test-support")]
    if let Some(at) = testing::now() {
        return at;
    }
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default()
}

/// A test's control of the clock (`test-support` only), per thread: [`freeze`] stops it at the
/// wall time, and [`advance`] moves it on, so a test can move time past a deadline between two
/// steps of the code under test (on a current-thread runtime, all of it runs on the test's thread)
/// without sleeping, and no pause of the test's process moves it.
#[cfg(feature = "test-support")]
pub mod testing {
    use std::cell::Cell;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    thread_local! {
        static FROZEN: Cell<Option<Duration>> = const { Cell::new(None) };
    }

    /// Stop this thread's clock at the wall time: every read returns it until [`advance`]d.
    pub fn freeze() {
        let at = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
        FROZEN.with(|frozen| frozen.set(Some(at)));
    }

    /// Move this thread's clock `by` later; frozen first ([`freeze`]) if it is not.
    pub fn advance(by: Duration) {
        if now().is_none() {
            freeze();
        }
        FROZEN.with(|frozen| frozen.set(frozen.get().map(|at| at.saturating_add(by))));
    }

    /// This thread's clock, when it is frozen.
    pub(crate) fn now() -> Option<Duration> {
        FROZEN.with(Cell::get)
    }
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

    #[cfg(feature = "test-support")]
    #[test]
    fn a_thread_s_clock_freezes_and_advances_for_that_thread_alone() {
        testing::freeze();
        let frozen = epoch_micros();
        std::thread::sleep(Duration::from_millis(5));
        assert_eq!(epoch_micros(), frozen, "frozen: wall time does not move it");
        testing::advance(Duration::from_secs(3600));
        assert_eq!(epoch_micros(), frozen + 3_600_000_000);
        let other = std::thread::spawn(epoch_micros).join().unwrap();
        assert!(other - frozen < 3_600_000_000, "another thread's clock is its own");
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
