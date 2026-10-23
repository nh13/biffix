//! The reconcile tick's Postgres queries, one module per table or concern: the
//! DescribeJobs backstop and the reaper ([`reconcile`]), the work dirs due for deletion
//! ([`retention`]), a sweep's resume point ([`cursor`]), host lifecycles ([`hosts`]), the jobs
//! to price ([`pricing`]) and the status probe's signals and samples ([`status`]). The event-log writer and the projection fold they read are
//! `biffix_db`'s.
//!
//! Integration-tested against the Postgres in the LocalStack harness; those tests are
//! `#[ignore]`d so the default `cargo test` needs no daemon.

pub mod cursor;
pub mod hosts;
pub mod pricing;
pub mod reconcile;
pub mod retention;
pub mod status;
