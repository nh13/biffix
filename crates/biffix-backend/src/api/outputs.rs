//! Run outputs: the objects a run's engine registered (`task_outputs` events, folded into
//! `run_output`), listed and downloaded through biffix's own auth.
//!
//! Every S3 coordinate is derived server-side from the run's registered outputs: the client names a
//! run and an opaque output id, never a key. A download is further limited to objects biffix may
//! serve ([`checkable`]) — a workflow can *declare* any key as an output, so registration alone must
//! never let a viewer presign, say, another team's staged private-repo source.

use std::time::Duration;

use axum::extract::{Path, State};
use axum::{Extension, Json};
use futures::StreamExt;

use biffix_core::ids::RunId;

pub(crate) use biffix_api::{
    OutputPreview, OutputUrl, RunOutput, RunOutputPage, RunOutputQueryRequest,
};

use crate::api::ApiError;
use crate::auth::actor::AuthedActor;
use crate::object_view::{presigned_link, Link, LinkError};
use crate::runs::access::load_visible_run;
use crate::runs::outputs::{aws_error, output_scope, output_view};
use crate::store::outputs::{
    checkable, needs_head, ObjectMeta, OutputQuery, OutputRow, OutputScope,
};
use crate::store::{RunDetailView, StoreError};
use crate::AppState;

/// Default and maximum page sizes for the output list.
const DEFAULT_PAGE: i64 = 500;
const MAX_PAGE: i64 = 2000;

/// S3 HEADs one list request may make to fill in sizes, and how many run at once. Bounded so a
/// 5,000-output run's list stays fast; later requests (the UI re-asks) check the rest.
const HEADS_PER_REQUEST: usize = 64;
const HEAD_CONCURRENCY: usize = 16;
/// The list answers with whatever HEADs finished by then; the rest stay pending.
const HEAD_BUDGET: Duration = Duration::from_secs(3);

/// `POST /runs/{run_id}/outputs/query` — a page of the objects the run registered, ordered by S3
/// URI. The filters are in a JSON body, so a task key, a directory or a cursor (all of which can
/// name a sample or a file) never appears in a request URL. An output registered outside the
/// output prefix is shown by its engine path or bucket key instead and is never under any
/// `prefix`; the role `*` is every role, including ones this build does not know (the role is an
/// open enum).
#[utoipa::path(
    post, path = "/runs/{run_id}/outputs/query",
    params(("run_id" = String, Path, description = "biffix run id")),
    request_body = RunOutputQueryRequest,
    responses((status = 200, body = RunOutputPage), (status = 403), (status = 404))
)]
pub(crate) async fn query_outputs(
    State(state): State<AppState>,
    Extension(actor): Extension<AuthedActor>,
    Path(rid): Path<RunId>,
    Json(q): Json<RunOutputQueryRequest>,
) -> Result<Json<RunOutputPage>, ApiError> {
    let (detail, bucket, scope) =
        visible_run_outputs(&state, &actor, &rid, ApiError::status_only).await?;
    let output_prefix = detail.output_prefix.clone();
    let roles: Vec<String> =
        q.roles.iter().map(|r| r.trim()).filter(|r| !r.is_empty()).map(str::to_string).collect();
    let limit = q.limit.unwrap_or(DEFAULT_PAGE).clamp(1, MAX_PAGE);
    let query = OutputQuery {
        // An empty task is no filter, as an absent one.
        task: q.task.filter(|t| !t.is_empty()),
        // No roles named ⇒ the default set (everything but intermediates).
        roles: (!roles.is_empty()).then_some(roles),
        // A tree directory is relative to the output prefix; without a prefix there is no tree.
        uri_prefix: match (&output_prefix, q.prefix.as_deref()) {
            (Some(base), Some(p)) if !p.is_empty() => Some(format!("{base}{p}")),
            _ => None,
        },
        after: q.after.filter(|a| !a.is_empty()),
        limit,
        checkable_bucket: Some(bucket.clone()),
    };
    let mut page =
        state.store.list_outputs_in(&scope, &query).await.map_err(ApiError::status_only)?;
    let checked = enrich(&state, &mut page.rows, &bucket).await;
    // A full page may have more behind it; the cursor is the last row's URI (keyset on s3_uri).
    let next_cursor = (page.rows.len() as i64 == limit)
        .then(|| page.rows.last().map(|r| r.s3_uri.clone()))
        .flatten();
    let outputs = page
        .rows
        .iter()
        .map(|row| output_view(row, &rid, output_prefix.as_deref(), &bucket))
        .collect();
    Ok(Json(RunOutputPage {
        outputs,
        total: page.total,
        output_prefix,
        next_cursor,
        enrichment_pending: (page.unchecked - checked as i64).max(0),
    }))
}

