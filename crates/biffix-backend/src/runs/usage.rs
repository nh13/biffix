//! Spend over a period: the estimated cost of the runs submitted in it, grouped by month, workflow,
//! submitter, instance type or project. Estimates come from `attempt_cost` (EC2 prices); a run
//! counts in the period it was submitted in. Pure helpers; the query is the store's.

use biffix_core::time::{civil_from_days, days_from_civil};

/// What the spend is grouped by.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UsageGroup {
    Month,
    Workflow,
    Submitter,
    InstanceType,
    Project,
}

impl UsageGroup {
    /// The wire spelling (`month`, `workflow`, `submitter`, `instance_type`, `project`).
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            UsageGroup::Month => "month",
            UsageGroup::Workflow => "workflow",
            UsageGroup::Submitter => "submitter",
            UsageGroup::InstanceType => "instance_type",
            UsageGroup::Project => "project",
        }
    }
}

impl std::str::FromStr for UsageGroup {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "month" => UsageGroup::Month,
            "workflow" => UsageGroup::Workflow,
            "submitter" => UsageGroup::Submitter,
            "instance_type" => UsageGroup::InstanceType,
            "project" => UsageGroup::Project,
            other => {
                return Err(format!(
                    "unknown group_by `{other}` (month, workflow, submitter, instance_type or \
                     project)"
                ))
            }
        })
    }
}

/// One group's spend.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct UsageTotals {
    pub key: String,
    /// Runs with a cost estimate (a run on several instance types counts in each).
    pub runs: i64,
    pub compute_usd: f64,
    pub ebs_usd: f64,
}

/// The groups and the period's overall total (runs counted once).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct UsageReport {
    pub rows: Vec<UsageTotals>,
    pub total: UsageTotals,
}

const MICROS_PER_DAY: i64 = 86_400_000_000;

/// Epoch microseconds at the start (UTC) of a `YYYY-MM-DD` date.
pub fn parse_date(text: &str) -> Result<i64, String> {
    let bad = || format!("`{text}` is not a date (YYYY-MM-DD)");
    // Exactly `YYYY-MM-DD`: integer parsing alone would take `2026-9-1` or `+2026-09-01`.
    let shape = text.len() == 10
        && text.bytes().enumerate().all(|(i, b)| match i {
            4 | 7 => b == b'-',
            _ => b.is_ascii_digit(),
        });
    if !shape {
        return Err(bad());
    }
    let (y, m, d) = (&text[0..4], &text[5..7], &text[8..10]);
    let (year, month, day): (i64, u32, u32) = (
        y.parse().map_err(|_| bad())?,
        m.parse().map_err(|_| bad())?,
        d.parse().map_err(|_| bad())?,
    );
    if !(1..=12).contains(&month) || day == 0 || day > 31 || !(1970..=9999).contains(&year) {
        return Err(bad());
    }
    let days = days_from_civil(year, month, day);
    // Reject dates that roll over (2026-02-30).
    if civil_from_days(days) != (year, month, day) {
        return Err(bad());
    }
    Ok(days * MICROS_PER_DAY)
}

/// A `YYYY-MM-DD` spelling of epoch microseconds (UTC).
pub(crate) fn format_date(micros: i64) -> String {
    let (y, m, d) = civil_from_days(micros.div_euclid(MICROS_PER_DAY));
    format!("{y:04}-{m:02}-{d:02}")
}

/// The period `[from, to)` in epoch microseconds: the given dates (`to` exclusive), each defaulting
/// to the current month's bounds. `to` must come after `from`.
pub(crate) fn period(
    from: Option<&str>,
    to: Option<&str>,
    now_micros: i64,
) -> Result<(i64, i64), String> {
    let (y, m, _) = civil_from_days(now_micros.div_euclid(MICROS_PER_DAY));
    let month_start = days_from_civil(y, m, 1) * MICROS_PER_DAY;
    let (ny, nm) = if m == 12 { (y + 1, 1) } else { (y, m + 1) };
    let next_month = days_from_civil(ny, nm, 1) * MICROS_PER_DAY;
    let from = from.filter(|s| !s.is_empty()).map(parse_date).transpose()?;
    let to = to.filter(|s| !s.is_empty()).map(parse_date).transpose()?;
    match (from, to) {
        (Some(f), None) if next_month <= f => Err(format!(
            "`from` {} is not before the default `to` ({}, the start of next month); pass `to`",
            format_date(f),
            format_date(next_month)
        )),
        (None, Some(t)) if t <= month_start => Err(format!(
            "`to` {} is not after the default `from` ({}, the start of this month); pass `from`",
            format_date(t),
            format_date(month_start)
        )),
        (f, t) => {
            let (from, to) = (f.unwrap_or(month_start), t.unwrap_or(next_month));
            if to <= from {
                return Err("`to` must be after `from`".into());
            }
            Ok((from, to))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_parse_to_the_start_of_the_day_and_bad_ones_are_refused() {
        assert_eq!(parse_date("1970-01-02"), Ok(MICROS_PER_DAY));
        assert_eq!(format_date(parse_date("2026-09-24").unwrap()), "2026-09-24");
        for bad in [
            "2026-02-30",
            "2026-13-01",
            "2026-9",
            "yesterday",
            "1969-12-31",
            "",
            "2026-9-1",
            "+2026-09-01",
            "2026-09-+1",
            " 2026-09-01",
        ] {
            assert!(parse_date(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_period_defaults_to_the_current_month() {
        let now = parse_date("2026-09-24").unwrap() + 3_600_000_000;
        let (from, to) = period(None, None, now).unwrap();
        assert_eq!(
            (format_date(from), format_date(to)),
            ("2026-09-01".into(), "2026-10-01".into())
        );
        let dec = parse_date("2026-12-31").unwrap();
        let (_, to) = period(None, None, dec).unwrap();
        assert_eq!(format_date(to), "2027-01-01");
        let (from, to) = period(Some("2026-01-01"), Some("2026-04-01"), now).unwrap();
        assert_eq!(
            (format_date(from), format_date(to)),
            ("2026-01-01".into(), "2026-04-01".into())
        );
        assert!(period(Some("2026-04-01"), Some("2026-04-01"), now).is_err());
        // A lone bound past the other's default names the default, not the caller's mistake.
        assert_eq!(
            period(Some("2026-10-01"), None, now),
            Err("`from` 2026-10-01 is not before the default `to` (2026-10-01, the start of next \
                 month); pass `to`"
                .into())
        );
        assert_eq!(
            period(None, Some("2026-08-15"), now),
            Err("`to` 2026-08-15 is not after the default `from` (2026-09-01, the start of this \
                 month); pass `from`"
                .into())
        );
        assert!(period(Some("2026-09-15"), None, now).is_ok());
    }

    #[test]
    fn group_by_parses_its_five_spellings() {
        for g in ["month", "workflow", "submitter", "instance_type", "project"] {
            assert_eq!(g.parse::<UsageGroup>().unwrap().as_str(), g);
        }
        assert!("team".parse::<UsageGroup>().is_err());
    }
}
