//! The build fold: `build_projection`, one row per image-build subject, from its build events.

use biffix_core::builds::{fold_builds, BuildProjection};
use biffix_core::event::Kind;
use sqlx::PgConnection;

use super::rows_to_events;
use crate::error::DbError;
use crate::rules::kind_strings;

/// The stored `kind`s of build events.
pub(super) fn build_kinds() -> Vec<String> {
    kind_strings(&[
        Kind::BuildRequested,
        Kind::BuildStarted,
        Kind::BuildSucceeded,
        Kind::BuildFailed,
    ])
}

/// Re-fold one build subject from all its events and persist it (`build_projection`).
pub(super) async fn fold_build_subject(
    conn: &mut PgConnection,
    subject: &str,
) -> Result<(), DbError> {
    let rows = sqlx::query(
        r"SELECT seq, payload FROM run_events
             WHERE kind = ANY($2) AND payload->>'subject' = $1
             ORDER BY ts_micros, seq",
    )
    .bind(subject)
    .bind(build_kinds())
    .fetch_all(&mut *conn)
    .await?;
    let events = rows_to_events(&rows);
    match fold_builds(&events).remove(subject) {
        Some(build) => upsert_build(conn, &build).await?,
        // The fold no longer produces this subject (its events stopped decoding, say): drop the row
        // rather than leave an older fold's result behind (`projection == fold`).
        None => {
            sqlx::query("DELETE FROM build_projection WHERE subject = $1")
                .bind(subject)
                .execute(&mut *conn)
                .await?;
        }
    }
    Ok(())
}

/// Re-derive the whole `build_projection` from the log: drop every row, then re-fold every build
/// subject the log names through [`fold_build_subject`], so a `fold_builds` fix is applied by replay
/// and a row whose subject no longer appears in the log is gone. The caller holds the projection
/// lock and owns the transaction, so readers never see the table empty.
pub(super) async fn rebuild_build_projection(conn: &mut PgConnection) -> Result<(), DbError> {
    sqlx::query("DELETE FROM build_projection").execute(&mut *conn).await?;
    let subjects: Vec<String> = sqlx::query_scalar(
        r"SELECT DISTINCT payload->>'subject' FROM run_events
             WHERE kind = ANY($1) AND payload->>'subject' IS NOT NULL",
    )
    .bind(build_kinds())
    .fetch_all(&mut *conn)
    .await?;
    for subject in &subjects {
        fold_build_subject(&mut *conn, subject).await?;
    }
    Ok(())
}

async fn upsert_build(conn: &mut PgConnection, build: &BuildProjection) -> Result<(), DbError> {
    let outputs = serde_json::to_value(&build.outputs)?;
    let request = (!build.request.is_null()).then(|| build.request.clone());
    sqlx::query(
        r"INSERT INTO build_projection
             (subject, state, build_arn, outputs, reason, logs_url, request, builds, updated_at)
           VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)
           ON CONFLICT (subject) DO UPDATE SET
             state = EXCLUDED.state, build_arn = EXCLUDED.build_arn, outputs = EXCLUDED.outputs,
             reason = EXCLUDED.reason, logs_url = EXCLUDED.logs_url, request = EXCLUDED.request,
             builds = EXCLUDED.builds, updated_at = EXCLUDED.updated_at",
    )
    .bind(&build.subject)
    .bind(build.state.as_str())
    .bind(&build.build_arn)
    .bind(outputs)
    .bind(&build.reason)
    .bind(&build.logs_url)
    .bind(request)
    .bind(build.builds as i32)
    .bind(build.updated.as_micros())
    .execute(&mut *conn)
    .await?;
    Ok(())
}
