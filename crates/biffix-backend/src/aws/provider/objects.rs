//! S3 objects: HEADs and existence checks, listings, ranged reads, writes and presigned URLs.

use std::time::Duration;

use async_trait::async_trait;
use aws_sdk_s3::error::ProvideErrorMetadata;

use super::errors::{classify, classify_invalid, OrClassify};
use super::AwsCloudProvider;
use crate::aws::objects::{
    ListedObject, ObjectHead, ObjectPage, ObjectStore, PresignedSourceUpload, RangeBody,
};
use crate::aws::AwsError;

/// Split `s3://bucket/key` into its bucket and key, or an [`AwsError::S3`] describing the
/// malformation. The returned slices borrow from `uri`.
pub(super) fn split_s3_uri(uri: &str) -> Result<(&str, &str), AwsError> {
    biffix_core::s3_layout::split_s3_uri(uri)
        .ok_or_else(|| AwsError::S3(format!("not an s3://bucket/key uri: {uri}")))
}

/// The size of a whole object from a `Content-Range` header: `bytes 0-1023/52341` (a partial
/// read) or `bytes */52341` (a 416's). `None` when absent or the size is unknown (`*`).
/// An ETag as S3 answers it (quoted), without its quotes.
pub(super) fn unquoted_etag(etag: &str) -> String {
    etag.trim_matches('"').to_string()
}

/// A presigned URL's configuration: valid for `expires`.
pub(super) fn presign_config(
    expires: Duration,
) -> Result<aws_sdk_s3::presigning::PresigningConfig, AwsError> {
    aws_sdk_s3::presigning::PresigningConfig::expires_in(expires)
        .map_err(|e| AwsError::S3(format!("presign config: {e}")))
}

fn content_range_total(range: Option<&str>) -> Option<u64> {
    range?.rsplit_once('/')?.1.parse().ok()
}

/// The object's size when a range starting at `start` got S3's 416 (`InvalidRange`, the start at
/// or past the end): from the answer's `Content-Range` when it has one; else 0 for a range from the
/// first byte (any object with a byte satisfies it, so the object is empty); else unknown.
fn past_end_size(start: u64, content_range: Option<&str>) -> Option<u64> {
    content_range_total(content_range).or((start == 0).then_some(0))
}

/// S3's version id for an object that has none of its own (written before versioning was
/// enabled, or while it was suspended): it does not identify the content.
const NULL_VERSION_ID: &str = "null";

#[async_trait]
impl ObjectStore for AwsCloudProvider {
    async fn prefix_exists(&self, uri: &str) -> Result<bool, AwsError> {
        let (bucket, key) = split_s3_uri(uri)?;
        let resp = self
            .s3
            .list_objects_v2()
            .bucket(bucket)
            .prefix(key)
            .max_keys(1)
            .send()
            .await
            .or_classify(AwsError::S3)?;
        Ok(resp.key_count().unwrap_or(0) > 0 || !resp.contents().is_empty())
    }

    async fn presign_source_put(
        &self,
        bucket: &str,
        key: &str,
        write_once: bool,
    ) -> Result<PresignedSourceUpload, AwsError> {
        // The URL is short-lived: register happens immediately after the client uploads, so a tight
        // window is enough and limits exposure of the signed URL.
        let config = presign_config(Duration::from_secs(900))?;
        let mut put = self.s3.put_object().bucket(bucket).key(key);
        if write_once {
            // S3's conditional write: the PUT creates the object or fails with 412.
            put = put.if_none_match("*");
        }
        let presigned = put.presigned(config).await.or_classify(AwsError::S3)?;
        Ok(PresignedSourceUpload {
            url: presigned.uri().to_string(),
            // Every header the signature covers (bar `host`, which the URL carries).
            headers: presigned
                .headers()
                .filter(|(name, _)| !name.eq_ignore_ascii_case("host"))
                .map(|(name, value)| (name.to_string(), value.to_string()))
                .collect(),
            uri: format!("s3://{bucket}/{key}"),
        })
    }

    async fn presign_get(
        &self,
        uri: &str,
        expires: Duration,
        content_disposition: Option<&str>,
        content_type: Option<&str>,
    ) -> Result<String, AwsError> {
        let (bucket, key) = split_s3_uri(uri)?;
        let config = presign_config(expires)?;
        let presigned = self
            .s3
            .get_object()
            .bucket(bucket)
            .key(key)
            .set_response_content_disposition(content_disposition.map(str::to_string))
            .set_response_content_type(content_type.map(str::to_string))
            .presigned(config)
            .await
            .or_classify(AwsError::S3)?;
        Ok(presigned.uri().to_string())
    }

