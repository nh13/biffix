//! The AWS SDK side of decoding: what the reconciler reads from DescribeJobs (a job's start and
//! terminal) and DescribeInstances, mapped the way `biffix_core::eventbridge::decode` maps the
//! same facts in EventBridge JSON, so a reconciled fact and a live event record a job and host
//! identically.

#[cfg(feature = "aws")]
use biffix_core::eventbridge::decode::{
    container_instance_id, queue_name, requested_resources, QueueContext, RequestedResources,
};
#[cfg(feature = "aws")]
use biffix_core::eventbridge::describe::{
    prior_attempts_before, start_time_millis, terminal_time_millis, DescribedJobFacts,
    DescribedStartedJob, DescribedTerminalJob, TerminalStatus,
};
#[cfg(feature = "aws")]
use biffix_core::eventbridge::execution::{BatchJobSnapshot, ExecutionFacts};
#[cfg(feature = "aws")]
use biffix_core::wire::aws_batch::BatchStatus;

/// A DescribeJobs result's terminal fact, as the EventBridge decoder maps the same job's live
/// terminal, or `None` while the job is still active (or has no id). The job is a coordinator when
/// its queue is one of `queue_context`'s, or when the control plane bound it as one
/// (`bound_coordinator`: the fold decides by that binding, so a coordinator on a queue the deploy
/// does not list is still the coordinator).
#[cfg(feature = "aws")]
pub fn described_terminal(
    job: &aws_sdk_batch::types::JobDetail,
    queue_context: &QueueContext,
    bound_coordinator: bool,
    poll_millis: i64,
) -> Option<DescribedTerminalJob> {
    let status = match job.status().map(aws_sdk_batch::types::JobStatus::as_str) {
        Some("SUCCEEDED") => TerminalStatus::Succeeded,
        Some("FAILED") => TerminalStatus::Failed,
        _ => return None,
    };
    let batch_job_id = job.job_id()?;
    let container = job.container();
    Some(DescribedTerminalJob {
        batch_job_id: batch_job_id.to_owned(),
        status,
        stopped_at_millis: terminal_time_millis(
            job.stopped_at(),
            job.attempts().iter().map(aws_sdk_batch::types::AttemptDetail::stopped_at),
            poll_millis,
        ),
        exit_code: container.and_then(aws_sdk_batch::types::ContainerDetail::exit_code),
        status_reason: biffix_core::translate::batch_reason(
            job.status_reason(),
            container.and_then(|value| value.reason()),
        ),
        facts: described_facts(job, queue_context, bound_coordinator),
    })
}

/// The start a DescribeJobs result reports, as the EventBridge decoder maps the same job's live
/// RUNNING, or `None` when Batch reports none (not started yet, ended without running, no id).
#[cfg(feature = "aws")]
pub fn described_start(
    job: &aws_sdk_batch::types::JobDetail,
    queue_context: &QueueContext,
    bound_coordinator: bool,
) -> Option<DescribedStartedJob> {
    let batch_job_id = job.job_id()?;
    let attempts = job.attempts();
    let started_at_millis = start_time_millis(
        job.started_at(),
        attempts.iter().map(aws_sdk_batch::types::AttemptDetail::started_at),
    )?;
    let prior_attempts = prior_attempts_before(
        started_at_millis,
        attempts.iter().map(|attempt| (attempt.started_at(), attempt.stopped_at())),
    );
    Some(DescribedStartedJob {
        batch_job_id: batch_job_id.to_owned(),
        started_at_millis,
        prior_attempts,
        facts: described_facts(job, queue_context, bound_coordinator),
    })
}

/// What a start and a terminal read alike from one DescribeJobs result, read in one place so the
/// two cannot drift apart: `job`'s log stream, host (`container_instance_id` of its
/// container-instance ARN), queue name and request, as the EventBridge decoder reads them. The job
/// is a coordinator when its queue is one of `queue_context`'s, or when the control plane bound it
/// as one (`bound_coordinator`).
#[cfg(feature = "aws")]
fn described_facts(
    job: &aws_sdk_batch::types::JobDetail,
    queue_context: &QueueContext,
    bound_coordinator: bool,
) -> DescribedJobFacts {
    let container = job.container();
    let queue_arn = job.job_queue().unwrap_or_default();
    DescribedJobFacts {
        log_stream: container.and_then(|value| value.log_stream_name()).map(str::to_owned),
        instance_id: container
            .and_then(|value| value.container_instance_arn())
            .map(|arn| container_instance_id(arn).to_owned()),
        queue: job.job_queue().map(|arn| queue_name(arn).to_owned()),
        is_coordinator: bound_coordinator || queue_context.is_coordinator(queue_arn),
        requested: container.map(requested_resources_from_container).unwrap_or_default(),
    }
}

