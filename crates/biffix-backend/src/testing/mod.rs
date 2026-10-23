//! Test doubles for the backend's handlers, with no AWS and no database, shared by the backend's
//! integration tests and the CLI's end-to-end test (the `test-support` feature):
//!
//! - [`MemStore`]: every [`Store`] trait (runs, tasks, outputs, launch and prefix claims, …),
//!   mirroring `pg/` where a handler depends on it;
//! - [`MemEventSink`]: the [`EventSink`], deduping on `dedup_key` like `pg/event_sink.rs`, and
//!   optionally folding what it records into a [`MemStore`] ([`MemEventSink::folding_into`]);
//! - [`FakeCloud`]: the [`CloudProvider`] (Batch, S3, CloudWatch Logs, CodeBuild, SNS);
//! - [`MemRegistry`]: the [`RegistryStore`] (workflow versions, builds, coordinator variants);
//! - [`MemSettings`]: the [`SettingsStore`];
//! - [`MemReferenceStore`] and [`MemExperimentStore`]: the [`ReferenceStore`] and the
//!   [`ExperimentStore`];
//!
//! plus [`TestState`], which builds an [`crate::AppState`] over them; fixture builders
//! ([`run_detail`], [`task_view`]); request helpers ([`call`], [`send`], [`response_json`], ...);
//! statically signed principals ([`static_auth`], [`user_token`], [`hs256_token`]). The pg tests'
//! schema-isolated pool is `biffix_db::testing`'s.
//!
//! [`Store`]: crate::store::Store
//! [`EventSink`]: crate::store::EventSink
//! [`CloudProvider`]: crate::aws::CloudProvider
//! [`RegistryStore`]: crate::store::registry::RegistryStore
//! [`SettingsStore`]: crate::store::settings::SettingsStore
//! [`ReferenceStore`]: crate::store::references::ReferenceStore
//! [`ExperimentStore`]: crate::store::experiments::ExperimentStore

mod fake_cloud;
mod fixtures;
mod http;
mod mem_event_sink;
mod mem_experiments;
mod mem_references;
mod mem_registry;
mod mem_settings;
mod mem_store;
mod scenarios;
mod state;
mod tokens;

pub use fake_cloud::*;
pub use fixtures::*;
pub use http::*;
pub use mem_event_sink::*;
pub use mem_experiments::*;
pub use mem_references::*;
pub use mem_registry::*;
pub use mem_settings::*;
pub use mem_store::*;
pub use scenarios::*;
pub use state::{TestState, TEST_CAPABILITY_SECRET};
pub use tokens::*;
