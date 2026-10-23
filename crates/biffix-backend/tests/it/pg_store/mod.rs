//! The backend's Postgres stores against the real schema: `PgStore` (reading the
//! projection the ingest crate maintains, and the launch/prefix claims and parked launches),
//! `PgRegistryStore`, `PgSettingsStore` and `PgReferenceStore`, plus the shared migrations.
//! `#[ignore]`d; they need the Postgres harness:
//!
//!   docker compose -f dev/docker-compose.yml up -d postgres
//!   BIFFIX_TEST_PG=postgres://biffix:biffix@localhost:5433/biffix \
//!     cargo test -p biffix-backend --all-features --test it -- --include-ignored pg_store::
//!
//! Most read tests seed the projection tables by hand, then read them back through the store
//! trait; `a_folded_run_reads_back_as_the_fold_says` seeds through the ingest writer instead and
//! checks the reads against the fold.

#![cfg(feature = "aws")]

use biffix_backend::pg::PgStore;
use biffix_backend::store::cost::ListCost;
use biffix_backend::store::runs::{RunFilter, RunQuery};
use biffix_backend::testing;
use biffix_core::ids::{RunId, TaskKey};

mod attempts;
mod claims;
mod cost;
mod experiments;
mod fold;
mod notifications;
mod outputs;
mod registry;
mod runs;
mod settings;
mod status;
mod tasks;

/// A pool in a `label`-private schema (see `biffix_db::testing::isolated_pool`), migrated with the
/// real migrations (not hand-written DDL, so a fixture cannot drift from the schema the queries run
/// against), with no rows. `label` must be unique per test, so the tests run in parallel.
pub(crate) async fn migrated_pool(label: &str) -> sqlx::PgPool {
    let pool = biffix_db::testing::isolated_pool("be", label).await;
    biffix_db::migrate::run(&pool).await.unwrap();
    pool
}

/// Fail unless the database's collation orders `before` before `after` though bytewise it comes
/// after: a test that a query orders bytewise (`COLLATE "C"`) proves nothing on a database where
/// the two orders agree on its values.
async fn assert_collation_disagrees(pool: &sqlx::PgPool, before: &str, after: &str) {
    assert!(before.as_bytes() > after.as_bytes(), "{before} {after}");
    let orders_otherwise: bool = sqlx::query_scalar("SELECT $1::text < $2::text")
        .bind(before)
        .bind(after)
        .fetch_one(pool)
        .await
        .unwrap();
    assert!(
        orders_otherwise,
        "the harness database must use a non-C collation that orders {before} before {after}"
    );
}

/// Run each of `sqls` (statements without binds), in order.
async fn exec_all(pool: &sqlx::PgPool, sqls: &[&'static str]) {
    for sql in sqls {
        sqlx::query(*sql).execute(pool).await.unwrap();
    }
}

/// [`migrated_pool`], seeded with read-path rows: runs `r1` (running, one task) and `r2`.
async fn seeded_pool(label: &str) -> sqlx::PgPool {
    let pool = migrated_pool(label).await;
    exec_all(
        &pool,
        &[
            "INSERT INTO run_projection \
               (run_id, state, completed, total, submitted_at, submitter, workflow_version) \
             VALUES ('r1','running',1,3, 1700000000000000, 'user:alice', 'nf-core/rnaseq @ 3.14.0')",
            "INSERT INTO run_projection (run_id, state) VALUES ('r2','succeeded')",
            "INSERT INTO task_projection (run_id, task_key, state, attempts) \
             VALUES ('r1','align:sample=A','running',2)",
        ],
    )
    .await;
    pool
}

/// A minimal registered version `id` (a Nextflow one from an S3 source, its own source content
/// hash the id's last part, so versions never share a (source, build)); a test overrides the
/// fields it is about.
fn registered_version(id: &str) -> biffix_backend::store::registry::RegisteredVersion {
    use biffix_backend::register::workflow::{WorkflowSource, WorkflowVersion};
    use biffix_workflow::engines::Engine;
    biffix_backend::store::registry::RegisteredVersion {
        name: "wf".into(),
        project: "p".into(),
        source: WorkflowSource::S3 { uri: "s3://b/src.tar.gz".into(), sha256: None },
        version: WorkflowVersion {
            id: id.into(),
            build: 0,
            git_sha: "abc".into(),
            source_content_hash: id.rsplit(':').next().unwrap().into(),
            image_digest: String::new(),
            image_repository: None,
            image_registry_id: None,
            image_region: None,
            engine: Engine::Nextflow,
            engine_version: "25.10.4".into(),
            entrypoint: "main.nf".into(),
            target: None,
            form_schema: serde_json::json!({"fields": [], "warnings": []}),
            queue_classes: vec![],
            about_markdown: None,
            runtime_key: "runtime-key".into(),
            runtime_recipe: serde_json::json!({"engine": "nextflow"}),
        },
        created_by: None,
        created_by_label: None,
        created_at_micros: 1,
        registration_inputs: biffix_api::RegistrationInputs {
            source_mode: "s3".into(),
            cli_version: "0.0.0".into(),
            ..Default::default()
        },
        registration_inputs_hash: format!("inputs-{id}"),
        images: Vec::new(),
    }
}
