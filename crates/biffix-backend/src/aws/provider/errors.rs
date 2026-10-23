//! How an SDK failure reads: one classification for every operation ([`classify`]).

use aws_sdk_s3::error::{DisplayErrorContext, ProvideErrorMetadata, SdkError};

use crate::aws::AwsError;

/// Error codes that mean AWS is shedding load or failing on its side.
const UNAVAILABLE_CODES: &[&str] = &[
    "Throttling",
    "ThrottlingException",
    "ThrottledException",
    "TooManyRequestsException",
    "RequestLimitExceeded",
    "SlowDown",
    "ServiceUnavailable",
    // Another conditional write to the key is in flight: retry (S3's advice).
    "ConditionalRequestConflict",
    "InternalError",
    "InternalFailure",
    "RequestTimeout",
];

/// Error codes that mean the deploy's IAM does not allow the call.
const FORBIDDEN_CODES: &[&str] = &["AccessDenied", "AccessDeniedException", "AuthorizationError"];

/// Error codes that mean the object, upload or stream does not exist.
const NOT_FOUND_CODES: &[&str] =
    &["NoSuchKey", "NoSuchUpload", "NotFound", "ResourceNotFoundException"];

/// Error codes that mean S3 refused the request as the caller made it (a multipart part list that
/// does not match the upload's parts).
const INVALID_CODES: &[&str] = &["InvalidPart", "InvalidPartOrder", "EntityTooSmall"];

/// Classify an AWS failure from its parts: `transport` (a timeout or dispatch failure: no answer),
/// the HTTP `status` and error `code` of the answer, and the full `message`. A timeout, dispatch
/// failure, throttling or 5xx is [`AwsError::Unavailable`]; a 403 or access-denied code is
/// [`AwsError::Forbidden`]; a 404 or not-found code [`AwsError::NotFound`]; a rejected part list
/// [`AwsError::Invalid`]; anything else is the service's own variant (`service`). The status
/// matters for a HEAD, whose error answer has no body and so no code.
fn classify_parts(
    service: fn(String) -> AwsError,
    transport: bool,
    status: Option<u16>,
    code: Option<&str>,
    message: String,
) -> AwsError {
    let is = |codes: &[&str]| code.is_some_and(|c| codes.contains(&c));
    if transport || is(UNAVAILABLE_CODES) || status.is_some_and(|s| s == 429 || s >= 500) {
        AwsError::Unavailable(message)
    } else if is(FORBIDDEN_CODES) || status == Some(403) {
        AwsError::Forbidden(message)
    } else if is(NOT_FOUND_CODES) || status == Some(404) {
        AwsError::NotFound
    } else if is(INVALID_CODES) {
        AwsError::Invalid(message)
    } else {
        service(message)
    }
}

/// `err` (an operation on `resource`, a resource the deploy configures) as a not-found reads for
/// it: [`AwsError::Misconfigured`] naming it, since the caller named nothing missing; any other
/// `err` as it is. `message` is the failure's whole message.
fn configured(resource: &str, err: AwsError, message: impl FnOnce() -> String) -> AwsError {
    match err {
        AwsError::NotFound => {
            AwsError::Misconfigured(format!("{resource} does not exist ({})", message()))
        }
        other => other,
    }
}

/// [`classify`] for an operation on `resource`, a resource the deploy configures (a CodeBuild
/// project, the alerts topic): its not-found is the deploy's fault ([`AwsError::Misconfigured`]),
/// not a 404 for the caller.
pub(super) fn classify_configured<E>(
    service: fn(String) -> AwsError,
    resource: &str,
    err: &SdkError<E>,
) -> AwsError
where
    E: ProvideErrorMetadata + std::error::Error + 'static,
{
    configured(resource, classify(service, err), || DisplayErrorContext(err).to_string())
}

/// Classify an SDK failure ([`classify_parts`]), keeping its whole cause chain in the message (a
/// bare `into_service_error().to_string()` reads just "unhandled error" for a timeout).
pub(super) fn classify<E>(service: fn(String) -> AwsError, err: &SdkError<E>) -> AwsError
where
    E: ProvideErrorMetadata + std::error::Error + 'static,
{
    classify_parts(
        service,
        matches!(err, SdkError::TimeoutError(_) | SdkError::DispatchFailure(_)),
        err.raw_response().map(|r| r.status().as_u16()),
        err.code(),
        DisplayErrorContext(err).to_string(),
    )
}

/// [`classify`] as a method on an SDK call's result: `.send().await.or_classify(AwsError::S3)?`.
pub(super) trait OrClassify<T> {
    fn or_classify(self, service: fn(String) -> AwsError) -> Result<T, AwsError>;
}

impl<T, E> OrClassify<T> for Result<T, SdkError<E>>
where
    E: ProvideErrorMetadata + std::error::Error + 'static,
{
    fn or_classify(self, service: fn(String) -> AwsError) -> Result<T, AwsError> {
        self.map_err(|e| classify(service, &e))
    }
}

