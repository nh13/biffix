//! The idle gate: whether a scheduled Lambda tick (the reconcile tick, the backend sweep) may leave
//! the database asleep.
//!
//! Aurora Serverless v2 pauses after a few minutes with no connection, and a tick every 5 minutes
//! that touches the database would never let it. So a scheduled tick first asks AWS Batch, never
//! the database, whether anything is going on: a job on a coordinator queue that is active, or
//! that stopped within [`RECENT_MILLIS`]. When nothing is, the tick skips its database work. One
//! tick an hour runs in full regardless ([`by_schedule`]), so the backstop work still happens on an
//! idle tenant, just hourly; an invocation that is not a scheduled event (a manual invoke, a CodeBuild
//! state change) is never gated, and neither is a tick whose check failed (fail open).
//!
//! A tick that runs in full also says how soon the next full one is sure to follow
//! ([`Why::next_full_within_secs`]): the status probe stamps that on its samples, so the status page
//! can tell an idle tenant's hour-old sample from a reconciler that stopped reporting.
//!
//! The decisions are pure functions ([`by_schedule`], [`activity_of`], [`by_activity`]); the
//! Batch check (feature `aws`) is [`check`].

use std::fmt::Display;

use chrono::Timelike;

/// How long after a coordinator stops its run still counts as activity: long enough for the
/// work that follows a run's end (its last events folded, its provenance crate written after the
/// 10-minute settle, its jobs priced) to happen at the 5-minute cadence.
pub const RECENT_MILLIS: i64 = 15 * 60 * 1000;

/// The schedule's period: a tick every 5 minutes.
pub const TICK_SECS: i64 = 5 * 60;

/// The most time between two full ticks on an idle tenant: the hour's first tick always runs.
pub const HOURLY_SECS: i64 = 60 * 60;

/// Why a tick runs in full.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Why {
    /// Not a scheduled event: a manual invoke, or an event of another kind.
    NotScheduled,
    /// The hour's first tick (its scheduled time is in the hour's first five minutes).
    Hourly,
    /// A coordinator job is active.
    Active,
    /// A coordinator job stopped within [`RECENT_MILLIS`].
    RecentlyStopped,
    /// The activity check failed, so the tick runs rather than risk skipping real work.
    CheckFailed,
}

impl Why {
    /// The most seconds until the next tick that is sure to run in full, as this one's reason
    /// tells it. Only an active coordinator promises the next scheduled tick: it is then either
    /// still active or stopped within one period, so within [`RECENT_MILLIS`]. Every other reason
    /// promises only the hour's first tick, even when the next tick may well run (a recently
    /// stopped job, a failing check): over-promising would report a healthy idle tenant down.
    pub fn next_full_within_secs(self) -> i64 {
        match self {
            Why::Active => TICK_SECS,
            Why::NotScheduled | Why::Hourly | Why::RecentlyStopped | Why::CheckFailed => {
                HOURLY_SECS
            }
        }
    }
}

/// What a scheduled tick does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tick {
    /// Its full work.
    Full(Why),
    /// Nothing: no database access, so an idle cluster stays paused.
    Idle,
}

/// What the coordinator queues show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Activity {
    /// A job is SUBMITTED, PENDING, RUNNABLE, STARTING or RUNNING.
    Active,
    /// No job is active, but one stopped within [`RECENT_MILLIS`].
    RecentlyStopped,
    /// Neither.
    Idle,
}

/// One job summary as the check reads it: its status (Batch's name) and, once stopped, when (epoch
/// milliseconds).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobSeen<'a> {
    pub status: &'a str,
    pub stopped_at_millis: Option<i64>,
}

/// The decision the invoke payload alone makes: `Some` when the tick runs in full whatever the
/// queues show, `None` when the activity check decides. Only an EventBridge scheduled event
/// (`detail-type` `Scheduled Event` with a `time`) is gated, and its hour's first tick is not:
/// the schedule fires on the minutes that are multiples of 5, so exactly one tick an hour has a
/// minute below 5. A scheduled event whose `time` does not parse runs in full.
pub fn by_schedule(payload: &serde_json::Value) -> Option<Why> {
    if payload.get("detail-type").and_then(|v| v.as_str()) != Some("Scheduled Event") {
        return Some(Why::NotScheduled);
    }
    let minute = payload
        .get("time")
        .and_then(|v| v.as_str())
        .and_then(|time| chrono::DateTime::parse_from_rfc3339(time).ok())
        .map(|time| time.minute());
    match minute {
        Some(minute) if minute >= 5 => None,
        Some(_) => Some(Why::Hourly),
        None => Some(Why::NotScheduled),
    }
}

