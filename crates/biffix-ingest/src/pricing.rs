//! Tier-1 cost estimate for one Batch job (a task attempt or a run's coordinator): what its host's
//! compute and root volume cost for the time the job ran. Pure, so the arithmetic is unit-tested
//! without AWS; the reconcile Lambda gathers the inputs (host facts, spot price history, the
//! queue's volume spec, and its region's list prices: the checked-in table for
//! [`PRICE_TABLE_REGION`], the AWS Pricing API's price list elsewhere, read through
//! [`on_demand_hourly`] and [`gp3_prices`]) and stores the result in `attempt_cost`.
//!
//! It is an estimate, not the bill: it prices the job's own run time (not host boot/idle), assumes
//! one job per instance, and ignores Savings Plans and credits. Same-region S3 traffic is free and
//! S3 goes through the VPC gateway endpoint, so data transfer is taken as $0.

use std::collections::BTreeMap;
use std::sync::OnceLock;
use std::time::Duration;

/// How close to leaving the pricing window (`biffix_db::rules::PRICING_WINDOW`) a job whose inputs
/// are still unavailable (a failed spot-history or region-price lookup) may be deferred; past this
/// it is priced with the on-demand fallback, or recorded as unpriceable, rather than left pending.
pub const DEFER_UNTIL_WINDOW_END: Duration = Duration::from_secs(24 * 3600);

/// The region the checked-in on-demand table and [`Gp3Prices::US_EAST_1`] are for; another region's
/// prices come from the AWS Pricing API.
pub const PRICE_TABLE_REGION: &str = "us-east-1";

/// The checked-in us-east-1 Linux on-demand price table ({instance type: USD/hour}), refreshed by
/// `scripts/refresh_ec2_ondemand_prices.py`.
const ONDEMAND_US_EAST_1: &str = include_str!("../data/ec2_ondemand_us-east-1.json");

/// Hours in a month, as AWS bills EBS (GB-month prorated by the hour).
const HOURS_PER_MONTH: f64 = 730.0;
/// The IOPS and throughput (MiB/s) a gp3 volume gets free.
const GP3_FREE_IOPS: f64 = 3000.0;
const GP3_FREE_MBPS: f64 = 125.0;

/// A region's gp3 list prices (USD per month): storage per GB, IOPS above the free 3,000 per IOPS,
/// throughput above the free 125 MiB/s per MiB/s.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Gp3Prices {
    pub gb_month: f64,
    pub iops_month: f64,
    pub mbps_month: f64,
}

impl Gp3Prices {
    /// [`PRICE_TABLE_REGION`]'s.
    pub const US_EAST_1: Gp3Prices =
        Gp3Prices { gb_month: 0.08, iops_month: 0.005, mbps_month: 0.04 };
}

/// The on-demand hourly price of `instance_type` in [`PRICE_TABLE_REGION`], if the table has it.
///
/// # Panics
///
/// If the checked-in `data/ec2_ondemand_us-east-1.json` does not parse as `{type: price}` — only
/// possible after a bad hand edit, and caught by this module's tests.
pub fn ondemand_price(instance_type: &str) -> Option<f64> {
    static TABLE: OnceLock<BTreeMap<String, f64>> = OnceLock::new();
    TABLE
        .get_or_init(|| {
            serde_json::from_str(ONDEMAND_US_EAST_1).expect("the checked-in price table parses")
        })
        .get(instance_type)
        .copied()
}

/// The region of an availability zone name: `us-east-1a` → `us-east-1`, `us-gov-west-1b` →
/// `us-gov-west-1`, a Local Zone such as `us-west-2-lax-1a` → `us-west-2` (the region is everything
/// up to the first dash-separated part that starts with a digit, keeping that part's digits).
/// `None` when the name has no such part.
pub fn az_region(az: &str) -> Option<&str> {
    let mut offset = 0;
    for part in az.split('-') {
        let digits = part.bytes().take_while(u8::is_ascii_digit).count();
        if digits > 0 && offset > 0 {
            return Some(&az[..offset + digits]);
        }
        offset += part.len() + 1;
    }
    None
}

