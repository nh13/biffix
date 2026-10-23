//! Source timestamps.
//!
//! On the wire `ts` is float epoch seconds UTC. Internally we hold integer
//! **microseconds** so fold-ordering and dedup tie-breaks are exact and not subject to float
//! rounding. Serialization round-trips through the float wire form.
//!
//! Also a proleptic-Gregorian civil-date conversion ([`days_from_civil`] / [`civil_from_days`]),
//! which the backend's date parsing and formatting use. (The EventBridge decoders parse RFC 3339
//! with chrono, not with this.)

use std::borrow::Cow;

use schemars::{JsonSchema, Schema, SchemaGenerator};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A source-time instant, stored as microseconds since the Unix epoch (UTC).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Timestamp {
    micros: i64,
}

// On the wire a Timestamp is float epoch seconds, so its JSON Schema is that of an
// `f64` — not the internal `{micros}` representation.
impl JsonSchema for Timestamp {
    fn schema_name() -> Cow<'static, str> {
        "Timestamp".into()
    }
    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        <f64 as JsonSchema>::json_schema(generator)
    }
    fn inline_schema() -> bool {
        true
    }
}

impl Timestamp {
    pub fn from_micros(micros: i64) -> Self {
        Self { micros }
    }

    pub fn as_micros(self) -> i64 {
        self.micros
    }

    /// `secs` since the epoch, rounded to the nearest microsecond (to avoid accumulating float
    /// error). Saturates at the representable range, so it is for trusted in-process values; a
    /// wire `ts` goes through [`Timestamp::checked_from_epoch_seconds`].
    pub fn from_epoch_seconds(secs: f64) -> Self {
        Self { micros: (secs * 1_000_000.0).round() as i64 }
    }

    /// `millis` since the epoch, as AWS reports its job timestamps.
    pub fn from_epoch_millis(millis: i64) -> Self {
        Self::from_epoch_seconds(millis as f64 / 1_000.0)
    }

    /// `secs` since the epoch, or `None` when it is not finite or its microseconds do not fit in
    /// an `i64` (about ±292,000 years).
    pub fn checked_from_epoch_seconds(secs: f64) -> Option<Self> {
        // 2^63, exact as an f64: `i64::MAX as f64` rounds up to it, so the bound is exclusive.
        const LIMIT: f64 = 9_223_372_036_854_775_808.0;
        let micros = (secs * 1_000_000.0).round();
        (micros.is_finite() && (-LIMIT..LIMIT).contains(&micros))
            .then_some(Self { micros: micros as i64 })
    }

    pub fn as_epoch_seconds(self) -> f64 {
        self.micros as f64 / 1_000_000.0
    }
}

impl Serialize for Timestamp {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_f64(self.as_epoch_seconds())
    }
}

impl<'de> Deserialize<'de> for Timestamp {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        // The fold orders events by `ts`, so a value it cannot represent is refused, never clamped
        // to the first or last instant.
        let secs = f64::deserialize(d)?;
        Timestamp::checked_from_epoch_seconds(secs)
            .ok_or_else(|| serde::de::Error::custom(format!("ts {secs} is out of range")))
    }
}

/// Days since 1970-01-01 of a proleptic Gregorian date (Howard Hinnant's `days_from_civil`).
///
/// Pure arithmetic: the caller bounds `year` (so the multiplies cannot overflow) and checks that
/// the date exists, e.g. by round-tripping through [`civil_from_days`] (`2026-02-30` rolls over).
pub fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let m = i64::from(month);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The `(year, month, day)` of a day count since 1970-01-01 (Hinnant's `civil_from_days`).
pub fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// Epoch microseconds `micros` as `YYYY-MM-DD<sep>HH:MM:SS` in UTC (whole seconds; no offset), the
/// one place the workspace formats a UTC date and time: `sep` is `T` for ISO 8601 / RFC 3339, a
/// space for the form a spreadsheet reads as a date and time.
pub fn utc_datetime(micros: i64, sep: char) -> String {
    const MICROS_PER_DAY: i64 = 86_400_000_000;
    let (y, m, d) = civil_from_days(micros.div_euclid(MICROS_PER_DAY));
    let secs = micros.rem_euclid(MICROS_PER_DAY) / 1_000_000;
    format!("{y:04}-{m:02}-{d:02}{sep}{:02}:{:02}:{:02}", secs / 3600, (secs / 60) % 60, secs % 60)
}

#[cfg(test)]
mod civil_tests {
    use super::*;

    #[test]
    fn a_ts_that_does_not_fit_in_microseconds_is_refused_not_clamped() {
        for text in ["1e300", "-1e300", "9223372036854.776"] {
            assert!(serde_json::from_str::<Timestamp>(text).is_err(), "{text} must not parse");
        }
        assert_eq!(serde_json::from_str::<Timestamp>("1.5").unwrap().as_micros(), 1_500_000);
        assert_eq!(serde_json::from_str::<Timestamp>("-1.5").unwrap().as_micros(), -1_500_000);
        // The last whole second that fits (its micros as near as an f64 holds them).
        assert!(serde_json::from_str::<Timestamp>("9223372036854").is_ok());
    }

    #[test]
    fn a_utc_datetime_is_formatted_with_the_separator_asked_for() {
        assert_eq!(utc_datetime(0, 'T'), "1970-01-01T00:00:00");
        assert_eq!(utc_datetime(1_759_453_261_500_000, ' '), "2025-10-03 01:01:01");
        // Before the epoch: the day and the time of day both count back.
        assert_eq!(utc_datetime(-1_000_000, 'T'), "1969-12-31T23:59:59");
    }

    #[test]
    fn civil_dates_round_trip() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2000, 3, 1), 11_017);
        assert_eq!(days_from_civil(1969, 12, 31), -1);
        // Year 0 (a leap year) and the days either side of a century that is not one.
        assert_eq!(days_from_civil(0, 1, 1), -719_528);
        for days in [-719_528, -1, 0, 59, 60, 11_016, 20_000, 20_720, 2_932_896] {
            let (y, m, d) = civil_from_days(days);
            assert_eq!(days_from_civil(y, m, d), days, "{days}");
        }
        assert_eq!(civil_from_days(days_from_civil(2024, 2, 29)), (2024, 2, 29));
        assert_eq!(civil_from_days(days_from_civil(2100, 2, 29)), (2100, 3, 1));
    }
}
