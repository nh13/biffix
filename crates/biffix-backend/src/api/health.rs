//! Liveness, readiness, and the endpoints behind the service status page.

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::{Extension, Json};
use biffix_api::StatusHistoryQuery;

use crate::api::ApiError;
use crate::auth::actor::{authorize_msg, AuthedActor};
use crate::auth::rbac::Capability;
use crate::store::status::{StatusBucketView, StatusComponentView};
use crate::AppState;

// The status tier, report and history types are the shared wire contract (biffix-api). The tier's
// worse/parse helpers live on the type there; the handler rolls components up with them below.
pub(crate) use biffix_api::{
    ComponentHealth, ComponentHistory, HealthStatus, StatusHistory, StatusReport, UptimeBucket,
};

/// Liveness — the process is up. Always 200 if we can serve.
#[utoipa::path(get, path = "/healthz", responses((status = 200, description = "alive")))]
pub(crate) async fn healthz() -> StatusCode {
    StatusCode::OK
}

/// Readiness — dependencies reachable (the projection store answers).
#[utoipa::path(get, path = "/readyz", responses((status = 200), (status = 503)))]
pub(crate) async fn readyz(State(state): State<AppState>) -> StatusCode {
    match state.store.health_check().await {
        Ok(()) => StatusCode::OK,
        Err(_) => StatusCode::SERVICE_UNAVAILABLE,
    }
}

/// The reconcile schedule's period, and the most time between two full ticks on an idle tenant
/// (`biffix_db::idle::{TICK_SECS, HOURLY_SECS}`, which the API surface builds without).
const TICK_SECS: i64 = 300;
const HOURLY_SECS: i64 = 3_600;
/// How long past its promised next sample a sample still counts as fresh: one tick, for the probe
/// running late in its tick (an Aurora resume, a slow reconcile step).
const GRACE_SECS: i64 = TICK_SECS;

/// Whether `sample` is still current at `now`: it is no older than the time it promised its next
/// sample within, plus [`GRACE_SECS`]. The probe samples only on a tick that runs in full: every 5
/// minutes while a coordinator is active, but only hourly on an idle tenant, so a fixed 5-minute
/// heartbeat would call an idle tenant's reconciler dead. Each sample says which: a tick that saw
/// an active coordinator promises the next one within 5 minutes, any other within the hour. So a
/// reconciler that stops is seen within 10 minutes of a sample that saw a run, and at most 65
/// minutes after any other; a sample with no promise (written before samples carried one) is held
/// to the hour. Telling idle from down this way needs no database write on a skipped tick, which
/// would keep the database from pausing.
fn fresh(sample: &StatusComponentView, now: f64) -> bool {
    let promised = sample.next_within_secs.unwrap_or(HOURLY_SECS);
    let max_age = promised.saturating_add(GRACE_SECS) as f64;
    sample.sampled_at.is_some_and(|at| now - at <= max_age)
}

/// The components whose health is sampled by the reconcile probe (vs. computed live in the handler).
const SAMPLED: &[&str] = &["event_ingest", "batch_compute", "reconciler", "alarms"];

/// The service-status page data. Reads ONLY Postgres: a live `SELECT 1` for the database plus the
/// latest probe sample per component (the reconcile lambda does the AWS calls and writes samples).
/// A stale sample is escalated — the probe not reporting is itself a fault.
#[utoipa::path(get, path = "/status", responses((status = 200, body = StatusReport), (status = 403)))]
pub(crate) async fn status(
    State(state): State<AppState>,
    Extension(actor): Extension<AuthedActor>,
) -> Result<Json<StatusReport>, ApiError> {
    authorize_msg(&state, &actor, Capability::ViewRuns)?;
    let db_ok = state.store.health_check().await.is_ok();
    // The page still renders when the samples cannot be read (the database row says why), but the
    // sampled components say so rather than claiming the probe never ran.
    let samples = state
        .store
        .status_components()
        .await
        .inspect_err(|e| tracing::warn!(error = %e, "status: status samples unavailable"))
        .ok();
    Ok(Json(report(db_ok, samples.as_deref(), biffix_core::clock::epoch_secs())))
}

