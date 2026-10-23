//! EventBridge decode tests — pure, no AWS. Uses real EventBridge event shapes.

use super::ctx;
use biffix_core::event::Kind;
use biffix_core::event::{EventData, TerminationCategory};
use biffix_core::eventbridge::decode::{decode_batch, decode_ecs_host, decode_spot, DecodeError};

#[test]
fn worker_running_uses_batch_detail_timestamp_when_present() {
    let ev = serde_json::json!({
        "time": "2026-09-12T04:12:31Z",
        "detail": {
            "jobId": "job-1",
            "jobQueue": "arn:aws:batch:us-east-1:123:job-queue/biffix-dev-cpu-spot",
            "status": "RUNNING",
            "startedAt": 1789186351123_i64,
            "container": {"logStreamName": "ls/1"}
        }
    });
    let re = decode_batch(&ev, &ctx()).unwrap();
    assert_eq!(re.kind(), Kind::TaskStarted);
    assert_eq!(re.batch_job_id.unwrap().as_str(), "job-1");
    assert_eq!(re.ts.as_micros(), 1_789_186_351_123_000);
}

#[test]
fn worker_running_extracts_container_instance_id() {
    let ev = serde_json::json!({
        "time": "2026-09-12T04:12:31Z",
        "detail": {
            "jobId": "job-1",
            "jobQueue": "arn:aws:batch:us-east-1:123:job-queue/biffix-dev-cpu-spot",
            "status": "RUNNING",
            "startedAt": 1789186351123_i64,
            "container": {
                "logStreamName": "ls/1",
                "containerInstanceArn":
                    "arn:aws:ecs:us-east-1:123:container-instance/biffix/abc123def456"
            }
        }
    });
    let re = decode_batch(&ev, &ctx()).unwrap();
    // Only the id (ARN's last segment) is kept, not the full ARN.
    assert_eq!(re.instance_id.unwrap().as_str(), "abc123def456");
}

#[test]
fn batch_event_records_the_queue_name_not_its_arn() {
    let ev = serde_json::json!({
        "time": "2026-09-12T04:12:31Z",
        "detail": {
            "jobId": "job-1",
            "jobQueue": "arn:aws:batch:us-east-1:123:job-queue/biffix-dev-cpu-spot",
            "status": "RUNNABLE"
        }
    });
    let re = decode_batch(&ev, &ctx()).unwrap();
    assert_eq!(re.queue.as_deref(), Some("biffix-dev-cpu-spot"));
}

#[test]
fn a_queued_job_is_timed_at_its_creation_else_at_the_envelope() {
    let detail = serde_json::json!({
        "jobId": "job-1",
        "jobQueue": "q/biffix-dev-cpu-spot",
        "status": "RUNNABLE",
        "createdAt": 1_789_186_351_123_i64
    });
    let ev = serde_json::json!({"time": "2026-09-12T04:12:31Z", "detail": detail});
    let re = decode_batch(&ev, &ctx()).unwrap();
    assert_eq!(re.ts.as_micros(), 1_789_186_351_123_000);
    // No createdAt: the envelope's whole-second time.
    let mut bare = ev.clone();
    bare["detail"].as_object_mut().unwrap().remove("createdAt");
    let re = decode_batch(&bare, &ctx()).unwrap();
    assert_eq!(re.ts.as_micros(), 1_789_186_351_000_000);
}

#[test]
fn worker_running_without_container_instance_has_no_instance_id() {
    let ev = serde_json::json!({
        "time": "2026-09-12T04:12:31Z",
        "detail": {
            "jobId": "job-1",
            "jobQueue": "q/biffix-dev-cpu-spot",
            "status": "RUNNING",
            "container": {"logStreamName": "ls/1"}
        }
    });
    let re = decode_batch(&ev, &ctx()).unwrap();
    assert!(re.instance_id.is_none());
}

