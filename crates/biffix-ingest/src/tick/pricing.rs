//! The pricing step: estimate the cost of every finished job whose host is known.

use std::collections::{BTreeMap, BTreeSet};

use biffix_core::clock::{micros, since_epoch};
use biffix_db::rules::PRICING_WINDOW;
use lambda_runtime::Error;

use super::hosts::HOST_LIFECYCLE_WINDOW;
use super::{aws_error, micros_before, note, State};
use crate::budget::Deadline;
use crate::config::{Capacity, QueuePricing};
use crate::pricing::{
    az_region, defer_pricing, gp3_prices, job_lifecycle, on_demand_hourly, ondemand_price,
    price_job, spot_history_windows, Gp3Prices, HistoryLookup, JobHost, PricingInputs, SpotHistory,
    Unpriceable, DEFER_UNTIL_WINDOW_END, PRICE_TABLE_REGION,
};
use crate::store::pricing::{unpriced_jobs, JobRole, UnpricedJob};

/// The region whose endpoint serves the AWS Pricing API (it answers for every region).
const PRICING_API_REGION: &str = "us-east-1";

/// A Pricing API client for `sdk`'s credentials, on the API's endpoint.
pub(super) fn pricing_client(sdk: &aws_config::SdkConfig) -> aws_sdk_pricing::Client {
    let config = aws_sdk_pricing::config::Builder::from(sdk)
        .region(aws_sdk_pricing::config::Region::new(PRICING_API_REGION))
        .build();
    aws_sdk_pricing::Client::from_conf(config)
}

/// The list prices of regions other than [`PRICE_TABLE_REGION`] (whose are checked in), read from
/// the Pricing API as jobs need them and kept for the life of the Lambda container: list prices
/// change rarely, and a new container reads them afresh.
#[derive(Default)]
pub(super) struct PriceBook {
    /// `(region, instance type)` → its on-demand hourly price, `None` when the region lists none.
    on_demand: BTreeMap<(String, String), Option<f64>>,
    gp3: BTreeMap<String, Gp3Prices>,
}

/// Jobs fetched per page of the pricing queue.
const PRICING_BATCH: i64 = 500;

/// Pages of each role's pricing queue one tick works through. A page whose jobs are all left for a
/// later tick (their spot history could not be read) is paged past, so they hold back newer jobs
/// only once a role has this many pages of them; a backlog drains over ticks.
const PRICING_PAGES_PER_TICK: usize = 10;

/// A spot price history's key: (instance type, AZ).
type SpotKey = (String, String);

/// What one tick's pricing has done so far.
#[derive(Default)]
struct PricingTick {
    priced: usize,
    deferred: usize,
    unpriceable: usize,
    /// The spot histories whose lookup failed this tick: their jobs wait for a later tick, and
    /// later pages do not ask again.
    failed: BTreeSet<SpotKey>,
    /// The regions whose list prices could not be read this tick: their jobs wait too.
    failed_regions: BTreeSet<String>,
    first_error: Option<Error>,
}

/// The tick's fixed pricing inputs (epoch micros).
struct PricingClock {
    /// A job that finished before this leaves the window within a day (see [`defer_pricing`]).
    leaving_before: i64,
    /// A host first seen at or after this may still get its lifecycle resolved.
    resolvable_since: i64,
}

/// Price every finished job (task attempt or coordinator) whose host is known and that has no cost
/// yet, a page of each role's oldest-first queue at a time while the invocation has time left
/// (`deadline`). Spot jobs are priced from EC2's spot price history for their type and AZ, fetched
/// once per (type, AZ) per page over the union of that group's windows. A job whose inputs are only
/// transiently missing is left for a later tick ([`crate::pricing::defer_pricing`]) and paged past,
/// and one that can never be priced is recorded as such, so it leaves the queue. Returns how many
/// were priced.
pub(super) async fn price_finished_jobs(state: &State, deadline: Deadline) -> Result<usize, Error> {
    let now = since_epoch();
    let since = micros_before(now, PRICING_WINDOW);
    let clock = PricingClock {
        leaving_before: since.saturating_add(micros(DEFER_UNTIL_WINDOW_END)),
        resolvable_since: micros_before(now, HOST_LIFECYCLE_WINDOW),
    };
    let mut tick = PricingTick::default();
    for role in [JobRole::Task, JobRole::Coordinator] {
        let mut after: Option<UnpricedJob> = None;
        for _ in 0..PRICING_PAGES_PER_TICK {
            if !deadline.allows_another(since_epoch()) {
                break;
            }
            let jobs = match unpriced_jobs(&state.pool, role, since, after.as_ref(), PRICING_BATCH)
                .await
            {
                Ok(jobs) => jobs,
                Err(error) => {
                    note(&mut tick.first_error, error.into());
                    break;
                }
            };
            price_page(state, &jobs, &clock, &mut tick).await;
            // A short page is the end of the queue.
            if (jobs.len() as i64) < PRICING_BATCH {
                break;
            }
            after = jobs.into_iter().last();
        }
    }
    if tick.deferred + tick.unpriceable > 0 {
        tracing::info!(
            deferred = tick.deferred,
            unpriceable = tick.unpriceable,
            "pricing: jobs not priced this tick"
        );
    }
    tick.first_error.map_or(Ok(tick.priced), Err)
}

