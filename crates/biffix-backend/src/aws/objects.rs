//! S3 objects: submit's input checks, the data browser, outputs and their previews, presigned
//! reads and source uploads ([`ObjectStore`]).

use async_trait::async_trait;

use super::AwsError;

/// An object's facts from an S3 HEAD.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ObjectHead {
    pub(crate) size: Option<i64>,
    /// The ETag, without its quotes.
    pub(crate) etag: Option<String>,
    /// The object's S3 version id; `None` in a bucket without versioning, and for S3's `"null"`
    /// version (an object written before versioning was enabled, or while it was suspended), which
    /// does not identify the content.
    pub(crate) version_id: Option<String>,
    /// Epoch micros.
    pub(crate) last_modified: Option<i64>,
}

/// One object of a listing.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct ListedObject {
    pub(crate) key: String,
    pub(crate) size: i64,
    /// Epoch micros.
    pub(crate) last_modified: Option<i64>,
    pub(crate) etag: Option<String>,
}

/// One page of an S3 listing (return of [`ObjectStore::list_objects`]).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ObjectPage {
    /// The common prefixes ("folders") under the listed prefix, when listed with a delimiter.
    pub(crate) prefixes: Vec<String>,
    pub(crate) objects: Vec<ListedObject>,
    /// S3's continuation token for the next page.
    pub(crate) next_token: Option<String>,
}

/// Bytes read from a ranged S3 GET.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RangeBody {
    pub(crate) bytes: Vec<u8>,
    /// The whole object's size, from `Content-Range`; `None` when S3 did not say.
    pub(crate) total_size: Option<u64>,
}

/// A presigned PUT for a staged workflow-source tarball (return of
/// [`ObjectStore::presign_source_put`]).
#[derive(Debug, Clone)]
pub struct PresignedSourceUpload {
    /// The presigned URL the client PUTs the tarball body to.
    pub(crate) url: String,
    /// The headers the PUT must send (they are signed): `If-None-Match: *` for a write-once one.
    pub(crate) headers: std::collections::BTreeMap<String, String>,
    /// The `s3://bucket/key` the object lands at — recorded verbatim as the version's `kind:s3`
    /// source.
    pub(crate) uri: String,
}

/// The `s3` key prefix every staged workflow source lives under. Register validates that a
/// `kind:s3` source uri stays within this prefix in the project bucket, so a caller cannot point a
/// version at an arbitrary object.
pub(crate) const SOURCE_PREFIX: &str = "workflow-sources/";

/// The content-addressed S3 key (relative to the project bucket) a workflow-source tarball is
/// staged at. Kept in one place so the presigner and the register-time uri validation agree.
/// `version_id` is `{git_sha}:{content_hash}`, so identical source re-stages to the same key.
pub(crate) fn source_object_key(version_id: &str) -> String {
    format!("{SOURCE_PREFIX}{version_id}/source.tar.gz")
}

/// The content-addressed S3 key (relative to the project bucket) an image build's context tarball
/// is staged at, beside its workflow's source: `tree_hash` is the context files' content hash.
pub(crate) fn context_object_key(version_id: &str, tree_hash: &str) -> String {
    format!("{SOURCE_PREFIX}{version_id}/contexts/{tree_hash}.tar.gz")
}

/// The S3 operations the backend needs on objects (see the module docs).
#[async_trait]
pub trait ObjectStore: Send + Sync {
    /// Report whether an object exists at `s3://bucket/key` (submit's input-existence check),
    /// and if so its provenance tag: the S3 version id when the object has one (see
    /// [`ObjectHead::version_id`]), else the unquoted `ETag`, else an empty string. `Ok(None)`
    /// means the object is absent.
    async fn head_object(&self, uri: &str) -> Result<Option<String>, AwsError> {
        Ok(self
            .head_object_meta(uri)
            .await?
            .map(|head| head.version_id.or(head.etag).unwrap_or_default()))
    }

    /// Report whether any object exists under the `s3://bucket/prefix/` prefix (submit existence
    /// check for a `WidgetHint::S3Dir` input). A prefix has no single ETag, so unlike
    /// [`Self::head_object`] this reports only presence, not a provenance tag.
    async fn prefix_exists(&self, uri: &str) -> Result<bool, AwsError>;

    /// Issue a presigned PUT URL for staging a workflow-source tarball at the content-addressed key
    /// `key` in the project bucket `bucket`: a [`source_object_key`] (private-repo support:
    /// S3-staged source) or a [`context_object_key`] (an image build's context). The client uploads
    /// the tarball to this URL over plain HTTPS — it needs no AWS credentials; the backend's own
    /// role signs the URL. Returns the URL to PUT to, the headers the PUT must send, and the
    /// resulting `s3://` uri to record.
    ///
    /// `write_once` makes the PUT conditional (`If-None-Match: *`, a signed header, so the client
    /// cannot drop it): it creates the object, or fails with 412 when the key already holds one.
    /// A staged source is write-once, since a version runs whatever its key holds.
    async fn presign_source_put(
        &self,
        bucket: &str,
        key: &str,
        write_once: bool,
    ) -> Result<PresignedSourceUpload, AwsError>;

    /// Issue a presigned GET for `s3://bucket/key`, valid for `expires`, whose response carries
    /// `content_disposition` when given (an attachment to save it under a clean name, or inline to
    /// view it) and, when given, `content_type` in place of whatever S3 stored. The backend's own
    /// role signs it; the caller has already checked the object is one it may serve (a registered
    /// output of a run the viewer can see, inside the project bucket).
    async fn presign_get(
        &self,
        uri: &str,
        expires: std::time::Duration,
        content_disposition: Option<&str>,
        content_type: Option<&str>,
    ) -> Result<String, AwsError>;

    /// List one page of `bucket` under `prefix`: with `delimited`, the direct children only (sub
    /// folders as `prefixes`); without, every object at any depth. `token` continues a prior page.
    async fn list_objects(
        &self,
        bucket: &str,
        prefix: &str,
        delimited: bool,
        token: Option<&str>,
        max_keys: i32,
    ) -> Result<ObjectPage, AwsError>;

    /// Write `body` to `s3://bucket/key` (a run's generated samplesheet); returns the object's
    /// ETag.
    async fn put_object(
        &self,
        uri: &str,
        body: Vec<u8>,
        content_type: &str,
    ) -> Result<String, AwsError>;

    /// HEAD `s3://bucket/key` for its size, etag and modification time; `Ok(None)` when absent.
    async fn head_object_meta(&self, uri: &str) -> Result<Option<ObjectHead>, AwsError>;

    /// Read bytes `start..=end` of `s3://bucket/key` (fewer when the object is shorter), with the
    /// object's full size when S3 reports it. An empty object reads as empty with size 0; a `start`
    /// at or past the end of a longer object reads as empty with its real size (or `None` when S3
    /// does not report it); an absent object is [`AwsError::NotFound`].
    async fn get_object_range(
        &self,
        uri: &str,
        start: u64,
        end: u64,
    ) -> Result<RangeBody, AwsError>;
}