/// Roll the live checks and the probe's latest samples (`None`: they could not be read) up into the
/// status report, as of `now` (epoch seconds).
fn report(db_ok: bool, samples: Option<&[StatusComponentView]>, now: f64) -> StatusReport {
    let find = |name: &str| samples.and_then(|s| s.iter().find(|c| c.name == name));
    let fresh = |c: &StatusComponentView| fresh(c, now);

    // The API answered this request, but the probe checks it the way users reach it (through the
    // public URL and CDN) and whether it can reach its database: a fresh probe result wins.
    let (api_status, api_detail) = match find("api") {
        Some(c) if fresh(c) => (HealthStatus::parse(&c.status), c.detail.clone()),
        _ => (HealthStatus::Ok, None),
    };
    let mut components = vec![
        ComponentHealth { name: "api".into(), status: api_status, detail: api_detail },
        ComponentHealth {
            name: "database".into(),
            status: if db_ok { HealthStatus::Ok } else { HealthStatus::Down },
            detail: (!db_ok).then(|| "unreachable".to_string()),
        },
    ];

    for &name in SAMPLED {
        let (status, detail) = match (samples.is_some(), find(name)) {
            (false, _) => (HealthStatus::Degraded, Some("status samples unavailable".to_string())),
            (true, None) => {
                (HealthStatus::Degraded, Some("awaiting first status sample".to_string()))
            }
            (true, Some(c)) if fresh(c) => (HealthStatus::parse(&c.status), c.detail.clone()),
            // The probe isn't reporting: the reconciler is down; other sampled signals are unknown
            // (at least degraded), keeping whatever worse status was last seen.
            (true, Some(_)) if name == "reconciler" => {
                (HealthStatus::Down, Some("not reporting".to_string()))
            }
            (true, Some(c)) => (
                HealthStatus::parse(&c.status).worse(HealthStatus::Degraded),
                Some("status probe stale".to_string()),
            ),
        };
        components.push(ComponentHealth { name: name.into(), status, detail });
    }

    let overall = components.iter().fold(HealthStatus::Ok, |acc, c| acc.worse(c.status));
    StatusReport { status: overall, components }
}

/// The probe cadence while a run is active (seconds); the "expected samples per bucket" divisor.
const PROBE_CADENCE_SECS: i64 = TICK_SECS;
/// Target number of history bars, kept the same across windows so the strip's granularity is
/// consistent regardless of the selected range. The bucket width is `window / TARGET_BARS`, floored
/// at the probe cadence (so short windows aren't split finer than the data sampled — a 1h window
/// yields fewer bars because only ~12 samples exist).
const TARGET_BARS: i64 = 90;
/// The window an absent `?window=` means.
const DEFAULT_WINDOW: &str = "90d";

/// Resolve a window keyword to `(window_secs, bucket_secs)`, or `None` for an unrecognized keyword.
/// The bucket is sized to hit TARGET_BARS for that window (floored at the probe cadence), giving a
/// uniform bar count across ranges. An unrecognized *explicit* value is a client error (mirroring
/// `runs::parse_filter`), not a silent fall-through to 90d — only an absent `window` defaults.
fn window_spec(window: &str) -> Option<(i64, i64)> {
    let window_secs: i64 = match window {
        "1h" => 3_600,
        "1d" => 86_400,
        "1w" => 604_800,
        "90d" => 7_776_000,
        _ => return None,
    };
    let bucket_secs = (window_secs / TARGET_BARS).max(PROBE_CADENCE_SECS);
    Some((window_secs, bucket_secs))
}