/// Price one page of the queue into `tick`.
async fn price_page(
    state: &State,
    jobs: &[UnpricedJob],
    clock: &PricingClock,
    tick: &mut PricingTick,
) {
    fn queue_of<'s>(state: &'s State, job: &UnpricedJob) -> Option<&'s QueuePricing> {
        job.queue.as_ref().and_then(|q| state.config.queue_pricing.get(q))
    }
    fn lifecycle_of<'j>(state: &State, job: &'j UnpricedJob) -> Option<&'j str> {
        job_lifecycle(job.lifecycle.as_deref(), queue_of(state, job).and_then(|q| q.capacity))
    }
    let spot_key = |job: &UnpricedJob| -> Option<SpotKey> {
        let az = job.availability_zone.as_ref()?;
        (lifecycle_of(state, job) == Some(Capacity::Spot.as_str()))
            .then(|| (job.instance_type.clone(), az.clone()))
    };

    let region_of = |job: &UnpricedJob| -> String {
        job.availability_zone
            .as_deref()
            .and_then(az_region)
            .or(state.config.deploy_region.as_deref())
            .unwrap_or(PRICE_TABLE_REGION)
            .to_owned()
    };
    for job in jobs {
        let region = region_of(job);
        if region != PRICE_TABLE_REGION && !tick.failed_regions.contains(&region) {
            if let Err(error) = read_region_prices(state, &region, &job.instance_type).await {
                tracing::warn!(region, error = %error,
                    "region list prices unavailable; deferring its jobs");
                tick.failed_regions.insert(region);
                note(&mut tick.first_error, error);
            }
        }
    }

    let windows = spot_history_windows(jobs.iter().filter_map(|job| {
        Some((spot_key(job)?, job.started_micros as f64 / 1e6, job.finished_micros as f64 / 1e6))
    }));
    let mut histories: BTreeMap<SpotKey, SpotHistory> = BTreeMap::new();
    for (key, (start, end)) in windows {
        if tick.failed.contains(&key) {
            continue;
        }
        let (instance_type, az) = &key;
        match spot_history(state, instance_type, az, start, end).await {
            Ok(points) => {
                histories.insert(key, SpotHistory::new(points));
            }
            Err(error) => {
                tracing::warn!(instance_type, az, error = %error,
                    "spot price history unavailable; deferring those jobs");
                tick.failed.insert(key);
                note(&mut tick.first_error, error);
            }
        }
    }

    for job in jobs {
        let lifecycle = lifecycle_of(state, job);
        let key = spot_key(job);
        let history = match &key {
            None => HistoryLookup::NotNeeded,
            Some(key) if tick.failed.contains(key) => HistoryLookup::Failed,
            Some(_) => HistoryLookup::Fetched,
        };
        let region = region_of(job);
        let region_prices_failed = tick.failed_regions.contains(&region);
        if defer_pricing(PricingInputs {
            host_lifecycle_known: job.lifecycle.is_some(),
            host_lifecycle_resolvable: job.host_first_seen_micros >= clock.resolvable_since,
            history,
            region_prices_failed,
            leaving_window: job.finished_micros < clock.leaving_before,
        }) {
            tick.deferred += 1;
            continue;
        }
        let (on_demand_per_hour, gp3) = if region == PRICE_TABLE_REGION {
            (ondemand_price(&job.instance_type), Gp3Prices::US_EAST_1)
        } else {
            let book = lock(&state.prices);
            (
                book.on_demand.get(&(region.clone(), job.instance_type.clone())).copied().flatten(),
                book.gp3.get(&region).copied().unwrap_or(Gp3Prices::US_EAST_1),
            )
        };
        let host = JobHost {
            instance_type: &job.instance_type,
            on_demand_per_hour,
            gp3,
            spot: lifecycle.map(|l| l == Capacity::Spot.as_str()),
            started_secs: job.started_micros as f64 / 1e6,
            finished_secs: job.finished_micros as f64 / 1e6,
            volume: queue_of(state, job).and_then(|q| q.volume),
        };
        // Leaving the window with its region's prices still unread: never priceable now.
        let priced = if region_prices_failed {
            Err(Unpriceable::RegionPricesUnavailable)
        } else {
            price_job(&host, key.as_ref().and_then(|k| histories.get(k)))
        };
        let written = match priced {
            Ok(cost) => {
                tick.priced += 1;
                crate::store::pricing::record_job_cost(&state.pool, job, lifecycle, &cost).await
            }
            Err(reason) => {
                tracing::warn!(
                    batch_job_id = job.batch_job_id,
                    instance_type = job.instance_type,
                    availability_zone = job.availability_zone,
                    reason = reason.as_str(),
                    "job can never be priced; recording it as unpriceable"
                );
                tick.unpriceable += 1;
                crate::store::pricing::record_unpriceable(&state.pool, job, reason.as_str()).await
            }
        };
        if let Err(error) = written {
            note(&mut tick.first_error, error.into());
        }
    }
}