/// What `jobs`, the summaries listed from the coordinator queues, show at `now_millis`.
pub fn activity_of<'a>(jobs: impl IntoIterator<Item = JobSeen<'a>>, now_millis: i64) -> Activity {
    let mut activity = Activity::Idle;
    for job in jobs {
        if ACTIVE_STATUSES.contains(&job.status) {
            return Activity::Active;
        }
        if job.stopped_at_millis.is_some_and(|at| now_millis - at <= RECENT_MILLIS) {
            activity = Activity::RecentlyStopped;
        }
    }
    activity
}

/// The tick's decision from the activity check's outcome: in full unless the queues are idle,
/// and in full when the check failed (fail open).
pub fn by_activity(activity: Result<Activity, impl Display>) -> Tick {
    match activity {
        Ok(Activity::Active) => Tick::Full(Why::Active),
        Ok(Activity::RecentlyStopped) => Tick::Full(Why::RecentlyStopped),
        Ok(Activity::Idle) => Tick::Idle,
        Err(error) => {
            tracing::warn!(%error, "idle check failed: running the tick in full");
            Tick::Full(Why::CheckFailed)
        }
    }
}

/// The Batch job statuses of a job that has not stopped.
pub const ACTIVE_STATUSES: [&str; 5] = ["SUBMITTED", "PENDING", "RUNNABLE", "STARTING", "RUNNING"];

/// The terminal statuses, whose recent jobs count as activity.
#[cfg(feature = "aws")]
const STOPPED_STATUSES: [&str; 2] = ["SUCCEEDED", "FAILED"];

/// The most ListJobs pages of one terminal status a check reads per queue. Batch keeps terminal
/// jobs for days and does not document the order it lists them in, so the check reads them all;
/// past this many (thousands of runs in Batch's retention), it gives up and the tick runs in full.
#[cfg(feature = "aws")]
const MAX_STOPPED_PAGES: usize = 10;

/// The tick's decision for an invoke `payload` from the reconcile or sweep schedule: the payload's
/// own ([`by_schedule`]), else what `queues` (coordinator queue names or ARNs) show now. It logs
/// one line when the tick is skipped as idle.
#[cfg(feature = "aws")]
pub async fn check(
    batch: &aws_sdk_batch::Client,
    queues: &[String],
    payload: &serde_json::Value,
) -> Tick {
    if let Some(why) = by_schedule(payload) {
        return Tick::Full(why);
    }
    let now = biffix_core::clock::millis(biffix_core::clock::since_epoch());
    let tick = by_activity(queue_activity(batch, queues, now).await);
    if tick == Tick::Idle {
        tracing::info!(
            "tick skipped as idle: no coordinator job active or stopped in the last {} min",
            RECENT_MILLIS / 60_000
        );
    }
    tick
}

