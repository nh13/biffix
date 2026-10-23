//! The one database migration entry point.
//!
//! The schema lives in versioned, checksummed `.sql` files under `migrations/`, embedded at compile
//! time by [`sqlx::migrate!`]: `0001_init.sql` is the baseline, and each later schema change is a
//! new numbered file. sqlx records what it applied in the `_sqlx_migrations` version table and runs
//! the files in order. An applied migration is checksum-locked, so a change is always a new file,
//! never an edit to an applied one.

use std::sync::LazyLock;

use sqlx::migrate::Migrator;

use crate::error::DbError;

/// The embedded migrations. A version the database has applied but this binary does not embed is
/// ignored (`ignore_missing`): a deploy updates its stacks one at a time and a rollback runs the
/// previous release, so a binary must still start once a newer one has migrated. Migrations are
/// additive, so the older code is safe on the newer schema.
static MIGRATOR: LazyLock<Migrator> = LazyLock::new(|| {
    let mut migrator = sqlx::migrate!("./migrations");
    migrator.set_ignore_missing(true);
    migrator
});

/// Apply all pending migrations. This is the ONE migrate that the backend and ingest both
/// call. `Migrator::run` takes its own advisory lock, so concurrent cold starts are serialized
/// while Aurora resumes, with no bespoke lock to keep in sync.
pub async fn run(pool: &sqlx::PgPool) -> Result<(), DbError> {
    Ok(MIGRATOR.run(pool).await?)
}

#[cfg(test)]
mod tests {
    use super::MIGRATOR;

    /// Every migration's checksum (the SHA-384 of its file, which sqlx records in
    /// `_sqlx_migrations`). A database refuses to start (`VersionMismatch`) once an applied file's
    /// checksum changes, and any deploy of the branch applies every migration on it, so a
    /// migration's bytes never change once it lands: a schema change is a new numbered file, pinned
    /// here in the commit that adds it.
    const APPLIED: &[(i64, &str)] = &[
        (
            1,
            "c203cc3c154795a5d97585f82b6be98cdbe4a07ee00f424bf2d61aa08d57e655\
             e7983b575ef90eb1ab5dbc251ee50465",
        ),
        (
            2,
            "12e1a4943e15efb812ba9f53c1930c50fc8fb7982cabde87e7182dc6c87348cd\
             d17292c523993073209d8bee2ef9510a",
        ),
    ];

    #[test]
    fn every_migration_keeps_its_version_and_checksum() {
        let embedded: Vec<(i64, String)> =
            MIGRATOR.iter().map(|m| (m.version, hex::encode(&*m.checksum))).collect();
        let pinned: Vec<(i64, String)> =
            APPLIED.iter().map(|(version, checksum)| (*version, (*checksum).to_owned())).collect();
        assert_eq!(
            embedded, pinned,
            "a migration changed, or a new one is not pinned in APPLIED yet"
        );
    }
}