/// A DescribeJobs result as the snapshot `execution_events` builds its execution events from:
/// the same snapshot `batch_snapshot` reads from the same job's EventBridge `detail`, so a
/// reconciled execution and a live one are the same event. `None` when `job` has no id or no
/// status.
///
/// Like `batch_snapshot`, it stores what AWS sent (an empty ARN or queue, a zero time, a queue's
/// ARN rather than its name) and leaves what that means to the builder.
#[cfg(feature = "aws")]
pub fn described_snapshot(job: &aws_sdk_batch::types::JobDetail) -> Option<BatchJobSnapshot> {
    let owned = |value: Option<&str>| value.map(str::to_owned);
    let current =
        job.container().map_or_else(ExecutionFacts::default, |container| ExecutionFacts {
            task_arn: owned(container.task_arn()),
            container_instance_arn: owned(container.container_instance_arn()),
            log_stream: owned(container.log_stream_name()),
            exit_code: container.exit_code(),
            container_reason: owned(container.reason()),
            ..ExecutionFacts::default()
        });
    // Each `attempts[]` entry is one execution, with its own container block.
    let attempts = job.attempts().iter().map(|entry| {
        let container = entry.container();
        ExecutionFacts {
            task_arn: owned(container.and_then(|value| value.task_arn())),
            container_instance_arn: owned(
                container.and_then(|value| value.container_instance_arn()),
            ),
            log_stream: owned(container.and_then(|value| value.log_stream_name())),
            exit_code: container.and_then(aws_sdk_batch::types::AttemptContainerDetail::exit_code),
            container_reason: owned(container.and_then(|value| value.reason())),
            status_reason: owned(entry.status_reason()),
            started_at_millis: entry.started_at(),
            stopped_at_millis: entry.stopped_at(),
        }
    });
    Some(BatchJobSnapshot {
        batch_job_id: job.job_id()?.to_owned(),
        status: BatchStatus::from_wire(job.status()?.as_str()),
        queue: owned(job.job_queue()),
        started_at_millis: job.started_at(),
        stopped_at_millis: job.stopped_at(),
        status_reason: owned(job.status_reason()),
        current,
        attempts: attempts.collect(),
    })
}

