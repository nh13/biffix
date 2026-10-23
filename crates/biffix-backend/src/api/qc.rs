//! QC previews of a run's outputs: a FastQC result as data, from the pipeline's own
//! `_fastqc.zip`, and what a genome browser needs to show an alignment output.
//!
//! Both start from one output of a run the caller can see that biffix serves (the same rule as the
//! output's download); what they read from there is [`crate::qc::viewer`]'s. The alignment itself
//! is never proxied (the API's response and time limits are far below a BAM); the browser reads it
//! through presigned Range GETs.

use axum::extract::{Path, Query, State};
use axum::{Extension, Json};

pub(crate) use biffix_api::{
    AlignmentLinks, AlignmentReference, AlignmentTrack, AlignmentViewQuery, AnnotationTrack,
    FastqcModule, FastqcReport,
};
use biffix_core::ids::RunId;

use crate::api::outputs::servable_output_of;
use crate::api::ApiError;
use crate::auth::actor::AuthedActor;
use crate::qc::viewer::{alignment_view_of, fastqc_report};
use crate::AppState;

// ── FastQC ────────────────────────────────────────────────────────────────────────────────────

/// `GET /runs/{run_id}/outputs/{output_id}/fastqc` — the pipeline's own FastQC result for an
/// output, as data: the output's `_fastqc.zip` itself, the zip beside a `_fastqc.html`, or, for a
/// FASTQ, the zip FastQC names after it (`S1_R1.fastq.gz` → `S1_R1_fastqc.zip`) among the run's
/// registered outputs, when it is unambiguously that FASTQ's. 404 when there is none (the client
/// falls back to the HTML report); 422 when the zip is too large or not FastQC's.
#[utoipa::path(
    get, path = "/runs/{run_id}/outputs/{output_id}/fastqc",
    params(
        ("run_id" = String, Path, description = "biffix run id"),
        ("output_id" = String, Path, description = "the output's opaque id (from the list)"),
    ),
    responses(
        (status = 200, body = FastqcReport), (status = 403),
        (status = 404, description = "no FastQC result for this output"),
        (status = 422, description = "the FastQC zip is too large or unreadable"),
        (status = 502), (status = 503)
    )
)]
pub(crate) async fn output_fastqc(
    State(state): State<AppState>,
    Extension(actor): Extension<AuthedActor>,
    Path((run_id, output_id)): Path<(RunId, String)>,
) -> Result<Json<FastqcReport>, ApiError> {
    let (detail, scope, row) = servable_output_of(&state, &actor, &run_id, &output_id).await?;
    Ok(Json(fastqc_report(&state, &detail, &scope, &row).await?))
}

// ── alignments ───────────────────────────────────────────────────────────────────────────────

/// `GET /runs/{run_id}/outputs/{output_id}/alignment-view` — presigned Range-readable links to a BAM
/// or CRAM output and its index, the reference to show it against (the run's recorded reference
/// bundle, or `?reference=`; a bundle's `igv_genome` names a hosted genome instead), and where to
/// open (the first contig with mapped reads, read from the file's own index and header; skipped
/// with `?locus=false`, as a client renewing its links does). The links last an hour at most: ask
/// again before `expires_at`, or when a read is refused. 409 when the output is not a BAM/CRAM or
/// has no index.
#[utoipa::path(
    get, path = "/runs/{run_id}/outputs/{output_id}/alignment-view",
    params(
        ("run_id" = String, Path, description = "biffix run id"),
        ("output_id" = String, Path, description = "the output's opaque id (from the list)"),
        AlignmentViewQuery,
    ),
    responses(
        (status = 200, body = AlignmentLinks), (status = 403),
        (status = 404, description = "no such output, or no such reference bundle"),
        (status = 409, description = "not a BAM or CRAM, or it has no index"),
        (status = 502), (status = 503)
    )
)]
pub(crate) async fn alignment_view(
    State(state): State<AppState>,
    Extension(actor): Extension<AuthedActor>,
    Path((run_id, output_id)): Path<(RunId, String)>,
    Query(q): Query<AlignmentViewQuery>,
) -> Result<Json<AlignmentLinks>, ApiError> {
    let (detail, scope, row) = servable_output_of(&state, &actor, &run_id, &output_id).await?;
    let links =
        alignment_view_of(&state, &detail, &scope, &row, q.reference.as_deref(), q.locus).await?;
    Ok(Json(links))
}

// ── routes ───────────────────────────────────────────────────────────────────────────────────────

/// An output's QC views: FastQC and the alignment viewer: the routes behind the Cognito middleware.
pub(super) fn routes() -> axum::Router<AppState> {
    axum::Router::new()
        .route("/runs/{run_id}/outputs/{output_id}/fastqc", axum::routing::get(output_fastqc))
        .route(
            "/runs/{run_id}/outputs/{output_id}/alignment-view",
            axum::routing::get(alignment_view),
        )
}

/// An output's QC views: FastQC and the alignment viewer: the OpenAPI operations, tagged `qc` in
/// [`super::ApiDoc`].
#[derive(utoipa::OpenApi)]
#[openapi(
    paths(output_fastqc, alignment_view),
    components(schemas(
        FastqcReport,
        FastqcModule,
        AlignmentLinks,
        AlignmentTrack,
        AlignmentReference,
        AnnotationTrack
    ))
)]
pub(super) struct Doc;
