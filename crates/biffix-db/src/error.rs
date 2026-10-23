//! [`DbError`]: what the event-log writer, the projection fold and the shared queries fail with.

/// A failed database operation.
#[derive(Debug, thiserror::Error)]
pub enum DbError {
    /// A query or connection failed.
    #[error("db: {0}")]
    Db(#[from] sqlx::Error),
    /// A projection value did not serialize to JSON for its JSONB column.
    #[error("serialize: {0}")]
    Serialize(#[from] serde_json::Error),
    /// The schema migrations could not be applied (see [`crate::migrate`]).
    #[error("migrate: {0}")]
    Migrate(#[from] sqlx::migrate::MigrateError),
}