/// `GET /runs/{run_id}/outputs/{output_id}` — one output of the run, shaped as in the list. A
/// client that names an output by id (a link to an open file) can show it without paging through
/// the whole list to find it. 404 when the id is not an output of this run; a file it reused from
/// a run it resumed counts as its output.
#[utoipa::path(
    get, path = "/runs/{run_id}/outputs/{output_id}",
    params(
        ("run_id" = String, Path, description = "biffix run id"),
        ("output_id" = String, Path, description = "the output's opaque id (from the list)"),
    ),
    responses((status = 200, body = RunOutput), (status = 403), (status = 404))
)]
pub(crate) async fn get_output(
    State(state): State<AppState>,
    Extension(actor): Extension<AuthedActor>,
    Path((rid, output_id)): Path<(RunId, String)>,
) -> Result<Json<RunOutput>, ApiError> {
    // The id resolves only within THIS run's outputs, so a viewer cannot name another run's
    // object (beyond those this run reused).
    let (detail, bucket, scope) =
        visible_run_outputs(&state, &actor, &rid, ApiError::status_only).await?;
    let mut row = state
        .store
        .get_output_in(&scope, &output_id)
        .await
        .map_err(ApiError::status_only)?
        .ok_or(ApiError::NotFound(String::new()))?;
    enrich(&state, std::slice::from_mut(&mut row), &bucket).await;
    Ok(Json(output_view(&row, &rid, detail.output_prefix.as_deref(), &bucket)))
}

/// The visible run `rid`, its project's bucket, and the scope its output ids resolve in
/// ([`output_scope`]); a failure reading the scope answers through `scope_error`.
async fn visible_run_outputs(
    state: &AppState,
    actor: &AuthedActor,
    rid: &RunId,
    scope_error: fn(StoreError) -> ApiError,
) -> Result<(RunDetailView, String, OutputScope), ApiError> {
    let detail = load_visible_run(state, actor, rid).await?;
    let bucket = state.projects.current().await.for_run(&detail).bucket.clone();
    let scope = output_scope(state, &detail).await.map_err(scope_error)?;
    Ok((detail, bucket, scope))
}

/// HEAD up to [`HEADS_PER_REQUEST`] of `rows` that S3 has not been checked for, record the answers,
/// and fold them into `rows`. Returns how many were checked. Best-effort: a failed HEAD or cache
/// write leaves the row unchecked for a later request, never fails the list. Only servable objects
/// are checked (in the run's project `bucket`) — the backend can read nothing else.
async fn enrich(state: &AppState, rows: &mut [OutputRow], bucket: &str) -> usize {
    let now = biffix_core::clock::epoch_micros();
    let uris: Vec<String> = rows
        .iter()
        .filter(|r| needs_head(r.meta.as_ref(), now) && checkable(&r.s3_uri, bucket))
        .take(HEADS_PER_REQUEST)
        .map(|r| r.s3_uri.clone())
        .collect();
    if uris.is_empty() {
        return 0;
    }
    let mut metas: Vec<(String, ObjectMeta)> = Vec::with_capacity(uris.len());
    let heads = futures::stream::iter(uris)
        .map(|uri| async move {
            let head = state.cloud.head_object_meta(&uri).await;
            (uri, head)
        })
        .buffer_unordered(HEAD_CONCURRENCY)
        .for_each(|(uri, head)| {
            match head {
                Ok(head) => metas.push((uri, object_meta(head, now))),
                Err(e) => tracing::warn!(error = %e, "list_outputs: HEAD failed"),
            }
            futures::future::ready(())
        });
    // Whatever finished inside the budget is kept; the rest stay pending for the next request.
    let _ = tokio::time::timeout(HEAD_BUDGET, heads).await;
    if metas.is_empty() {
        return 0;
    }
    if let Err(e) = state.store.put_object_meta(&metas).await {
        tracing::warn!(error = %e, "list_outputs: caching S3 metadata failed");
    }
    let checked = metas.len();
    for (uri, meta) in metas {
        if let Some(row) = rows.iter_mut().find(|r| r.s3_uri == uri) {
            row.meta = Some(meta);
        }
    }
    checked
}

