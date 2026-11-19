//! The cross-language fixture tables: cases asserted against biffix-workflow's Rust code (each
//! module's `tests`) and emitted to `schemas/` for the UI's, the plugins' and the coordinator's ports to replay.

pub(crate) mod image_ref;
pub(crate) mod queue_selector;
pub(crate) mod sheet;
pub(crate) mod validate;
