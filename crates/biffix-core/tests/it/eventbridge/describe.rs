//! DescribeJobs terminal facts (`eventbridge::describe`) against the EventBridge decoders. A
//! reconciled terminal must be the very event the live EventBridge one decodes to, and the
//! reconciler's envelope must route as one.

use super::ctx;
use biffix_core::event::Kind;
use biffix_core::eventbridge::decode::decode_batch;
use biffix_core::eventbridge::describe::{
    terminal_run_event, DescribedTerminalJob, TerminalStatus,
};
use biffix_core::eventbridge::{route, Ingested, NormalizedIngestEnvelope};

#[test]
fn the_reconcilers_envelope_routes_to_its_event() {
    let event = terminal_run_event(&DescribedTerminalJob {
        batch_job_id: "job-reconciled".into(),
        status: TerminalStatus::Succeeded,
        stopped_at_millis: 1_789_186_351_123,
        exit_code: Some(0),
        status_reason: None,
        log_stream: None,
        instance_id: None,
        queue: None,
        is_coordinator: false,
        requested: Default::default(),
    });
    // The reconciler enqueues the typed envelope, exactly as the ingest reconcile tick does.
    let value = serde_json::to_value(NormalizedIngestEnvelope::new(event.clone())).unwrap();
    match route(&value, &ctx()).unwrap().unwrap() {
        Ingested::Event(routed) => {
            assert_eq!(routed.kind(), Kind::TaskSucceeded);
            assert_eq!(routed, event);
        }
        other => panic!("expected a run event, got {other:?}"),
    }
}

#[test]
fn reconciliation_and_eventbridge_share_terminal_event_identity() {
    let eventbridge = serde_json::json!({
        "time": "2026-09-12T04:12:32Z",
        "detail": {
            "jobId": "job-reconciled",
            "jobQueue": "q/biffix-dev-cpu-spot",
            "status": "FAILED",
            "stoppedAt": 1_789_186_352_456_i64,
            "statusReason": "Host EC2 instance terminated (spot)",
            "container": {
                "exitCode": 137,
                "logStreamName": "batch/job-reconciled",
                "containerInstanceArn": "arn:aws:ecs:us-east-1:1:container-instance/c/ci-9",
                "resourceRequirements": [
                    {"value": "2", "type": "VCPU"},
                    {"value": "7680", "type": "MEMORY"}
                ]
            }
        }
    });
    let live = decode_batch(&eventbridge, &ctx()).unwrap();
    let reconciled = terminal_run_event(&DescribedTerminalJob {
        batch_job_id: "job-reconciled".into(),
        status: TerminalStatus::Failed,
        stopped_at_millis: 1_789_186_352_456,
        exit_code: Some(137),
        status_reason: Some("Host EC2 instance terminated (spot)".into()),
        log_stream: Some("batch/job-reconciled".into()),
        // DescribeJobs' containerInstanceArn, reduced by the same last-segment rule.
        instance_id: Some(
            biffix_core::eventbridge::decode::container_instance_id(
                "arn:aws:ecs:us-east-1:1:container-instance/c/ci-9",
            )
            .into(),
        ),
        queue: Some("biffix-dev-cpu-spot".into()),
        is_coordinator: false,
        // DescribeJobs' container.resourceRequirements, parsed by the same rule.
        requested: biffix_core::eventbridge::decode::requested_resources(
            [("VCPU", "2"), ("MEMORY", "7680")],
            None,
            None,
        ),
    });

    assert_eq!(reconciled, live);
    assert_eq!(live.requested_vcpus, Some(2.0));

    // A coordinator's terminal (its queue on the live side, `is_coordinator` on the reconciled
    // one) is the same fact both ways too, Batch's account of the stop included.
    let mut coordinator = eventbridge.clone();
    coordinator["detail"]["jobQueue"] = "q/biffix-dev-coordinator".into();
    let live_coordinator = decode_batch(&coordinator, &ctx()).unwrap();
    let reconciled_coordinator = terminal_run_event(&DescribedTerminalJob {
        batch_job_id: "job-reconciled".into(),
        status: TerminalStatus::Failed,
        stopped_at_millis: 1_789_186_352_456,
        exit_code: Some(137),
        status_reason: Some("Host EC2 instance terminated (spot)".into()),
        log_stream: Some("batch/job-reconciled".into()),
        instance_id: Some("ci-9".into()),
        queue: Some("biffix-dev-coordinator".into()),
        is_coordinator: true,
        requested: biffix_core::eventbridge::decode::requested_resources(
            [("VCPU", "2"), ("MEMORY", "7680")],
            None,
            None,
        ),
    });
    assert_eq!(reconciled_coordinator, live_coordinator);
    assert_eq!(live_coordinator.kind(), biffix_core::event::Kind::CoordinatorFailed);
    assert_eq!(live.requested_memory_mib, Some(7680));
    assert_eq!(reconciled.dedup_key, "aws-batch:job-reconciled:FAILED");

    // A minimal EventBridge envelope with NO `stoppedAt` times the fact from envelope `time`
    // instead — a different source-time than reconciliation's `stoppedAt`. The dedup key must still
    // match (it is keyed on (job, status) alone), so the two never store as two terminals.
    let no_stopped_at = serde_json::json!({
        "time": "2026-09-12T04:12:32Z",
        "detail": {
            "jobId": "job-reconciled",
            "jobQueue": "q/biffix-dev-cpu-spot",
            "status": "FAILED",
            "statusReason": "Host EC2 instance terminated (spot)",
            "container": { "exitCode": 137, "logStreamName": "batch/job-reconciled" }
        }
    });
    let live_fallback = decode_batch(&no_stopped_at, &ctx()).unwrap();
    assert_ne!(
        live_fallback.ts, reconciled.ts,
        "the fallback variant is timed differently (envelope time, not stoppedAt)"
    );
    assert_eq!(
        live_fallback.dedup_key, reconciled.dedup_key,
        "a terminal missing stoppedAt must share the reconciled terminal's dedup key"
    );
}
