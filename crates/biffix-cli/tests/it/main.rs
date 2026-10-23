//! The CLI's integration tests, as one test binary: the API client against local stubs, the
//! end-to-end run against the real backend router, and the `biffix` binary's own warnings.

mod api_client;
mod api_session;
mod common;
mod e2e;
mod help_lint;
mod inspect_memory;
mod login;
mod properties;
mod refusals;
