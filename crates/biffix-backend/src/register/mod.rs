//! Registration path, for Snakemake and Nextflow workflows alike.
//! Static-declarative: biffix never executes workflow code to register it. The client (the CLI)
//! reads the source's `biffix.yaml` and param schema and sends the engine, engine version and
//! FormSchema with the request; the backend validates them, pins every declared image to a
//! digest (public registries anonymously via [`crate::aws::oci`], private ECR via [`crate::aws::ecr`]), and
//! freezes an immutable WorkflowVersion ([`version`]).

pub(crate) mod catalog;
pub mod pipeline;
pub mod version;
pub mod workflow;
