//! A sweep's saved resume point between ticks (`reconcile_cursor`).

use sqlx::postgres::PgPool;

use biffix_db::DbError;

/// Read a reconcile sweep's resume point (`reconcile_cursor`), if one is saved.
pub async fn reconcile_cursor(pool: &PgPool, name: &str) -> Result<Option<String>, DbError> {
    Ok(sqlx::query_scalar::<_, String>("SELECT value FROM reconcile_cursor WHERE name = $1")
        .bind(name)
        .fetch_optional(pool)
        .await?)
}

/// Save (`Some`) or clear (`None`, a completed pass) a reconcile sweep's resume point.
pub async fn set_reconcile_cursor(
    pool: &PgPool,
    name: &str,
    value: Option<&str>,
) -> Result<(), DbError> {
    match value {
        Some(value) => {
            sqlx::query(
                r"INSERT INTO reconcile_cursor (name, value) VALUES ($1, $2)
                   ON CONFLICT (name) DO UPDATE SET value = EXCLUDED.value, updated_at = now()",
            )
            .bind(name)
            .bind(value)
            .execute(pool)
            .await?;
        }
        None => {
            sqlx::query("DELETE FROM reconcile_cursor WHERE name = $1")
                .bind(name)
                .execute(pool)
                .await?;
        }
    }
    Ok(())
}
