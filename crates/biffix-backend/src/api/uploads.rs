//! Browser uploads into the project bucket: resumable S3 multipart uploads the
//! browser sends straight to S3 through presigned part URLs, so a large FASTQ never passes through
//! the API. Only under `uploads/` — the one prefix the backend may write for uploads — and only when
//! the deploy enabled them (the bucket has the CORS rule browsers need).
//!
//! Every call names the object key again; it is re-checked each time, so no call can reach a key the
//! upload was not started for.
//!
//! An upload never replaces an object: start refuses a key that is taken (409, so the UI can offer
//! another name before any byte is sent), and completion is create-only in S3 itself, which also
//! stops two uploads racing to one key. A replaced object would silently change an input a
//! finished run recorded by URI.

use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::{Extension, Json};
use futures::{StreamExt, TryStreamExt};

pub(crate) use biffix_api::{
    StartedUpload, UploadCompleteRequest, UploadKeyRequest, UploadPartUrl, UploadPartUrlList,
    UploadPartsRequest, UploadStartRequest, UploadStatus, UploadedFile, UploadedPart,
};

use crate::api::ApiError;
use crate::auth::actor::AuthedActor;
use crate::auth::rbac::Capability;
use crate::aws::{uploads::UploadCompletion, AwsError};
use crate::AppState;

/// The prefix uploads go under (the backend's upload grant covers only it).
pub(crate) const UPLOADS_PREFIX: &str = "uploads/";
const MIB: u64 = 1024 * 1024;
/// S3 multipart limits: at most 10,000 parts, each at most 5 GiB; objects at most 5 TiB.
const MAX_PARTS: u64 = 10_000;
const MAX_OBJECT: u64 = 5 * 1024 * 1024 * MIB;
/// The smallest part biffix uses (S3's floor is 5 MiB), and the part size for very large files.
const MIN_PART: u64 = 8 * MIB;
const LARGE_PART: u64 = 64 * MIB;
const LARGE_FILE: u64 = 100 * 1024 * MIB;
/// Part URLs one request may ask for, and how long each stays valid.
const MAX_URLS: usize = 100;
const PART_URL_TTL: Duration = Duration::from_secs(15 * 60);
/// Part URLs presigned at once.
const PRESIGN_CONCURRENCY: usize = 16;

/// The part size for a file of `size` bytes: at least 8 MiB (64 MiB over 100 GiB), whole MiB, and
/// large enough that the file needs at most 10,000 parts. Returns `(part size, parts)`.
pub(crate) fn plan_parts(size: u64) -> (u64, u32) {
    let floor = if size > LARGE_FILE { LARGE_PART } else { MIN_PART };
    let needed = size.div_ceil(MAX_PARTS).div_ceil(MIB) * MIB;
    let part = floor.max(needed);
    let parts = size.div_ceil(part).max(1);
    (part, parts as u32)
}

/// Check an upload key: under `uploads/`, a plain relative path (no empty, `.` or `..` segments,
/// no control characters), not a folder, at most 1024 bytes.
pub(crate) fn check_key(key: &str) -> Result<(), String> {
    let bad = |why: &str| Err(format!("upload key `{key}` {why}"));
    let Some(rest) = key.strip_prefix(UPLOADS_PREFIX) else {
        return bad("must be under uploads/");
    };
    if key.len() > 1024 {
        return bad("is longer than 1024 bytes");
    }
    if key.chars().any(char::is_control) {
        return bad("contains a control character");
    }
    if rest.is_empty() || rest.ends_with('/') {
        return bad("names a folder, not a file");
    }
    if crate::config::data_sources::has_unplain_segment(rest) {
        return bad("has an empty, `.` or `..` path segment");
    }
    Ok(())
}

/// The object URI of an upload key in `project`'s bucket (the default project's when it names
/// none), after checking the caller may launch workflows, uploads are on there and the key is
/// allowed.
async fn upload_uri(
    state: &AppState,
    actor: &AuthedActor,
    project: Option<&str>,
    key: &str,
) -> Result<String, ApiError> {
    crate::auth::actor::authorize_msg(state, actor, Capability::LaunchWorkflow)?;
    let project = crate::api::actor::request_project(state, project).await?;
    if !project.uploads || project.bucket.is_empty() {
        return Err(ApiError::Forbidden(
            "browser uploads are not enabled for this project's bucket".into(),
        ));
    }
    check_key(key).map_err(ApiError::BadRequest)?;
    Ok(format!("s3://{}/{key}", project.bucket))
}