#[test]
fn coordinator_succeeded_decodes_to_coordinator_succeeded() {
    let ev = serde_json::json!({
        "time": "2026-09-12T04:12:31Z",
        "detail": {
            "jobId": "coord-1",
            "jobQueue": "arn:aws:batch:us-east-1:123:job-queue/biffix-dev-coordinator",
            "status": "SUCCEEDED",
            "container": {"exitCode": 0}
        }
    });
    let re = decode_batch(&ev, &ctx()).unwrap();
    assert_eq!(re.kind(), Kind::CoordinatorSucceeded);
}

#[test]
fn worker_failed_carries_exit_and_reason() {
    let ev = serde_json::json!({
        "time": "2026-09-12T04:12:31Z",
        "detail": {
            "jobId": "job-2",
            "jobQueue": "q/biffix-dev-cpu-spot",
            "status": "FAILED",
            "statusReason": "Host EC2 instance terminated (spot)",
            "container": {"exitCode": 137}
        }
    });
    let re = decode_batch(&ev, &ctx()).unwrap();
    assert_eq!(re.kind(), Kind::TaskFailed);
    // The test name promises exit + reason — actually pin them (and the derived category).
    match re.data {
        EventData::TaskFailed { exit_code, termination_category, status_reason, .. } => {
            assert_eq!(exit_code, 137);
            assert_eq!(termination_category, Some(TerminationCategory::Spot));
            assert_eq!(status_reason.as_deref(), Some("Host EC2 instance terminated (spot)"));
        }
        other => panic!("expected TaskFailed, got {other:?}"),
    }
}

#[test]
fn missing_job_id_is_malformed_not_panic() {
    let ev = serde_json::json!({
        "time": "2026-09-12T04:12:31Z",
        "detail": {"jobQueue": "q/x", "status": "RUNNING"}
    });
    assert!(matches!(decode_batch(&ev, &ctx()), Err(DecodeError::Malformed(_))));
}

#[test]
fn missing_time_is_malformed_not_epoch_zero() {
    // A missing/unparseable `time` must fail (→ DLQ), never silently stamp epoch 0 and mis-order.
    let ev = serde_json::json!({
        "detail": {"jobId": "job-1", "jobQueue": "q/x", "status": "SUCCEEDED", "container": {"exitCode": 0}}
    });
    assert!(matches!(decode_batch(&ev, &ctx()), Err(DecodeError::Malformed(_))));
}

#[test]
fn pre_1970_time_decodes_without_panicking() {
    // Regression: routing through Duration::from_secs_f64 panicked on negative epoch seconds,
    // which would poison the whole SQS batch. A pre-1970 time must decode to a negative instant.
    let ev = serde_json::json!({
        "time": "1969-01-01T00:00:00Z",
        "detail": {"jobId": "job-1", "jobQueue": "q/x", "status": "RUNNING"}
    });
    let re = decode_batch(&ev, &ctx()).expect("must not panic on pre-1970 time");
    assert!(re.ts.as_micros() < 0, "pre-1970 maps to a negative instant");
}

#[test]
fn spot_warning_decodes() {
    let ev = serde_json::json!({
        "time": "2026-09-12T04:12:31Z",
        "detail": {"instance-id": "i-0999"}
    });
    let re = decode_spot(&ev).unwrap();
    assert_eq!(re.kind(), Kind::SpotReclaimWarning);
    assert_eq!(re.instance_id.unwrap().as_str(), "i-0999");
}

#[test]
fn dedup_key_is_the_same_across_redeliveries_of_one_fact() {
    let ev = serde_json::json!({
        "id": "delivery-1",
        "time": "2026-09-12T04:12:31Z",
        "detail": {"jobId": "job-1", "jobQueue": "q/x", "status": "SUCCEEDED", "container": {"exitCode": 0}}
    });
    // EventBridge redelivers the same state change under a new envelope id and time, and the detail
    // may carry fields the first delivery did not: none of that may change the fact's identity.
    let redelivered = serde_json::json!({
        "id": "delivery-2",
        "time": "2026-09-12T04:13:05Z",
        "detail": {
            "jobId": "job-1", "jobQueue": "q/x", "status": "SUCCEEDED", "container": {"exitCode": 0},
            "attempts": [], "foo": "bar"
        }
    });
    let a = decode_batch(&ev, &ctx()).unwrap().dedup_key;
    let b = decode_batch(&redelivered, &ctx()).unwrap().dedup_key;
    assert_eq!(a, "aws-batch:job-1:SUCCEEDED");
    assert_eq!(a, b);
}