/// The price book, even if a panic poisoned it (its maps are always whole).
fn lock(prices: &std::sync::Mutex<PriceBook>) -> std::sync::MutexGuard<'_, PriceBook> {
    prices.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Make sure the price book holds `region`'s gp3 prices and `instance_type`'s on-demand price
/// there, reading what it lacks from the Pricing API. An instance type the region does not list is
/// recorded as such (the job is then unpriceable); a gp3 price list that lacks a dimension is an
/// error, so its jobs wait rather than be priced from part of it.
async fn read_region_prices(state: &State, region: &str, instance_type: &str) -> Result<(), Error> {
    let (has_gp3, has_type) = {
        let book = lock(&state.prices);
        (
            book.gp3.contains_key(region),
            book.on_demand.contains_key(&(region.to_owned(), instance_type.to_owned())),
        )
    };
    if !has_gp3 {
        let list = get_products(state, &[("regionCode", region), ("volumeApiName", "gp3")]).await?;
        let prices = gp3_prices(&list).ok_or_else(|| {
            Error::from(format!(
                "the Pricing API's gp3 price list for {region} lacks a storage, IOPS or \
                 throughput price"
            ))
        })?;
        lock(&state.prices).gp3.insert(region.to_owned(), prices);
    }
    if !has_type {
        // The filters `scripts/refresh_ec2_ondemand_prices.py` reads the checked-in table with.
        let list = get_products(
            state,
            &[
                ("regionCode", region),
                ("instanceType", instance_type),
                ("operatingSystem", "Linux"),
                ("tenancy", "Shared"),
                ("preInstalledSw", "NA"),
                ("capacitystatus", "Used"),
                ("licenseModel", "No License required"),
            ],
        )
        .await?;
        lock(&state.prices)
            .on_demand
            .insert((region.to_owned(), instance_type.to_owned()), on_demand_hourly(&list));
    }
    Ok(())
}

/// Every EC2 price-list document the Pricing API's `GetProducts` returns for `filters` (exact
/// attribute matches).
async fn get_products(state: &State, filters: &[(&str, &str)]) -> Result<Vec<String>, Error> {
    use aws_sdk_pricing::types::{Filter, FilterType};
    let filters = filters
        .iter()
        .map(|(field, value)| {
            Filter::builder().r#type(FilterType::TermMatch).field(*field).value(*value).build()
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut list = Vec::new();
    let mut pages = state
        .pricing
        .get_products()
        .service_code("AmazonEC2")
        .format_version("aws_v1")
        .set_filters(Some(filters))
        .max_results(100)
        .into_paginator()
        .send();
    while let Some(page) = pages.next().await {
        let page = page.map_err(|error| aws_error("Pricing GetProducts", error))?;
        list.extend(page.price_list.unwrap_or_default());
    }
    Ok(list)
}

/// EC2's Linux spot price history for `instance_type` in `az` covering `[start, end]` (epoch
/// seconds), as `(effective_from, usd_per_hour)` points. The window is widened back a day so the
/// price already in effect at `start` is included.
async fn spot_history(
    state: &State,
    instance_type: &str,
    az: &str,
    start: f64,
    end: f64,
) -> Result<Vec<(f64, f64)>, Error> {
    use aws_sdk_ec2::primitives::DateTime;
    let mut points = Vec::new();
    let mut pages = state
        .ec2
        .describe_spot_price_history()
        .instance_types(instance_type.into())
        .availability_zone(az)
        .product_descriptions("Linux/UNIX")
        .start_time(DateTime::from_secs((start - 86_400.0) as i64))
        .end_time(DateTime::from_secs(end.ceil() as i64))
        .into_paginator()
        .send();
    while let Some(page) = pages.next().await {
        let page = page.map_err(|error| aws_error("DescribeSpotPriceHistory", error))?;
        for entry in page.spot_price_history() {
            let (Some(ts), Some(price)) =
                (entry.timestamp(), entry.spot_price().and_then(|p| p.parse::<f64>().ok()))
            else {
                continue;
            };
            points.push((ts.secs() as f64, price));
        }
    }
    Ok(points)
}
