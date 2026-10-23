//! biffix-core's integration tests, as one test binary: the fold, the translator, the EventBridge
//! decoders and the contract's wire shapes, one module each.

mod capability;
mod common;
mod coordinator_binding;
mod dedup;
mod engine_schema_version;
mod eventbridge;
mod reducer_fold;
mod translate;
mod wire_roundtrip;
