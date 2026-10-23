//! The `/status` page's component health: the probe's pure classifiers, from the signals the
//! reconcile tick gathers (`tick::probe`, which writes one `status_samples` row per component every
//! tick through `store::status`). Pure, so every rule is unit-tested without AWS or a database.

/// A component's health at a point in time, stored as its `as_str` in `status_samples.status`,
/// which the backend reads as `biffix_api::HealthStatus` (the same spellings).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Health {
    Ok,
    Degraded,
    Down,
}

impl Health {
    pub fn as_str(self) -> &'static str {
        match self {
            Health::Ok => "ok",
            Health::Degraded => "degraded",
            Health::Down => "down",
        }
    }
}

// Fixed thresholds for the read-model-lag ("too far behind") signal. They default to the same 900s
// as the reconciler's stale window (`tick::describe`'s STALE_AFTER) but are deliberately
// independent of it: this measures how far the read model trails the log, a different concern from
// how long a Batch job may sit non-terminal.
const INGEST_DOWN_SECS: i64 = 900;
const INGEST_DEGRADED_SECS: i64 = 120;
// A run still `submitted` longer than this is "stuck" (Batch hasn't placed the coordinator). But
// only within a recent window — a run stuck for many hours is an abandoned zombie, not evidence
// Batch is failing *now*, so it must not perma-degrade the live signal. A genuine current outage
// still surfaces via freshly-submitted runs entering the window.
pub const STUCK_RUN_SECS: i64 = 900;
pub const STUCK_RUN_MAX_SECS: i64 = 7200;
// The ingest queue's oldest un-consumed message. A fan-out burst drains in seconds, so a message
// this old means the ingest Lambda is not consuming (throttled, crashing, or disabled).
const INGEST_QUEUE_DEGRADED_SECS: i64 = 300;
const INGEST_QUEUE_DOWN_SECS: i64 = 900;
/// The result of probing the API's public readiness route: the HTTP status, or why no response
/// came back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ApiProbe {
    /// No readiness URL is configured (no public app domain), so the API was not probed.
    NotConfigured,
    Status(u16),
    Unreachable(String),
}

/// Classify API health from its readiness probe. `/readyz` answers 200 when the backend can reach
/// its database and 503 when it cannot; anything else, or no answer, means callers cannot use it.
pub fn classify_api(probe: &ApiProbe) -> (Health, Option<String>) {
    match probe {
        ApiProbe::NotConfigured => {
            (Health::Ok, Some("not probed (no public app domain configured)".into()))
        }
        ApiProbe::Status(200) => (Health::Ok, None),
        ApiProbe::Status(503) => (
            Health::Down,
            Some("the API is up but not ready (it cannot reach its database)".into()),
        ),
        ApiProbe::Status(code) => {
            (Health::Degraded, Some(format!("readiness check returned HTTP {code}")))
        }
        ApiProbe::Unreachable(why) => (Health::Down, Some(format!("API unreachable: {why}"))),
    }
}

/// Classify read-model / ingest health from the age (seconds) of the oldest un-folded event (`None`
/// = fully folded) and the dead-letter-queue depth. The PRIMARY queue depth is deliberately NOT an
/// input: it is >0 during every fan-out burst and drains in seconds, so it is noise, not health.
pub fn classify_ingest(
    unfolded_age_secs: Option<i64>,
    dlq_depth: i64,
    queue_age_secs: i64,
) -> (Health, Option<String>) {
    if queue_age_secs > INGEST_QUEUE_DOWN_SECS {
        return (
            Health::Down,
            Some(format!(
                "ingest queue's oldest event has waited {queue_age_secs}s — events are not being ingested"
            )),
        );
    }
    if let Some(age) = unfolded_age_secs.filter(|&age| age > INGEST_DOWN_SECS) {
        return (Health::Down, Some(format!("read model {age}s behind the event log")));
    }
    if dlq_depth > 0 {
        return (
            Health::Degraded,
            Some(format!("{dlq_depth} event(s) in the dead-letter queue — redrive required")),
        );
    }
    if let Some(age) = unfolded_age_secs.filter(|&age| age > INGEST_DEGRADED_SECS) {
        return (Health::Degraded, Some(format!("read model {age}s behind the event log")));
    }
    if queue_age_secs > INGEST_QUEUE_DEGRADED_SECS {
        return (
            Health::Degraded,
            Some(format!("ingest queue's oldest event has waited {queue_age_secs}s")),
        );
    }
    (Health::Ok, None)
}

/// Classify Batch launch health. A bad COORDINATOR queue/compute-environment is DOWN — nothing can
/// launch. A bad WORKER queue-class, or runs stuck in `submitted`, is DEGRADED — submits land but
/// tasks may never place. Each `*_problem` is what is wrong with that queue set (`None` when it is
/// healthy).
pub fn classify_batch(
    coordinator_problem: Option<String>,
    worker_problem: Option<String>,
    stuck_runs: i64,
) -> (Health, Option<String>) {
    if let Some(problem) = coordinator_problem {
        return (Health::Down, Some(problem));
    }
    if let Some(problem) = worker_problem {
        return (Health::Degraded, Some(problem));
    }
    if stuck_runs > 0 {
        return (
            Health::Degraded,
            Some(format!(
                "{stuck_runs} run(s) stuck in submitted for over {}m",
                STUCK_RUN_SECS / 60
            )),
        );
    }
    (Health::Ok, None)
}