fn aws_error(e: AwsError) -> ApiError {
    match e {
        AwsError::NotFound => ApiError::NotFound("no such upload".into()),
        AwsError::Forbidden(m) => ApiError::Forbidden(format!(
            "biffix may not upload there; an admin should check its grant: {m}"
        )),
        // A part list that does not match what S3 holds (InvalidPart, InvalidPartOrder,
        // EntityTooSmall) is the caller's mistake.
        AwsError::Invalid(m) => ApiError::BadRequest(m),
        e @ AwsError::Unavailable(_) => ApiError::Unavailable(e.to_string()),
        // Already worded as an S3 failure: not twice.
        e @ AwsError::S3(_) => ApiError::BadGateway(e.to_string()),
        other => ApiError::BadGateway(format!("s3 error: {other}")),
    }
}

/// The 409 for a key an object already holds.
fn taken(key: &str) -> ApiError {
    ApiError::Conflict(format!("an object already exists at {key}; choose another name"))
}

/// `POST /data/uploads` — start an upload; the response says how to split the file. 409 when an
/// object already exists at the key: uploads never replace one.
#[utoipa::path(
    post,
    path = "/data/uploads",
    request_body = UploadStartRequest,
    responses(
        (status = 201, body = StartedUpload), (status = 400), (status = 403),
        (status = 409, description = "an object already exists at the key; choose another name"),
        (status = 502), (status = 503)
    )
)]
pub(crate) async fn start_upload(
    State(state): State<AppState>,
    Extension(actor): Extension<AuthedActor>,
    Json(req): Json<UploadStartRequest>,
) -> Result<(StatusCode, Json<StartedUpload>), ApiError> {
    let uri = upload_uri(&state, &actor, req.project.as_deref(), &req.key).await?;
    if req.size > MAX_OBJECT {
        return Err(ApiError::BadRequest("a file may be at most 5 TiB".into()));
    }
    let content_type =
        req.content_type.as_deref().filter(|t| !t.is_empty()).unwrap_or("application/octet-stream");
    // An early answer, before any byte is sent; completion re-checks in S3 itself.
    if state.cloud.head_object_meta(&uri).await.map_err(aws_error)?.is_some() {
        return Err(taken(&req.key));
    }
    let upload_id =
        state.cloud.create_multipart_upload(&uri, content_type).await.map_err(aws_error)?;
    let (part_size, parts) = plan_parts(req.size);
    Ok((
        StatusCode::CREATED,
        Json(StartedUpload { upload_id, key: req.key, uri, part_size, parts }),
    ))
}

/// `POST /data/uploads/{upload_id}/parts` — presigned `PUT` URLs for parts (at most 100 at a time).
#[utoipa::path(
    post,
    path = "/data/uploads/{upload_id}/parts",
    params(("upload_id" = String, Path, description = "Upload id")),
    request_body = UploadPartsRequest,
    responses(
        (status = 200, body = UploadPartUrlList), (status = 400), (status = 403), (status = 502),
        (status = 503)
    )
)]
pub(crate) async fn part_urls(
    State(state): State<AppState>,
    Extension(actor): Extension<AuthedActor>,
    Path(upload_id): Path<String>,
    Json(req): Json<UploadPartsRequest>,
) -> Result<Json<UploadPartUrlList>, ApiError> {
    let uri = upload_uri(&state, &actor, req.project.as_deref(), &req.key).await?;
    if req.part_numbers.is_empty() || req.part_numbers.len() > MAX_URLS {
        return Err(ApiError::BadRequest(format!("ask for 1–{MAX_URLS} parts at a time")));
    }
    if let Some(n) = req.part_numbers.iter().find(|n| !(1..=MAX_PARTS as i32).contains(n)) {
        return Err(ApiError::BadRequest(format!("part number {n} is not in 1–{MAX_PARTS}")));
    }
    // In request order (`buffered`, not `buffer_unordered`).
    let (cloud, uri, upload_id) = (&state.cloud, &uri, &upload_id);
    let urls = futures::stream::iter(req.part_numbers)
        .map(|n| async move {
            cloud
                .presign_upload_part(uri, upload_id, n, PART_URL_TTL)
                .await
                .map(|url| UploadPartUrl { part_number: n, url })
        })
        .buffered(PRESIGN_CONCURRENCY)
        .try_collect()
        .await
        .map_err(aws_error)?;
    Ok(Json(UploadPartUrlList { urls }))
}

