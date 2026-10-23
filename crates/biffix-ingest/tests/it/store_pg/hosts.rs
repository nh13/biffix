//! The host registry, host lifecycle lookups and spot warnings.

use super::*;

/// EC2's interruption warning for spot instance `i-9`.
fn spot_warning() -> RunEvent {
    ev(
        7.0,
        EventSource::AwsEc2,
        "aws-ec2:i-9:interruption:7000000",
        EventData::SpotReclaimWarning {},
    )
    .with_instance("i-9")
}

/// The whole `host_registry` row for `ci-1`, minus `received_at` (wall-clock insert time).
async fn host_row(pool: &sqlx::PgPool) -> serde_json::Value {
    sqlx::query_scalar(
        "SELECT to_jsonb(h) - 'received_at' AS row FROM host_registry h \
          WHERE container_instance_id = 'ci-1'",
    )
    .fetch_one(pool)
    .await
    .unwrap()
}

#[tokio::test]
#[ignore = "requires the LocalStack/Postgres harness"]
async fn host_registry_row_is_independent_of_event_order_and_redelivery() {
    let forward = common::isolated_pool("host_registry_order_forward").await;
    for fact in [host(1, 100), host(2, 200), host(3, 300)] {
        ingest_batch(&forward, &[], &[fact]).await.unwrap();
    }
    let reverse = common::isolated_pool("host_registry_order_reverse").await;
    for fact in [host(3, 300), host(1, 100), host(3, 300), host(2, 200)] {
        ingest_batch(&reverse, &[], &[fact]).await.unwrap();
    }

    let row = host_row(&forward).await;
    assert_eq!(row, host_row(&reverse).await);
    // Blanks from the registration event are filled by later ones; the kept raw detail is the
    // highest version's; first_seen_at is the earliest observation.
    assert_eq!(row["agent_version"], "1.106.2");
    assert_eq!(row["detail_version"], 3);
    assert_eq!(row["raw_detail"]["version"], 3);
    assert_eq!(row["first_seen_at"], 100);
    assert_eq!(row["instance_type"], "c6i.large");
    assert_eq!(row["registered_memory_mib"], 3787);
}

#[tokio::test]
#[ignore = "requires the LocalStack/Postgres harness"]
async fn ingest_batch_with_only_hosts_appends_no_events() {
    let pool = common::isolated_pool("ingest_batch_with_only_hosts_appends_no_events").await;
    let appended = ingest_batch(&pool, &[], &[host(1, 100)]).await.unwrap();
    assert_eq!(appended, 0);
    let events: i64 =
        sqlx::query_scalar("SELECT COUNT(*) AS n FROM run_events").fetch_one(&pool).await.unwrap();
    assert_eq!(events, 0, "host facts never enter the event log");
}

#[tokio::test]
#[ignore = "requires the LocalStack/Postgres harness"]
async fn host_lifecycle_is_looked_up_once_and_only_within_the_window() {
    use biffix_ingest::store::hosts::{hosts_missing_lifecycle, record_host_lifecycle};
    let pool = common::isolated_pool("host_lifecycle_lookup_window").await;
    let mut old = host(1, 100);
    old.container_instance_id = "ci-old".into();
    old.ec2_instance_id = "i-old".into();
    ingest_batch(&pool, &[], &[old, host(1, 5_000)]).await.unwrap();

    // Only hosts first seen within the window are candidates.
    let missing = hosts_missing_lifecycle(&pool, 1_000, 10).await.unwrap();
    assert_eq!(missing, ["i-0715de4fdba56a059"]);

    record_host_lifecycle(&pool, "i-0715de4fdba56a059", "spot").await.unwrap();
    // A later answer never overwrites the first (a host's lifecycle is invariant).
    record_host_lifecycle(&pool, "i-0715de4fdba56a059", "on_demand").await.unwrap();
    assert_eq!(host_row(&pool).await["lifecycle"], "spot");
    assert!(hosts_missing_lifecycle(&pool, 1_000, 10).await.unwrap().is_empty());
}

#[tokio::test]
#[ignore = "requires the LocalStack/Postgres harness"]
async fn spot_warnings_are_recorded_for_readers_to_attribute() {
    let pool = common::isolated_pool("spot_warnings_are_recorded").await;
    let warning = spot_warning();
    // Redelivered: recorded once.
    ingest_batch(&pool, std::slice::from_ref(&warning), &[]).await.unwrap();
    ingest_batch(&pool, &[warning], &[]).await.unwrap();
    let rows: Vec<(String, i64)> =
        sqlx::query_as("SELECT ec2_instance_id, warned_at FROM spot_interruption")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(rows, vec![("i-9".to_string(), 7_000_000)]);
}

/// `spot_interruption` is a projection of the log: a refold's first page rebuilds it, dropping rows
/// no logged warning supports and restoring ones that went missing.
#[tokio::test]
#[ignore = "requires the LocalStack/Postgres harness"]
async fn a_refold_rebuilds_spot_interruption_from_the_log() {
    let pool = common::isolated_pool("a_refold_rebuilds_spot_interruption").await;
    let warning = spot_warning();
    ingest_batch(&pool, &[warning], &[]).await.unwrap();
    sqlx::query("DELETE FROM spot_interruption").execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO spot_interruption (ec2_instance_id, warned_at) VALUES ('i-stale', 1)")
        .execute(&pool)
        .await
        .unwrap();

    refold_runs(&pool, None, 10).await.unwrap();
    let rows: Vec<(String, i64)> =
        sqlx::query_as("SELECT ec2_instance_id, warned_at FROM spot_interruption")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(rows, vec![("i-9".to_string(), 7_000_000)]);
}
