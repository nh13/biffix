//! The one error type HTTP handlers answer a failed request with.
//!
//! Each variant is an HTTP status; its message is the response body, as plain text, for the caller
//! to read (the UI shows it). An empty message answers with the status alone, no body: the reads
//! that have always answered so keep doing it, and a client then words the failure itself.
//! [`ApiError::status`] is the one place a kind maps to its status. A service's failure arrives as a
//! [`ServiceError`], which maps to the kind of the same name; each domain failure (store, AWS, image
//! build, submit, launch, registration) maps through the `ServiceError` it is.

use axum::extract::rejection::JsonRejection;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

use crate::aws::AwsError;
use crate::error::ServiceError;
use crate::images::build_follow::BuildError;
use crate::register::version::RegisterError;
use crate::runs::launch::LaunchError;
use crate::runs::submit::SubmitError;
use crate::store::StoreError;

/// A failed request: its kind (so its status) and the message the caller reads.
#[derive(Debug, thiserror::Error)]
pub(crate) enum ApiError {
    /// 400: the request is malformed or asks for what cannot be done.
    #[error("{0}")]
    BadRequest(String),
    /// 401: no credential, or one that does not verify.
    #[error("{0}")]
    Unauthorized(String),
    /// 403: the caller may not do this.
    #[error("{0}")]
    Forbidden(String),
    /// 404: what the request names does not exist (or the caller cannot see it).
    #[error("{0}")]
    NotFound(String),
    /// 409: the request conflicts with what is recorded (a duplicate, a state that forbids it).
    #[error("{0}")]
    Conflict(String),
    /// 413: the request body is larger than accepted.
    #[error("{0}")]
    PayloadTooLarge(String),
    /// 422: what the request names exists but cannot be shown (too large, an unreadable format).
    #[error("{0}")]
    Unprocessable(String),
    /// 500: our fault (a misconfigured deploy, a corrupt stored value).
    #[error("{0}")]
    Internal(String),
    /// 502: AWS (or another upstream) failed in a way retrying will not fix.
    #[error("{0}")]
    BadGateway(String),
    /// 503: the store or AWS is unreachable or shedding load; retry.
    #[error("{0}")]
    Unavailable(String),
    /// A JSON body axum refused for a reason other than its content (say, no JSON content type):
    /// answered as axum answers it.
    #[error(transparent)]
    Rejection(JsonRejection),
}

impl ApiError {
    /// The HTTP status the error answers with.
    pub(crate) fn status(&self) -> StatusCode {
        match self {
            ApiError::BadRequest(_) => StatusCode::BAD_REQUEST,
            ApiError::Unauthorized(_) => StatusCode::UNAUTHORIZED,
            ApiError::Forbidden(_) => StatusCode::FORBIDDEN,
            ApiError::NotFound(_) => StatusCode::NOT_FOUND,
            ApiError::Conflict(_) => StatusCode::CONFLICT,
            ApiError::PayloadTooLarge(_) => StatusCode::PAYLOAD_TOO_LARGE,
            ApiError::Unprocessable(_) => StatusCode::UNPROCESSABLE_ENTITY,
            ApiError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
            ApiError::BadGateway(_) => StatusCode::BAD_GATEWAY,
            ApiError::Unavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
            ApiError::Rejection(r) => r.status(),
        }
    }

    /// The message the caller reads, or `None` for a rejection (answered as axum answers it).
    fn message_mut(&mut self) -> Option<&mut String> {
        match self {
            ApiError::BadRequest(m)
            | ApiError::Unauthorized(m)
            | ApiError::Forbidden(m)
            | ApiError::NotFound(m)
            | ApiError::Conflict(m)
            | ApiError::PayloadTooLarge(m)
            | ApiError::Unprocessable(m)
            | ApiError::Internal(m)
            | ApiError::BadGateway(m)
            | ApiError::Unavailable(m) => Some(m),
            ApiError::Rejection(_) => None,
        }
    }

    /// The same kind of error with `message` in place of its own (a rejection keeps axum's).
    pub(crate) fn with_message(mut self, message: impl Into<String>) -> Self {
        if let Some(m) = self.message_mut() {
            *m = message.into();
        }
        self
    }

    /// `err` as an API error, its message made from `err` itself (`|e| format!("reading x: {e}")`).
    pub(crate) fn reworded<E: Into<ApiError>>(err: E, message: impl FnOnce(&E) -> String) -> Self {
        let message = message(&err);
        err.into().with_message(message)
    }

    /// `err` as an API error with no message: the reads that answer with a status alone.
    pub(crate) fn status_only(err: impl Into<ApiError>) -> Self {
        err.into().with_message(String::new())
    }
}

/// A store failure where a missing row is a 404 worded by `message`; any other answers as itself.
pub(crate) fn not_found_as(
    message: impl FnOnce() -> String,
) -> impl FnOnce(StoreError) -> ApiError {
    move |e| match e {
        StoreError::NotFound => ApiError::NotFound(message()),
        other => other.into(),
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = self.status();
        match self {
            ApiError::Rejection(rejection) => rejection.into_response(),
            ApiError::BadRequest(m)
            | ApiError::Unauthorized(m)
            | ApiError::Forbidden(m)
            | ApiError::NotFound(m)
            | ApiError::Conflict(m)
            | ApiError::PayloadTooLarge(m)
            | ApiError::Unprocessable(m)
            | ApiError::Internal(m)
            | ApiError::BadGateway(m)
            | ApiError::Unavailable(m) => {
                if m.is_empty() {
                    status.into_response()
                } else {
                    (status, m).into_response()
                }
            }
        }
    }
}