/// `POST /data/uploads/{upload_id}/status` — the parts already uploaded, so a reload can resume.
/// The key is in the body, so no file name reaches a request URL.
#[utoipa::path(
    post,
    path = "/data/uploads/{upload_id}/status",
    params(("upload_id" = String, Path, description = "Upload id")),
    request_body = UploadKeyRequest,
    responses(
        (status = 200, body = UploadStatus), (status = 400), (status = 403), (status = 404),
        (status = 502), (status = 503)
    )
)]
pub(crate) async fn upload_status(
    State(state): State<AppState>,
    Extension(actor): Extension<AuthedActor>,
    Path(upload_id): Path<String>,
    Json(req): Json<UploadKeyRequest>,
) -> Result<Json<UploadStatus>, ApiError> {
    let (state, actor) = (&state, &actor);
    let (upload_id, project, key) = (upload_id.as_str(), req.project.as_deref(), req.key.as_str());
    let uri = upload_uri(state, actor, project, key).await?;
    let parts = state
        .cloud
        .list_upload_parts(&uri, upload_id)
        .await
        .map_err(aws_error)?
        .into_iter()
        .map(|p| UploadedPart { part_number: p.part_number, size: p.size, etag: p.etag })
        .collect();
    Ok(Json(UploadStatus { parts }))
}

/// `POST /data/uploads/{upload_id}/complete` — assemble the file from its parts (every part, in
/// order, with the ETag S3 returned for it). 409 when an object reached the key since the upload
/// started: nothing is replaced, and the upload is cancelled (upload the file under another name).
#[utoipa::path(
    post,
    path = "/data/uploads/{upload_id}/complete",
    params(("upload_id" = String, Path, description = "Upload id")),
    request_body = UploadCompleteRequest,
    responses(
        (status = 200, body = UploadedFile), (status = 400), (status = 403), (status = 404),
        (status = 409, description = "an object already exists at the key; choose another name"),
        (status = 502), (status = 503)
    )
)]
pub(crate) async fn complete_upload(
    State(state): State<AppState>,
    Extension(actor): Extension<AuthedActor>,
    Path(upload_id): Path<String>,
    Json(req): Json<UploadCompleteRequest>,
) -> Result<Json<UploadedFile>, ApiError> {
    let uri = upload_uri(&state, &actor, req.project.as_deref(), &req.key).await?;
    let mut parts: Vec<(i32, String)> =
        req.parts.into_iter().map(|p| (p.part_number, p.etag)).collect();
    parts.sort_by_key(|(n, _)| *n);
    let contiguous = parts.iter().enumerate().all(|(i, (n, _))| *n == i as i32 + 1);
    if parts.is_empty() || !contiguous {
        return Err(ApiError::BadRequest(
            "parts must be numbered 1, 2, 3… with none missing".into(),
        ));
    }
    match state
        .cloud
        .complete_multipart_upload(&uri, &upload_id, &parts)
        .await
        .map_err(aws_error)?
    {
        UploadCompletion::Created(etag) => Ok(Json(UploadedFile { uri, etag })),
        UploadCompletion::Exists => {
            // The parts can never become this key; don't leave them to the lifecycle rule.
            if let Err(e) = state.cloud.abort_multipart_upload(&uri, &upload_id).await {
                tracing::warn!(error = %e, uri, "aborting an upload whose key is taken failed");
            }
            Err(taken(&req.key))
        }
    }
}

