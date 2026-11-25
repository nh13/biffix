//! DescribeJobs facts (`eventbridge::describe`) against the EventBridge decoders. A reconciled
//! start or terminal must be the very event the live EventBridge one decodes to, and the
//! reconciler's envelope must route as one.

use super::{job_event, route_one};
use biffix_core::event::Kind;
use biffix_core::eventbridge::describe::{
    prior_attempts_before, start_time_millis, started_run_event, terminal_run_event,
    DescribedJobFacts, DescribedStartedJob, DescribedTerminalJob, TerminalStatus,
};
use biffix_core::eventbridge::{Ingested, NormalizedIngestEnvelope};

#[test]
fn the_reconcilers_envelope_routes_to_its_event() {
    let event = terminal_run_event(&DescribedTerminalJob {
        batch_job_id: "job-reconciled".into(),
        status: TerminalStatus::Succeeded,
        stopped_at_millis: 1_789_186_351_123,
        exit_code: Some(0),
        status_reason: None,
        facts: DescribedJobFacts {
            log_stream: None,
            instance_id: None,
            queue: None,
            is_coordinator: false,
            requested: Default::default(),
        },
    });
    // The reconciler enqueues the typed envelope, exactly as the ingest reconcile tick does.
    let value = serde_json::to_value(NormalizedIngestEnvelope::new(event.clone())).unwrap();
    match route_one(&value) {
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
    let live = job_event(&eventbridge);
    let reconciled = terminal_run_event(&DescribedTerminalJob {
        batch_job_id: "job-reconciled".into(),
        status: TerminalStatus::Failed,
        stopped_at_millis: 1_789_186_352_456,
        exit_code: Some(137),
        status_reason: Some("Host EC2 instance terminated (spot)".into()),
        facts: DescribedJobFacts {
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
        },
    });

    assert_eq!(reconciled, live);
    assert_eq!(live.requested_vcpus, Some(2.0));

    // A coordinator's terminal (its queue on the live side, `is_coordinator` on the reconciled
    // one) is the same fact both ways too, Batch's account of the stop included.
    let mut coordinator = eventbridge.clone();
    coordinator["detail"]["jobQueue"] = "q/biffix-dev-coordinator".into();
    let live_coordinator = job_event(&coordinator);
    let reconciled_coordinator = terminal_run_event(&DescribedTerminalJob {
        batch_job_id: "job-reconciled".into(),
        status: TerminalStatus::Failed,
        stopped_at_millis: 1_789_186_352_456,
        exit_code: Some(137),
        status_reason: Some("Host EC2 instance terminated (spot)".into()),
        facts: DescribedJobFacts {
            log_stream: Some("batch/job-reconciled".into()),
            instance_id: Some("ci-9".into()),
            queue: Some("biffix-dev-coordinator".into()),
            is_coordinator: true,
            requested: biffix_core::eventbridge::decode::requested_resources(
                [("VCPU", "2"), ("MEMORY", "7680")],
                None,
                None,
            ),
        },
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
    let live_fallback = job_event(&no_stopped_at);
    assert_ne!(
        live_fallback.ts, reconciled.ts,
        "the fallback variant is timed differently (envelope time, not stoppedAt)"
    );
    assert_eq!(
        live_fallback.dedup_key, reconciled.dedup_key,
        "a terminal missing stoppedAt must share the reconciled terminal's dedup key"
    );
}

/// A live RUNNING state change for `job-started`, with `attempts` as Batch lists them.
fn live_running(attempts: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "time": "2026-09-12T04:12:32Z",
        "detail": {
            "jobId": "job-started",
            "jobQueue": "q/biffix-dev-cpu-spot",
            "status": "RUNNING",
            "startedAt": 1_789_186_350_000_i64,
            "attempts": attempts,
            "container": {
                "logStreamName": "batch/job-started",
                "containerInstanceArn": "arn:aws:ecs:us-east-1:1:container-instance/c/ci-7",
                "resourceRequirements": [
                    {"value": "4", "type": "VCPU"},
                    {"value": "16384", "type": "MEMORY"}
                ]
            }
        }
    })
}

/// The reconciled start matching [`live_running`], `prior_attempts` attempts after the first.
fn reconciled_start(prior_attempts: u32) -> DescribedStartedJob {
    DescribedStartedJob {
        batch_job_id: "job-started".into(),
        started_at_millis: 1_789_186_350_000,
        prior_attempts,
        facts: DescribedJobFacts {
            log_stream: Some("batch/job-started".into()),
            instance_id: Some(
                biffix_core::eventbridge::decode::container_instance_id(
                    "arn:aws:ecs:us-east-1:1:container-instance/c/ci-7",
                )
                .into(),
            ),
            queue: Some("biffix-dev-cpu-spot".into()),
            is_coordinator: false,
            requested: biffix_core::eventbridge::decode::requested_resources(
                [("VCPU", "4"), ("MEMORY", "16384")],
                None,
                None,
            ),
        },
    }
}

#[test]
fn reconciliation_and_eventbridge_share_start_event_identity() {
    let live = job_event(&live_running(serde_json::json!([])));
    let reconciled = started_run_event(&reconciled_start(0));
    assert_eq!(reconciled, live);
    assert_eq!(live.kind(), Kind::TaskStarted);
    assert_eq!(reconciled.dedup_key, "aws-batch:job-started:RUNNING");

    // A coordinator's start: its queue on the live side, `is_coordinator` on the reconciled one.
    let mut coordinator = live_running(serde_json::json!([]));
    coordinator["detail"]["jobQueue"] = "q/biffix-dev-coordinator".into();
    let live_coordinator = job_event(&coordinator);
    let start = reconciled_start(0);
    let reconciled_coordinator = started_run_event(&DescribedStartedJob {
        facts: DescribedJobFacts {
            queue: Some("biffix-dev-coordinator".into()),
            is_coordinator: true,
            ..start.facts.clone()
        },
        ..start
    });
    assert_eq!(reconciled_coordinator, live_coordinator);
    assert_eq!(live_coordinator.kind(), Kind::CoordinatorRunning);
}

#[test]
fn a_retried_jobs_reconciled_start_shares_the_retrys_key() {
    // Batch re-ran the job under its retryStrategy: the RUNNING of the second attempt lists the
    // first, finished attempt.
    let attempts = serde_json::json!([{
        "startedAt": 1_789_186_000_000_i64,
        "stoppedAt": 1_789_186_100_000_i64,
        "container": {"exitCode": 137}
    }]);
    let live = job_event(&live_running(attempts));
    let reconciled = started_run_event(&reconciled_start(1));
    assert_eq!(reconciled.dedup_key, live.dedup_key);
    assert!(reconciled.dedup_key.ends_with(":RUNNING:1"), "{}", reconciled.dedup_key);
    assert_eq!(reconciled, live);
}

/// The start the reconciler derives from a DescribeJobs-shaped `detail` (job-level `startedAt`
/// and `attempts`), through the same helpers the ingest decoder uses.
fn reconciled_from(detail: &serde_json::Value) -> biffix_core::event::RunEvent {
    let time =
        |value: &serde_json::Value, key: &str| value.get(key).and_then(serde_json::Value::as_i64);
    let attempts = detail["attempts"].as_array().cloned().unwrap_or_default();
    let started_at_millis = start_time_millis(
        time(detail, "startedAt"),
        attempts.iter().map(|attempt| time(attempt, "startedAt")),
    )
    .expect("a start");
    let prior_attempts = prior_attempts_before(
        started_at_millis,
        attempts.iter().map(|attempt| (time(attempt, "startedAt"), time(attempt, "stoppedAt"))),
    );
    started_run_event(&DescribedStartedJob {
        started_at_millis,
        prior_attempts,
        ..reconciled_start(0)
    })
}

#[test]
fn a_reconciled_start_derives_the_live_runnings_key_from_the_same_payload() {
    // One job's history: what the live RUNNING state change carried, and what DescribeJobs later
    // reports for the same job, running or finished. The count is derived from the payload on
    // both sides, never written in by hand.
    let (s1, e1) = (1_789_186_000_000_i64, 1_789_186_100_000_i64);
    let (s2, e2) = (1_789_186_350_000_i64, 1_789_186_400_000_i64);
    let attempt = |started: i64, stopped: i64| serde_json::json!({"startedAt": started, "stoppedAt": stopped, "container": {}});
    let snapshot = |status: &str, started_at: i64, attempts: serde_json::Value| {
        let mut event = live_running(attempts);
        event["detail"]["status"] = status.into();
        event["detail"]["startedAt"] = started_at.into();
        event
    };
    let cases = [
        // First attempt, running: the live RUNNING and DescribeJobs list no attempts.
        ("first attempt, running", snapshot("RUNNING", s1, serde_json::json!([])), None, 0),
        // First attempt, finished: DescribeJobs now lists it; it is the start's own.
        (
            "first attempt, finished",
            snapshot("RUNNING", s1, serde_json::json!([])),
            Some(snapshot("SUCCEEDED", s1, serde_json::json!([attempt(s1, e1)]))),
            0,
        ),
        // A retry, running: both list the earlier attempt only.
        ("running retry", snapshot("RUNNING", s2, serde_json::json!([attempt(s1, e1)])), None, 1),
        // A retry, finished: DescribeJobs lists both attempts; the retry's own does not count.
        (
            "finished retry",
            snapshot("RUNNING", s2, serde_json::json!([attempt(s1, e1)])),
            Some(snapshot("SUCCEEDED", s2, serde_json::json!([attempt(s1, e1), attempt(s2, e2)]))),
            1,
        ),
        // A retry after an attempt that never ran (no start, no stop): the live length counts it.
        (
            "retry after an attempt that never ran",
            snapshot("RUNNING", s2, serde_json::json!([{"container": {}}])),
            None,
            1,
        ),
    ];
    for (name, live_running, described, prior) in cases {
        let live = job_event(&live_running);
        let described = described.unwrap_or_else(|| live_running.clone());
        let reconciled = reconciled_from(&described["detail"]);
        assert_eq!(reconciled.dedup_key, live.dedup_key, "{name}");
        let expected = match prior {
            0 => "aws-batch:job-started:RUNNING".to_owned(),
            n => format!("aws-batch:job-started:RUNNING:{n}"),
        };
        assert_eq!(live.dedup_key, expected, "{name}");
        assert_eq!(reconciled.ts, live.ts, "{name}");
    }
}

#[test]
fn the_reconcilers_start_envelope_routes_to_its_event() {
    let event = started_run_event(&reconciled_start(0));
    let value = serde_json::to_value(NormalizedIngestEnvelope::new(event.clone())).unwrap();
    match route_one(&value) {
        Ingested::Event(routed) => {
            assert_eq!(routed.kind(), Kind::TaskStarted);
            assert_eq!(routed, event);
        }
        other => panic!("expected a run event, got {other:?}"),
    }
}
