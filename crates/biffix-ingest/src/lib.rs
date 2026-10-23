//! biffix ingest path: AWS events and engine events into the run-event log and its projections.
//!
//! The pure EventBridge decoding lives in `biffix_core::eventbridge`; this crate does the IO
//! around it.
//!
//! - [`decode`] — the reconciler's AWS SDK facts (DescribeJobs terminals and requests, EC2 host
//!   lifecycle), mapped as core's EventBridge decoders map the same facts. Pure; unit-tested.
//! - [`reconcile`] — the reaper's pure plan for a run's live children, from DescribeJobs' answer
//!   (the terminal facts themselves map in `biffix_core::eventbridge::describe`).
//! - [`pricing`] — pure tier-1 cost estimate of one Batch job from its host facts and run time.
//! - [`retention`] — pure selection of the Nextflow work dirs the reconcile tick may delete.
//! - [`cache_expiry`] — pure selection of the Snakemake cache entries the reconcile tick may delete.
//! - [`status`] — pure classification of the `/status` page's component health.
//! - [`budget`] — the tick's work budgets: a fair share of a bounded amount of work, and the
//!   invocation's deadline.
//! - [`config`] — the reconcile and ingest Lambdas' configuration from the environment.
//! - [`store`] — the reconcile tick's Postgres queries. The event-log writer and the
//!   projection fold are `biffix_db`'s. Integration-tested against the LocalStack/Postgres harness
//!   (`#[ignore]`d until the daemon is up).
//! - [`drain`] (feature `aws`) — the SQS → batched-writer drain loop for the Lambda.
//! - [`tick`] (feature `aws`) — the reconcile Lambda's scheduled tick: one module per step
//!   (reconcile + reap, the `/status` probe, host lifecycles, pricing, retention, cache expiry).

#![forbid(unsafe_code)]

pub mod budget;
pub mod cache_expiry;
pub mod config;
pub mod decode;
pub mod pricing;
pub mod reconcile;
pub mod retention;
pub mod status;

pub mod store;

#[cfg(feature = "aws")]
pub mod drain;
// The tick uses the AWS SDKs (Batch/EC2/SQS/S3/CloudWatch) and the pg store; gated like `drain` so
// a default (non-aws) build of the lib still compiles. Only the aws reconcile bin runs it.
#[cfg(feature = "aws")]
pub mod tick;
