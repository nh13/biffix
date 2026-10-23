//! The schema migrations every Lambda applies at cold start.

use super::*;

/// A deploy updates its stacks one at a time, and a rollback runs the previous release, so a
/// binary must still start once a newer one applied a migration it does not embed (migrations are
/// additive, so the older code is safe on the newer schema).
#[tokio::test]
#[ignore = "requires the LocalStack/Postgres harness"]
async fn an_older_release_starts_after_a_newer_one_migrated() {
    let pool = common::isolated_pool("an_older_release_starts_after_a_newer_one").await;
    sqlx::query(
        "INSERT INTO _sqlx_migrations (version, description, success, checksum, execution_time) \
         VALUES (9999, 'from a newer release', TRUE, '\\x00', 0)",
    )
    .execute(&pool)
    .await
    .unwrap();
    biffix_db::migrate::run(&pool).await.unwrap();
}