/// A gp3 root volume, as a queue-class launch template configures it.
#[derive(Clone, Copy, Debug, Default, PartialEq, serde::Deserialize)]
pub struct VolumeSpec {
    pub gb: f64,
    #[serde(default)]
    pub iops: f64,
    #[serde(default)]
    pub throughput: f64,
}

impl VolumeSpec {
    /// The volume's cost per hour at `prices`: storage, plus IOPS and throughput above gp3's free
    /// baseline.
    pub fn hourly_usd(&self, prices: &Gp3Prices) -> f64 {
        (self.gb * prices.gb_month
            + (self.iops - GP3_FREE_IOPS).max(0.0) * prices.iops_month
            + (self.throughput - GP3_FREE_MBPS).max(0.0) * prices.mbps_month)
            / HOURS_PER_MONTH
    }
}

/// The on-demand Linux hourly price (USD) in an AWS Pricing API `GetProducts` answer for one
/// instance type in one region (filtered as `scripts/refresh_ec2_ondemand_prices.py` filters, so it
/// names one product): the first positive `Hrs` price of its on-demand terms. `None` when the price
/// list has none (the region does not offer the type).
pub fn on_demand_hourly(price_list: &[String]) -> Option<f64> {
    price_list
        .iter()
        .flat_map(|item| on_demand_dimensions(item))
        .find_map(|(unit, usd)| (unit == "Hrs" && usd > 0.0).then_some(usd))
}

/// A region's gp3 prices from an AWS Pricing API `GetProducts` answer for `volumeApiName = gp3`:
/// its storage (`GB-Mo`), IOPS (`IOPS-Mo`) and throughput (per MiB/s, or per GiB/s, a 1024th of
/// it, per month) dimensions, each the highest price listed (the free baseline is a $0 tier).
/// `None` unless all three are present, so a region is never priced from a partial list.
pub fn gp3_prices(price_list: &[String]) -> Option<Gp3Prices> {
    let (mut gb, mut iops, mut mbps) = (None::<f64>, None::<f64>, None::<f64>);
    for (unit, usd) in price_list.iter().flat_map(|item| on_demand_dimensions(item)) {
        let unit = unit.to_ascii_lowercase();
        let (slot, usd) = match unit.as_str() {
            "gb-mo" => (&mut gb, usd),
            "iops-mo" => (&mut iops, usd),
            "mibps-mo" | "mbps-mo" => (&mut mbps, usd),
            "gibps-mo" => (&mut mbps, usd / 1024.0),
            _ => continue,
        };
        *slot = Some(slot.map_or(usd, |kept| kept.max(usd)));
    }
    Some(Gp3Prices { gb_month: gb?, iops_month: iops?, mbps_month: mbps? })
}

/// The `(unit, USD)` of every on-demand price dimension of one price-list document; nothing when it
/// does not parse.
fn on_demand_dimensions(item: &str) -> Vec<(String, f64)> {
    let Ok(doc) = serde_json::from_str::<serde_json::Value>(item) else {
        return Vec::new();
    };
    let terms = doc.pointer("/terms/OnDemand").and_then(serde_json::Value::as_object);
    terms
        .into_iter()
        .flat_map(|terms| terms.values())
        .filter_map(|term| term.get("priceDimensions")?.as_object())
        .flat_map(|dimensions| dimensions.values())
        .filter_map(|dimension| {
            let unit = dimension.get("unit")?.as_str()?;
            let usd = dimension.pointer("/pricePerUnit/USD")?.as_str()?;
            Some((unit.to_owned(), usd.parse().ok()?))
        })
        .collect()
}

/// Where a job's compute price came from — shown with the number so a reader knows how firm it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PriceSource {
    /// An on-demand host, at the list price.
    OnDemand,
    /// A spot host, priced from EC2's spot price history for its type and AZ over the job's window
    /// (what spot actually bills).
    SpotHistory,
    /// A spot host whose price history was unavailable, priced at on-demand (an upper bound).
    SpotAtOnDemand,
    /// A host whose spot/on-demand lifecycle is unknown, priced at on-demand.
    UnknownAtOnDemand,
}

