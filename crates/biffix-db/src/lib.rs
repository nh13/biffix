//! biffix's Postgres layer, shared by the backend and the ingest Lambdas: one schema, one
//! connection recipe, and one writer of the event log and its projection.
//!
//! - [`migrate`] — the schema migrations (`migrations/*.sql`, embedded at compile time) and their
//!   one entry point.
//! - [`url`] — the connection URL: `DATABASE_URL`, or the RDS-managed secret (`resolve`, feature
//!   `aws`).
//! - `secrets` (feature `aws`) — read a Secrets Manager secret's string.
//! - [`pool`] — the Aurora pool tuning, and the per-invocation release a Lambda's pool needs.
//! - [`idle`] — whether a scheduled tick may leave the database asleep (its AWS Batch check behind
//!   feature `aws`).
//! - [`log`] — the event-log writer: the ONE append path, which advances the projection in the
//!   same transaction.
//! - [`projection`] — the fold of the log into the projection tables, and its replay.
//! - [`rules`] — SQL rules that more than one crate's queries apply.
//! - `testing` (feature `test-support`) — a schema-isolated pool for the pg integration tests.
//!
//! Every fallible database function here returns [`DbError`].

#![forbid(unsafe_code)]

pub mod error;
mod hosts;
pub mod idle;
pub mod log;
pub mod migrate;
pub mod pool;
pub mod projection;
pub mod rules;
#[cfg(feature = "aws")]
pub mod secrets;
#[cfg(feature = "test-support")]
pub mod testing;
pub mod url;

pub use error::DbError;
