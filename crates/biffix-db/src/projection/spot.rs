//! `spot_interruption`: the EC2 instances a spot reclaim warning named.

use biffix_core::event::Kind;
use sqlx::PgConnection;

use crate::error::DbError;

/// Derive `spot_interruption` rows from the logged spot reclaim warnings with `seq` in
/// `(after_seq, through_seq]` (idempotent), where readers join them to the attempt that ran on the
/// named EC2 instance.
///
/// `spot_interruption` IS a projection — a pure function of the log (`spot_reclaim_warning` events,
/// keyed by the EC2 instance they name). It is not run-scoped: the warning carries no run or job,
/// so the run fold can never attribute it, and it is derived here rather than in the reducer. It is
/// advanced under the same cursor and lock as the run fold (so a row exists iff its event is
/// folded), and [`rebuild_spot_interruption`] re-derives it from the whole log, so a derivation fix
/// is applied by replay, never by patching rows. Contrast
/// `host_registry`, which caches ECS facts that are NOT in the log.
pub(super) async fn derive_spot_interruption(
    conn: &mut PgConnection,
    after_seq: i64,
    through_seq: i64,
) -> Result<(), DbError> {
    sqlx::query(
        r"INSERT INTO spot_interruption (ec2_instance_id, warned_at)
           SELECT payload->>'instance_id', ts_micros FROM run_events
            WHERE seq > $1 AND seq <= $2 AND kind = $3 AND payload->>'instance_id' IS NOT NULL
           ON CONFLICT DO NOTHING",
    )
    .bind(after_seq)
    .bind(through_seq)
    .bind(Kind::SpotReclaimWarning.as_str())
    .execute(conn)
    .await?;
    Ok(())
}

/// Re-derive the whole `spot_interruption` projection from the log: drop every row and replay every
/// spot reclaim warning through [`derive_spot_interruption`]. The caller holds the projection lock
/// and owns the transaction.
pub(super) async fn rebuild_spot_interruption(conn: &mut PgConnection) -> Result<(), DbError> {
    sqlx::query("DELETE FROM spot_interruption").execute(&mut *conn).await?;
    derive_spot_interruption(conn, 0, i64::MAX).await
}