/// Bucketed uptime history for the status bars, from the probe's `status_samples`.
#[utoipa::path(
    get, path = "/status/history",
    params(StatusHistoryQuery),
    responses(
        (status = 200, body = StatusHistory),
        (status = 400, description = "an unknown window"),
        (status = 403),
        (status = 503, description = "the status samples could not be read"),
    )
)]
pub(crate) async fn status_history(
    State(state): State<AppState>,
    Extension(actor): Extension<AuthedActor>,
    Query(q): Query<StatusHistoryQuery>,
) -> Result<Json<StatusHistory>, ApiError> {
    authorize_msg(&state, &actor, Capability::ViewRuns)?;
    // Absent `window` defaults to 90d; an explicit but unrecognized value is a 400 (not a silent
    // default), consistent with the `filter` param on `GET /runs`.
    let window = q.window.as_deref().unwrap_or(DEFAULT_WINDOW);
    let (window_secs, bucket_secs) = window_spec(window).ok_or_else(|| {
        ApiError::BadRequest(format!("unknown window `{window}` (1h, 1d, 1w or 90d)"))
    })?;
    // No history is not an empty history: a store failure is reported, not drawn as no bars.
    let rows = state.store.status_history(window_secs, bucket_secs).await?;
    Ok(Json(StatusHistory {
        bucket_secs,
        window_secs,
        expected_per_bucket: (bucket_secs / PROBE_CADENCE_SECS).max(1),
        components: group_history(rows),
    }))
}

/// Group the flat (bucket, component) rollup into per-component series, components in order of
/// first appearance and each one's buckets in row order.
fn group_history(rows: Vec<StatusBucketView>) -> Vec<ComponentHistory> {
    let mut components: Vec<ComponentHistory> = Vec::new();
    for row in rows {
        let bar =
            UptimeBucket { t: row.bucket, ok: row.ok, degraded: row.degraded, down: row.down };
        match components.iter_mut().find(|c| c.name == row.component) {
            Some(c) => c.buckets.push(bar),
            None => components.push(ComponentHistory { name: row.component, buckets: vec![bar] }),
        }
    }
    components
}

// ── routes ───────────────────────────────────────────────────────────────────────────────────────

/// Liveness, readiness and service status: the routes behind the Cognito middleware.
pub(super) fn routes() -> axum::Router<AppState> {
    axum::Router::new()
        .route("/status", axum::routing::get(status))
        .route("/status/history", axum::routing::get(status_history))
}

/// Liveness, readiness and service status: the routes open before a caller has a token.
pub(super) fn open_routes() -> axum::Router<AppState> {
    axum::Router::new()
        .route("/healthz", axum::routing::get(healthz))
        .route("/readyz", axum::routing::get(readyz))
}