// ── requested resources (what the job asked Batch for) ───────────────────────────────────────

/// A "Batch Job State Change" envelope whose `detail` is a Snakemake task job exactly as
/// `aws batch describe-jobs` returned it live (SUCCEEDED on an arch-c7g queue; account
/// ids and the image/role names scrubbed, `environment`/`command` dropped), wrapped in a
/// hand-written EventBridge envelope. It is a DescribeJobs capture, not a captured `Batch Job State
/// Change` event: AWS documents the event's `detail` as the DescribeJobs job shape, but swap in a
/// real captured event once one is taken from the ingest queue. Like every job both engines'
/// executors submit, it names its request only in `container.resourceRequirements` (string values,
/// MEMORY in MiB): the legacy `vcpus`/`memory` keys are absent.
fn live_task_job_event() -> serde_json::Value {
    serde_json::json!({
        "version": "0",
        "detail-type": "Batch Job State Change",
        "source": "aws.batch",
        "account": "123456789012",
        "time": "2026-09-27T13:47:16Z",
        "region": "us-east-1",
        "detail": {
            "jobArn": "arn:aws:batch:us-east-1:123456789012:job/ff79389f-0efb-45e0-8400-e171ff59d113",
            "jobName": "snakejob-hello-02b74b57-58f4-47d9-82fb-bc64ea53947d",
            "jobId": "ff79389f-0efb-45e0-8400-e171ff59d113",
            "jobQueue": "arn:aws:batch:us-east-1:123456789012:job-queue/biffix-acme-arch-c7g",
            "status": "SUCCEEDED",
            "attempts": [{
                "container": {
                    "containerInstanceArn": "arn:aws:ecs:us-east-1:123456789012:container-instance/AWSBatch-CEarchc7g-abc/5722e7cb963c4ac1861fae158b6c4430",
                    "taskArn": "arn:aws:ecs:us-east-1:123456789012:task/AWSBatch-CEarchc7g-abc/fe153e5584414ad2983a37b4cd79085b",
                    "exitCode": 0,
                    "logStreamName": "snakejob-def-hello-02b74b57-58f4-47d9-82fb-bc64ea53947d/default/fe153e5584414ad2983a37b4cd79085b",
                    "networkInterfaces": []
                },
                "startedAt": 1790447231312_i64,
                "stoppedAt": 1790447236769_i64,
                "statusReason": "Essential container in task exited"
            }],
            "statusReason": "Essential container in task exited",
            "createdAt": 1790447108263_i64,
            "startedAt": 1790447231312_i64,
            "stoppedAt": 1790447236769_i64,
            "dependsOn": [],
            "jobDefinition": "arn:aws:batch:us-east-1:123456789012:job-definition/snakejob-def-hello-02b74b57-58f4-47d9-82fb-bc64ea53947d:1",
            "parameters": {},
            "container": {
                "image": "123456789012.dkr.ecr.us-east-1.amazonaws.com/coordinator@sha256:0f98598252831765b95113ec56477e13fd188b6c08caee19bbbf0df8a43feded",
                "jobRoleArn": "arn:aws:iam::123456789012:role/RunBaseRole",
                "volumes": [],
                "mountPoints": [],
                "ulimits": [],
                "privileged": true,
                "exitCode": 0,
                "containerInstanceArn": "arn:aws:ecs:us-east-1:123456789012:container-instance/AWSBatch-CEarchc7g-abc/5722e7cb963c4ac1861fae158b6c4430",
                "taskArn": "arn:aws:ecs:us-east-1:123456789012:task/AWSBatch-CEarchc7g-abc/fe153e5584414ad2983a37b4cd79085b",
                "logStreamName": "snakejob-def-hello-02b74b57-58f4-47d9-82fb-bc64ea53947d/default/fe153e5584414ad2983a37b4cd79085b",
                "networkInterfaces": [],
                "resourceRequirements": [
                    {"value": "1", "type": "VCPU"},
                    {"value": "1024", "type": "MEMORY"}
                ],
                "secrets": []
            },
            "platformCapabilities": ["EC2"],
            "eksAttempts": [],
            "isTerminated": true
        }
    })
}

