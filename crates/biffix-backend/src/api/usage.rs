//! `GET /usage` — estimated spend over a period, for any viewer.

use axum::extract::{Query, State};
use axum::{Extension, Json};

use crate::auth::rbac::Capability;
use biffix_api::{UsageQuery, UsageReport, UsageRow};

use crate::api::ApiError;
use crate::auth::actor::{authorize_msg, AuthedActor};
use crate::runs::usage::{format_date, period, UsageGroup, UsageTotals};
use crate::AppState;

/// Estimated spend of the runs submitted in a period, grouped, with the period's total.
#[utoipa::path(
    get, path = "/usage",
    params(UsageQuery),
    responses((status = 200, body = UsageReport), (status = 400), (status = 403))
)]
pub(crate) async fn get_usage(
    State(state): State<AppState>,
    Extension(actor): Extension<AuthedActor>,
    Query(q): Query<UsageQuery>,
) -> Result<Json<UsageReport>, ApiError> {
    authorize_msg(&state, &actor, Capability::ViewRuns)?;
    let bad = |e: String| ApiError::BadRequest(e);
    let group: UsageGroup =
        q.group_by.as_deref().filter(|g| !g.is_empty()).unwrap_or("month").parse().map_err(bad)?;
    let (from, to) = period(q.from.as_deref(), q.to.as_deref(), biffix_core::clock::epoch_micros())
        .map_err(bad)?;
    let report = state.store.usage(from, to, group).await?;
    let row = |t: UsageTotals| UsageRow {
        total_usd: t.compute_usd + t.ebs_usd,
        key: t.key,
        runs: t.runs,
        compute_usd: t.compute_usd,
        ebs_usd: t.ebs_usd,
    };
    Ok(Json(UsageReport {
        from: format_date(from),
        to: format_date(to),
        group_by: group.as_str().to_string(),
        rows: report.rows.into_iter().map(row).collect(),
        total: row(report.total),
    }))
}

// ── routes ───────────────────────────────────────────────────────────────────────────────────────

/// Usage and spend: the routes behind the Cognito middleware.
pub(super) fn routes() -> axum::Router<AppState> {
    axum::Router::new().route("/usage", axum::routing::get(get_usage))
}

/// Usage and spend: the OpenAPI operations, tagged `usage` in [`super::ApiDoc`].
#[derive(utoipa::OpenApi)]
#[openapi(paths(get_usage), components(schemas(biffix_api::UsageReport, biffix_api::UsageRow)))]
pub(super) struct Doc;
