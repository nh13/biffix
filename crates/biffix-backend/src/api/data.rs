//! The file picker's view of S3: the data sources, one folder of one, a name
//! search under a folder, and existence checks for pasted paths. Viewer-level: seeing what inputs
//! exist is part of preparing a run, and every location is one the deploy chose to expose.
//!
//! Callers name a source by id and a path relative to its root; the S3 key is derived here
//! ([`DataSource::key_of`]), so no request can list or HEAD outside the configured sources, nor
//! under a prefix a source hides ([`DataSource::hides`], [`DataSource::shows`]).

use axum::extract::{Query, State};
use axum::{Extension, Json};
use futures::StreamExt;

pub(crate) use biffix_api::{
    DataFile, DataFileRequest, DataFindRequest, DataFindResult, DataFolder, DataFolderPage,
    DataHead, DataHeadList, DataHeadRequest, DataListRequest, DataSourcesQuery,
};
use biffix_api::{OutputPreview, OutputUrl};

use crate::api::ApiError;
use crate::auth::actor::AuthedActor;
use crate::auth::rbac::Capability;
use crate::aws::{objects::ListedObject, AwsError};
use crate::config::data_sources::DataSource;
use crate::object_view::{self, presigned_link, Link, LinkError};
use crate::AppState;

/// Keys per listing page.
const LIST_PAGE: i32 = 1000;
/// Default and largest number of matches a search returns.
const FIND_DEFAULT: usize = 500;
const FIND_MAX: usize = 5000;
/// Keys a search reads before it gives up (so a search of a huge folder stays one fast request).
const FIND_SCAN_KEYS: usize = 20_000;
/// URIs one existence check may name, and how many HEADs run at once.
const HEAD_MAX: usize = 200;
const HEAD_CONCURRENCY: usize = 16;

/// The sources of `project` (the default project's when it names none; a 400 for one this deploy
/// does not configure), and whether browsers may upload into its bucket.
async fn sources(
    state: &AppState,
    project: Option<&str>,
) -> Result<(Vec<DataSource>, bool), ApiError> {
    let project = crate::api::actor::request_project(state, project).await?;
    Ok((project.sources(), project.uploads))
}

/// Source `id` of `project` (see [`sources`]).
async fn find_source(
    state: &AppState,
    project: Option<&str>,
    id: &str,
) -> Result<DataSource, ApiError> {
    crate::api::actor::request_project(state, project)
        .await?
        .find_source(id)
        .ok_or(ApiError::NotFound(format!("unknown data source `{id}`")))
}

/// A folder path relative to a source root: `""` or ending in `/`, never starting with one.
fn folder_path(path: Option<&str>) -> String {
    crate::config::data_sources::normalize_prefix(path.unwrap_or(""))
}

fn view_runs(state: &AppState, actor: &AuthedActor) -> Result<(), ApiError> {
    crate::auth::actor::authorize_msg(state, actor, Capability::ViewRuns)?;
    Ok(())
}

/// An S3 failure reading `source`: its kind as any AWS failure's ([`ApiError`]'s `From`), and for a
/// missing grant, which grant an admin should check.
fn aws_error(source: &DataSource, e: AwsError) -> ApiError {
    ApiError::reworded(e, |e| {
        match e {
        AwsError::Forbidden(_) => format!(
            "biffix cannot read {} (s3://{}/{}); an admin should check its grant in the deploy config",
            source.name, source.bucket, source.prefix
        ),
        AwsError::Unavailable(_) | AwsError::Invalid(_) => e.to_string(),
        // Already worded as an S3 failure: not twice.
        AwsError::S3(_) => e.to_string(),
        other => format!("s3 error: {other}"),
    }
    })
}

fn file_of(source: &DataSource, o: &ListedObject) -> Option<DataFile> {
    let path = source.path_of(&o.key)?;
    let name = biffix_workflow::text::basename(path);
    if name.is_empty() {
        return None; // a zero-byte "folder marker" object
    }
    Some(DataFile {
        name: name.to_string(),
        path: path.to_string(),
        uri: source.uri_of(&o.key),
        size: o.size,
        last_modified_micros: o.last_modified,
        etag: o.etag.clone(),
        viewer: biffix_workflow::viewer::viewer_of(name).map(str::to_string),
    })
}