/// [`live_task_job_event`] with its `detail.container` replaced.
fn task_job_with_container(container: serde_json::Value) -> serde_json::Value {
    let mut event = live_task_job_event();
    event["detail"]["container"] = container;
    event
}

#[test]
fn live_task_job_decodes_its_resource_requirements() {
    let re = decode_batch(&live_task_job_event(), &ctx()).unwrap();
    assert_eq!(re.kind(), Kind::TaskSucceeded);
    assert_eq!(re.requested_vcpus, Some(1.0));
    assert_eq!(re.requested_memory_mib, Some(1024));
    // The request is the job's, not the host's: the host is still identified separately.
    assert_eq!(re.instance_id.unwrap().as_str(), "5722e7cb963c4ac1861fae158b6c4430");
}

#[test]
fn queued_task_job_carries_its_request_before_it_has_a_host() {
    // RUNNABLE: no host yet, but the job definition's request is already in the event, so a task
    // waiting on capacity shows what it is waiting for.
    let mut event = task_job_with_container(serde_json::json!({
        "image": "img",
        "resourceRequirements": [
            {"type": "MEMORY", "value": "30000"},
            {"type": "VCPU", "value": "4"},
            {"type": "GPU", "value": "1"}
        ]
    }));
    event["detail"]["status"] = "RUNNABLE".into();
    let re = decode_batch(&event, &ctx()).unwrap();
    assert_eq!(re.kind(), Kind::TaskQueued);
    assert!(re.instance_id.is_none());
    assert_eq!(re.requested_vcpus, Some(4.0));
    assert_eq!(re.requested_memory_mib, Some(30_000));
}

#[test]
fn legacy_vcpus_and_memory_are_the_fallback() {
    let re = decode_batch(
        &task_job_with_container(serde_json::json!({"vcpus": 2, "memory": 4096})),
        &ctx(),
    )
    .unwrap();
    assert_eq!(re.requested_vcpus, Some(2.0));
    assert_eq!(re.requested_memory_mib, Some(4096));

    // Per resource: a requirement wins over its legacy field, and a missing one falls back.
    let re = decode_batch(
        &task_job_with_container(serde_json::json!({
            "vcpus": 8,
            "memory": 1,
            "resourceRequirements": [{"type": "MEMORY", "value": "2048"}]
        })),
        &ctx(),
    )
    .unwrap();
    assert_eq!(re.requested_vcpus, Some(8.0));
    assert_eq!(re.requested_memory_mib, Some(2048));
}

#[test]
fn fractional_vcpus_are_kept() {
    // Fargate-style quarter vCPU.
    let re = decode_batch(
        &task_job_with_container(serde_json::json!({
            "resourceRequirements": [
                {"type": "VCPU", "value": "0.25"},
                {"type": "MEMORY", "value": "512"}
            ]
        })),
        &ctx(),
    )
    .unwrap();
    assert_eq!(re.requested_vcpus, Some(0.25));
    assert_eq!(re.requested_memory_mib, Some(512));
}