/// [`requested_resources`] of a DescribeJobs `container` (the reconciler's view of the same block
/// the EventBridge event carries), so a reconciled start or terminal records the request the live
/// event would have.
#[cfg(feature = "aws")]
pub fn requested_resources_from_container(
    container: &aws_sdk_batch::types::ContainerDetail,
) -> RequestedResources {
    requested_resources(
        container
            .resource_requirements()
            .iter()
            .filter_map(|requirement| Some((requirement.r#type()?.as_str(), requirement.value()?))),
        container.vcpus().map(f64::from),
        container.memory().map(f64::from),
    )
}

/// The `host_registry.lifecycle` value for an EC2 instance's `InstanceLifecycle`. EC2 omits the
/// field for on-demand instances; any other value (`spot`, `capacity-block`, …) is kept, snake-cased.
/// Stored on the host's `host_registry` row beside its `HostFact`; the reconciler resolves it from
/// DescribeInstances.
pub fn host_lifecycle(instance_lifecycle: Option<&str>) -> String {
    match instance_lifecycle {
        None | Some("") => crate::config::Capacity::OnDemand.as_str().to_owned(),
        Some(value) => value.replace('-', "_"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_lifecycle_maps_absent_to_on_demand_and_keeps_others() {
        assert_eq!(host_lifecycle(None), "on_demand");
        assert_eq!(host_lifecycle(Some("")), "on_demand");
        assert_eq!(host_lifecycle(Some("spot")), "spot");
        assert_eq!(host_lifecycle(Some("capacity-block")), "capacity_block");
    }

    #[cfg(feature = "aws")]
    #[test]
    fn a_describe_jobs_terminal_maps_every_fact_the_event_carries() {
        use aws_sdk_batch::types::{AttemptDetail, ContainerDetail, JobDetail, JobStatus};
        let context = QueueContext { coordinator_queues: vec!["biffix-t-coordinator".into()] };
        let job = |status: JobStatus, queue: &str| {
            JobDetail::builder()
                .job_id("job-1")
                .status(status)
                .job_queue(format!("arn:aws:batch:us-east-1:1:job-queue/{queue}"))
                .status_reason("Essential container in task exited")
                .attempts(AttemptDetail::builder().stopped_at(7_000).build())
                .container(
                    ContainerDetail::builder()
                        .exit_code(137)
                        .reason("OutOfMemoryError: Container killed")
                        .log_stream_name("batch/default/abc")
                        .container_instance_arn(
                            "arn:aws:ecs:us-east-1:1:container-instance/cluster/ci-7",
                        )
                        .vcpus(2)
                        .memory(4096)
                        .build(),
                )
                .build()
        };
        assert_eq!(
            described_terminal(&job(JobStatus::Failed, "q-spot"), &context, false, 9_000),
            Some(DescribedTerminalJob {
                batch_job_id: "job-1".into(),
                status: TerminalStatus::Failed,
                // No job-level `stoppedAt`: the newest attempt's, never the poll time.
                stopped_at_millis: 7_000,
                exit_code: Some(137),
                status_reason: Some(
                    "Essential container in task exited: OutOfMemoryError: Container killed".into()
                ),
                facts: DescribedJobFacts {
                    log_stream: Some("batch/default/abc".into()),
                    instance_id: Some("ci-7".into()),
                    queue: Some("q-spot".into()),
                    is_coordinator: false,
                    requested: RequestedResources { vcpus: Some(2.0), memory_mib: Some(4096) },
                },
            })
        );
        let coordinator = |queue: &str, bound: bool| {
            described_terminal(&job(JobStatus::Succeeded, queue), &context, bound, 0)
                .unwrap()
                .facts
                .is_coordinator
        };
        // A listed coordinator queue, or the control plane's binding on any queue.
        assert!(coordinator("biffix-t-coordinator", false));
        assert!(coordinator("unlisted", true));
        assert!(!coordinator("unlisted", false));
        // Not terminal yet, or no id: nothing to report.
        assert_eq!(described_terminal(&job(JobStatus::Running, "q"), &context, false, 0), None);
        let no_id = JobDetail::builder().status(JobStatus::Succeeded).build();
        assert_eq!(described_terminal(&no_id, &context, false, 0), None);
    }

    #[cfg(feature = "aws")]
    #[test]
    fn a_describe_jobs_start_maps_every_fact_the_running_event_carries() {
        use aws_sdk_batch::types::{AttemptDetail, ContainerDetail, JobDetail, JobStatus};
        let context = QueueContext { coordinator_queues: vec!["biffix-t-coordinator".into()] };
        let container = || {
            ContainerDetail::builder()
                .log_stream_name("batch/default/abc")
                .container_instance_arn("arn:aws:ecs:us-east-1:1:container-instance/cluster/ci-7")
                .vcpus(2)
                .memory(4096)
                .build()
        };
        let job = |status: JobStatus, queue: &str| {
            JobDetail::builder()
                .job_id("job-1")
                .status(status)
                .job_queue(format!("arn:aws:batch:us-east-1:1:job-queue/{queue}"))
                .container(container())
        };
        let expected = DescribedStartedJob {
            batch_job_id: "job-1".into(),
            started_at_millis: 5_000,
            prior_attempts: 0,
            facts: DescribedJobFacts {
                log_stream: Some("batch/default/abc".into()),
                instance_id: Some("ci-7".into()),
                queue: Some("q-spot".into()),
                is_coordinator: false,
                requested: RequestedResources { vcpus: Some(2.0), memory_mib: Some(4096) },
            },
        };
        let attempt = |started: i64, stopped: i64| {
            AttemptDetail::builder().started_at(started).stopped_at(stopped).build()
        };

        // Running, first attempt: Batch lists no attempts yet.
        let running = job(JobStatus::Running, "q-spot").started_at(5_000).build();
        assert_eq!(described_start(&running, &context, false), Some(expected.clone()));
        // Finished: the attempt that started is listed, stopped after it started; not a prior one.
        let finished = job(JobStatus::Succeeded, "q-spot")
            .started_at(5_000)
            .stopped_at(7_000)
            .attempts(attempt(5_000, 7_000))
            .build();
        assert_eq!(described_start(&finished, &context, false), Some(expected.clone()));
        // A Batch retry, running: the first attempt stopped before it started.
        let retry = job(JobStatus::Running, "q-spot")
            .started_at(5_000)
            .attempts(attempt(1_000, 4_000))
            .build();
        assert_eq!(
            described_start(&retry, &context, false),
            Some(DescribedStartedJob { prior_attempts: 1, ..expected.clone() })
        );
        // A Batch retry, finished: both attempts are listed; only the earlier one is prior.
        let finished_retry = job(JobStatus::Succeeded, "q-spot")
            .started_at(5_000)
            .stopped_at(7_000)
            .attempts(attempt(1_000, 4_000))
            .attempts(attempt(5_000, 7_000))
            .build();
        assert_eq!(
            described_start(&finished_retry, &context, false),
            Some(DescribedStartedJob { prior_attempts: 1, ..expected.clone() })
        );
        // A retry after an attempt that never ran (no start, no stop): it counts, as the live
        // RUNNING's `len(attempts)` counts it.
        let after_unrun = job(JobStatus::Running, "q-spot")
            .started_at(5_000)
            .attempts(AttemptDetail::builder().build())
            .build();
        assert_eq!(
            described_start(&after_unrun, &context, false),
            Some(DescribedStartedJob { prior_attempts: 1, ..expected.clone() })
        );
        // No job-level start, but an attempt reports one.
        let from_attempt = job(JobStatus::Failed, "q-spot").attempts(attempt(5_000, 6_000)).build();
        assert_eq!(described_start(&from_attempt, &context, false), Some(expected.clone()));
        // Not started yet, or no id: nothing to report, and no time is made up.
        for status in [JobStatus::Runnable, JobStatus::Starting] {
            assert_eq!(described_start(&job(status, "q-spot").build(), &context, false), None);
        }
        let no_id = JobDetail::builder().status(JobStatus::Running).started_at(5_000).build();
        assert_eq!(described_start(&no_id, &context, false), None);
        // A listed coordinator queue, or the control plane's binding on any queue.
        let coordinator = |queue: &str, bound: bool| {
            let job = job(JobStatus::Running, queue).started_at(5_000).build();
            described_start(&job, &context, bound).unwrap().facts.is_coordinator
        };
        assert!(coordinator("biffix-t-coordinator", false));
        assert!(coordinator("unlisted", true));
        assert!(!coordinator("unlisted", false));
    }

    #[cfg(feature = "aws")]
    #[test]
    fn a_describe_jobs_container_maps_to_the_same_request_as_the_event() {
        use aws_sdk_batch::types::{ContainerDetail, ResourceRequirement, ResourceType};
        let requirement = |kind: ResourceType, value: &str| {
            ResourceRequirement::builder().r#type(kind).value(value).build()
        };
        let modern = ContainerDetail::builder()
            .resource_requirements(requirement(ResourceType::Gpu, "1"))
            .resource_requirements(requirement(ResourceType::Memory, "7680"))
            .resource_requirements(requirement(ResourceType::Vcpu, "2"))
            .build();
        assert_eq!(
            requested_resources_from_container(&modern),
            RequestedResources { vcpus: Some(2.0), memory_mib: Some(7680) }
        );
        // Legacy-only container: `vcpus`/`memory` (i32) are the request.
        let legacy = ContainerDetail::builder().vcpus(4).memory(8192).build();
        assert_eq!(
            requested_resources_from_container(&legacy),
            RequestedResources { vcpus: Some(4.0), memory_mib: Some(8192) }
        );
        // Per resource: a requirement wins over its legacy field; a missing one falls back.
        let mixed = ContainerDetail::builder()
            .resource_requirements(requirement(ResourceType::Memory, "2048"))
            .vcpus(8)
            .memory(1)
            .build();
        assert_eq!(
            requested_resources_from_container(&mixed),
            RequestedResources { vcpus: Some(8.0), memory_mib: Some(2048) }
        );
        assert_eq!(
            requested_resources_from_container(&ContainerDetail::builder().build()),
            RequestedResources::default()
        );
    }

    /// The retried job (`biffix_core::testing`) in `status`, built both ways from the same
    /// values: as the EventBridge `detail` JSON `retried_job_detail` gives, and as the SDK's
    /// `JobDetail` DescribeJobs returns. A RUNNING job is execution 2 still running: `attempts[]`
    /// lists only the lost execution, `container` is execution 2 with no exit code yet, and there
    /// is no job-level stop.
    #[cfg(feature = "aws")]
    fn retried(
        status: &str,
        lost_s: i64,
        gap_s: i64,
        run_s: i64,
    ) -> (serde_json::Value, aws_sdk_batch::types::JobDetail) {
        use aws_sdk_batch::types::{
            AttemptContainerDetail, AttemptDetail, ContainerDetail, JobDetail, JobStatus,
        };
        use biffix_core::testing::{
            execution_container_instance_arn, execution_log_stream, execution_task_arn,
            retried_job_detail, RETRIED_EXITED_REASON, RETRIED_JOB_ID, RETRIED_LOST_REASON,
            RETRIED_QUEUE_ARN,
        };
        let running = status == "RUNNING";
        let lost_at = 3_000 + lost_s * 1_000;
        let restarted_at = lost_at + gap_s * 1_000;
        let stopped_at = restarted_at + run_s * 1_000;

        let mut detail = retried_job_detail(status, lost_s, gap_s, run_s);
        if running {
            let object = detail.as_object_mut().unwrap();
            object.remove("stoppedAt");
            object["attempts"].as_array_mut().unwrap().truncate(1);
            object["container"].as_object_mut().unwrap().remove("exitCode");
        }

        let attempt_container = |n: u32, exit_code: Option<i32>| {
            AttemptContainerDetail::builder()
                .task_arn(execution_task_arn(n))
                .container_instance_arn(execution_container_instance_arn(n))
                .log_stream_name(execution_log_stream(n))
                .set_exit_code(exit_code)
                .build()
        };
        let lost = AttemptDetail::builder()
            .started_at(3_000)
            .stopped_at(lost_at)
            .status_reason(RETRIED_LOST_REASON)
            .container(attempt_container(1, None))
            .build();
        let finished = AttemptDetail::builder()
            .started_at(restarted_at)
            .stopped_at(stopped_at)
            .status_reason(RETRIED_EXITED_REASON)
            .container(attempt_container(2, Some(0)))
            .build();
        let job = JobDetail::builder()
            .job_id(RETRIED_JOB_ID)
            .job_queue(RETRIED_QUEUE_ARN)
            .status(JobStatus::from(status))
            .status_reason(RETRIED_EXITED_REASON)
            .started_at(restarted_at)
            .container(
                ContainerDetail::builder()
                    .task_arn(execution_task_arn(2))
                    .container_instance_arn(execution_container_instance_arn(2))
                    .log_stream_name(execution_log_stream(2))
                    .set_exit_code((!running).then_some(0))
                    .build(),
            )
            .attempts(lost);
        let job = if running { job } else { job.stopped_at(stopped_at).attempts(finished) };
        (detail, job.build())
    }

    #[cfg(feature = "aws")]
    #[test]
    fn a_described_job_and_its_live_event_read_into_one_snapshot() {
        use biffix_core::event::Kind;
        use biffix_core::eventbridge::decode::batch_snapshot;
        use biffix_core::eventbridge::execution::execution_events;
        use biffix_core::testing::{retried_job_snapshot, RETRIED_CASES};
        for (lost_s, gap_s, run_s) in RETRIED_CASES {
            let (detail, job) = retried("SUCCEEDED", lost_s, gap_s, run_s);
            let snapshot = described_snapshot(&job);
            assert!(snapshot.is_some(), "case {lost_s}/{gap_s}/{run_s}");
            assert_eq!(snapshot, batch_snapshot(&detail), "case {lost_s}/{gap_s}/{run_s}");
            let events = execution_events(snapshot.as_ref().unwrap());
            let kinds: Vec<_> = events.iter().map(|event| event.data.kind()).collect();
            assert_eq!(kinds, [Kind::BatchExecutionEnded, Kind::BatchExecutionEnded]);
        }
        // The first case is the retried case `biffix_core::testing` spells out.
        let (lost_s, gap_s, run_s) = RETRIED_CASES[0];
        let (_, job) = retried("SUCCEEDED", lost_s, gap_s, run_s);
        assert_eq!(described_snapshot(&job), Some(retried_job_snapshot()));
    }

    #[cfg(feature = "aws")]
    #[test]
    fn a_running_retry_reads_the_same_from_both_producers() {
        use biffix_core::eventbridge::decode::batch_snapshot;
        use biffix_core::eventbridge::execution::execution_events;
        use biffix_core::testing::{execution_task_arn, RETRIED_CASES};
        let (lost_s, gap_s, run_s) = RETRIED_CASES[0];
        let (detail, job) = retried("RUNNING", lost_s, gap_s, run_s);
        let snapshot = described_snapshot(&job);
        assert!(snapshot.is_some());
        assert_eq!(snapshot, batch_snapshot(&detail));
        let keys: Vec<_> = execution_events(snapshot.as_ref().unwrap())
            .into_iter()
            .map(|event| event.dedup_key)
            .collect();
        assert_eq!(
            keys,
            [
                format!("aws-batch-exec:{}:ended", execution_task_arn(1)),
                format!("aws-batch-exec:{}:started", execution_task_arn(2)),
            ]
        );
    }

    /// The fields the retried pair leaves at their usual values read the same both ways too: a
    /// container `reason` on one execution, a status this code does not know, and an empty queue.
    #[cfg(feature = "aws")]
    #[test]
    fn an_unknown_status_an_empty_queue_and_a_container_reason_read_the_same_both_ways() {
        use aws_sdk_batch::types::{
            AttemptContainerDetail, AttemptDetail, ContainerDetail, JobDetail, JobStatus,
        };
        use biffix_core::eventbridge::decode::batch_snapshot;
        use biffix_core::testing::{
            execution_container_instance_arn, execution_log_stream, execution_task_arn,
            RETRIED_EXITED_REASON, RETRIED_JOB_ID, RETRIED_LOST_REASON,
        };
        use biffix_core::wire::aws_batch::BatchStatus;
        const REASON: &str = "CannotInspectContainerError: Could not transition to inspecting";
        let detail = serde_json::json!({
            "jobId": RETRIED_JOB_ID,
            "jobQueue": "",
            "status": "FUTURE_STATE",
            "statusReason": RETRIED_EXITED_REASON,
            "startedAt": 831_000,
            "stoppedAt": 2_217_000,
            "container": {
                "taskArn": execution_task_arn(2),
                "containerInstanceArn": execution_container_instance_arn(2),
                "logStreamName": execution_log_stream(2),
                "exitCode": 0,
            },
            "attempts": [{
                "startedAt": 3_000,
                "stoppedAt": 684_000,
                "statusReason": RETRIED_LOST_REASON,
                "container": {
                    "taskArn": execution_task_arn(1),
                    "containerInstanceArn": execution_container_instance_arn(1),
                    "logStreamName": execution_log_stream(1),
                    "reason": REASON,
                },
            }],
        });
        let job = JobDetail::builder()
            .job_id(RETRIED_JOB_ID)
            .job_queue("")
            .status(JobStatus::from("FUTURE_STATE"))
            .status_reason(RETRIED_EXITED_REASON)
            .started_at(831_000)
            .stopped_at(2_217_000)
            .container(
                ContainerDetail::builder()
                    .task_arn(execution_task_arn(2))
                    .container_instance_arn(execution_container_instance_arn(2))
                    .log_stream_name(execution_log_stream(2))
                    .exit_code(0)
                    .build(),
            )
            .attempts(
                AttemptDetail::builder()
                    .started_at(3_000)
                    .stopped_at(684_000)
                    .status_reason(RETRIED_LOST_REASON)
                    .container(
                        AttemptContainerDetail::builder()
                            .task_arn(execution_task_arn(1))
                            .container_instance_arn(execution_container_instance_arn(1))
                            .log_stream_name(execution_log_stream(1))
                            .reason(REASON)
                            .build(),
                    )
                    .build(),
            )
            .build();
        let snapshot = described_snapshot(&job);
        assert_eq!(snapshot, batch_snapshot(&detail));
        // The pair exercises what it claims to: each reader kept the three values as AWS sent them.
        let snapshot = snapshot.unwrap();
        assert_eq!(snapshot.status, BatchStatus::Unknown("FUTURE_STATE".into()));
        assert!(!snapshot.status.is_terminal());
        assert_eq!(snapshot.queue.as_deref(), Some(""));
        assert_eq!(snapshot.attempts[0].container_reason.as_deref(), Some(REASON));
    }

    #[cfg(feature = "aws")]
    #[test]
    fn a_job_without_an_id_or_a_status_has_no_snapshot() {
        use aws_sdk_batch::types::{JobDetail, JobStatus};
        assert_eq!(
            described_snapshot(&JobDetail::builder().status(JobStatus::Succeeded).build()),
            None
        );
        assert_eq!(described_snapshot(&JobDetail::builder().job_id("j").build()), None);
    }
}