/// `GET /data-sources` — where inputs can be picked from in a project (the default one unless
/// `project` names another), its bucket first.
#[utoipa::path(
    get, path = "/data-sources",
    params(DataSourcesQuery),
    responses((status = 200, body = Vec<biffix_api::DataSource>), (status = 400), (status = 403))
)]
pub(crate) async fn list_sources(
    State(state): State<AppState>,
    Extension(actor): Extension<AuthedActor>,
    Query(q): Query<DataSourcesQuery>,
) -> Result<Json<Vec<biffix_api::DataSource>>, ApiError> {
    view_runs(&state, &actor)?;
    let (sources, uploads) = sources(&state, q.project.as_deref()).await?;
    Ok(Json(sources.iter().map(|s| s.view(uploads)).collect()))
}

/// `POST /data/list` — one folder of a data source: its sub-folders and files, a page at a time.
/// The query is in the body, so no folder name reaches a request URL.
#[utoipa::path(
    post, path = "/data/list",
    request_body = DataListRequest,
    responses(
        (status = 200, body = DataFolderPage), (status = 400, description = "a cursor S3 did not issue, or an unknown project"),
        (status = 403), (status = 404), (status = 502),
        (status = 503)
    )
)]
pub(crate) async fn list_folder(
    State(state): State<AppState>,
    Extension(actor): Extension<AuthedActor>,
    Json(q): Json<DataListRequest>,
) -> Result<Json<DataFolderPage>, ApiError> {
    let (state, actor) = (&state, &actor);
    let (source, path) =
        visible_folder(state, actor, q.project.as_deref(), &q.source, q.path.as_deref()).await?;
    let prefix = format!("{}{}", source.key_of(&path), q.starts_with.as_deref().unwrap_or(""));
    let page = state
        .cloud
        .list_objects(&source.bucket, &prefix, true, q.cursor.as_deref(), LIST_PAGE)
        .await
        .map_err(|e| aws_error(&source, e))?;
    let folders = page
        .prefixes
        .iter()
        .filter_map(|p| source.path_of(p))
        .filter(|p| !source.hides(p))
        .map(|p| DataFolder {
            name: biffix_workflow::text::basename(p.trim_end_matches('/')).to_string(),
            path: p.to_string(),
        })
        .collect();
    let files = page
        .objects
        .iter()
        .filter_map(|o| file_of(&source, o))
        .filter(|f| !source.hides(&f.path))
        .collect();
    Ok(Json(DataFolderPage {
        source: source.id,
        path,
        folders,
        files,
        next_cursor: page.next_token,
    }))
}

/// Whether `name` contains `needle` (already lowercase), ignoring case. An ASCII name is compared
/// without allocating: a search reads up to [`FIND_SCAN_KEYS`] names.
fn contains_ignoring_case(name: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    if name.is_ascii() && needle.is_ascii() {
        return name
            .as_bytes()
            .windows(needle.len())
            .any(|w| w.eq_ignore_ascii_case(needle.as_bytes()));
    }
    name.to_lowercase().contains(needle)
}

/// A name search in progress: the matches so far and the keys read.
struct Search<'a> {
    source: &'a DataSource,
    needle: String,
    limit: usize,
    files: Vec<DataFile>,
    scanned: usize,
}

