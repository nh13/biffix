//! Postgres store integration tests. `#[ignore]`d — require the harness:
//!
//!   docker compose -f dev/docker-compose.yml up -d postgres
//!   BIFFIX_TEST_PG=postgres://biffix:biffix@localhost:5433/biffix \
//!     cargo test -p biffix-ingest --test it -- --ignored store_pg::
//!
//! Verifies the event-sourcing invariants against a real DB: append is idempotent on dedup_key,
//! and the projection folds to the expected Run/Task state (projection == fold).

use biffix_core::event::EventData;
use biffix_core::event::RunEvent;
use biffix_core::eventbridge::decode::HostFact;
use biffix_core::ids::{BatchJobId, RunId};
use biffix_core::reducer::Projection;
use biffix_core::source::EventSource;
use biffix_core::time::Timestamp;
use biffix_db::log::ingest_batch;
use biffix_db::projection::refold_runs;
use biffix_ingest::store::reconcile::ended_runs_with_live_children;
use biffix_ingest::store::retention::{work_due_for_deletion, work_still_due};
use sqlx::{AssertSqlSafe, Row};

use crate::common;
use biffix_core::testing::{event as ev, RunEventExt};
use common::append;

mod attempts;
mod builds;
mod experiments;
mod fold;
mod hosts;
mod log;
mod migrate;
mod notifications;
mod outputs;
mod pricing;
mod runs;
mod samples;

/// `ev` attributed to `run`.
fn run_ev(ts: f64, source: EventSource, dedup: &str, run: &str, data: EventData) -> RunEvent {
    ev(ts, source, dedup, data).with_run(run)
}

/// The control plane's anchor of `run`, with placeholder fields.
fn submitted(ts: f64, dedup: &str, run: &str) -> RunEvent {
    run_ev(
        ts,
        EventSource::BiffixControlPlane,
        dedup,
        run,
        EventData::RunSubmitted(Box::new(biffix_core::testing::anchor())),
    )
}

/// Snakemake's submit of `run`'s task `task` as Batch job `job`, its first attempt.
fn task_submitted(ts: f64, dedup: &str, run: &str, task: &str, job: &str) -> RunEvent {
    run_ev(ts, EventSource::Snakemake, dedup, run, EventData::TaskSubmitted { image: None })
        .with_task(task)
        .with_attempt(1)
        .with_job(job)
}

/// A host fact for container instance `ci-1` as ECS reports it at `detail_version`: the
/// registration event (version 1) carries no agent version yet; later ones do.
fn host(detail_version: i64, observed_at_micros: i64) -> HostFact {
    let agent_version = (detail_version > 1).then(|| "1.106.2".to_string());
    HostFact {
        container_instance_id: "ci-1".into(),
        ec2_instance_id: "i-0715de4fdba56a059".into(),
        cluster_arn: "arn:aws:ecs:us-east-1:123:cluster/AWSBatch-CEcpuspot-abc".into(),
        instance_type: Some("c6i.large".into()),
        availability_zone: Some("us-east-1b".into()),
        cpu_architecture: Some("x86_64".into()),
        os_family: Some("LINUX".into()),
        ami_id: Some("ami-0d8e124066f7ed4f7".into()),
        subnet_id: Some("subnet-0fd0ff7018ea662af".into()),
        registered_vcpus: Some(2.0),
        registered_memory_mib: Some(3787),
        gpu_count: None,
        agent_version: agent_version.clone(),
        docker_version: None,
        detail_version,
        raw_detail: serde_json::json!({"version": detail_version, "agentVersion": agent_version}),
        observed_at_micros,
    }
}
