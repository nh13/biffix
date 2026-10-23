//! The host registry's lifecycle column (`host_registry.lifecycle`), resolved from EC2.

use sqlx::postgres::PgPool;

use biffix_db::DbError;

/// The EC2 instances of hosts whose spot/on-demand lifecycle is still unknown and that were first
/// seen at or after `since_micros` (older ones have aged out of EC2's DescribeInstances), each once
/// (an instance whose container instance re-registered has several host rows), oldest first, at
/// most `limit`.
pub async fn hosts_missing_lifecycle(
    pool: &PgPool,
    since_micros: i64,
    limit: i64,
) -> Result<Vec<String>, DbError> {
    Ok(sqlx::query_scalar(
        r"SELECT ec2_instance_id FROM host_registry
             WHERE lifecycle IS NULL AND first_seen_at >= $1
             GROUP BY ec2_instance_id
             ORDER BY min(first_seen_at), ec2_instance_id
             LIMIT $2",
    )
    .bind(since_micros)
    .bind(limit.max(1))
    .fetch_all(pool)
    .await?)
}

/// Record the EC2 lifecycle of every host row for `ec2_instance_id` that lacks one. A host's
/// lifecycle never changes, so filling a blank only is idempotent. Not part of the event log or the
/// fold, so it takes no projection lock.
pub async fn record_host_lifecycle(
    pool: &PgPool,
    ec2_instance_id: &str,
    lifecycle: &str,
) -> Result<(), DbError> {
    sqlx::query(
        "UPDATE host_registry SET lifecycle = $2 WHERE ec2_instance_id = $1 AND lifecycle IS NULL",
    )
    .bind(ec2_instance_id)
    .bind(lifecycle)
    .execute(pool)
    .await?;
    Ok(())
}
