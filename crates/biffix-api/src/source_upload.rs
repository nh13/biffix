//! `source-upload` — presign a PUT for staging a workflow-source tarball (S3-staged source, for
//! private repos). The client stages the source before it registers the version.

use serde::{Deserialize, Serialize};

/// `POST /workflows/source-upload` request: ask the backend to presign a PUT for staging a
/// workflow-source tarball. The client computes the source identity (`{git_sha}:{content_hash}`)
/// locally — the id the registered version extends with its build number — so the staged object is
/// content-addressed and can be uploaded before the `POST /workflows` call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct SourceUploadRequest {
    /// The source identity (`{git_sha}:{content_hash}`, a version id without its `:{build}`) the
    /// source is staged under. Every build registered from this source shares it.
    pub version_id: String,
    /// Stage an image build's context instead of the source: the content hash of the context's
    /// files (64 hex), which names the tarball beside the source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_tree_hash: Option<String>,
    /// The project the version is registered in, whose bucket the tarball is staged into. A
    /// deploy of one project stages into it whatever this names.
    pub project: String,
}

/// `POST /workflows/source-upload` response: where and how to upload the tarball.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct SourceUploadTarget {
    /// The presigned URL to PUT the `source.tar.gz` body to (plain HTTPS; no AWS creds needed).
    pub url: String,
    /// Headers the PUT must send as given (the URL's signature covers them). A source's PUT is
    /// write-once (`If-None-Match: *`): a version runs whatever its staged key holds, so a staged
    /// source is never replaced, and a 412 means this source is already staged there. A build
    /// context's PUT has none (its build checks the tarball's sha256).
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub headers: std::collections::BTreeMap<String, String>,
    /// The `s3://` uri the object lands at — record this verbatim as the version's `kind:s3` source.
    pub uri: String,
}
