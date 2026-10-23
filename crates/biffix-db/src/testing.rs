//! A private, freshly reset Postgres schema per test, so the pg integration tests run in PARALLEL
//! (feature `test-support`; never in a production build).
//!
//! Each test gets a private schema with `search_path` pinned to it on every pooled connection.
//! Because all migrations and app SQL use unqualified table names, they resolve into that schema,
//! so tests with distinct labels never share tables and need no `--test-threads=1`.
//!
//! This is hand-rolled rather than `#[sqlx::test]`: that macro needs `DATABASE_URL` (the harness
//! uses `BIFFIX_TEST_PG`), creates a database per test, and applies migrations itself, bypassing the
//! production [`crate::migrate::run`] these tests are meant to exercise.

use sqlx::postgres::PgPoolOptions;
use sqlx::{AssertSqlSafe, Executor, PgPool};

/// A pool pinned to the private schema [`schema_name`] gives `prefix` and `label`, NOT migrated
/// (the caller runs [`crate::migrate::run`], which some tests exercise themselves). `prefix` names
/// the calling crate's suite (`be`, `ing`, …) so two test binaries running at once never share a
/// schema; `label` must be unique per test within it. The schema is dropped and recreated first, so
/// a re-run starts clean and schemas do not accumulate across runs.
pub async fn isolated_pool(prefix: &str, label: &str) -> PgPool {
    let url =
        std::env::var("BIFFIX_TEST_PG").expect("BIFFIX_TEST_PG must be set for --ignored pg tests");
    let schema = schema_name(prefix, label);
    // Reset the schema on a throwaway connection BEFORE building the pool that pins search_path.
    let admin = PgPoolOptions::new().max_connections(1).connect(&url).await.unwrap();
    // Ensure pg_trgm is installed in `public` (the baseline migration's gin_trgm_ops indexes need
    // it). The admin connection's default search_path puts it there. Kept in `public`, off the
    // private schema, so the per-test search_path below can find the operator class without any
    // app table living there. Two tests racing `IF NOT EXISTS` can make the loser fail with
    // unique_violation (23505, on pg_extension's name index) or duplicate_object (42710); the
    // extension exists either way, so only those are ignored. Anything else (no permission, package
    // missing) fails here, not later as a confusing migration error.
    if let Err(e) = admin.execute("CREATE EXTENSION IF NOT EXISTS pg_trgm").await {
        let code = e.as_database_error().and_then(sqlx::error::DatabaseError::code);
        assert!(
            matches!(code.as_deref(), Some("23505" | "42710")),
            "CREATE EXTENSION pg_trgm failed: {e}"
        );
    }
    admin
        .execute(AssertSqlSafe(format!("DROP SCHEMA IF EXISTS \"{schema}\" CASCADE")))
        .await
        .unwrap();
    admin.execute(AssertSqlSafe(format!("CREATE SCHEMA \"{schema}\""))).await.unwrap();
    admin.close().await;
    PgPoolOptions::new()
        .max_connections(4)
        .after_connect(move |conn, _meta| {
            let schema = schema.clone();
            Box::pin(async move {
                // Private schema first (so unqualified tables are created and read there,
                // isolated), then `public` for the pg_trgm operator class. `CREATE TABLE IF NOT
                // EXISTS` targets the first schema, so a stale same-named table in `public` never
                // shadows it.
                conn.execute(AssertSqlSafe(format!("SET search_path TO \"{schema}\", public")))
                    .await?;
                Ok(())
            })
        })
        .connect(&url)
        .await
        .unwrap()
}

/// `t_<prefix>_<readable label>_<hash>`, both parts sanitised to `[a-z0-9_]` (identifier-safe, no
/// injection) and bounded (the prefix is a crate's short suite name, `be` or `ing`). Postgres
/// silently truncates identifiers past 63 bytes, so an unbounded name lets two long labels that
/// agree on their first characters share (and `DROP ... CASCADE`) one schema.
/// The first 40 sanitised characters of the label keep it readable; 16 hex characters of the raw
/// label's SHA-256 keep labels that differ only later, or only in case or punctuation (`a-b` vs
/// `a_b`), apart.
pub fn schema_name(prefix: &str, label: &str) -> String {
    use sha2::Digest;
    let sanitize = |text: &str| -> String {
        text.chars()
            .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '_' })
            .collect()
    };
    let readable: String = sanitize(label).chars().take(40).collect();
    let hash = hex::encode(sha2::Sha256::digest(label.as_bytes()));
    let schema = format!("t_{}_{readable}_{}", sanitize(prefix), &hash[..16]);
    assert!(schema.len() <= 63, "schema name {schema} exceeds Postgres's 63-byte identifier limit");
    schema
}

#[cfg(test)]
mod tests {
    use super::schema_name;

    #[test]
    fn schema_names_are_prefixed_bounded_and_distinct() {
        let a = schema_name("ing", "a-b");
        assert!(a.starts_with("t_ing_a_b_"), "{a}");
        // Same sanitised prefix, different raw labels: the hash keeps them apart.
        assert_ne!(a, schema_name("ing", "a_b"));
        // Another crate's suite never shares the schema.
        assert_ne!(a, schema_name("be", "a-b"));
        let long = schema_name("ing", &"x".repeat(200));
        assert!(long.len() <= 63, "{long}");
        assert!(schema_name("a\"b", "c").starts_with("t_a_b_c_"));
    }
}
