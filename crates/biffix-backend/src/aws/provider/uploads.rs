//! S3 multipart uploads, created, presigned per part, listed, completed create-only and aborted.

use std::time::Duration;

use async_trait::async_trait;
use aws_sdk_s3::error::ProvideErrorMetadata;

use super::errors::{classify, OrClassify};
use super::objects::{presign_config, split_s3_uri, unquoted_etag};
use super::AwsCloudProvider;
use crate::aws::uploads::{MultipartUploads, UploadCompletion, UploadedPart};
use crate::aws::AwsError;

/// Whether an S3 answer (its HTTP `status` and error `code`) is a failed `If-None-Match: *`
/// precondition: an object is already at the key.
fn precondition_failed(status: Option<u16>, code: Option<&str>) -> bool {
    status == Some(412) || code == Some("PreconditionFailed")
}

#[async_trait]
impl MultipartUploads for AwsCloudProvider {
    async fn create_multipart_upload(
        &self,
        uri: &str,
        content_type: &str,
    ) -> Result<String, AwsError> {
        let (bucket, key) = split_s3_uri(uri)?;
        let out = self
            .s3
            .create_multipart_upload()
            .bucket(bucket)
            .key(key)
            .content_type(content_type)
            .send()
            .await
            .or_classify(AwsError::S3)?;
        out.upload_id()
            .map(str::to_string)
            .ok_or_else(|| AwsError::S3("CreateMultipartUpload returned no upload id".into()))
    }

    async fn presign_upload_part(
        &self,
        uri: &str,
        upload_id: &str,
        part_number: i32,
        expires: Duration,
    ) -> Result<String, AwsError> {
        let (bucket, key) = split_s3_uri(uri)?;
        let config = presign_config(expires)?;
        let presigned = self
            .s3
            .upload_part()
            .bucket(bucket)
            .key(key)
            .upload_id(upload_id)
            .part_number(part_number)
            .presigned(config)
            .await
            .or_classify(AwsError::S3)?;
        Ok(presigned.uri().to_string())
    }

    async fn list_upload_parts(
        &self,
        uri: &str,
        upload_id: &str,
    ) -> Result<Vec<UploadedPart>, AwsError> {
        let (bucket, key) = split_s3_uri(uri)?;
        let mut parts = Vec::new();
        // The SDK's paginator follows `next_part_number_marker` while `is_truncated`, and stops on
        // a repeated marker.
        let mut pages = self
            .s3
            .list_parts()
            .bucket(bucket)
            .key(key)
            .upload_id(upload_id)
            .into_paginator()
            .send();
        while let Some(out) = pages.next().await {
            let out = out.or_classify(AwsError::S3)?;
            parts.extend(out.parts().iter().map(|p| UploadedPart {
                part_number: p.part_number().unwrap_or_default(),
                size: p.size().unwrap_or_default(),
                etag: unquoted_etag(p.e_tag().unwrap_or_default()),
            }));
        }
        Ok(parts)
    }

    async fn complete_multipart_upload(
        &self,
        uri: &str,
        upload_id: &str,
        parts: &[(i32, String)],
    ) -> Result<UploadCompletion, AwsError> {
        use aws_sdk_s3::types::{CompletedMultipartUpload, CompletedPart};
        let (bucket, key) = split_s3_uri(uri)?;
        let completed = CompletedMultipartUpload::builder()
            .set_parts(Some(
                parts
                    .iter()
                    .map(|(n, etag)| {
                        CompletedPart::builder()
                            .part_number(*n)
                            .e_tag(format!("\"{}\"", etag.trim_matches('"')))
                            .build()
                    })
                    .collect(),
            ))
            .build();
        let result = self
            .s3
            .complete_multipart_upload()
            .bucket(bucket)
            .key(key)
            .upload_id(upload_id)
            .multipart_upload(completed)
            // Create-only: S3 answers 412 when an object is already at the key.
            .if_none_match("*")
            .send()
            .await;
        let out = match result {
            Ok(out) => out,
            Err(e)
                if precondition_failed(e.raw_response().map(|r| r.status().as_u16()), e.code()) =>
            {
                return Ok(UploadCompletion::Exists)
            }
            Err(e) => return Err(classify(AwsError::S3, &e)),
        };
        Ok(UploadCompletion::Created(unquoted_etag(out.e_tag().unwrap_or_default())))
    }

    async fn abort_multipart_upload(&self, uri: &str, upload_id: &str) -> Result<(), AwsError> {
        let (bucket, key) = split_s3_uri(uri)?;
        self.s3
            .abort_multipart_upload()
            .bucket(bucket)
            .key(key)
            .upload_id(upload_id)
            .send()
            .await
            .or_classify(AwsError::S3)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_create_only_precondition_is_an_existing_object() {
        assert!(precondition_failed(Some(412), Some("PreconditionFailed")));
        assert!(precondition_failed(Some(412), None));
        assert!(precondition_failed(None, Some("PreconditionFailed")));
        // A concurrent conditional write (409) or any other failure is not.
        assert!(!precondition_failed(Some(409), Some("ConditionalRequestConflict")));
        assert!(!precondition_failed(Some(400), Some("InvalidPart")));
        assert!(!precondition_failed(None, None));
    }
}