/// The cached form of a HEAD answer (`None` ⇒ the object is absent).
fn object_meta(head: Option<crate::aws::objects::ObjectHead>, checked_at: i64) -> ObjectMeta {
    match head {
        Some(h) => ObjectMeta {
            present: true,
            size: h.size,
            etag: h.etag,
            last_modified: h.last_modified,
            checked_at,
        },
        None => {
            ObjectMeta { present: false, size: None, etag: None, last_modified: None, checked_at }
        }
    }
}

/// Resolve `output_id` to an output of the visible run `run_id` that biffix serves: 404 when the id
/// is not an output of THIS run, 403 when it is not [`checkable`].
async fn servable_output(
    state: &AppState,
    actor: &AuthedActor,
    run_id: RunId,
    output_id: &str,
) -> Result<OutputRow, ApiError> {
    servable_output_of(state, actor, &run_id, output_id).await.map(|(_, _, row)| row)
}

/// [`servable_output`], with the run it belongs to and the scope of its outputs
/// ([`output_scope`]).
pub(crate) async fn servable_output_of(
    state: &AppState,
    actor: &AuthedActor,
    rid: &RunId,
    output_id: &str,
) -> Result<(RunDetailView, OutputScope, OutputRow), ApiError> {
    // The id resolves only within THIS run's outputs (its own, and those it reused from a run it
    // resumed), so a viewer cannot name another run's object.
    let (detail, bucket, scope) = visible_run_outputs(state, actor, rid, ApiError::from).await?;
    let row = state
        .store
        .get_output_in(&scope, output_id)
        .await?
        .ok_or(ApiError::NotFound("no such output of this run".into()))?;
    if !checkable(&row.s3_uri, &bucket) {
        return Err(ApiError::Forbidden(
            "biffix does not serve this output (it is outside the project bucket, or a staged \
             workflow source)"
                .into(),
        ));
    }
    Ok((detail, scope, row))
}

/// An output's view or download link ([`presigned_link`]).
async fn output_link(
    state: &AppState,
    actor: &AuthedActor,
    run_id: RunId,
    output_id: &str,
    link: Link,
) -> Result<Json<OutputUrl>, ApiError> {
    let row = servable_output(state, actor, run_id, output_id).await?;
    presigned_link(state.cloud.as_ref(), &row.s3_uri, link).await.map(Json).map_err(|e| match e {
        LinkError::NoViewer(m) => ApiError::Conflict(m),
        LinkError::Aws(e) => aws_error("presigning an output link", e).into(),
    })
}

/// `GET /runs/{run_id}/outputs/{output_id}/download` — a short-lived presigned URL that downloads
/// the object as an attachment. 404 when the id is not an output of this run; 403 when the output
/// is one biffix does not serve ([`checkable`]).
#[utoipa::path(
    get, path = "/runs/{run_id}/outputs/{output_id}/download",
    params(
        ("run_id" = String, Path, description = "biffix run id"),
        ("output_id" = String, Path, description = "the output's opaque id (from the list)"),
    ),
    responses(
        (status = 200, body = OutputUrl), (status = 403), (status = 404), (status = 502),
        (status = 503)
    )
)]
pub(crate) async fn download_output(
    State(state): State<AppState>,
    Extension(actor): Extension<AuthedActor>,
    Path((run_id, output_id)): Path<(RunId, String)>,
) -> Result<Json<OutputUrl>, ApiError> {
    output_link(&state, &actor, run_id, &output_id, Link::Download).await
}