impl Search<'_> {
    /// Take `o` when it matches. `true` when it is a match past the limit: the search is truncated
    /// (a match was left out), so stop.
    fn consider(&mut self, o: &ListedObject) -> bool {
        let Some(path) = self.source.path_of(&o.key) else {
            return false;
        };
        let name = biffix_workflow::text::basename(path);
        if name.is_empty() || self.source.hides(path) || !contains_ignoring_case(name, &self.needle)
        {
            return false;
        }
        if self.files.len() == self.limit {
            return true;
        }
        self.files.extend(file_of(self.source, o));
        false
    }

    /// Search every key under `prefix`, at any depth. `true` when the search stopped with keys or
    /// matches left: a match past the limit, or the scan budget spent before the listing ended.
    async fn scan(&mut self, state: &AppState, prefix: &str) -> Result<bool, ApiError> {
        let mut token: Option<String> = None;
        loop {
            if self.scanned >= FIND_SCAN_KEYS {
                return Ok(true);
            }
            let page = state
                .cloud
                .list_objects(&self.source.bucket, prefix, false, token.as_deref(), LIST_PAGE)
                .await
                .map_err(|e| aws_error(self.source, e))?;
            self.scanned += page.objects.len();
            for o in &page.objects {
                if self.consider(o) {
                    return Ok(true);
                }
            }
            match page.next_token {
                Some(next) => token = Some(next),
                None => return Ok(false),
            }
        }
    }

    /// Search a source's root: its folders one level down, each searched in full unless hidden, so
    /// the keys under a hidden prefix (the project bucket's `biffix/`, which can hold millions) are
    /// never read and never count against the scan budget. Returns as [`Search::scan`] does.
    async fn scan_root(&mut self, state: &AppState, root: &str) -> Result<bool, ApiError> {
        let mut token: Option<String> = None;
        loop {
            if self.scanned >= FIND_SCAN_KEYS {
                return Ok(true);
            }
            let page = state
                .cloud
                .list_objects(&self.source.bucket, root, true, token.as_deref(), LIST_PAGE)
                .await
                .map_err(|e| aws_error(self.source, e))?;
            self.scanned += page.objects.len() + page.prefixes.len();
            // Key order across files and folders, as a recursive listing would give.
            let mut entries: Vec<(&str, Option<&ListedObject>)> = page
                .objects
                .iter()
                .map(|o| (o.key.as_str(), Some(o)))
                .chain(page.prefixes.iter().map(|p| (p.as_str(), None)))
                .collect();
            entries.sort_by_key(|(key, _)| *key);
            for (key, object) in entries {
                let stopped = match object {
                    Some(o) => self.consider(o),
                    None if self.source.path_of(key).is_some_and(|p| self.source.hides(p)) => false,
                    None => self.scan(state, key).await?,
                };
                if stopped {
                    return Ok(true);
                }
            }
            match page.next_token {
                Some(next) => token = Some(next),
                None => return Ok(false),
            }
        }
    }
}

/// `POST /data/find` — files under a folder, at any depth, whose name contains `q`. `truncated`
/// when a match was left out: more than `limit` matched, or the search stopped at its scan budget
/// before reading the whole folder. The query is in the body, so no folder name or search text
/// reaches a request URL.
#[utoipa::path(
    post, path = "/data/find",
    request_body = DataFindRequest,
    responses(
        (status = 200, body = DataFindResult), (status = 400, description = "a cursor S3 did not issue, or an unknown project"),
        (status = 403), (status = 404), (status = 502),
        (status = 503)
    )
)]
pub(crate) async fn find_files(
    State(state): State<AppState>,
    Extension(actor): Extension<AuthedActor>,
    Json(q): Json<DataFindRequest>,
) -> Result<Json<DataFindResult>, ApiError> {
    let (state, actor) = (&state, &actor);
    let (source, path) =
        visible_folder(state, actor, q.project.as_deref(), &q.source, q.path.as_deref()).await?;
    let mut search = Search {
        source: &source,
        needle: q.q.trim().to_lowercase(),
        limit: q.limit.unwrap_or(FIND_DEFAULT).clamp(1, FIND_MAX),
        files: Vec::new(),
        scanned: 0,
    };
    let key = source.key_of(&path);
    // Hidden prefixes sit only at a source's root.
    let truncated = if path.is_empty() {
        search.scan_root(state, &key).await?
    } else {
        search.scan(state, &key).await?
    };
    let files = search.files;
    Ok(Json(DataFindResult { source: source.id, files, truncated }))
}