/// [`classify`], except that a failure whose code is one of `invalid` — the codes with which this
/// operation refuses what its caller passed it (S3's `InvalidArgument` for a continuation token it
/// did not issue) — is [`AwsError::Invalid`], the caller's fault.
pub(super) fn classify_invalid<E>(
    service: fn(String) -> AwsError,
    invalid: &[&str],
    err: &SdkError<E>,
) -> AwsError
where
    E: ProvideErrorMetadata + std::error::Error + 'static,
{
    refused_as_invalid(err.code(), invalid, || DisplayErrorContext(err).to_string())
        .unwrap_or_else(|| classify(service, err))
}

/// [`AwsError::Invalid`] when `code` is one of `invalid`.
fn refused_as_invalid(
    code: Option<&str>,
    invalid: &[&str],
    message: impl FnOnce() -> String,
) -> Option<AwsError> {
    code.filter(|c| invalid.contains(c)).map(|_| AwsError::Invalid(message()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_an_operation_s_own_refusal_codes_are_the_caller_s() {
        let invalid = ["InvalidArgument"];
        assert!(matches!(
            refused_as_invalid(Some("InvalidArgument"), &invalid, || "m".into()),
            Some(AwsError::Invalid(m)) if m == "m"
        ));
        assert!(refused_as_invalid(Some("AccessDenied"), &invalid, || "m".into()).is_none());
        assert!(refused_as_invalid(None, &invalid, || "m".into()).is_none());
        // Without the operation's codes, S3's InvalidArgument is S3's own failure (a 502).
        assert!(matches!(
            classify_parts(AwsError::S3, false, Some(400), Some("InvalidArgument"), "m".into()),
            AwsError::S3(_)
        ));
    }

    #[test]
    fn a_missing_resource_the_deploy_configures_is_its_fault_not_a_404() {
        let missing = classify_parts(
            AwsError::CodeBuild,
            false,
            Some(400),
            Some("ResourceNotFoundException"),
            "m".into(),
        );
        assert!(matches!(missing, AwsError::NotFound));
        let read =
            configured("CodeBuild project biffix-images", missing, || "no such project".into());
        assert_eq!(
            read.to_string(),
            "this deploy is misconfigured: CodeBuild project biffix-images does not exist (no such \
             project)"
        );
        assert!(matches!(
            crate::error::ServiceError::from(read),
            crate::error::ServiceError::Internal(_)
        ));
        // Anything else reads as ever.
        let throttled = configured("x", AwsError::Unavailable("slow".into()), || "m".into());
        assert!(matches!(throttled, AwsError::Unavailable(_)));
    }

    #[test]
    fn aws_failures_are_classified_by_status_and_code() {
        let s3 = |transport, status, code: Option<&str>| {
            classify_parts(AwsError::S3, transport, status, code, "m".into())
        };
        // No answer, throttling or a 5xx: AWS is unavailable.
        assert!(matches!(s3(true, None, None), AwsError::Unavailable(_)));
        assert!(matches!(s3(false, Some(503), Some("SlowDown")), AwsError::Unavailable(_)));
        assert!(matches!(s3(false, Some(500), None), AwsError::Unavailable(_)));
        assert!(matches!(s3(false, Some(429), None), AwsError::Unavailable(_)));
        assert!(matches!(
            classify_parts(AwsError::Sns, false, Some(400), Some("Throttling"), "m".into()),
            AwsError::Unavailable(_)
        ));
        // A missing grant, by code or by a HEAD's bodiless 403.
        assert!(matches!(s3(false, Some(403), Some("AccessDenied")), AwsError::Forbidden(_)));
        assert!(matches!(s3(false, Some(403), None), AwsError::Forbidden(_)));
        assert!(matches!(
            classify_parts(AwsError::Sns, false, Some(403), Some("AuthorizationError"), "m".into()),
            AwsError::Forbidden(_)
        ));
        // Absent, by code or by a HEAD's bodiless 404.
        assert!(matches!(s3(false, Some(404), None), AwsError::NotFound));
        assert!(matches!(s3(false, Some(404), Some("NoSuchUpload")), AwsError::NotFound));
        assert!(matches!(
            classify_parts(
                AwsError::Logs,
                false,
                Some(400),
                Some("ResourceNotFoundException"),
                "m".into()
            ),
            AwsError::NotFound
        ));
        // A part list that does not match the upload is the caller's.
        for code in INVALID_CODES {
            assert!(matches!(s3(false, Some(400), Some(code)), AwsError::Invalid(_)));
        }
        // Anything else is the service's, with its message.
        assert!(matches!(
            s3(false, Some(400), Some("InvalidBucketName")),
            AwsError::S3(m) if m == "m"
        ));
        assert!(matches!(
            classify_parts(AwsError::Batch, false, Some(400), Some("ClientException"), "m".into()),
            AwsError::Batch(_)
        ));
    }

    #[test]
    fn an_sdk_failure_keeps_its_cause() {
        let err: SdkError<aws_sdk_s3::operation::head_object::HeadObjectError> =
            SdkError::timeout_error("connect timed out after 3.1s");
        match classify(AwsError::S3, &err) {
            AwsError::Unavailable(m) => assert!(m.contains("connect timed out"), "{m}"),
            other => panic!("{other:?}"),
        }
    }
}
