#![forbid(unsafe_code)]
//! `biffix-core` — the canonical source of the biffix RunEvent contract.
//!
//! Two layers, with a translator bridging them:
//! - per-source **wire events** (the producer contract) — [`wire`];
//! - the normalized **RunEvent** the log stores and the reducers fold — [`event`];
//!
//! and the [`translate`] functions that map each wire event into a RunEvent; [`eventbridge`]
//! decodes raw AWS EventBridge events (and the ingest queue's own envelopes) into RunEvents through
//! them.
//!
//! Plus the [`reducer`] folds (Task/Run projection, with [`state`], [`builds`] and
//! [`sample_progress`]), the [`dedup`] key discipline, the [`failure`] classifier, and the small
//! helpers every binary shares ([`env`](mod@env), [`capability`], [`s3_layout`], [`clock`]). This
//! crate is the single source of truth from which the RunEvent and wire JSON Schema artifacts are
//! generated.
//!
//! Every other biffix crate depends on it, and it depends on none of them: `biffix-db` and
//! `biffix-ingest` (the ingest Lambdas) link only this, the contract; the workflow-authoring code
//! (form schema, manifest, validation, images, runtimes) lives in `biffix-workflow`, which builds
//! on it. `xtask`'s layering tests hold that direction.

pub mod builds;
pub mod capability;
pub mod clock;
pub mod dedup;
pub mod env;
pub mod event;
pub mod eventbridge;
pub mod failure;
pub mod ids;
pub mod ingest_limits;
mod open_enum_schema;
pub mod reducer;
pub mod s3_layout;
pub mod sample_progress;
pub mod source;
pub mod state;
// Logging setup for the deploy's Lambdas, behind `lambda`.
#[cfg(feature = "lambda")]
pub mod telemetry;
// Test builders for the contract types (`event`, `anchor`), for this and the other crates' tests.
#[cfg(any(test, feature = "test-support"))]
pub mod testing;
pub mod time;
pub mod translate;
pub mod wire;

// Crate-internal open-enum schema macro, re-exported at the crate root so the enum definitions can
// invoke it as `crate::impl_open_enum_schema!` without `#[macro_export]` leaking it publicly.
pub(crate) use open_enum_schema::impl_open_enum_schema;
