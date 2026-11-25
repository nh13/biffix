//! The EventBridge decoders and the ingest-queue router (`biffix_core::eventbridge`).

mod decode;
mod describe;
mod execution;
mod route;

use biffix_core::event::RunEvent;
use biffix_core::eventbridge::decode::{decode_batch, QueueContext};
use biffix_core::eventbridge::{route, Ingested};
use biffix_core::testing::retried_job_detail;

/// The queue context the tests decode under: one coordinator queue.
fn ctx() -> QueueContext {
    QueueContext { coordinator_queues: vec!["biffix-dev-coordinator".into()] }
}

/// The job-level event of a Batch job state change: the first event it decodes to, ahead of its
/// executions' events.
fn job_event(ev: &serde_json::Value) -> RunEvent {
    decode_batch(ev, &ctx()).unwrap().remove(0)
}

/// What `ev` routes to, which must be exactly one item.
fn route_one(ev: &serde_json::Value) -> Ingested {
    let mut routed = route(ev, &ctx()).unwrap();
    assert_eq!(routed.len(), 1, "expected one item, got {routed:?}");
    routed.remove(0)
}

/// A "Batch Job State Change" envelope for the retried job ([`retried_job_detail`]) in `status`.
fn retried_event(status: &str, lost_s: i64, gap_s: i64, run_s: i64) -> serde_json::Value {
    serde_json::json!({
        "source": "aws.batch",
        "detail-type": "Batch Job State Change",
        "time": "2026-09-12T04:12:31Z",
        "detail": retried_job_detail(status, lost_s, gap_s, run_s),
    })
}
