#![forbid(unsafe_code)]
//! `biffix-workflow` — workflow authoring and registration: what a workflow version is and how it
//! is checked, shared by the backend and the CLI so both derive the same answer.
//!
//! - the `biffix.yaml` [`manifest`] and the [`formschema`] derived from the engine's own schema,
//!   with the [`validate`] param validator and the [`samples`] sheet and detection rules;
//! - the workflow [`version`] id (normalized source tree + content hash), the [`about`] page,
//!   [`viewer`] file kinds, report [`glob`]s, and the sha256 [`digest`] and [`text`] helpers;
//! - [`images`] (references, content keys, registry media types) found by the [`scan`] of the
//!   source, or built from a [`pixi`] or Dockerfile recipe;
//! - the supported [`engines`] and the engine [`runtime`] pins and recipes.
//!
//! It builds on `biffix-core` (the RunEvent contract) and nothing else in the workspace. The
//! backend, the API types, the CLI and `xtask` (the FormSchema schema and the shared test-case
//! tables) depend on it; the ingest Lambdas (`biffix-ingest`, `biffix-db`) do not.

pub mod about;
pub mod digest;
pub mod engines;
pub mod formschema;
pub mod glob;
pub mod images;
pub mod manifest;
pub mod pixi;
pub mod runtime;
pub mod samples;
pub mod scan;
pub mod text;
pub mod validate;
pub mod version;
pub mod viewer;