/// `POST /data/head` — whether each pasted or imported path exists (and its size). A URI no data
/// source shows ([`DataSource::shows`]: outside every source, a folder, or under a prefix biffix
/// hides) is reported as unknown, not absent: biffix cannot say, a run's role might.
#[utoipa::path(
    post, path = "/data/head",
    request_body = DataHeadRequest,
    responses((status = 200, body = DataHeadList), (status = 400), (status = 403))
)]
pub(crate) async fn head_objects(
    State(state): State<AppState>,
    Extension(actor): Extension<AuthedActor>,
    Json(req): Json<DataHeadRequest>,
) -> Result<Json<DataHeadList>, ApiError> {
    view_runs(&state, &actor)?;
    if req.uris.len() > HEAD_MAX {
        return Err(ApiError::BadRequest(format!("at most {HEAD_MAX} uris per request")));
    }
    let (all, _) = sources(&state, req.project.as_deref()).await?;
    let objects = futures::stream::iter(req.uris.into_iter().map(|uri| {
        let cloud = state.cloud.clone();
        let visible = all.iter().any(|s| s.shows(&uri));
        async move {
            let unknown = |uri| DataHead { uri, exists: None, size: None };
            if !visible {
                return unknown(uri);
            }
            match cloud.head_object_meta(&uri).await {
                Ok(Some(head)) => DataHead { uri, exists: Some(true), size: head.size },
                Ok(None) => DataHead { uri, exists: Some(false), size: None },
                // A failed HEAD says nothing about the object; leave it unknown, but say why (a
                // missing grant would otherwise leave every pasted path unknown, silently).
                Err(e) => {
                    tracing::warn!(%uri, error = %e, "data head: could not HEAD the object");
                    unknown(uri)
                }
            }
        }
    }))
    .buffered(HEAD_CONCURRENCY)
    .collect::<Vec<_>>()
    .await;
    Ok(Json(DataHeadList { objects }))
}

// ── one file: preview, view and download (the Data page) ────────────────────────────────────────

/// The URI of one file a viewer may see: inside the source, not a folder, not hidden. Only a `..`
/// segment is refused (it could climb out of the source): an empty or `.` segment is a key S3 can
/// hold (`a//b`), and a viewer may open what is there, where a writer may not create one
/// ([`crate::config::data_sources::has_unplain_segment`]).
async fn file_uri(
    state: &AppState,
    actor: &AuthedActor,
    q: &DataFileRequest,
) -> Result<(DataSource, String), ApiError> {
    view_runs(state, actor)?;
    let source = find_source(state, q.project.as_deref(), &q.source).await?;
    let path = q.path.trim_start_matches('/');
    if path.is_empty() || path.ends_with('/') || path.split('/').any(|p| p == "..") {
        return Err(ApiError::BadRequest(format!("`{}` is not a file path", q.path)));
    }
    if source.hides(path) {
        return Err(ApiError::NotFound("no such file".into()));
    }
    let uri = source.uri_of(&source.key_of(path));
    Ok((source, uri))
}

/// The folder `path` (none: the root) of the source named `source`, for a caller who may view runs;
/// a 404 for a folder the source hides.
async fn visible_folder(
    state: &AppState,
    actor: &AuthedActor,
    project: Option<&str>,
    source: &str,
    path: Option<&str>,
) -> Result<(DataSource, String), ApiError> {
    view_runs(state, actor)?;
    let source = find_source(state, project, source).await?;
    let path = folder_path(path);
    if source.hides(&path) {
        return Err(ApiError::NotFound(format!("no folder `{path}`")));
    }
    Ok((source, path))
}

fn link_error(source: &DataSource, e: AwsError) -> ApiError {
    match e {
        AwsError::NotFound => ApiError::NotFound("no such file".into()),
        other => aws_error(source, other),
    }
}

/// A view or download link to one file of a source.
async fn file_link(
    state: &AppState,
    actor: &AuthedActor,
    q: &DataFileRequest,
    link: Link,
) -> Result<Json<OutputUrl>, ApiError> {
    let (source, uri) = file_uri(state, actor, q).await?;
    presigned_link(state.cloud.as_ref(), &uri, link).await.map(Json).map_err(|e| match e {
        LinkError::NoViewer(m) => ApiError::Conflict(m),
        LinkError::Aws(e) => link_error(&source, e),
    })
}

