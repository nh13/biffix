//! Routing tests (pure): classify an EventBridge envelope and decode it.

use super::ctx;
use biffix_core::event::{Kind, RunEvent};
use biffix_core::eventbridge::{route, Ingested, NormalizedIngestEnvelope};
use biffix_core::ids::BatchJobId;
use biffix_core::time::Timestamp;
use biffix_core::translate::from_aws_batch;
use biffix_core::wire::aws_batch::{AwsBatchEvent, BatchStatus};

/// A SUCCEEDED terminal for `job` as the ingest reconciler builds one from DescribeJobs (its
/// `terminal_run_event`): the shape the reconciler wraps in the envelope.
fn reconciled_terminal(job: &str) -> RunEvent {
    from_aws_batch(&AwsBatchEvent {
        ts: Timestamp::from_epoch_seconds(1_789_186_351_123_f64 / 1_000.0),
        batch_job_id: BatchJobId::new(job),
        status: BatchStatus::Succeeded,
        exit_code: Some(0),
        status_reason: None,
        log_stream: None,
        instance_id: None,
        queue: None,
        is_coordinator: false,
        requested_vcpus: None,
        requested_memory_mib: None,
        prior_attempts: None,
    })
}

/// Route `ev` and expect a run event (not a host fact).
fn route_event(ev: &serde_json::Value) -> RunEvent {
    match route(ev, &ctx()).unwrap().unwrap() {
        Ingested::Event(event) => event,
        other => panic!("expected a run event, got {other:?}"),
    }
}

#[test]
fn routes_batch_state_change() {
    let ev = serde_json::json!({
        "source": "aws.batch",
        "detail-type": "Batch Job State Change",
        "time": "2026-09-12T04:12:31Z",
        "detail": {"jobId": "j1", "jobQueue": "q/biffix-dev-cpu-spot", "status": "RUNNING"}
    });
    let re = route_event(&ev);
    assert_eq!(re.kind(), Kind::TaskStarted);
}

#[test]
fn routes_spot_warning() {
    let ev = serde_json::json!({
        "source": "aws.ec2",
        "detail-type": "EC2 Spot Instance Interruption Warning",
        "time": "2026-09-12T04:12:31Z",
        "detail": {"instance-id": "i-1"}
    });
    let re = route_event(&ev);
    assert_eq!(re.kind(), Kind::SpotReclaimWarning);
}

#[test]
fn routes_ecs_container_instance_state_change_to_host() {
    let ev = serde_json::json!({
        "source": "aws.ecs",
        "detail-type": "ECS Container Instance State Change",
        "time": "2026-09-23T05:57:16Z",
        "detail": {
            "clusterArn": "arn:aws:ecs:us-east-1:123:cluster/AWSBatch-CEcpuspot-abc",
            "containerInstanceArn":
                "arn:aws:ecs:us-east-1:123:container-instance/AWSBatch-CEcpuspot-abc/06bf75",
            "ec2InstanceId": "i-0715de4fdba56a059",
            "version": 1
        }
    });
    match route(&ev, &ctx()).unwrap().unwrap() {
        Ingested::Host(host) => {
            assert_eq!(host.container_instance_id, "06bf75");
            assert_eq!(host.ec2_instance_id, "i-0715de4fdba56a059");
        }
        other => panic!("expected a host fact, got {other:?}"),
    }
}

#[test]
fn ignores_unrelated_events() {
    let ev = serde_json::json!({
        "source": "aws.s3",
        "detail-type": "Object Created",
        "detail": {}
    });
    assert!(route(&ev, &ctx()).unwrap().is_none());
}

#[test]
fn routes_normalized_reconciliation_event_to_writer() {
    let event = reconciled_terminal("job-reconciled");
    // The reconciler enqueues the typed envelope (ingest's tests route its own event too).
    let value = serde_json::to_value(NormalizedIngestEnvelope::new(event)).unwrap();
    let routed = route_event(&value);
    assert_eq!(routed.kind(), Kind::TaskSucceeded);
    assert_eq!(routed.batch_job_id.unwrap().as_str(), "job-reconciled");
}

