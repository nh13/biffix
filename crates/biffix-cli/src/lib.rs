//! `biffix` CLI library — the scriptable OAuth public client for the control-plane API.
//!
//! Split lib/bin so the auth flows, API client, and command logic are unit- and
//! integration-testable (against fake OAuth + API servers) without invoking the binary. Only what
//! the binary and the integration tests use is `pub`; the rest is crate-internal.

#![forbid(unsafe_code)]

pub mod api;
pub mod auth;
pub mod cli;
pub mod commands;
pub mod config;
pub mod error;
pub(crate) mod fsutil;
pub(crate) mod local;
pub mod params;
pub mod render;
pub mod secret;
#[cfg(test)]
pub(crate) mod test_git;