/// `POST /data/preview` — the start of a file as text (gzip/bgzip decompressed), read by the
/// backend; a binary file comes back as `kind: binary`. The file is named in the body, so no file
/// name reaches a request URL.
#[utoipa::path(
    post,
    path = "/data/preview",
    request_body = DataFileRequest,
    responses(
        (status = 200, body = OutputPreview), (status = 400), (status = 403), (status = 404),
        (status = 502), (status = 503)
    )
)]
pub(crate) async fn preview_file(
    State(state): State<AppState>,
    Extension(actor): Extension<AuthedActor>,
    Json(q): Json<DataFileRequest>,
) -> Result<Json<OutputPreview>, ApiError> {
    let (state, actor, q) = (&state, &actor, &q);
    let (source, uri) = file_uri(state, actor, q).await?;
    object_view::preview(state.cloud.as_ref(), &uri)
        .await
        .map(Json)
        .map_err(|e| link_error(&source, e))
}

/// `POST /data/view-url` — a short-lived URL showing an image, PDF or HTML file inline, its
/// content type set from its name. 409 for a file with no such viewer.
#[utoipa::path(
    post,
    path = "/data/view-url",
    request_body = DataFileRequest,
    responses(
        (status = 200, body = OutputUrl), (status = 400), (status = 403), (status = 404),
        (status = 409, description = "the file cannot be shown in the browser; download it"),
        (status = 502), (status = 503)
    )
)]
pub(crate) async fn view_file_url(
    State(state): State<AppState>,
    Extension(actor): Extension<AuthedActor>,
    Json(q): Json<DataFileRequest>,
) -> Result<Json<OutputUrl>, ApiError> {
    file_link(&state, &actor, &q, Link::View).await
}

/// `POST /data/download-url` — a short-lived URL downloading a file as an attachment.
#[utoipa::path(
    post,
    path = "/data/download-url",
    request_body = DataFileRequest,
    responses(
        (status = 200, body = OutputUrl), (status = 400), (status = 403), (status = 404),
        (status = 502), (status = 503)
    )
)]
pub(crate) async fn download_file_url(
    State(state): State<AppState>,
    Extension(actor): Extension<AuthedActor>,
    Json(q): Json<DataFileRequest>,
) -> Result<Json<OutputUrl>, ApiError> {
    file_link(&state, &actor, &q, Link::Download).await
}

// ── routes ───────────────────────────────────────────────────────────────────────────────────────

/// The data browser: the project's data sources, folders and files: the routes behind the Cognito
/// middleware.
pub(super) fn routes() -> axum::Router<AppState> {
    axum::Router::new()
        .route("/data-sources", axum::routing::get(list_sources))
        // Each read that names a folder or file is a POST taking it in the body, so no identifier
        // reaches a request URL.
        .route("/data/list", axum::routing::post(list_folder))
        .route("/data/find", axum::routing::post(find_files))
        .route("/data/head", axum::routing::post(head_objects))
        .route("/data/preview", axum::routing::post(preview_file))
        .route("/data/view-url", axum::routing::post(view_file_url))
        .route("/data/download-url", axum::routing::post(download_file_url))
}

/// The data browser: the project's data sources, folders and files: the OpenAPI operations, tagged
/// `data` in [`super::ApiDoc`].
#[derive(utoipa::OpenApi)]
#[openapi(
    paths(
        list_sources,
        list_folder,
        find_files,
        head_objects,
        preview_file,
        view_file_url,
        download_file_url
    ),
    components(schemas(
        biffix_api::DataSource,
        DataFolderPage,
        DataFolder,
        DataFile,
        DataFindResult,
        DataHeadRequest,
        DataListRequest,
        DataFindRequest,
        DataFileRequest,
        DataHead,
        DataHeadList
    ))
)]
pub(super) struct Doc;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_matches_a_fragment_in_any_case() {
        assert!(contains_ignoring_case("P01_R1.fastq.gz", "r1"));
        assert!(contains_ignoring_case("P01_R1.fastq.gz", ""));
        assert!(!contains_ignoring_case("P01_R2.fastq.gz", "r1"));
        assert!(!contains_ignoring_case("r", "r1"));
        // Non-ASCII names fall back to Unicode lowercasing.
        assert!(contains_ignoring_case("ÉCHANTILLON.fq", "échantillon"));
        assert!(contains_ignoring_case("échantillon.fq", "chan"));
    }
}