#[test]
fn absent_or_unusable_requests_decode_as_unknown() {
    let unknown = |event: serde_json::Value| {
        let re = decode_batch(&event, &ctx()).unwrap();
        (re.requested_vcpus, re.requested_memory_mib)
    };
    // No container block at all (a multi-node or ECS-properties job keeps its containers
    // elsewhere): the event still decodes, the request is just unknown.
    let mut no_container = live_task_job_event();
    no_container["detail"].as_object_mut().unwrap().remove("container");
    no_container["detail"]["ecsProperties"] = serde_json::json!({
        "taskProperties": [{"containers": [{"name": "main"}]}]
    });
    assert_eq!(unknown(no_container), (None, None));
    // A container that names no request.
    assert_eq!(unknown(task_job_with_container(serde_json::json!({"image": "img"}))), (None, None));
    // Values that are not positive numbers, or the wrong JSON shape, are ignored, not guessed.
    assert_eq!(
        unknown(task_job_with_container(serde_json::json!({
            "resourceRequirements": [
                {"type": "VCPU", "value": "lots"},
                {"type": "MEMORY", "value": "0"},
                {"type": "MEMORY"},
                "VCPU"
            ],
            "vcpus": -1,
            "memory": null
        }))),
        (None, None)
    );
    assert_eq!(
        unknown(task_job_with_container(
            serde_json::json!({"resourceRequirements": {"VCPU": "4"}})
        )),
        (None, None)
    );
}

#[test]
fn numeric_requirement_values_are_read_not_dropped() {
    // Batch emits string values; a JSON number for the same fact is read, not silently dropped
    // (which would fall back to legacy fields modern jobs do not carry).
    let re = decode_batch(
        &task_job_with_container(serde_json::json!({
            "resourceRequirements": [
                {"type": "VCPU", "value": 4},
                {"type": "MEMORY", "value": 30000}
            ]
        })),
        &ctx(),
    )
    .unwrap();
    assert_eq!(re.requested_vcpus, Some(4.0));
    assert_eq!(re.requested_memory_mib, Some(30_000));
}

#[test]
fn an_unusable_requirement_falls_back_to_its_legacy_field() {
    let re = decode_batch(
        &task_job_with_container(serde_json::json!({
            "resourceRequirements": [
                {"type": "VCPU", "value": "lots"},
                {"type": "MEMORY", "value": "0"}
            ],
            "vcpus": 2,
            "memory": 4096
        })),
        &ctx(),
    )
    .unwrap();
    assert_eq!(re.requested_vcpus, Some(2.0));
    assert_eq!(re.requested_memory_mib, Some(4096));
}

// ── ECS container-instance state change (host facts) ─────────────────────────────────────────

/// An "ECS Container Instance State Change" envelope in the shape captured live from a
/// Batch-managed cluster (trimmed of `ecs.capability.*` attributes). `version` 1 is the
/// registration event, which ECS sends before the agent reports `versionInfo`.
fn ecs_host_event(version: i64) -> serde_json::Value {
    let version_info = if version == 1 {
        serde_json::json!({})
    } else {
        serde_json::json!({
            "agentHash": "cfabccdd",
            "agentVersion": "1.106.2",
            "dockerVersion": "DockerVersion: 25.0.16"
        })
    };
    serde_json::json!({
        "version": "0",
        "detail-type": "ECS Container Instance State Change",
        "source": "aws.ecs",
        "time": "2026-09-23T05:57:16Z",
        "detail": {
            "agentConnected": true,
            "attachments": [],
            "attributes": [
                {"name": "ecs.ami-id", "value": "ami-0d8e124066f7ed4f7"},
                {"name": "ecs.availability-zone", "value": "us-east-1b"},
                {"name": "ecs.cpu-architecture", "value": "x86_64"},
                {"name": "ecs.os-type", "value": "linux"},
                {"name": "ecs.os-family", "value": "LINUX"},
                {"name": "ecs.subnet-id", "value": "subnet-0fd0ff7018ea662af"},
                {"name": "ecs.instance-type", "value": "c6i.large"},
                {"name": "ecs.capability.privileged-container"}
            ],
            "clusterArn": "arn:aws:ecs:us-east-1:123:cluster/AWSBatch-CEcpuspot-abc",
            "containerInstanceArn":
                "arn:aws:ecs:us-east-1:123:container-instance/AWSBatch-CEcpuspot-abc/06bf7503ede4",
            "ec2InstanceId": "i-0715de4fdba56a059",
            "pendingTasksCount": 0,
            "registeredAt": "2026-09-23T05:57:16.173Z",
            "registeredResources": [
                {"name": "CPU", "type": "INTEGER", "integerValue": 2048},
                {"name": "MEMORY", "type": "INTEGER", "integerValue": 3787},
                {"name": "PORTS", "type": "STRINGSET", "stringSetValue": ["22", "2376"]},
                {"name": "PORTS_UDP", "type": "STRINGSET", "stringSetValue": []}
            ],
            "remainingResources": [],
            "runningTasksCount": 0,
            "status": "ACTIVE",
            "updatedAt": "2026-09-23T05:57:16.352Z",
            "version": version,
            "versionInfo": version_info
        }
    })
}

