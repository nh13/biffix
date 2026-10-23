#![forbid(unsafe_code)]
//! The biffix HTTP wire contract: the request/response types the control-plane API exchanges with
//! its clients (the CLI and the static UI).
//!
//! These types live in their own crate so the backend (which *serves* them) and the CLI (which
//! *consumes* them) share one definition instead of hand-mirroring it. That makes drift a compile
//! error rather than a runtime surprise, and it keeps the domain id newtypes (`RunId`, …) typed on
//! both sides instead of degrading them to bare strings on a round trip through the OpenAPI schema.
//!
//! `utoipa::ToSchema` is derived only under the `openapi` feature, which the backend enables to
//! build its OpenAPI document; the CLI depends on this crate without it and never compiles utoipa.
//!
//! Evolution policy (what keeps an old CLI working against a newer server): response changes are
//! additive-only — a new field is `Option`/`#[serde(default)]`, and removing or renaming a field is
//! a breaking (major) change. Every already-optional field below is `#[serde(default)]` so a server
//! that omits it still deserializes, and the round-trip proptest pins the serialize/deserialize
//! pairing so a `skip_serializing_if` can never again ship without its matching default.

//!
//! The rule for what lives here: **every request/response type the OpenAPI document publishes**,
//! whether or not a Rust client consumes it today. The backend keeps no wire types of its own (a
//! handler module re-exports the ones it serves), so a client never has to ask which crate owns an
//! endpoint's shape.
//!
//! The types are grouped by endpoint family into private modules and re-exported flat here, so
//! every type's path is `biffix_api::<Type>` whichever module defines it.

mod auth;
mod data;
mod events;
mod experiments;
mod filters;
mod health;
mod notifications;
mod projects;
mod qc;
mod references;
mod register;
mod runs;
mod samples;
mod settings;
mod source_upload;
mod submit;

pub use auth::*;
pub use data::*;
pub use events::*;
pub use experiments::*;
pub use filters::*;
pub use health::*;
pub use notifications::*;
pub use projects::*;
pub use qc::*;
pub use references::*;
pub use register::*;
pub use runs::*;
pub use samples::*;
pub use settings::*;
pub use source_upload::*;
pub use submit::*;
