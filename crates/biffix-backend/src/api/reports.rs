//! A run's reports and its MultiQC data, served from what [`crate::qc::reports`] resolves. Only
//! registered outputs are ever offered, so a report opens through the same output endpoints (and
//! the same serving rules) as any other output.

use axum::extract::{Path, State};
use axum::{Extension, Json};

use biffix_api::{FastqcStatusReport, QcGeneralStats, RunReportList};
use biffix_core::ids::RunId;

use crate::api::ApiError;
use crate::auth::actor::AuthedActor;
use crate::qc::reports::{fastqc_status_of, general_stats_of, run_reports};
use crate::runs::access::load_visible_run;
use crate::AppState;

/// `GET /runs/{run_id}/reports` — the run's reports.
#[utoipa::path(
    get, path = "/runs/{run_id}/reports",
    params(("run_id" = String, Path, description = "biffix run id")),
    responses((status = 200, body = RunReportList), (status = 403), (status = 404), (status = 503))
)]
pub(crate) async fn list_reports(
    State(state): State<AppState>,
    Extension(actor): Extension<AuthedActor>,
    Path(run_id): Path<RunId>,
) -> Result<Json<RunReportList>, ApiError> {
    let detail = load_visible_run(&state, &actor, &run_id).await?;
    Ok(Json(run_reports(&state, &detail).await?))
}

/// `GET /runs/{run_id}/qc/general-stats` — the MultiQC report's General Statistics as data: the
/// values from `multiqc_data/multiqc_general_stats.txt` beside the report, with the columns'
/// metadata from `multiqc_data/multiqc_data.json` (plain column names when that is missing). The
/// report is the one `/reports` lists with kind `multiqc`.
#[utoipa::path(
    get, path = "/runs/{run_id}/qc/general-stats",
    params(("run_id" = String, Path, description = "biffix run id")),
    responses(
        (status = 200, body = QcGeneralStats), (status = 403), (status = 404), (status = 422),
        (status = 502), (status = 503)
    )
)]
pub(crate) async fn qc_general_stats(
    State(state): State<AppState>,
    Extension(actor): Extension<AuthedActor>,
    Path(run_id): Path<RunId>,
) -> Result<Json<QcGeneralStats>, ApiError> {
    let detail = load_visible_run(&state, &actor, &run_id).await?;
    Ok(Json(general_stats_of(&state, &detail).await?))
}

/// `GET /runs/{run_id}/qc/fastqc-status` — each FastQC result's module statuses and basic
/// statistics as MultiQC recorded them, each attributed to the run's sample that owns it: for a
/// pipeline that publishes MultiQC's output but not FastQC's own zips. Read from every FastQC table
/// in `multiqc_data/` beside the report `/reports` lists with kind `multiqc` (one per FastQC
/// section: `multiqc_fastqc.txt`, and `multiqc_fastqc_<anchor>.txt` for a section with an anchor
/// or a repeated one), each result tagged with its section. 404 when the run has no MultiQC report,
/// or the report no FastQC table: MultiQC wrote its data in another format (`data_format`, said in
/// the message), or as `multiqc_data.zip` (`--zip-data-dir`, not read), or ran no FastQC module.
#[utoipa::path(
    get, path = "/runs/{run_id}/qc/fastqc-status",
    params(("run_id" = String, Path, description = "biffix run id")),
    responses(
        (status = 200, body = FastqcStatusReport), (status = 403),
        (status = 404, description = "no MultiQC report, or no FastQC table (TSV) in its data"),
        (status = 422, description = "MultiQC's FastQC tables are too large to read"),
        (status = 502), (status = 503)
    )
)]
pub(crate) async fn qc_fastqc_status(
    State(state): State<AppState>,
    Extension(actor): Extension<AuthedActor>,
    Path(run_id): Path<RunId>,
) -> Result<Json<FastqcStatusReport>, ApiError> {
    let detail = load_visible_run(&state, &actor, &run_id).await?;
    Ok(Json(fastqc_status_of(&state, &detail).await?))
}

// ── routes ───────────────────────────────────────────────────────────────────────────────────────

/// A run's reports and its MultiQC data: the routes behind the Cognito middleware.
pub(super) fn routes() -> axum::Router<AppState> {
    axum::Router::new()
        .route("/runs/{run_id}/reports", axum::routing::get(list_reports))
        .route("/runs/{run_id}/qc/general-stats", axum::routing::get(qc_general_stats))
        .route("/runs/{run_id}/qc/fastqc-status", axum::routing::get(qc_fastqc_status))
}

/// A run's reports and its MultiQC data: the OpenAPI operations, tagged `reports` in
/// [`super::ApiDoc`].
#[derive(utoipa::OpenApi)]
#[openapi(
    paths(list_reports, qc_general_stats, qc_fastqc_status),
    components(schemas(
        biffix_api::RunReport,
        biffix_api::RunReportList,
        biffix_api::QcGeneralStats,
        biffix_api::QcColumn,
        biffix_api::QcSample,
        biffix_api::FastqcStatusReport,
        biffix_api::FastqcStatusSample,
        biffix_api::FastqcModuleStatus,
        biffix_api::FastqcBasicStats
    ))
)]
pub(super) struct Doc;