#[test]
fn ecs_host_event_decodes_ids_type_az_and_resources() {
    let host = decode_ecs_host(&ecs_host_event(2)).unwrap();
    // The same last-segment rule decode_batch applies, so attempts and hosts join on one key.
    assert_eq!(host.container_instance_id, "06bf7503ede4");
    assert_eq!(host.ec2_instance_id, "i-0715de4fdba56a059");
    assert!(host.cluster_arn.ends_with("cluster/AWSBatch-CEcpuspot-abc"));
    assert_eq!(host.instance_type.as_deref(), Some("c6i.large"));
    assert_eq!(host.availability_zone.as_deref(), Some("us-east-1b"));
    assert_eq!(host.cpu_architecture.as_deref(), Some("x86_64"));
    assert_eq!(host.os_family.as_deref(), Some("LINUX"));
    assert_eq!(host.ami_id.as_deref(), Some("ami-0d8e124066f7ed4f7"));
    assert_eq!(host.subnet_id.as_deref(), Some("subnet-0fd0ff7018ea662af"));
    assert_eq!(host.registered_vcpus, Some(2.0));
    assert_eq!(host.registered_memory_mib, Some(3787));
    assert_eq!(host.gpu_count, None);
    assert_eq!(host.agent_version.as_deref(), Some("1.106.2"));
    assert_eq!(host.docker_version.as_deref(), Some("DockerVersion: 25.0.16"));
    assert_eq!(host.detail_version, 2);
    assert_eq!(host.raw_detail["status"], "ACTIVE");
    // Source time is detail.updatedAt (sub-second), not the envelope's whole-second `time`.
    assert_eq!(host.observed_at_micros / 1_000, 1_790_143_036_352);
}

#[test]
fn ecs_registration_event_decodes_without_version_info() {
    let host = decode_ecs_host(&ecs_host_event(1)).unwrap();
    assert_eq!(host.instance_type.as_deref(), Some("c6i.large"));
    assert_eq!(host.agent_version, None);
    assert_eq!(host.docker_version, None);
}

#[test]
fn ecs_host_event_without_attributes_or_resources_decodes_with_none() {
    let mut ev = ecs_host_event(3);
    let detail = ev["detail"].as_object_mut().unwrap();
    detail.remove("attributes");
    detail.remove("registeredResources");
    let host = decode_ecs_host(&ev).unwrap();
    assert_eq!(host.instance_type, None);
    assert_eq!(host.availability_zone, None);
    assert_eq!(host.registered_vcpus, None);
    assert_eq!(host.registered_memory_mib, None);
}

#[test]
fn ecs_host_event_counts_registered_gpus() {
    let mut ev = ecs_host_event(2);
    ev["detail"]["registeredResources"].as_array_mut().unwrap().push(serde_json::json!({
        "name": "GPU", "type": "STRINGSET", "stringSetValue": ["GPU-a", "GPU-b"]
    }));
    assert_eq!(decode_ecs_host(&ev).unwrap().gpu_count, Some(2));
}

#[test]
fn ecs_host_event_missing_ec2_instance_id_is_malformed() {
    let mut ev = ecs_host_event(2);
    ev["detail"].as_object_mut().unwrap().remove("ec2InstanceId");
    assert!(matches!(decode_ecs_host(&ev), Err(DecodeError::Malformed(_))));
}

