//! The deploy's configuration: each entry point's settings, read once from the environment
//! ([`env`](mod@env)), per-project contexts ([`projects`]) and their data sources
//! ([`data_sources`]), each queue's spot/on-demand capacity ([`queue_capacity`]), and the
//! production wiring every entry point composes its state with (`compose`), and the time a request
//! may take ([`API_LAMBDA_TIMEOUT`], [`REQUEST_WAIT_BUDGET`]).

#[cfg(feature = "aws")]
pub mod compose;
pub mod data_sources;
pub mod env;
pub mod projects;
pub(crate) mod queue_capacity;

/// The API Lambda's timeout (`infra/biffix_infra/constructs/backend_api.py`, which an infra test
/// holds to this value): the HTTP API gives an integration 30 s. A request still running then is
/// cut off with the gateway's own error, never the handler's answer.
pub const API_LAMBDA_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(29);

/// The most a handler spends waiting (on a claim) or reading (a large download) against the clock
/// before it must answer: the rest of [`API_LAMBDA_TIMEOUT`] is for the work after it and the
/// answer itself, so a request out of time gets the handler's answer (a 503 to retry, a 413).
pub const REQUEST_WAIT_BUDGET: std::time::Duration =
    std::time::Duration::from_secs(API_LAMBDA_TIMEOUT.as_secs() - 9);