/// A service's error answers with its kind's status, its message as the body.
impl From<ServiceError> for ApiError {
    fn from(err: ServiceError) -> Self {
        match err {
            ServiceError::Invalid(m) => ApiError::BadRequest(m),
            ServiceError::Unauthorized(m) => ApiError::Unauthorized(m),
            ServiceError::Forbidden(m) => ApiError::Forbidden(m),
            ServiceError::NotFound(m) => ApiError::NotFound(m),
            ServiceError::Conflict(m) => ApiError::Conflict(m),
            ServiceError::TooLarge(m) => ApiError::PayloadTooLarge(m),
            ServiceError::Unprocessable(m) => ApiError::Unprocessable(m),
            ServiceError::Internal(m) => ApiError::Internal(m),
            ServiceError::Upstream(m) => ApiError::BadGateway(m),
            ServiceError::Unavailable(m) => ApiError::Unavailable(m),
        }
    }
}

/// Each domain failure answers as the [`ServiceError`] it maps to (where that mapping lives).
macro_rules! via_service_error {
    ($($t:ty),* $(,)?) => {$(
        impl From<$t> for ApiError {
            fn from(err: $t) -> Self {
                ServiceError::from(err).into()
            }
        }
    )*};
}

via_service_error!(StoreError, AwsError, BuildError, SubmitError, LaunchError, RegisterError);

impl From<JsonRejection> for ApiError {
    fn from(rejection: JsonRejection) -> Self {
        ApiError::Rejection(rejection)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parts(e: ApiError) -> (StatusCode, String) {
        let status = e.status();
        (status, e.to_string())
    }

    #[test]
    fn a_store_error_keeps_a_conflict_s_reason_and_the_status() {
        assert_eq!(
            parts(StoreError::Conflict("name taken".into()).into()),
            (StatusCode::CONFLICT, "name taken".to_string())
        );
        assert_eq!(
            parts(StoreError::Unavailable("down".into()).into()),
            (StatusCode::SERVICE_UNAVAILABLE, "store unavailable: down".to_string())
        );
    }

    #[test]
    fn a_store_error_maps_each_variant() {
        let cases = [
            (StoreError::NotFound, StatusCode::NOT_FOUND),
            (StoreError::Conflict("dup".into()), StatusCode::CONFLICT),
            (StoreError::Unavailable("down".into()), StatusCode::SERVICE_UNAVAILABLE),
            (StoreError::Serialization("corrupt".into()), StatusCode::INTERNAL_SERVER_ERROR),
            (StoreError::Rejected("nul".into()), StatusCode::BAD_REQUEST),
        ];
        for (e, status) in cases {
            assert_eq!(ApiError::from(e).status(), status);
        }
    }

    #[test]
    fn aws_failures_map_to_the_status_that_names_whose_fault_it_is() {
        let cases = [
            (AwsError::Invalid("x".into()), StatusCode::BAD_REQUEST),
            (AwsError::Forbidden("x".into()), StatusCode::FORBIDDEN),
            (AwsError::NotFound, StatusCode::NOT_FOUND),
            (AwsError::Unavailable("x".into()), StatusCode::SERVICE_UNAVAILABLE),
            (AwsError::S3("x".into()), StatusCode::BAD_GATEWAY),
            // A capability this deploy does not configure (e.g. no image-import project) is ours.
            (AwsError::Unsupported("x".into()), StatusCode::INTERNAL_SERVER_ERROR),
        ];
        for (e, status) in cases {
            let message = e.to_string();
            assert_eq!(parts(ApiError::from(e)), (status, message));
        }
        let v = BuildError::Aws(AwsError::Forbidden("x".into()));
        assert_eq!(ApiError::from(v).status(), StatusCode::FORBIDDEN);
        let v = BuildError::Store(StoreError::Unavailable("down".into()));
        let message = v.to_string();
        assert_eq!(parts(ApiError::from(v)), (StatusCode::SERVICE_UNAVAILABLE, message));
    }

    #[test]
    fn an_empty_message_answers_with_the_status_alone() {
        let bare = ApiError::NotFound(String::new()).into_response();
        assert_eq!(bare.status(), StatusCode::NOT_FOUND);
        assert!(bare.headers().get(axum::http::header::CONTENT_TYPE).is_none());
        let said = ApiError::Conflict("taken".into()).into_response();
        assert_eq!(said.status(), StatusCode::CONFLICT);
        assert!(said
            .headers()
            .get(axum::http::header::CONTENT_TYPE)
            .is_some_and(|t| t.to_str().unwrap().starts_with("text/plain")));
    }

    #[test]
    fn with_message_keeps_the_kind_and_context_prefixes_the_message() {
        let e = ApiError::from(StoreError::NotFound).with_message("unknown run r1");
        assert_eq!(parts(e), (StatusCode::NOT_FOUND, "unknown run r1".into()));
        let e = ApiError::from(ServiceError::Upstream("boom".into()).context("reading x"));
        assert_eq!(parts(e), (StatusCode::BAD_GATEWAY, "reading x: boom".into()));
    }
}