/// Liveness, readiness and service status: the OpenAPI operations, tagged `health` in
/// [`super::ApiDoc`].
#[derive(utoipa::OpenApi)]
#[openapi(
    paths(healthz, readyz, status, status_history),
    components(schemas(
        StatusReport,
        ComponentHealth,
        HealthStatus,
        biffix_api::StatusHistory,
        biffix_api::ComponentHistory,
        biffix_api::UptimeBucket
    ))
)]
pub(super) struct Doc;

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: f64 = 1_790_000_000.0;
    /// The oldest a sample from a tick that saw an active coordinator stays fresh: 10 minutes.
    const ACTIVE_MAX_AGE: f64 = (TICK_SECS + GRACE_SECS) as f64;
    /// The oldest any other tick's sample stays fresh: 65 minutes.
    const IDLE_MAX_AGE: f64 = (HOURLY_SECS + GRACE_SECS) as f64;

    /// A sample `age_secs` old from a tick that saw an active coordinator.
    fn sample(name: &str, status: &str, age_secs: Option<f64>) -> StatusComponentView {
        StatusComponentView {
            name: name.into(),
            status: status.into(),
            detail: Some(format!("{name} detail")),
            sampled_at: age_secs.map(|age| NOW - age),
            next_within_secs: Some(TICK_SECS),
        }
    }

    /// The same sample from a tick that promised only the hour's next (`None`: no promise at all).
    fn promising(mut sample: StatusComponentView, next: Option<i64>) -> StatusComponentView {
        sample.next_within_secs = next;
        sample
    }

    fn component<'a>(r: &'a StatusReport, name: &str) -> &'a ComponentHealth {
        r.components.iter().find(|c| c.name == name).unwrap()
    }

    #[test]
    fn fresh_samples_report_as_sampled() {
        let samples = [
            sample("event_ingest", "ok", Some(10.0)),
            sample("batch_compute", "ok", Some(10.0)),
            sample("reconciler", "ok", Some(10.0)),
            sample("alarms", "degraded", Some(10.0)),
        ];
        let r = report(true, Some(&samples), NOW);
        assert_eq!(component(&r, "alarms").status, HealthStatus::Degraded);
        assert_eq!(component(&r, "alarms").detail.as_deref(), Some("alarms detail"));
        assert_eq!(component(&r, "reconciler").status, HealthStatus::Ok);
        assert_eq!(r.status, HealthStatus::Degraded, "the worst component");
    }

    #[test]
    fn a_sampled_tier_the_backend_cannot_read_is_not_shown_healthy() {
        // A tier a newer probe added, or a spelling that drifted from the API's.
        let samples = [
            sample("api", "bogus", Some(10.0)),
            sample("event_ingest", "ok", Some(10.0)),
            sample("batch_compute", "ok", Some(10.0)),
            sample("reconciler", "ok", Some(10.0)),
            sample("alarms", "Down", Some(10.0)),
        ];
        let r = report(true, Some(&samples), NOW);
        assert_eq!(component(&r, "api").status, HealthStatus::Degraded);
        assert_eq!(component(&r, "alarms").status, HealthStatus::Degraded);
        assert_eq!(r.status, HealthStatus::Degraded);
    }

    #[test]
    fn a_stale_reconciler_is_down_and_other_stale_samples_at_least_degraded() {
        let samples = [
            // Stale reconciler, even one last seen ok: the probe is not reporting.
            sample("reconciler", "ok", Some(ACTIVE_MAX_AGE + 1.0)),
            // Stale and last seen ok: unknown now, so degraded.
            sample("event_ingest", "ok", Some(ACTIVE_MAX_AGE + 1.0)),
            // Stale and last seen down: keeps the worse tier.
            sample("batch_compute", "down", Some(ACTIVE_MAX_AGE + 1.0)),
            // Never stamped: counts as stale.
            sample("alarms", "ok", None),
        ];
        let r = report(true, Some(&samples), NOW);
        let reconciler = component(&r, "reconciler");
        assert_eq!(reconciler.status, HealthStatus::Down);
        assert_eq!(reconciler.detail.as_deref(), Some("not reporting"));
        let ingest = component(&r, "event_ingest");
        assert_eq!(ingest.status, HealthStatus::Degraded);
        assert_eq!(ingest.detail.as_deref(), Some("status probe stale"));
        assert_eq!(component(&r, "batch_compute").status, HealthStatus::Down);
        assert_eq!(component(&r, "alarms").status, HealthStatus::Degraded);
        assert_eq!(r.status, HealthStatus::Down);
    }

    #[test]
    fn an_idle_tenants_hourly_sample_is_fresh_until_the_hour_has_passed() {
        // The status probe on an idle tenant: an hour-old sample is what the idle gate leaves.
        let idle = |age: f64| {
            [
                promising(sample("reconciler", "ok", Some(age)), Some(HOURLY_SECS)),
                promising(sample("event_ingest", "ok", Some(age)), Some(HOURLY_SECS)),
                promising(sample("batch_compute", "ok", Some(age)), Some(HOURLY_SECS)),
                // Written before samples carried a promise: held to the hour too.
                promising(sample("alarms", "ok", Some(age)), None),
            ]
        };
        for age in [ACTIVE_MAX_AGE + 1.0, 3_600.0, IDLE_MAX_AGE] {
            let r = report(true, Some(&idle(age)), NOW);
            assert_eq!(r.status, HealthStatus::Ok, "{age}s old");
            assert_eq!(component(&r, "reconciler").detail.as_deref(), Some("reconciler detail"));
        }
        // Past the hour and its grace, the reconciler has stopped reporting.
        let r = report(true, Some(&idle(IDLE_MAX_AGE + 1.0)), NOW);
        assert_eq!(component(&r, "reconciler").status, HealthStatus::Down);
        assert_eq!(component(&r, "reconciler").detail.as_deref(), Some("not reporting"));
        assert_eq!(component(&r, "event_ingest").detail.as_deref(), Some("status probe stale"));
        assert_eq!(component(&r, "alarms").detail.as_deref(), Some("status probe stale"));
        assert_eq!(r.status, HealthStatus::Down);
    }

    #[test]
    fn a_run_holds_the_reconciler_to_the_five_minute_tick() {
        // The last sample saw an active coordinator, so the next tick was sure to sample.
        let fresh = report(true, Some(&[sample("reconciler", "ok", Some(ACTIVE_MAX_AGE))]), NOW);
        assert_eq!(component(&fresh, "reconciler").status, HealthStatus::Ok);
        let stale =
            report(true, Some(&[sample("reconciler", "ok", Some(ACTIVE_MAX_AGE + 1.0))]), NOW);
        assert_eq!(component(&stale, "reconciler").status, HealthStatus::Down);
    }

    #[cfg(feature = "aws")]
    #[test]
    fn the_cadences_are_the_idle_gates() {
        assert_eq!(TICK_SECS, biffix_db::idle::TICK_SECS);
        assert_eq!(HOURLY_SECS, biffix_db::idle::HOURLY_SECS);
    }

    #[test]
    fn missing_and_unreadable_samples_are_told_apart() {
        let none = report(true, Some(&[]), NOW);
        for &name in SAMPLED {
            let c = component(&none, name);
            assert_eq!(c.status, HealthStatus::Degraded, "{name}");
            assert_eq!(c.detail.as_deref(), Some("awaiting first status sample"), "{name}");
        }
        let unreadable = report(false, None, NOW);
        for &name in SAMPLED {
            assert_eq!(
                component(&unreadable, name).detail.as_deref(),
                Some("status samples unavailable"),
                "{name}"
            );
        }
        assert_eq!(component(&unreadable, "database").status, HealthStatus::Down);
        assert_eq!(unreadable.status, HealthStatus::Down);
    }

    #[test]
    fn only_a_fresh_api_probe_overrides_the_live_answer() {
        let fresh = report(true, Some(&[sample("api", "down", Some(1.0))]), NOW);
        assert_eq!(component(&fresh, "api").status, HealthStatus::Down);
        let stale = report(true, Some(&[sample("api", "down", Some(ACTIVE_MAX_AGE + 1.0))]), NOW);
        assert_eq!(component(&stale, "api").status, HealthStatus::Ok);
    }

    #[test]
    fn each_window_has_its_span_and_bucket() {
        // (window, span, bucket, expected samples per bucket)
        for (window, span, bucket, per_bucket) in [
            ("1h", 3_600, 300, 1),
            ("1d", 86_400, 960, 3),
            ("1w", 604_800, 6_720, 22),
            ("90d", 7_776_000, 86_400, 288),
        ] {
            assert_eq!(window_spec(window), Some((span, bucket)), "{window}");
            assert_eq!((bucket / PROBE_CADENCE_SECS).max(1), per_bucket, "{window}");
        }
        assert_eq!(window_spec("1y"), None);
        assert_eq!(window_spec(""), None);
        assert!(window_spec(DEFAULT_WINDOW).is_some());
    }

    #[test]
    fn history_rows_group_by_component_in_bucket_order() {
        let row = |bucket: i64, component: &str, ok: i64| StatusBucketView {
            bucket,
            component: component.into(),
            ok,
            degraded: 0,
            down: 1,
        };
        let grouped = group_history(vec![
            row(0, "reconciler", 1),
            row(0, "alarms", 2),
            row(300, "reconciler", 3),
            row(300, "alarms", 4),
        ]);
        let names: Vec<&str> = grouped.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["reconciler", "alarms"]);
        let reconciler: Vec<(i64, i64)> = grouped[0].buckets.iter().map(|b| (b.t, b.ok)).collect();
        assert_eq!(reconciler, [(0, 1), (300, 3)]);
        assert_eq!(grouped[1].buckets[1].ok, 4);
        assert!(group_history(vec![]).is_empty());
    }
}