impl PriceSource {
    pub fn as_str(self) -> &'static str {
        match self {
            PriceSource::OnDemand => "on_demand",
            PriceSource::SpotHistory => "spot_history",
            PriceSource::SpotAtOnDemand => "spot_at_on_demand",
            PriceSource::UnknownAtOnDemand => "unknown_at_on_demand",
        }
    }
}

/// Why a job can never be priced; recorded (`unpriceable_job.reason`) so it leaves the queue.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unpriceable {
    /// The job has no positive run time.
    NoRunTime,
    /// The host's region lists no on-demand price for its instance type.
    UnknownInstanceType,
    /// The host's region's list prices could not be read before the job left the pricing window.
    RegionPricesUnavailable,
}

impl Unpriceable {
    pub fn as_str(self) -> &'static str {
        match self {
            Unpriceable::NoRunTime => "no_run_time",
            Unpriceable::UnknownInstanceType => "unknown_instance_type",
            Unpriceable::RegionPricesUnavailable => "region_prices_unavailable",
        }
    }
}

/// A spot price history of `(effective_from, usd_per_hour)` points (epoch seconds), sorted by time
/// once on construction so every job priced from it reuses the order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SpotHistory(Vec<(f64, f64)>);

impl SpotHistory {
    pub fn new(mut points: Vec<(f64, f64)>) -> Self {
        points.sort_by(|a, b| a.0.total_cmp(&b.0));
        SpotHistory(points)
    }
}

/// The cost of spot capacity over `[start, end]` (epoch seconds) from a price history: each price
/// applies from its point until the next. The price in effect at `start` is the latest point at or
/// before it (the earliest point when none precedes it). `None` for an empty history or an empty
/// window.
pub fn spot_cost(history: &SpotHistory, start: f64, end: f64) -> Option<f64> {
    let points = &history.0;
    if points.is_empty() || end <= start {
        return None;
    }
    // The price in effect at `start`.
    let mut price = points.iter().rev().find(|(t, _)| *t <= start).unwrap_or(&points[0]).1;
    let mut at = start;
    let mut usd = 0.0;
    for &(t, p) in points.iter().filter(|(t, _)| *t > start && *t < end) {
        usd += price * (t - at) / 3600.0;
        at = t;
        price = p;
    }
    usd += price * (end - at) / 3600.0;
    Some(usd)
}

/// One job's estimated cost.
#[derive(Clone, Debug, PartialEq)]
pub struct JobCost {
    pub hours: f64,
    /// The job's average compute price over its window (USD/hour).
    pub price_per_hour: f64,
    pub price_source: PriceSource,
    pub compute_usd: f64,
    pub ebs_usd: f64,
}

/// What is known about the host a job ran on and how long it ran.
#[derive(Clone, Debug, PartialEq)]
pub struct JobHost<'a> {
    pub instance_type: &'a str,
    /// The host type's on-demand list price (USD/hour) in its region; `None` when the region lists
    /// none.
    pub on_demand_per_hour: Option<f64>,
    /// The host region's gp3 prices, for its root volume.
    pub gp3: Gp3Prices,
    /// `Some(true)` spot, `Some(false)` on-demand, `None` unknown.
    pub spot: Option<bool>,
    pub started_secs: f64,
    pub finished_secs: f64,
    pub volume: Option<VolumeSpec>,
}

/// Price one job. `spot_history` is the host type's price history in its AZ over (at least) the
/// job's window, when the job ran on spot and it could be fetched. `Err` says why the job can
/// never be priced.
pub fn price_job(
    host: &JobHost<'_>,
    spot_history: Option<&SpotHistory>,
) -> Result<JobCost, Unpriceable> {
    let seconds = host.finished_secs - host.started_secs;
    if seconds <= 0.0 {
        return Err(Unpriceable::NoRunTime);
    }
    let hours = seconds / 3600.0;
    let on_demand = host.on_demand_per_hour.ok_or(Unpriceable::UnknownInstanceType)?;
    let (compute_usd, price_source) = match host.spot {
        Some(false) => (on_demand * hours, PriceSource::OnDemand),
        None => (on_demand * hours, PriceSource::UnknownAtOnDemand),
        Some(true) => {
            match spot_history.and_then(|h| spot_cost(h, host.started_secs, host.finished_secs)) {
                Some(usd) => (usd, PriceSource::SpotHistory),
                None => (on_demand * hours, PriceSource::SpotAtOnDemand),
            }
        }
    };
    let ebs_usd = host.volume.map_or(0.0, |v| v.hourly_usd(&host.gp3) * hours);
    Ok(JobCost { hours, price_per_hour: compute_usd / hours, price_source, compute_usd, ebs_usd })
}