    async fn list_objects(
        &self,
        bucket: &str,
        prefix: &str,
        delimited: bool,
        token: Option<&str>,
        max_keys: i32,
    ) -> Result<ObjectPage, AwsError> {
        let mut req = self.s3.list_objects_v2().bucket(bucket).prefix(prefix).max_keys(max_keys);
        if delimited {
            req = req.delimiter("/");
        }
        if let Some(t) = token {
            req = req.continuation_token(t);
        }
        // S3 refuses a continuation token it did not issue with `InvalidArgument`: the caller's.
        let refused: &[&str] = if token.is_some() { &["InvalidArgument"] } else { &[] };
        let out = req.send().await.map_err(|e| classify_invalid(AwsError::S3, refused, &e))?;
        Ok(ObjectPage {
            prefixes: out
                .common_prefixes()
                .iter()
                .filter_map(|p| p.prefix().map(String::from))
                .collect(),
            objects: out
                .contents()
                .iter()
                .filter_map(|o| {
                    Some(ListedObject {
                        key: o.key()?.to_string(),
                        size: o.size().unwrap_or(0),
                        last_modified: o
                            .last_modified()
                            .and_then(|t| t.to_millis().ok())
                            .map(|ms| ms * 1000),
                        etag: o.e_tag().map(unquoted_etag),
                    })
                })
                .collect(),
            next_token: out.next_continuation_token().map(String::from),
        })
    }

    async fn put_object(
        &self,
        uri: &str,
        body: Vec<u8>,
        content_type: &str,
    ) -> Result<String, AwsError> {
        let (bucket, key) = split_s3_uri(uri)?;
        let out = self
            .s3
            .put_object()
            .bucket(bucket)
            .key(key)
            .content_type(content_type)
            .body(aws_sdk_s3::primitives::ByteStream::from(body))
            .send()
            .await
            .or_classify(AwsError::S3)?;
        Ok(unquoted_etag(out.e_tag().unwrap_or_default()))
    }

    async fn head_object_meta(&self, uri: &str) -> Result<Option<ObjectHead>, AwsError> {
        let (bucket, key) = split_s3_uri(uri)?;
        match self.s3.head_object().bucket(bucket).key(key).send().await {
            Ok(out) => Ok(Some(ObjectHead {
                size: out.content_length(),
                etag: out.e_tag().map(unquoted_etag),
                version_id: out.version_id().filter(|v| *v != NULL_VERSION_ID).map(str::to_string),
                last_modified: out
                    .last_modified()
                    .and_then(|t| t.to_millis().ok())
                    .map(|ms| ms * 1000),
            })),
            // A 404 means "absent" (a normal validation result), not a failure; any other error
            // (a missing grant, throttling) is surfaced so submit does not silently pass. HEAD's
            // error answers have no body, so `classify` reads the status.
            Err(err) => match classify(AwsError::S3, &err) {
                AwsError::NotFound => Ok(None),
                e => Err(e),
            },
        }
    }

    async fn get_object_range(
        &self,
        uri: &str,
        start: u64,
        end: u64,
    ) -> Result<RangeBody, AwsError> {
        // S3 ignores a malformed range (`bytes=10-5`) and answers the whole object, which would
        // be read with no cap: refuse it here.
        if start > end {
            return Err(AwsError::Invalid(format!(
                "byte range {start}-{end} of {uri} ends before it starts"
            )));
        }
        let (bucket, key) = split_s3_uri(uri)?;
        let resp = self
            .s3
            .get_object()
            .bucket(bucket)
            .key(key)
            .range(format!("bytes={start}-{end}"))
            .send()
            .await;
        let out = match resp {
            Ok(out) => out,
            // S3 answers a range that starts at or past the end of the object with 416
            // InvalidRange: an empty object, or a longer one read past its end.
            Err(err) if err.code() == Some("InvalidRange") => {
                let content_range =
                    err.raw_response().and_then(|r| r.headers().get("content-range"));
                return Ok(RangeBody {
                    bytes: Vec::new(),
                    total_size: past_end_size(start, content_range),
                });
            }
            Err(err) => return Err(classify(AwsError::S3, &err)),
        };
        let total_size = content_range_total(out.content_range());
        let bytes = out
            .body
            .collect()
            .await
            // The answer's body stream broke off (a dropped connection): retryable, as any
            // other transport failure.
            .map_err(|e| AwsError::Unavailable(format!("reading object body: {e}")))?
            .into_bytes()
            .into();
        Ok(RangeBody { bytes, total_size })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_an_s3_bucket_and_key_uri_is_split() {
        assert_eq!(split_s3_uri("s3://b/k/x").unwrap(), ("b", "k/x"));
        assert_eq!(split_s3_uri("s3://b/").unwrap(), ("b", ""));
        for bad in ["s3://b", "https://b/k", "s3:///k"] {
            assert!(matches!(split_s3_uri(bad), Err(AwsError::S3(_))), "{bad}");
        }
    }

    #[test]
    fn a_read_past_the_end_reports_the_real_size_or_none() {
        assert_eq!(content_range_total(Some("bytes 0-1023/52341")), Some(52341));
        assert_eq!(content_range_total(Some("bytes */29")), Some(29));
        assert_eq!(content_range_total(Some("bytes 0-9/*")), None);
        assert_eq!(content_range_total(None), None);
        // From the first byte, a 416 means the object is empty.
        assert_eq!(past_end_size(0, None), Some(0));
        // Past the end of a longer object: its size when S3 says, else unknown (never 0).
        assert_eq!(past_end_size(1000, Some("bytes */29")), Some(29));
        assert_eq!(past_end_size(1000, None), None);
    }
}
