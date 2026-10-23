//! `host_registry`: the ECS container-instance facts the ingest drain records beside the events of
//! its batch. Not part of the event log or the fold (they are facts the log does not carry).

use biffix_core::eventbridge::decode::HostFact;
use sqlx::{AssertSqlSafe, PgConnection};

use crate::error::DbError;

/// The `host_registry` columns an upsert fills; every one is optional on a given ECS event.
const HOST_DESCRIPTIVE_COLUMNS: [&str; 12] = [
    "instance_type",
    "availability_zone",
    "cpu_architecture",
    "os_family",
    "ami_id",
    "subnet_id",
    "registered_vcpus",
    "registered_memory_mib",
    "gpu_count",
    "agent_version",
    "docker_version",
    "raw_detail",
];

/// Upsert one host's facts. The result is a function of the SET of events seen for the container
/// instance, not their arrival order. The modeled facts are invariant for a container instance's
/// lifetime and only their presence varies between events (e.g. `versionInfo` is empty on the first
/// registration event), so filling blanks from any event converges; `raw_detail`, which does differ
/// per event and is never null, is kept from the highest `detail_version`. `first_seen_at` is the
/// minimum observed time. Redelivery is a no-op.
pub(crate) async fn record_host(conn: &mut PgConnection, host: &HostFact) -> Result<(), DbError> {
    let updates: Vec<String> = HOST_DESCRIPTIVE_COLUMNS
        .iter()
        .map(|c| {
            format!(
                "{c} = CASE WHEN EXCLUDED.detail_version > host_registry.detail_version \
                   THEN COALESCE(EXCLUDED.{c}, host_registry.{c}) \
                   ELSE COALESCE(host_registry.{c}, EXCLUDED.{c}) END"
            )
        })
        .collect();
    let sql = format!(
        r"INSERT INTO host_registry (container_instance_id, ec2_instance_id, cluster_arn,
               {columns}, detail_version, first_seen_at)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17)
           ON CONFLICT (container_instance_id) DO UPDATE SET
               {updates},
               detail_version = GREATEST(host_registry.detail_version, EXCLUDED.detail_version),
               first_seen_at = LEAST(host_registry.first_seen_at, EXCLUDED.first_seen_at)",
        columns = HOST_DESCRIPTIVE_COLUMNS.join(", "),
        updates = updates.join(",\n               "),
    );
    sqlx::query(AssertSqlSafe(sql))
        .bind(&host.container_instance_id)
        .bind(&host.ec2_instance_id)
        .bind(&host.cluster_arn)
        .bind(&host.instance_type)
        .bind(&host.availability_zone)
        .bind(&host.cpu_architecture)
        .bind(&host.os_family)
        .bind(&host.ami_id)
        .bind(&host.subnet_id)
        .bind(host.registered_vcpus)
        .bind(host.registered_memory_mib)
        .bind(host.gpu_count)
        .bind(&host.agent_version)
        .bind(&host.docker_version)
        .bind(&host.raw_detail)
        .bind(host.detail_version)
        .bind(host.observed_at_micros)
        .execute(conn)
        .await?;
    Ok(())
}