/// What `queues` show at `now_millis`: each active status is one ListJobs call of at most one job
/// (the first found answers), then each terminal status is paged for a recent `stoppedAt`.
#[cfg(feature = "aws")]
async fn queue_activity(
    batch: &aws_sdk_batch::Client,
    queues: &[String],
    now_millis: i64,
) -> Result<Activity, String> {
    use aws_sdk_batch::error::DisplayErrorContext;
    use aws_sdk_batch::types::JobStatus;

    // With no queue to look at, nothing would ever look active: run in full rather than skip.
    if queues.is_empty() {
        return Err("no coordinator queue configured".to_owned());
    }
    let list_error = |queue: &str, error: &dyn std::error::Error| {
        format!("ListJobs {queue}: {}", DisplayErrorContext(error))
    };
    for queue in queues {
        for status in ACTIVE_STATUSES {
            let page = batch
                .list_jobs()
                .job_queue(queue)
                .job_status(JobStatus::from(status))
                .max_results(1)
                .send()
                .await
                .map_err(|e| list_error(queue, &e))?;
            if !page.job_summary_list().is_empty() {
                return Ok(Activity::Active);
            }
        }
        for status in STOPPED_STATUSES {
            let mut pages = batch
                .list_jobs()
                .job_queue(queue)
                .job_status(JobStatus::from(status))
                .into_paginator()
                .page_size(100)
                .send();
            let mut read = 0;
            while let Some(page) = pages.next().await {
                if read == MAX_STOPPED_PAGES {
                    return Err(format!(
                        "{queue}: more than {MAX_STOPPED_PAGES} pages of {status} jobs"
                    ));
                }
                read += 1;
                let page = page.map_err(|e| list_error(queue, &e))?;
                let jobs = page.job_summary_list().iter().map(|job| JobSeen {
                    status: job.status().map_or("", JobStatus::as_str),
                    stopped_at_millis: job.stopped_at(),
                });
                let activity = activity_of(jobs, now_millis);
                if activity != Activity::Idle {
                    return Ok(activity);
                }
            }
        }
    }
    Ok(Activity::Idle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn scheduled(time: &str) -> serde_json::Value {
        json!({"detail-type": "Scheduled Event", "source": "aws.events", "time": time, "detail": {}})
    }

    fn job(status: &str, stopped_at_millis: Option<i64>) -> JobSeen<'_> {
        JobSeen { status, stopped_at_millis }
    }

    const NOW: i64 = 1_800_000_000_000;

    #[test]
    fn the_hours_first_tick_and_every_other_invoke_run_in_full() {
        assert_eq!(by_schedule(&scheduled("2026-10-22T13:00:00Z")), Some(Why::Hourly));
        assert_eq!(by_schedule(&scheduled("2026-10-22T13:04:59Z")), Some(Why::Hourly));
        // The rest of the hour's ticks ask the queues.
        assert_eq!(by_schedule(&scheduled("2026-10-22T13:05:00Z")), None);
        assert_eq!(by_schedule(&scheduled("2026-10-22T13:55:00Z")), None);
        // A manual invoke, a CodeBuild state change and an unreadable time are never gated.
        assert_eq!(by_schedule(&json!({})), Some(Why::NotScheduled));
        assert_eq!(
            by_schedule(&json!({"detail-type": "CodeBuild Build State Change", "time": "x"})),
            Some(Why::NotScheduled)
        );
        assert_eq!(by_schedule(&scheduled("not a time")), Some(Why::NotScheduled));
    }

    #[test]
    fn an_active_or_recently_stopped_coordinator_keeps_the_tick() {
        // Active, whatever else is listed.
        for status in ACTIVE_STATUSES {
            let jobs = [job("SUCCEEDED", Some(NOW - 3_600_000)), job(status, None)];
            assert_eq!(activity_of(jobs, NOW), Activity::Active, "{status}");
        }
        // Stopped within the window, at its edge, and just past it.
        assert_eq!(
            activity_of([job("FAILED", Some(NOW - 60_000))], NOW),
            Activity::RecentlyStopped
        );
        assert_eq!(
            activity_of([job("SUCCEEDED", Some(NOW - RECENT_MILLIS))], NOW),
            Activity::RecentlyStopped
        );
        assert_eq!(
            activity_of([job("SUCCEEDED", Some(NOW - RECENT_MILLIS - 1))], NOW),
            Activity::Idle
        );
        // No jobs at all.
        assert_eq!(activity_of([], NOW), Activity::Idle);
    }

    #[test]
    fn only_idle_queues_skip_and_a_failed_check_runs_in_full() {
        assert_eq!(by_activity(Ok::<_, String>(Activity::Active)), Tick::Full(Why::Active));
        assert_eq!(
            by_activity(Ok::<_, String>(Activity::RecentlyStopped)),
            Tick::Full(Why::RecentlyStopped)
        );
        assert_eq!(by_activity(Ok::<_, String>(Activity::Idle)), Tick::Idle);
        assert_eq!(by_activity(Err("AccessDenied")), Tick::Full(Why::CheckFailed));
    }

    #[test]
    fn only_an_active_coordinator_promises_the_next_tick() {
        assert_eq!(Why::Active.next_full_within_secs(), TICK_SECS);
        for why in [Why::NotScheduled, Why::Hourly, Why::RecentlyStopped, Why::CheckFailed] {
            assert_eq!(why.next_full_within_secs(), HOURLY_SECS, "{why:?}");
        }
    }
}
