//! The one error the backend's services (a run's submit, cancel and reads, registration, the QC
//! reads) refuse or fail a request with, by whose fault it is; the HTTP layer answers each kind
//! with its status (`crate::api::ApiError`).
//!
//! Its message is what the caller reads (the UI shows it). An empty message answers with the kind
//! alone: the reads that have always answered so keep doing it, and a client then words the
//! failure itself. Each failure maps to a kind in one `From` impl: the store's and AWS's here, each
//! domain error's (an image build, a submit, a launch, a registration) beside it.

use crate::aws::AwsError;
use crate::store::StoreError;

/// A refused or failed request: its kind and the message the caller reads.
#[derive(Debug, thiserror::Error)]
pub enum ServiceError {
    /// The request is malformed or asks for what cannot be done (400).
    #[error("{0}")]
    Invalid(String),
    /// No credential, or one that does not verify (401).
    #[error("{0}")]
    Unauthorized(String),
    /// The caller may not do this (403).
    #[error("{0}")]
    Forbidden(String),
    /// What the request names does not exist, or the caller cannot see it (404).
    #[error("{0}")]
    NotFound(String),
    /// The request conflicts with what is recorded: a duplicate, a state that forbids it (409).
    #[error("{0}")]
    Conflict(String),
    /// The request is larger than accepted (413).
    #[error("{0}")]
    TooLarge(String),
    /// What the request names exists but cannot be shown: too large, an unreadable format (422).
    #[error("{0}")]
    Unprocessable(String),
    /// Our fault: a misconfigured deploy, a corrupt stored value (500).
    #[error("{0}")]
    Internal(String),
    /// AWS (or another upstream) failed in a way retrying will not fix (502).
    #[error("{0}")]
    Upstream(String),
    /// The store or AWS is unreachable or shedding load; retry (503).
    #[error("{0}")]
    Unavailable(String),
}

impl ServiceError {
    /// Whether the failure is ours or an upstream's rather than the caller's (a 5xx).
    pub(crate) fn is_server_error(&self) -> bool {
        matches!(
            self,
            ServiceError::Internal(_) | ServiceError::Upstream(_) | ServiceError::Unavailable(_)
        )
    }

    /// The kind's short name, as its HTTP reason phrase reads (`Forbidden`, `Service Unavailable`).
    pub(crate) fn reason(&self) -> &'static str {
        match self {
            ServiceError::Invalid(_) => "Bad Request",
            ServiceError::Unauthorized(_) => "Unauthorized",
            ServiceError::Forbidden(_) => "Forbidden",
            ServiceError::NotFound(_) => "Not Found",
            ServiceError::Conflict(_) => "Conflict",
            ServiceError::TooLarge(_) => "Payload Too Large",
            ServiceError::Unprocessable(_) => "Unprocessable Entity",
            ServiceError::Internal(_) => "Internal Server Error",
            ServiceError::Upstream(_) => "Bad Gateway",
            ServiceError::Unavailable(_) => "Service Unavailable",
        }
    }

    /// The same kind of error with `message` in place of its own.
    pub(crate) fn with_message(self, message: impl Into<String>) -> Self {
        let message = message.into();
        match self {
            ServiceError::Invalid(_) => ServiceError::Invalid(message),
            ServiceError::Unauthorized(_) => ServiceError::Unauthorized(message),
            ServiceError::Forbidden(_) => ServiceError::Forbidden(message),
            ServiceError::NotFound(_) => ServiceError::NotFound(message),
            ServiceError::Conflict(_) => ServiceError::Conflict(message),
            ServiceError::TooLarge(_) => ServiceError::TooLarge(message),
            ServiceError::Unprocessable(_) => ServiceError::Unprocessable(message),
            ServiceError::Internal(_) => ServiceError::Internal(message),
            ServiceError::Upstream(_) => ServiceError::Upstream(message),
            ServiceError::Unavailable(_) => ServiceError::Unavailable(message),
        }
    }

    /// `err` as a service error, its message made from `err` itself (`|e| format!("reading x: {e}")`).
    pub(crate) fn reworded<E: Into<ServiceError>>(
        err: E,
        message: impl FnOnce(&E) -> String,
    ) -> Self {
        let message = message(&err);
        err.into().with_message(message)
    }

    /// `err` as a service error with no message: the reads that answer with the kind alone.
    pub(crate) fn status_only(err: impl Into<ServiceError>) -> Self {
        err.into().with_message(String::new())
    }

    /// The same kind of error with `context` before its message (`<context>: <message>`).
    pub(crate) fn context(self, context: impl std::fmt::Display) -> Self {
        let message = format!("{context}: {self}");
        self.with_message(message)
    }
}

/// A store failure: a missing row is not found, a uniqueness/precondition conflict a conflict (its
/// own reason as the message), an unreachable store unavailable, a value the store refused the
/// caller's, and a corrupt stored value ours. A server-side failure is logged: the store's fault, worth an operator's look.
impl From<StoreError> for ServiceError {
    fn from(err: StoreError) -> Self {
        let error = match &err {
            StoreError::NotFound => ServiceError::NotFound(String::new()),
            StoreError::Conflict(_) => ServiceError::Conflict(String::new()),
            StoreError::Unavailable(_) => ServiceError::Unavailable(String::new()),
            StoreError::Serialization(_) => ServiceError::Internal(String::new()),
            StoreError::Rejected(_) => ServiceError::Invalid(String::new()),
        };
        if error.is_server_error() {
            tracing::warn!(error = %err, "store error");
        }
        let message = match err {
            StoreError::Conflict(m) => m,
            other => other.to_string(),
        };
        error.with_message(message)
    }
}

/// An AWS failure where a service has no more specific reading: the caller's mistake is invalid, a
/// missing grant forbidden, a missing object not found, an AWS outage or throttle unavailable
/// (retryable), a resource the deploy configures missing — or a capability it does not configure
/// at all — ours, and any other AWS fault upstream.
impl From<AwsError> for ServiceError {
    fn from(err: AwsError) -> Self {
        let message = err.to_string();
        match err {
            AwsError::Invalid(_) => ServiceError::Invalid(message),
            AwsError::Forbidden(_) => ServiceError::Forbidden(message),
            AwsError::NotFound => ServiceError::NotFound(message),
            AwsError::Unavailable(_) => ServiceError::Unavailable(message),
            AwsError::Misconfigured(_) | AwsError::Unsupported(_) => {
                ServiceError::Internal(message)
            }
            _ => ServiceError::Upstream(message),
        }
    }
}
