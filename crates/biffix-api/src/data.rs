//! Data sources: where the launch form's file picker looks for inputs.

use serde::{Deserialize, Serialize};

/// A place a run's inputs can come from: the project's own bucket, or an S3 location an admin
/// registered in the deploy config (a sequencing core's drop folder, a reference library).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct DataSource {
    /// Stable id (`project` for the project bucket).
    pub id: String,
    /// The name people know it by ("Sequencing core drop").
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// `project`, `data` or `references`.
    pub kind: String,
    /// Its root as `s3://bucket/prefix/`.
    pub uri: String,
    /// Files can be uploaded here: the project bucket, when the deploy has enabled browser uploads
    /// to it (its CORS rule is in place).
    pub writable: bool,
    /// Paths (relative to the root) the Data page hides: a folder or file whose path starts with
    /// one of these cannot be listed, previewed or downloaded through the source, unless its path
    /// also starts with one of `browsable_exceptions`. The server's own rule, so a client can tell
    /// whether a link into the source would open rather than copy it. Empty (absent) when the
    /// source hides nothing.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hidden_prefixes: Vec<String>,
    /// Paths under a `hidden_prefixes` entry that are browsable after all (a project's run
    /// folders, `biffix/runs/`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub browsable_exceptions: Vec<String>,
}

/// `GET /data-sources` query: whose sources to list.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::IntoParams))]
#[cfg_attr(feature = "openapi", into_params(parameter_in = Query))]
pub struct DataSourcesQuery {
    /// The project whose sources to use (its bucket is the `project` source); absent ⇒ the
    /// deploy's default project. A deploy of one project uses it whatever this names.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
}

/// `POST /data/uploads`: start a resumable upload into the project bucket.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct UploadStartRequest {
    /// The object key in the project bucket; must be under `uploads/`.
    pub key: String,
    /// The file's size in bytes.
    pub size: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    /// The project whose bucket the upload is in; absent ⇒ the deploy's default project. A deploy
    /// of one project uses it whatever this names.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
}

/// A started upload: send `parts` parts of `part_size` bytes (the last may be shorter).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct StartedUpload {
    pub upload_id: String,
    pub key: String,
    /// Where the file will be (`s3://<project bucket>/<key>`).
    pub uri: String,
    pub part_size: u64,
    pub parts: u32,
}

/// `POST /data/uploads/{upload_id}/status` and `/abort` request: the upload's object key, in the
/// body so a file name never reaches a request URL or an access log.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct UploadKeyRequest {
    pub key: String,
    /// The project whose bucket the upload is in; absent ⇒ the deploy's default project. A deploy
    /// of one project uses it whatever this names.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
}

/// `POST /data/uploads/{upload_id}/parts`: URLs to `PUT` parts to.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct UploadPartsRequest {
    pub key: String,
    pub part_numbers: Vec<i32>,
    /// The project whose bucket the upload is in; absent ⇒ the deploy's default project. A deploy
    /// of one project uses it whatever this names.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct UploadPartUrl {
    pub part_number: i32,
    /// A presigned `PUT` URL, valid for 15 minutes; the response's `ETag` header names the part.
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct UploadPartUrlList {
    pub urls: Vec<UploadPartUrl>,
}

/// A part S3 already holds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct UploadedPart {
    pub part_number: i32,
    pub size: i64,
    pub etag: String,
}

/// `POST /data/uploads/{upload_id}/status`: the parts already uploaded, to resume.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct UploadStatus {
    pub parts: Vec<UploadedPart>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct UploadCompletePart {
    pub part_number: i32,
    pub etag: String,
}

/// `POST /data/uploads/{upload_id}/complete`: assemble the file from its parts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct UploadCompleteRequest {
    pub key: String,
    pub parts: Vec<UploadCompletePart>,
    /// The project whose bucket the upload is in; absent ⇒ the deploy's default project. A deploy
    /// of one project uses it whatever this names.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct UploadedFile {
    pub uri: String,
    pub etag: String,
}

/// A sub-folder in a listing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct DataFolder {
    /// The folder's own name (no slashes).
    pub name: String,
    /// Its path relative to the source root, ending in `/`.
    pub path: String,
}

/// An object in a listing or a search.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct DataFile {
    /// The object's base name.
    pub name: String,
    /// Its path relative to the source root.
    pub path: String,
    /// `s3://bucket/key`, as a param value.
    pub uri: String,
    pub size: i64,
    /// Last modified, epoch microseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_modified_micros: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub etag: Option<String>,
    /// How the browser can show it, from its name (`table`, `text`, `image`, `pdf`, `html`), as for
    /// run outputs; absent when it can only be downloaded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub viewer: Option<String>,
}

/// `POST /data/list`: one folder of a data source, one page at a time.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct DataFolderPage {
    pub source: String,
    /// The folder listed, relative to the source root (`""` for the root, else ending in `/`).
    pub path: String,
    pub folders: Vec<DataFolder>,
    pub files: Vec<DataFile>,
    /// Pass back as `cursor` for the next page; absent on the last.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

/// `POST /data/find`: objects under a folder (at any depth) whose name contains a fragment.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct DataFindResult {
    pub source: String,
    pub files: Vec<DataFile>,
    /// A match may have been left out: more files matched than the limit, or the search stopped
    /// at its scan budget before reading the whole folder.
    pub truncated: bool,
}

/// `POST /data/list` request: in a body, so folder names (which can name samples) never reach a
/// request URL or an access log.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct DataListRequest {
    /// Data source id.
    pub source: String,
    /// The folder, relative to the source root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Only names in the folder that start with this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub starts_with: Option<String>,
    /// Cursor from a prior page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    /// The project whose sources to use (its bucket is the `project` source); absent ⇒ the
    /// deploy's default project. A deploy of one project uses it whatever this names.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
}

/// `POST /data/find` request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct DataFindRequest {
    /// Data source id.
    pub source: String,
    /// The folder to search under.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// A fragment of the file name (case-insensitive).
    pub q: String,
    /// Most matches (default 500, max 5000).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
    /// The project whose sources to use (its bucket is the `project` source); absent ⇒ the
    /// deploy's default project. A deploy of one project uses it whatever this names.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
}

/// `POST /data/preview`, `/data/view-url` and `/data/download-url` request: one file of a source,
/// named in the body rather than the request URL.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct DataFileRequest {
    /// Data source id.
    pub source: String,
    /// The file, relative to the source root.
    pub path: String,
    /// The project whose sources to use (its bucket is the `project` source); absent ⇒ the
    /// deploy's default project. A deploy of one project uses it whatever this names.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
}

/// `POST /data/head` request: which objects to look up.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct DataHeadRequest {
    /// `s3://` URIs; at most 200.
    pub uris: Vec<String>,
    /// The project whose sources to use (its bucket is the `project` source); absent ⇒ the
    /// deploy's default project. A deploy of one project uses it whatever this names.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
}

/// One looked-up object.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct DataHead {
    pub uri: String,
    /// `true` exists; `false` absent; absent when the URI is outside every data source (biffix
    /// cannot see it, but a run's role might).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exists: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<i64>,
}

/// `POST /data/head` response, in request order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct DataHeadList {
    pub objects: Vec<DataHead>,
}