#[test]
fn a_job_that_never_started_keeps_the_container_reason() {
    let ev = serde_json::json!({
        "id": "e2", "source": "aws.batch", "detail-type": "Batch Job State Change",
        "time": "2026-05-24T12:00:00Z",
        "detail": {
            "jobId": "job-42", "status": "FAILED", "jobQueue": "arn:aws:batch:us-east-1:1:job-queue/worker",
            "statusReason": "Task failed to start", "stoppedAt": 1779624000000_i64,
            "container": {"exitCode": -1,
                          "reason": "CannotPullContainerError: manifest unknown: Requested image not found"}
        }
    });
    let re = decode_batch(&ev, &ctx()).unwrap();
    match re.data {
        EventData::TaskFailed { status_reason, .. } => assert_eq!(
            status_reason.as_deref(),
            Some("Task failed to start: CannotPullContainerError: manifest unknown: Requested image not found")
        ),
        other => panic!("expected TaskFailed, got {other:?}"),
    }
}

#[test]
fn an_unparseable_updated_at_falls_back_to_registered_at() {
    let mut ev = ecs_host_event(2);
    ev["detail"]["updatedAt"] = serde_json::json!("not a time");
    let host = decode_ecs_host(&ev).unwrap();
    // detail.registeredAt, 2026-09-23T05:57:16.173Z.
    assert_eq!(host.observed_at_micros / 1_000, 1_790_143_036_173);
}

#[test]
fn unparseable_host_times_fall_back_to_the_envelope_time() {
    let mut ev = ecs_host_event(2);
    ev["detail"]["updatedAt"] = serde_json::json!("2026-02-31T00:00:00Z");
    ev["detail"].as_object_mut().unwrap().remove("registeredAt");
    ev["time"] = serde_json::json!("2026-09-23T05:57:17Z");
    let host = decode_ecs_host(&ev).unwrap();
    assert_eq!(host.observed_at_micros, 1_790_143_037_000_000);
    // With no parseable time anywhere, the fact is malformed.
    ev["time"] = serde_json::json!("soon");
    assert!(matches!(decode_ecs_host(&ev), Err(DecodeError::Malformed(_))));
}

/// A Batch state change for `job-1` in `status`, with `attempts` finished attempts listed.
fn batch_with_attempts(status: &str, attempts: usize) -> serde_json::Value {
    let mut detail = serde_json::json!({
        "jobId": "job-1",
        "jobQueue": "arn:aws:batch:us-east-1:123:job-queue/biffix-dev-cpu-spot",
        "status": status,
        "startedAt": 1789186351123_i64,
        "stoppedAt": 1789186352123_i64,
        "container": {"exitCode": 1}
    });
    if attempts > 0 {
        detail["attempts"] =
            serde_json::Value::Array(vec![serde_json::json!({"exitCode": 1}); attempts]);
    }
    serde_json::json!({"time": "2026-09-12T04:12:31Z", "detail": detail})
}

#[test]
fn prior_attempts_keep_a_batch_retrys_edges_apart_but_terminals_collapse() {
    // The first attempt's RUNNING and a Batch-internal retry's RUNNING (same job id, one attempt
    // already finished) must get different keys, or the retry's edge is dropped as a duplicate.
    let first = decode_batch(&batch_with_attempts("RUNNING", 0), &ctx()).unwrap();
    let retry = decode_batch(&batch_with_attempts("RUNNING", 1), &ctx()).unwrap();
    assert_eq!(first.dedup_key, "aws-batch:job-1:RUNNING");
    assert_eq!(retry.dedup_key, "aws-batch:job-1:RUNNING:1");
    let runnable = decode_batch(&batch_with_attempts("RUNNABLE", 2), &ctx()).unwrap();
    assert_eq!(runnable.dedup_key, "aws-batch:job-1:RUNNABLE:2");
    // A terminal ignores the attempt count, so it collapses with the reconciled terminal.
    let failed = decode_batch(&batch_with_attempts("FAILED", 2), &ctx()).unwrap();
    assert_eq!(failed.dedup_key, "aws-batch:job-1:FAILED");
}