/// `POST /data/uploads/{upload_id}/abort` — cancel an upload; S3 deletes its parts. The key is in
/// the body, so no file name reaches a request URL.
#[utoipa::path(
    post,
    path = "/data/uploads/{upload_id}/abort",
    params(("upload_id" = String, Path, description = "Upload id")),
    request_body = UploadKeyRequest,
    responses(
        (status = 204), (status = 400), (status = 403), (status = 404), (status = 502),
        (status = 503)
    )
)]
pub(crate) async fn abort_upload(
    State(state): State<AppState>,
    Extension(actor): Extension<AuthedActor>,
    Path(upload_id): Path<String>,
    Json(req): Json<UploadKeyRequest>,
) -> Result<StatusCode, ApiError> {
    let (state, actor) = (&state, &actor);
    let (upload_id, project, key) = (upload_id.as_str(), req.project.as_deref(), req.key.as_str());
    let uri = upload_uri(state, actor, project, key).await?;
    state.cloud.abort_multipart_upload(&uri, upload_id).await.map_err(aws_error)?;
    Ok(StatusCode::NO_CONTENT)
}

// ── routes ───────────────────────────────────────────────────────────────────────────────────────

/// Browser uploads into a project's bucket: the routes behind the Cognito middleware.
pub(super) fn routes() -> axum::Router<AppState> {
    axum::Router::new()
        .route("/data/uploads", axum::routing::post(start_upload))
        .route("/data/uploads/{upload_id}/parts", axum::routing::post(part_urls))
        .route("/data/uploads/{upload_id}/status", axum::routing::post(upload_status))
        .route("/data/uploads/{upload_id}/abort", axum::routing::post(abort_upload))
        .route("/data/uploads/{upload_id}/complete", axum::routing::post(complete_upload))
}

/// Browser uploads into a project's bucket: the OpenAPI operations, tagged `uploads` in
/// [`super::ApiDoc`].
#[derive(utoipa::OpenApi)]
#[openapi(
    paths(start_upload, part_urls, upload_status, complete_upload, abort_upload),
    components(schemas(
        UploadStartRequest,
        StartedUpload,
        UploadPartsRequest,
        UploadKeyRequest,
        UploadPartUrl,
        UploadPartUrlList,
        UploadStatus,
        UploadedPart,
        UploadCompleteRequest,
        biffix_api::UploadCompletePart,
        UploadedFile
    ))
)]
pub(super) struct Doc;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parts_are_at_least_8_mib_and_at_most_10000() {
        assert_eq!(plan_parts(0), (8 * MIB, 1));
        assert_eq!(plan_parts(1), (8 * MIB, 1));
        assert_eq!(plan_parts(8 * MIB), (8 * MIB, 1));
        assert_eq!(plan_parts(8 * MIB + 1), (8 * MIB, 2));
        // 10 GiB: 8 MiB parts.
        assert_eq!(plan_parts(10 * 1024 * MIB), (8 * MIB, 1280));
        // 90 GiB needs more than 8 MiB parts to stay under 10,000.
        let (part, parts) = plan_parts(90 * 1024 * MIB);
        assert!(part > 8 * MIB && part % MIB == 0 && parts <= 10_000);
        // Over 100 GiB: at least 64 MiB.
        assert_eq!(plan_parts(101 * 1024 * MIB).0, 64 * MIB);
        // 5 TiB still fits.
        let (part, parts) = plan_parts(MAX_OBJECT);
        assert!(parts <= 10_000 && part <= 5 * 1024 * MIB);
    }

    #[test]
    fn upload_keys_stay_under_uploads() {
        assert_eq!(check_key("uploads/2026-09-24/P01_R1.fastq.gz"), Ok(()));
        for bad in [
            "biffix/runs/x",
            "workflow-sources/x.tar.gz",
            "uploads/",
            "uploads/a/",
            "uploads//a",
            "uploads/../biffix/x",
            "uploads/./x",
            "uploads/a\nb",
            "/uploads/a",
        ] {
            assert!(check_key(bad).is_err(), "{bad}");
        }
        assert!(check_key(&format!("uploads/{}", "a".repeat(1100))).is_err());
    }

    #[test]
    fn an_s3_failure_says_so_once() {
        let message = aws_error(AwsError::S3("slow down".into())).to_string();
        assert_eq!(message, "s3 error: slow down");
        let message = aws_error(AwsError::Logs("gone".into())).to_string();
        assert_eq!(message, "s3 error: cloudwatch logs error: gone");
    }
}