#[test]
fn does_not_route_an_untagged_run_event() {
    // A bare RunEvent (no `biffix_ingest` tag) is not a queue message shape biffix produces, so it
    // must not be routed as normalized — routing keys on the explicit tag, never on a RunEvent's
    // incidental fields (`schema_version`/`dedup_key`).
    let bare = reconciled_terminal("job-x");
    let value = serde_json::to_value(bare).unwrap();
    assert!(
        route(&value, &ctx()).unwrap().is_none(),
        "an untagged RunEvent must not be mistaken for a biffix ingest envelope"
    );
}

// ── CodeBuild (captured from real builds, trimmed; the account id replaced) ──────────────────────

const BUILD_ARN: &str = "arn:aws:codebuild:us-east-1:123456789012:build/biffix-cb-probe:e5c35c6f-ffcb-45b3-aad8-07cd482716e4";

/// The build's CloudWatch console link (`additional-information.logs.deep-link`).
const DEEP_LINK: &str = "https://console.aws.amazon.com/cloudwatch/home?region=us-east-1#logsV2:log-groups/log-group/$252Faws$252Fcodebuild$252Fbiffix-cb-probe/log-events/e5c35c6f";

/// A CodeBuild "Build State Change" as EventBridge delivers it, with the parts that vary.
fn codebuild_event(
    status: &str,
    time: &str,
    env: serde_json::Value,
    extra: serde_json::Value,
) -> serde_json::Value {
    let mut info = serde_json::json!({
        "environment": {
            "type": "ARM_CONTAINER",
            "image": "aws/codebuild/amazonlinux-aarch64-standard:3.0",
            "environment-variables": env
        },
        "logs": {"deep-link": DEEP_LINK},
        "build-complete": status != "IN_PROGRESS"
    });
    info.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
    serde_json::json!({
        "version": "0",
        "id": "ed712705-ad52-2dba-60dd-a255de61aab5",
        "detail-type": "CodeBuild Build State Change",
        "source": "aws.codebuild",
        "account": "123456789012",
        "time": time,
        "region": "us-east-1",
        "resources": [BUILD_ARN],
        "detail": {
            "build-status": status,
            "project-name": "biffix-cb-probe",
            "build-id": BUILD_ARN,
            "current-phase": if status == "IN_PROGRESS" { "SUBMITTED" } else { "COMPLETED" },
            "additional-information": info
        }
    })
}

fn subject(value: &str) -> serde_json::Value {
    serde_json::json!([{"name": "BIFFIX_BUILD_SUBJECT", "type": "PLAINTEXT", "value": value}])
}

#[test]
fn routes_a_started_build_by_its_subject() {
    let ev = codebuild_event(
        "IN_PROGRESS",
        "2026-09-26T10:24:20Z",
        subject("runtime:probe"),
        serde_json::json!({}),
    );
    let re = route_event(&ev);
    assert_eq!(re.kind(), Kind::BuildStarted);
    assert_eq!(re.run_id, None);
    assert_eq!(re.dedup_key, format!("aws-codebuild:{BUILD_ARN}:IN_PROGRESS"));
    assert_eq!(biffix_core::builds::subject_of(&re), Some("runtime:probe"));
}

