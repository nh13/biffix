//! Per-source **wire events**.
//!
//! Each source has its own typed event. The [`engine`] event is the producer contract the engine
//! plugins post (published as `schemas/wire.engine.schema.json`); the AWS ones are what the ingest
//! decoder reads out of EventBridge JSON (`crate::eventbridge`). A thin per-source translator
//! ([`crate::translate`]) maps each into the normalized [`crate::event::RunEvent`] the log stores;
//! the reducers therefore see one vocabulary, never one per source.
//!
//! The sources: [`aws_batch`], [`aws_codebuild`], [`aws_ec2`] and [`engine`]. The [`engine`] shape
//! is shared by every engine plugin (Snakemake logger, nf-biffix observer); a per-engine
//! translator stamps the source. The control plane has no wire type: it is biffix itself, so it
//! builds its RunEvents directly with the builders in [`crate::event`].

pub mod aws_batch;
pub mod aws_codebuild;
pub mod aws_ec2;
pub mod engine;
