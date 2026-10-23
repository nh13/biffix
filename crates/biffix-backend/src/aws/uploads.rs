//! S3 multipart uploads: a user's file upload into a project bucket ([`MultipartUploads`]).

use async_trait::async_trait;

use super::AwsError;

/// What completing a multipart upload did ([`MultipartUploads::complete_multipart_upload`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UploadCompletion {
    /// The object was created; its ETag, without quotes.
    Created(String),
    /// An object already exists at the key (S3's 412 `PreconditionFailed`); nothing was written.
    Exists,
}

/// A part of a multipart upload S3 already holds (for resuming an upload).
#[derive(Clone, Debug, PartialEq)]
pub struct UploadedPart {
    pub(crate) part_number: i32,
    pub(crate) size: i64,
    pub(crate) etag: String,
}

/// A multipart upload's steps (see the module docs).
#[async_trait]
pub trait MultipartUploads: Send + Sync {
    /// Start a multipart upload of `uri`; returns its upload id.
    async fn create_multipart_upload(
        &self,
        uri: &str,
        content_type: &str,
    ) -> Result<String, AwsError>;

    /// A presigned `PUT` URL for one part of an upload.
    async fn presign_upload_part(
        &self,
        uri: &str,
        upload_id: &str,
        part_number: i32,
        expires: std::time::Duration,
    ) -> Result<String, AwsError>;

    /// The parts an upload already holds, by part number; `NotFound` for an unknown upload.
    async fn list_upload_parts(
        &self,
        uri: &str,
        upload_id: &str,
    ) -> Result<Vec<UploadedPart>, AwsError>;

    /// Assemble an upload from its parts (`(part number, ETag)`, ascending), create-only: S3's
    /// conditional complete (`If-None-Match: *`) never replaces an object already at the key, so an
    /// upload cannot overwrite another user's file, or an input a finished run recorded, even when
    /// two uploads to one key race. [`UploadCompletion::Exists`] then (nothing is written and the
    /// upload stays open; the caller aborts it). `NotFound` for an unknown upload.
    async fn complete_multipart_upload(
        &self,
        uri: &str,
        upload_id: &str,
        parts: &[(i32, String)],
    ) -> Result<UploadCompletion, AwsError>;

    /// Abandon an upload and delete its parts. `NotFound` for an unknown upload.
    async fn abort_multipart_upload(&self, uri: &str, upload_id: &str) -> Result<(), AwsError>;
}
