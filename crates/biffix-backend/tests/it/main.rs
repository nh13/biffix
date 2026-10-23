//! The backend's integration tests, as one test binary: one module per API area or component, so
//! the crate and its doubles (`biffix_backend::testing`) are linked once, not once per file.
//!
//! The Postgres suites (`pg_store`, `event_sink_pg`) are `#[ignore]`d and compiled only with
//! `--features pg`; with the harness up, run them by module:
//!
//! ```sh
//! docker compose -f dev/docker-compose.yml up -d postgres      # :5433
//! BIFFIX_TEST_PG=postgres://biffix:biffix@localhost:5433/biffix \
//!   cargo test -p biffix-backend --all-features --test it -- --include-ignored pg_store:: event_sink_pg::
//! ```

mod alerts_api;
mod cancel_api;
mod capability_gates;
mod config_env;
mod data_api;
mod event_sink_pg;
mod events_api;
mod experiments_api;
mod http_auth;
mod http_reads;
mod image_pinning;
mod images_api;
mod launch_sweep;
mod pg_store;
mod project_routing;
mod provenance;
mod qc_previews;
mod rbac;
mod registration;
mod rename_api;
mod rest_api;
mod resumed_outputs;
mod static_verifier;
mod submit_api;
mod submit_pipeline;
mod variants_api;