/// The spot/on-demand lifecycle a job is priced at: its host's own (`host_registry.lifecycle`),
/// else its queue's configured capacity (`BIFFIX_QUEUE_PRICING`), else unknown.
pub fn job_lifecycle(
    host: Option<&str>,
    queue_capacity: Option<crate::config::Capacity>,
) -> Option<&str> {
    host.or(queue_capacity.map(crate::config::Capacity::as_str))
}

/// The window each spot price history must cover: per key (instance type and AZ), the union of its
/// jobs' `(key, start, end)` run windows (epoch seconds), so one lookup per key prices them all.
pub fn spot_history_windows<K: Ord>(
    jobs: impl IntoIterator<Item = (K, f64, f64)>,
) -> BTreeMap<K, (f64, f64)> {
    let mut windows: BTreeMap<K, (f64, f64)> = BTreeMap::new();
    for (key, start, end) in jobs {
        windows
            .entry(key)
            .and_modify(|w| *w = (w.0.min(start), w.1.max(end)))
            .or_insert((start, end));
    }
    windows
}

/// Whether the spot price history a job needs could be read this tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HistoryLookup {
    /// The job is not priced from spot history (not spot, or no AZ to look it up by).
    NotNeeded,
    /// The lookup succeeded (possibly with no points, which is not transient).
    Fetched,
    /// The lookup failed this tick (throttling, a service error): a later tick may succeed.
    Failed,
}

/// What the pricer knows this tick about one job's inputs, for [`defer_pricing`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PricingInputs {
    /// The host's own spot/on-demand lifecycle is resolved (not a queue-capacity fallback).
    pub host_lifecycle_known: bool,
    /// The host is still young enough that EC2 may resolve its lifecycle on a later tick.
    pub host_lifecycle_resolvable: bool,
    pub history: HistoryLookup,
    /// The host region's list prices could not be read this tick.
    pub region_prices_failed: bool,
    /// The job is within [`DEFER_UNTIL_WINDOW_END`] of leaving the pricing window.
    pub leaving_window: bool,
}