/// Classify the deploy's CloudWatch alarms: any in ALARM is degraded, naming each by its
/// description (else its name). The alarms already page by email; this puts them on the status
/// page.
pub fn classify_alarms(firing: &[(String, Option<String>)]) -> (Health, Option<String>) {
    if firing.is_empty() {
        return (Health::Ok, None);
    }
    let what: Vec<&str> =
        firing.iter().map(|(name, description)| description.as_deref().unwrap_or(name)).collect();
    (
        Health::Degraded,
        Some(format!(
            "{} alarm{} firing: {}",
            firing.len(),
            if firing.len() == 1 { "" } else { "s" },
            what.join(" ")
        )),
    )
}

/// The first requested name absent from `returned`, as a "`<label> <name>` not found" detail.
/// Both DescribeJobQueues and DescribeComputeEnvironments silently omit a requested name that does
/// not exist, so a missing entry is DOWN, not healthy — this reports the first such name.
pub fn first_missing(
    requested: &[String],
    returned: &std::collections::HashSet<&str>,
    label: &str,
) -> Option<String> {
    requested
        .iter()
        .find(|name| !returned.contains(name.as_str()))
        .map(|name| format!("{label} {name} not found"))
}

/// Classify ingest health when some inputs may be unknown (`None` = could not be read this tick).
/// The known inputs are classified as usual; any unknown one makes the component at least Degraded,
/// naming the signal, because an unread input must never read as healthy (`None` unfolded age means
/// "caught up", and a zero DLQ depth or queue age means "nothing waiting").
pub fn classify_ingest_signals(
    unfolded_age_secs: Option<Option<i64>>,
    dlq_depth: Option<i64>,
    queue_age_secs: Option<i64>,
) -> (Health, Option<String>) {
    let unread: Vec<&str> = [
        ("unfolded_age_secs", unfolded_age_secs.is_none()),
        ("dlq_depth", dlq_depth.is_none()),
        ("ingest_queue_age_secs", queue_age_secs.is_none()),
    ]
    .into_iter()
    .filter_map(|(name, missing)| missing.then_some(name))
    .collect();
    with_unread(
        classify_ingest(
            unfolded_age_secs.flatten(),
            dlq_depth.unwrap_or(0),
            queue_age_secs.unwrap_or(0),
        ),
        &unread,
    )
}

/// [`classify_batch`] when the stuck-run count may be unknown (`None` = the query failed): an
/// unknown count makes the component at least Degraded, naming `stuck_run_count`.
pub fn classify_batch_signals(
    coordinator_problem: Option<String>,
    worker_problem: Option<String>,
    stuck_runs: Option<i64>,
) -> (Health, Option<String>) {
    let unread: &[&str] = if stuck_runs.is_none() { &["stuck_run_count"] } else { &[] };
    with_unread(
        classify_batch(coordinator_problem, worker_problem, stuck_runs.unwrap_or(0)),
        unread,
    )
}

