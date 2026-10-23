//! The ingest crate's integration tests, as one test binary. The Postgres suites (`store_pg`,
//! `reconcile_pg`, and `drain_pg`, which also needs `--features aws`) are `#[ignore]`d and, with the
//! harness up, run by module:
//!
//! ```sh
//! docker compose -f dev/docker-compose.yml up -d postgres      # :5433
//! BIFFIX_TEST_PG=postgres://biffix:biffix@localhost:5433/biffix \
//!   cargo test -p biffix-ingest --all-features --test it -- --include-ignored store_pg:: reconcile_pg:: drain_pg::
//! ```

mod common;
mod config_env;
mod drain_pg;
mod reconcile_pg;
mod store_pg;
