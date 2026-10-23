//! The Postgres stores (behind `feature = "aws"`). [`PgStore`] implements every
//! [`crate::store::Store`] trait, one file per trait; the registry, settings, reference bundles and
//! experiments have their own stores, and the write side is [`event_sink`]. What they all share is
//! here: the classification of a `sqlx::Error`, or of a `biffix_db` [`DbError`], into the API's
//! [`StoreError`] taxonomy, which each store's `?` relies on.
//!
//! [`PgStore`] reads the projection tables `biffix_db::projection` maintains (`run_projection`,
//! `task_projection`, …). Those tables are the shared read contract between the event-log writer
//! and the API; the only reads of the raw log (`run_events`) are the two the [`crate::store`] docs
//! name, `run_event` (a run's control-plane singletons, `run_anchor` among them) and
//! `provenance_due`.

mod cost;
mod event_log;
pub mod event_sink;
pub mod experiments;
mod notifications;
pub mod outputs;
mod pending;
mod provenance;
pub mod references;
pub mod registry;
mod runs;
pub mod settings;
mod status;
mod tasks;

use biffix_db::DbError;
use sqlx::postgres::PgPool;

use crate::store::StoreError;

/// The Postgres-backed [`crate::store::Store`]: the projection reads, the user state beside them
/// (notification reads, parked launches, provenance marks, cached S3 HEADs) and the advisory-lock
/// claims, over one pool.
pub struct PgStore {
    pool: PgPool,
}

impl PgStore {
    /// A store over `pool`.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

/// Convert stored epoch-microseconds (BIGINT) to the epoch-seconds the API DTO uses.
fn micros_to_secs(micros: Option<i64>) -> Option<f64> {
    micros.map(|m| m as f64 / 1_000_000.0)
}

impl From<sqlx::Error> for StoreError {
    fn from(e: sqlx::Error) -> Self {
        match &e {
            sqlx::Error::RowNotFound => StoreError::NotFound,
            // A unique/PK violation is a caller-visible conflict, not an outage.
            sqlx::Error::Database(db) if db.is_unique_violation() => {
                StoreError::Conflict(e.to_string())
            }
            // A stored value of the wrong type or shape is corrupt data, not an outage: retrying
            // cannot fix it.
            sqlx::Error::ColumnDecode { .. }
            | sqlx::Error::Decode(_)
            | sqlx::Error::ColumnNotFound(_) => StoreError::Serialization(e.to_string()),
            // A value Postgres refused as the request carried it: retrying the same value fails
            // the same way, so it is the caller's (a 4xx), never a 503 to retry.
            sqlx::Error::Database(db) if db.code().is_some_and(|c| refuses_the_value(&c)) => {
                StoreError::Rejected(e.to_string())
            }
            _ => StoreError::Unavailable(e.to_string()),
        }
    }
}

/// Whether an error's SQLSTATE `code` refuses a value the statement carried rather than reports
/// an outage: class 22 (data exception: a NUL in text, a number out of range, …), class 23
/// (integrity constraint violation: a NULL, a check or a missing reference; a unique violation is
/// read as a conflict before this) and 21000 (cardinality violation: an upsert naming one key
/// twice). Anything else — a lost connection, a serialization failure or deadlock (class 40), a
/// shutdown — is operational.
fn refuses_the_value(code: &str) -> bool {
    code.starts_with("22") || code.starts_with("23") || code == "21000"
}

/// An event-log writer or fold error in the same taxonomy. Without this every append failure —
/// including a plain connection drop — collapsed to `Serialization` (a 500), so a transient DB
/// outage on the ingest hot path was misreported as an internal bug instead of a retryable 503. A
/// `Db` error reuses the `sqlx::Error` classification above.
impl From<DbError> for StoreError {
    fn from(e: DbError) -> Self {
        match e {
            DbError::Db(db) => StoreError::from(db),
            DbError::Serialize(se) => StoreError::Serialization(se.to_string()),
            DbError::Migrate(me) => StoreError::Unavailable(me.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sqlx_errors_are_classified() {
        assert!(matches!(StoreError::from(sqlx::Error::RowNotFound), StoreError::NotFound));
        // A stored value of the wrong type or shape is corrupt data, not an outage.
        assert!(matches!(
            StoreError::from(sqlx::Error::ColumnNotFound("x".into())),
            StoreError::Serialization(_)
        ));
        assert!(matches!(
            StoreError::from(sqlx::Error::Decode("bad".into())),
            StoreError::Serialization(_)
        ));
        assert!(matches!(
            StoreError::from(sqlx::Error::ColumnDecode { index: "0".into(), source: "bad".into() }),
            StoreError::Serialization(_)
        ));
        // Anything operational is an outage (retryable).
        assert!(matches!(StoreError::from(sqlx::Error::PoolTimedOut), StoreError::Unavailable(_)));
    }

    #[test]
    fn a_refused_value_is_told_from_an_outage_by_its_sqlstate() {
        // A NUL in text, an invalid JSONB escape, a NULL, a check, a missing reference, an upsert
        // naming one key twice.
        for code in ["22021", "22P05", "23502", "23514", "23503", "21000"] {
            assert!(refuses_the_value(code), "{code}");
        }
        // A serialization failure, a deadlock, a lost connection, an admin shutdown, a timeout.
        for code in ["40001", "40P01", "08006", "57P01", "57014"] {
            assert!(!refuses_the_value(code), "{code}");
        }
    }

    #[test]
    fn db_errors_keep_their_classification() {
        // A database error is classified as the read side classifies it.
        assert!(matches!(
            StoreError::from(DbError::Db(sqlx::Error::PoolTimedOut)),
            StoreError::Unavailable(_)
        ));
        assert!(matches!(
            StoreError::from(DbError::Db(sqlx::Error::RowNotFound)),
            StoreError::NotFound
        ));
        // An event that does not serialize is a bug (500), not an outage.
        let serde = serde_json::from_str::<serde_json::Value>("{").unwrap_err();
        assert!(matches!(
            StoreError::from(DbError::Serialize(serde)),
            StoreError::Serialization(_)
        ));
        // A schema that cannot be migrated leaves the store unusable for now.
        assert!(matches!(
            StoreError::from(DbError::Migrate(sqlx::migrate::MigrateError::VersionMissing(1))),
            StoreError::Unavailable(_)
        ));
    }
}