/// `GET /runs/{run_id}/outputs/{output_id}/view-url` — a short-lived URL that shows an image, PDF
/// or HTML output inline. The content type is set from the file name, never from what S3 stored
/// (uploads are often `binary/octet-stream`). 409 for an output with no such viewer.
#[utoipa::path(
    get, path = "/runs/{run_id}/outputs/{output_id}/view-url",
    params(
        ("run_id" = String, Path, description = "biffix run id"),
        ("output_id" = String, Path, description = "the output's opaque id (from the list)"),
    ),
    responses(
        (status = 200, body = OutputUrl), (status = 403), (status = 404),
        (status = 409, description = "the file cannot be shown in the browser; download it"),
        (status = 502), (status = 503)
    )
)]
pub(crate) async fn view_output_url(
    State(state): State<AppState>,
    Extension(actor): Extension<AuthedActor>,
    Path((run_id, output_id)): Path<(RunId, String)>,
) -> Result<Json<OutputUrl>, ApiError> {
    output_link(&state, &actor, run_id, &output_id, Link::View).await
}

/// `GET /runs/{run_id}/outputs/{output_id}/preview` — the start of an output as text, read by the
/// backend (so it works whatever the bucket's CORS). A gzip/bgzip file is decompressed; a binary
/// file comes back as `kind: binary` with no text.
#[utoipa::path(
    get, path = "/runs/{run_id}/outputs/{output_id}/preview",
    params(
        ("run_id" = String, Path, description = "biffix run id"),
        ("output_id" = String, Path, description = "the output's opaque id (from the list)"),
    ),
    responses(
        (status = 200, body = OutputPreview), (status = 403), (status = 404), (status = 502),
        (status = 503)
    )
)]
pub(crate) async fn preview_output(
    State(state): State<AppState>,
    Extension(actor): Extension<AuthedActor>,
    Path((run_id, output_id)): Path<(RunId, String)>,
) -> Result<Json<OutputPreview>, ApiError> {
    let row = servable_output(&state, &actor, run_id, &output_id).await?;
    crate::object_view::preview(state.cloud.as_ref(), &row.s3_uri)
        .await
        .map(Json)
        .map_err(|e| aws_error("reading an output preview", e).into())
}

// ── routes ───────────────────────────────────────────────────────────────────────────────────────

/// A run's outputs: query, detail, links and previews: the routes behind the Cognito middleware.
pub(super) fn routes() -> axum::Router<AppState> {
    axum::Router::new()
        .route("/runs/{run_id}/outputs/query", axum::routing::post(query_outputs))
        .route("/runs/{run_id}/outputs/{output_id}", axum::routing::get(get_output))
        .route("/runs/{run_id}/outputs/{output_id}/download", axum::routing::get(download_output))
        .route("/runs/{run_id}/outputs/{output_id}/view-url", axum::routing::get(view_output_url))
        .route("/runs/{run_id}/outputs/{output_id}/preview", axum::routing::get(preview_output))
}

/// A run's outputs: query, detail, links and previews: the OpenAPI operations, tagged `outputs` in
/// [`super::ApiDoc`].
#[derive(utoipa::OpenApi)]
#[openapi(
    paths(query_outputs, get_output, download_output, view_output_url, preview_output),
    components(schemas(
        RunOutputPage,
        RunOutputQueryRequest,
        RunOutput,
        OutputUrl,
        OutputPreview
    ))
)]
pub(super) struct Doc;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn servable_only_in_the_project_bucket_and_outside_sources() {
        let b = "proj";
        for (uri, ok) in [
            ("s3://proj/biffix/runs/run-1/out.bam", true),
            ("s3://proj/runs/abc123/summary.tsv", true),
            ("s3://proj/top.txt", true),
            ("s3://other/biffix/runs/run-1/out.bam", false),
            ("s3://proj/workflow-sources/v1/source.tar.gz", false),
            ("s3://proj/biffix/runs/run-1/", false),
            ("s3://proj/", false),
            ("s3://proj", false),
            ("https://proj/x", false),
        ] {
            assert_eq!(checkable(uri, b), ok, "{uri}");
        }
        assert!(!checkable("s3://proj/x", ""), "no project bucket ⇒ nothing is servable");
    }
}