#[test]
fn a_build_state_change_needs_no_project_name_but_every_event_needs_its_detail() {
    use biffix_core::eventbridge::decode::DecodeError;
    // Nothing reads the project, so a change without one is still the build's.
    let mut ev = codebuild_event(
        "IN_PROGRESS",
        "2026-09-26T10:24:20Z",
        subject("runtime:probe"),
        serde_json::json!({}),
    );
    ev["detail"].as_object_mut().unwrap().remove("project-name");
    assert_eq!(route_event(&ev).kind(), Kind::BuildStarted);
    // An event routed by its source and detail-type with no detail is malformed, and says so.
    ev.as_object_mut().unwrap().remove("detail");
    match route(&ev, &ctx()) {
        Err(DecodeError::Malformed(m)) => assert_eq!(m, "missing detail"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_succeeded_build_carries_its_exported_variables() {
    let ev = codebuild_event(
        "SUCCEEDED",
        "2026-09-26T10:24:28Z",
        subject("runtime:probe"),
        serde_json::json!({
            "exported-environment-variables": [
                {"name": "DIGEST", "value": "sha256:0123456789abcdef"},
                {"name": "ARCHS", "value": "amd64,arm64"}
            ],
            "phases": [
                {"phase-type": "BUILD", "phase-status": "SUCCEEDED", "phase-context": [": "]},
                {"phase-type": "COMPLETED"}
            ]
        }),
    );
    let re = route_event(&ev);
    let biffix_core::event::EventData::BuildSucceeded { subject, build_arn, outputs, logs_url } =
        &re.data
    else {
        panic!("expected build_succeeded, got {:?}", re.data);
    };
    assert_eq!((subject.as_str(), build_arn.as_str()), ("runtime:probe", BUILD_ARN));
    // Exactly the exported variables: nothing leaks in from `environment-variables` (the subject).
    assert_eq!(
        outputs,
        &std::collections::BTreeMap::from([
            ("DIGEST".to_string(), "sha256:0123456789abcdef".to_string()),
            ("ARCHS".to_string(), "amd64,arm64".to_string()),
        ])
    );
    assert_eq!(logs_url.as_deref(), Some(DEEP_LINK));
}

#[test]
fn a_failed_build_names_the_failed_phase_and_its_message() {
    let ev = codebuild_event(
        "FAILED",
        "2026-09-26T10:25:10Z",
        subject("image:probe"),
        serde_json::json!({
            "phases": [
                {"phase-type": "PRE_BUILD", "phase-status": "SUCCEEDED", "phase-context": [": "]},
                {"phase-type": "BUILD", "phase-status": "FAILED", "phase-context": [
                    "COMMAND_EXECUTION_ERROR: Error while executing command: exit 3. Reason: exit status 3"
                ]},
                {"phase-type": "POST_BUILD", "phase-status": "SUCCEEDED", "phase-context": [": "]},
                {"phase-type": "COMPLETED"}
            ]
        }),
    );
    let re = route_event(&ev);
    let biffix_core::event::EventData::BuildFailed { subject, reason, .. } = &re.data else {
        panic!("expected build_failed, got {:?}", re.data);
    };
    assert_eq!(subject, "image:probe");
    assert_eq!(
        reason,
        "FAILED: BUILD: COMMAND_EXECUTION_ERROR: Error while executing command: exit 3. \
         Reason: exit status 3"
    );
}

#[test]
fn a_build_biffix_did_not_start_is_ignored() {
    let ev = codebuild_event(
        "SUCCEEDED",
        "2026-09-26T10:24:28Z",
        serde_json::json!([{"name": "OTHER", "type": "PLAINTEXT", "value": "x"}]),
        serde_json::json!({}),
    );
    assert!(route(&ev, &ctx()).unwrap().is_none());
}

#[test]
fn a_tagged_envelope_without_a_valid_event_is_malformed() {
    use biffix_core::eventbridge::decode::DecodeError;
    // One fault each: no event; an event that is no RunEvent; a valid event under a tag this build
    // does not know.
    let mut unknown_tag =
        serde_json::to_value(NormalizedIngestEnvelope::new(reconciled_terminal("job-x"))).unwrap();
    unknown_tag["biffix_ingest"] = "something_else".into();
    for bad in [
        serde_json::json!({"biffix_ingest": "run_event"}),
        serde_json::json!({"biffix_ingest": "run_event", "event": {}}),
        unknown_tag,
    ] {
        assert!(
            matches!(route(&bad, &ctx()), Err(DecodeError::Malformed(_))),
            "{bad} must be malformed, not ignored"
        );
    }
}