/// Whether to leave a job unpriced this tick because an input that is only transiently missing
/// may still arrive: a cost is written once, so pricing now would make an on-demand fallback
/// permanent. A job about to leave the pricing window is priced with the fallback instead of never.
pub fn defer_pricing(inputs: PricingInputs) -> bool {
    if inputs.leaving_window {
        return false;
    }
    let lifecycle_pending = !inputs.host_lifecycle_known && inputs.host_lifecycle_resolvable;
    lifecycle_pending || inputs.history == HistoryLookup::Failed || inputs.region_prices_failed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn the_price_table_has_the_fleet_types() {
        assert_eq!(ondemand_price("c6a.4xlarge"), Some(0.612));
        assert!(ondemand_price("c8g.16xlarge").is_some());
        assert_eq!(ondemand_price("not-a-type"), None);
    }

    #[test]
    fn an_az_names_its_region() {
        assert_eq!(az_region("us-east-1a"), Some("us-east-1"));
        assert_eq!(az_region("eu-west-2c"), Some("eu-west-2"));
        assert_eq!(az_region("us-gov-west-1b"), Some("us-gov-west-1"));
        assert_eq!(az_region("us-west-2-lax-1a"), Some("us-west-2"));
        assert_eq!(az_region("ap-southeast-10a"), Some("ap-southeast-10"));
        assert_eq!(az_region("nonsense"), None);
        assert_eq!(az_region("1a"), None);
        assert_eq!(az_region(""), None);
    }

    #[test]
    fn gp3_hourly_charges_only_above_the_free_baseline() {
        // 300 GB at $0.08 + 0 extra IOPS + 375 MB/s above 125 at $0.04, per 730 h.
        let v = VolumeSpec { gb: 300.0, iops: 3000.0, throughput: 500.0 };
        let prices = Gp3Prices::US_EAST_1;
        assert!(close(v.hourly_usd(&prices), (300.0 * 0.08 + 375.0 * 0.04) / 730.0));
        let small = VolumeSpec { gb: 30.0, iops: 0.0, throughput: 0.0 };
        assert!(close(small.hourly_usd(&prices), 30.0 * 0.08 / 730.0));
    }

    #[test]
    fn spot_cost_integrates_price_steps_across_the_window() {
        // $0.10/h in effect before the window, $0.20/h from t=1800, $0.40/h from t=7200 (after).
        // Given out of order: construction sorts once.
        let history = SpotHistory::new(vec![(7200.0, 0.40), (0.0, 0.10), (1800.0, 0.20)]);
        // Window 900..3600: 900 s at 0.10 + 1800 s at 0.20.
        let usd = spot_cost(&history, 900.0, 3600.0).unwrap();
        assert!(close(usd, 0.10 * 900.0 / 3600.0 + 0.20 * 1800.0 / 3600.0));
        // No point precedes the window: the earliest price is used from the start.
        let usd = spot_cost(&SpotHistory::new(vec![(1000.0, 0.30)]), 0.0, 3600.0).unwrap();
        assert!(close(usd, 0.30));
        assert_eq!(spot_cost(&SpotHistory::default(), 0.0, 10.0), None);
        assert_eq!(spot_cost(&history, 10.0, 10.0), None);
    }

    fn host(spot: Option<bool>) -> JobHost<'static> {
        JobHost {
            instance_type: "c6a.4xlarge",
            on_demand_per_hour: ondemand_price("c6a.4xlarge"),
            gp3: Gp3Prices::US_EAST_1,
            spot,
            started_secs: 0.0,
            finished_secs: 1800.0,
            volume: Some(VolumeSpec { gb: 73.0, iops: 0.0, throughput: 0.0 }),
        }
    }

    #[test]
    fn a_job_is_priced_by_its_lifecycle_with_honest_fallbacks() {
        let od = price_job(&host(Some(false)), None).unwrap();
        assert_eq!(od.price_source, PriceSource::OnDemand);
        assert!(close(od.compute_usd, 0.612 / 2.0));
        assert!(close(od.ebs_usd, 73.0 * 0.08 / 730.0 / 2.0));

        let history = SpotHistory::new(vec![(0.0, 0.2)]);
        let spot = price_job(&host(Some(true)), Some(&history)).unwrap();
        assert_eq!(spot.price_source, PriceSource::SpotHistory);
        assert!(close(spot.compute_usd, 0.1));
        assert!(close(spot.price_per_hour, 0.2));

        let no_history = price_job(&host(Some(true)), None).unwrap();
        assert_eq!(no_history.price_source, PriceSource::SpotAtOnDemand);
        assert!(close(no_history.compute_usd, od.compute_usd));

        let unknown = price_job(&host(None), None).unwrap();
        assert_eq!(unknown.price_source, PriceSource::UnknownAtOnDemand);

        // Another region's list prices, compute and volume alike.
        let mut eu = host(Some(false));
        eu.on_demand_per_hour = Some(1.0);
        eu.gp3 = Gp3Prices { gb_month: 0.0876, ..Gp3Prices::US_EAST_1 };
        let eu = price_job(&eu, None).unwrap();
        assert!(close(eu.compute_usd, 0.5));
        assert!(close(eu.ebs_usd, 73.0 * 0.0876 / 730.0 / 2.0));
    }

    #[test]
    fn a_job_that_can_never_be_priced_says_why() {
        let mut h = host(Some(false));
        h.finished_secs = h.started_secs;
        assert_eq!(price_job(&h, None), Err(Unpriceable::NoRunTime));
        let mut h = host(Some(false));
        h.on_demand_per_hour = None;
        assert_eq!(price_job(&h, None), Err(Unpriceable::UnknownInstanceType));
    }

    /// A price-list document as `GetProducts` returns it (one per product).
    fn price_list_item(dimensions: &[(&str, &str)]) -> String {
        let dimensions: serde_json::Map<String, serde_json::Value> = dimensions
            .iter()
            .enumerate()
            .map(|(i, (unit, usd))| {
                (
                    format!("SKU.TERM.{i}"),
                    serde_json::json!({"unit": unit, "pricePerUnit": {"USD": usd}}),
                )
            })
            .collect();
        serde_json::json!({
            "product": {"attributes": {"instanceType": "c6a.4xlarge"}},
            "terms": {"OnDemand": {"SKU.TERM": {"priceDimensions": dimensions}}},
        })
        .to_string()
    }

    #[test]
    fn a_regions_prices_are_read_from_its_price_list() {
        assert_eq!(on_demand_hourly(&[price_list_item(&[("Hrs", "0.6840000000")])]), Some(0.684));
        assert_eq!(on_demand_hourly(&[price_list_item(&[("Hrs", "0.0000000000")])]), None);
        assert_eq!(on_demand_hourly(&[]), None);
        assert_eq!(on_demand_hourly(&["not json".into()]), None);

        let gp3 = [
            price_list_item(&[("GB-Mo", "0.0880000000")]),
            price_list_item(&[("IOPS-Mo", "0.0000000000"), ("IOPS-Mo", "0.0055000000")]),
            price_list_item(&[("GiBps-mo", "45.0560000000")]),
        ];
        let prices = gp3_prices(&gp3).unwrap();
        assert!(close(prices.gb_month, 0.088));
        assert!(close(prices.iops_month, 0.0055));
        assert!(close(prices.mbps_month, 0.044));
        // A partial list prices nothing.
        assert_eq!(gp3_prices(&gp3[..2]), None);
    }

    #[test]
    fn a_job_is_priced_at_its_hosts_lifecycle_else_its_queues_capacity() {
        use crate::config::Capacity;
        assert_eq!(job_lifecycle(Some("on_demand"), Some(Capacity::Spot)), Some("on_demand"));
        assert_eq!(job_lifecycle(None, Some(Capacity::Spot)), Some("spot"));
        assert_eq!(job_lifecycle(None, None), None);
    }

    #[test]
    fn one_spot_history_covers_every_job_of_its_key() {
        let windows = spot_history_windows([
            ("a", 10.0, 20.0),
            ("b", 5.0, 6.0),
            ("a", 2.0, 15.0),
            ("a", 12.0, 30.0),
        ]);
        assert_eq!(
            windows.into_iter().collect::<Vec<_>>(),
            [("a", (2.0, 30.0)), ("b", (5.0, 6.0))]
        );
    }

    #[test]
    fn pricing_waits_only_for_inputs_that_may_still_arrive() {
        let ready = PricingInputs {
            host_lifecycle_known: true,
            host_lifecycle_resolvable: true,
            history: HistoryLookup::Fetched,
            region_prices_failed: false,
            leaving_window: false,
        };
        assert!(!defer_pricing(ready));
        // A failed history lookup waits for a later tick...
        let failed = PricingInputs { history: HistoryLookup::Failed, ..ready };
        assert!(defer_pricing(failed));
        // ...unless the job is about to leave the window: an upper bound beats no price.
        assert!(!defer_pricing(PricingInputs { leaving_window: true, ..failed }));
        // An unresolved lifecycle waits while the host is young enough to resolve...
        let unresolved = PricingInputs { host_lifecycle_known: false, ..ready };
        assert!(defer_pricing(unresolved));
        // ...and falls back once EC2 can no longer describe the host.
        assert!(!defer_pricing(PricingInputs { host_lifecycle_resolvable: false, ..unresolved }));
        // A job that needs no spot history is priced now.
        assert!(!defer_pricing(PricingInputs { history: HistoryLookup::NotNeeded, ..ready }));
        // Unread region prices wait too, until the job is about to leave the window.
        let no_prices = PricingInputs { region_prices_failed: true, ..ready };
        assert!(defer_pricing(no_prices));
        assert!(!defer_pricing(PricingInputs { leaving_window: true, ..no_prices }));
    }
}