/// Fold "these inputs could not be read" into a classification: unchanged when every input was
/// read; otherwise at least Degraded (a Down from the known inputs stays Down), with the unread
/// signals named ahead of any detail the known inputs produced.
fn with_unread(classified: (Health, Option<String>), unread: &[&str]) -> (Health, Option<String>) {
    if unread.is_empty() {
        return classified;
    }
    let (health, detail) = classified;
    let health = if health == Health::Down { Health::Down } else { Health::Degraded };
    let note = format!("could not read {}", unread.join(", "));
    let detail = match detail {
        Some(detail) => format!("{note}; {detail}"),
        None => note,
    };
    (health, Some(detail))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn firing_alarms_degrade_and_are_named_by_description() {
        assert_eq!(classify_alarms(&[]), (Health::Ok, None));
        let (h, d) = classify_alarms(&[
            ("biffix-x-ingest-DlqAlarm".into(), Some("Events are in the DLQ.".into())),
            ("biffix-x-api-5xx".into(), None),
        ]);
        assert_eq!(h, Health::Degraded);
        assert_eq!(d.as_deref(), Some("2 alarms firing: Events are in the DLQ. biffix-x-api-5xx"));
    }

    #[test]
    fn ingest_ok_when_caught_up_and_no_dlq() {
        assert_eq!(classify_ingest(None, 0, 0), (Health::Ok, None));
        assert_eq!(classify_ingest(Some(5), 0, 0), (Health::Ok, None));
        // Every input read and healthy: the signal-aware wrapper agrees.
        assert_eq!(classify_ingest_signals(Some(None), Some(0), Some(0)), (Health::Ok, None));
    }

    #[test]
    fn ingest_unread_inputs_degrade_and_are_named() {
        // A failed query/API call is not "caught up" / "empty DLQ" / "empty queue".
        assert_eq!(
            classify_ingest_signals(None, Some(0), Some(0)),
            (Health::Degraded, Some("could not read unfolded_age_secs".into()))
        );
        assert_eq!(
            classify_ingest_signals(Some(None), None, None),
            (Health::Degraded, Some("could not read dlq_depth, ingest_queue_age_secs".into()))
        );
        // A known input that is already Down stays Down, and keeps its own detail.
        assert_eq!(
            classify_ingest_signals(Some(Some(1000)), None, Some(0)),
            (
                Health::Down,
                Some("could not read dlq_depth; read model 1000s behind the event log".into())
            )
        );
        // A known Degraded input keeps its detail after the unread note.
        assert_eq!(
            classify_ingest_signals(None, Some(2), Some(0)),
            (
                Health::Degraded,
                Some(
                    "could not read unfolded_age_secs; 2 event(s) in the dead-letter queue — \
                     redrive required"
                        .into()
                )
            )
        );
    }

    #[test]
    fn batch_unread_stuck_count_degrades() {
        assert_eq!(
            classify_batch_signals(None, None, None),
            (Health::Degraded, Some("could not read stuck_run_count".into()))
        );
        assert_eq!(classify_batch_signals(None, None, Some(0)), (Health::Ok, None));
        // A bad coordinator queue is still Down.
        assert_eq!(
            classify_batch_signals(Some("ce invalid".into()), None, None),
            (Health::Down, Some("could not read stuck_run_count; ce invalid".into()))
        );
    }

    #[test]
    fn first_missing_names_the_first_absent_request() {
        let returned: std::collections::HashSet<&str> = ["a"].into_iter().collect();
        assert_eq!(
            first_missing(&["a".into(), "b".into()], &returned, "job queue"),
            Some("job queue b not found".into())
        );
        // A name requested by ARN counts as present when the response carries that ARN.
        let arn = "arn:aws:batch:us-east-1:1:job-queue/b";
        let returned: std::collections::HashSet<&str> = ["a", "b", arn].into_iter().collect();
        assert_eq!(first_missing(&["a".into(), arn.into()], &returned, "job queue"), None);
        // Order of the request decides which missing name is reported.
        let returned: std::collections::HashSet<&str> = std::collections::HashSet::new();
        assert_eq!(
            first_missing(&["x".into(), "y".into()], &returned, "compute environment"),
            Some("compute environment x not found".into())
        );
        assert_eq!(first_missing(&[], &returned, "job queue"), None);
    }

    #[test]
    fn ingest_degraded_on_dlq_or_moderate_lag() {
        assert_eq!(classify_ingest(None, 3, 0).0, Health::Degraded);
        assert_eq!(classify_ingest(Some(300), 0, 0).0, Health::Degraded);
    }

    #[test]
    fn ingest_down_on_large_lag_beats_dlq() {
        let (h, _) = classify_ingest(Some(1000), 5, 0);
        assert_eq!(h, Health::Down);
    }

    #[test]
    fn batch_down_on_bad_coordinator() {
        let (h, d) = classify_batch(Some("ce invalid".into()), None, 0);
        assert_eq!(h, Health::Down);
        assert_eq!(d.as_deref(), Some("ce invalid"));
    }

    #[test]
    fn batch_degraded_on_worker_or_stuck_runs() {
        assert_eq!(classify_batch(None, Some("x".into()), 0), (Health::Degraded, Some("x".into())));
        assert_eq!(
            classify_batch(None, None, 3),
            (Health::Degraded, Some("3 run(s) stuck in submitted for over 15m".into()))
        );
        assert_eq!(classify_batch(None, None, 0), (Health::Ok, None));
    }

    #[test]
    fn ingest_queue_backlog_age_degrades_then_downs() {
        assert_eq!(classify_ingest(None, 0, 60).0, Health::Ok);
        assert_eq!(
            classify_ingest(None, 0, 301),
            (Health::Degraded, Some("ingest queue's oldest event has waited 301s".into()))
        );
        assert_eq!(classify_ingest(None, 0, 901).0, Health::Down);
        // A stuck queue outranks a non-empty DLQ.
        assert_eq!(classify_ingest(None, 3, 901).0, Health::Down);
    }

    #[test]
    fn api_health_follows_the_readiness_probe() {
        assert_eq!(classify_api(&ApiProbe::Status(200)), (Health::Ok, None));
        assert_eq!(classify_api(&ApiProbe::Status(503)).0, Health::Down);
        assert_eq!(classify_api(&ApiProbe::Status(502)).0, Health::Degraded);
        assert_eq!(
            classify_api(&ApiProbe::Unreachable("dns".into())),
            (Health::Down, Some("API unreachable: dns".into()))
        );
        // Unprobed is not a failure, but says so.
        assert_eq!(
            classify_api(&ApiProbe::NotConfigured),
            (Health::Ok, Some("not probed (no public app domain configured)".into()))
        );
    }
}
