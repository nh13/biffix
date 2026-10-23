//! The AWS SDK side of decoding: what the reconciler reads from DescribeJobs and
//! DescribeInstances, mapped the way `biffix_core::eventbridge::decode` maps the same facts in
//! EventBridge JSON, so a reconciled fact and a live event record a job and host identically.

#[cfg(feature = "aws")]
use biffix_core::eventbridge::decode::{
    container_instance_id, queue_name, requested_resources, QueueContext, RequestedResources,
};
#[cfg(feature = "aws")]
use biffix_core::eventbridge::describe::{
    terminal_time_millis, DescribedTerminalJob, TerminalStatus,
};

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
    let queue_arn = job.job_queue().unwrap_or_default();
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
        log_stream: container.and_then(|value| value.log_stream_name()).map(str::to_owned),
        instance_id: container
            .and_then(|value| value.container_instance_arn())
            .map(|arn| container_instance_id(arn).to_owned()),
        queue: job.job_queue().map(|arn| queue_name(arn).to_owned()),
        is_coordinator: bound_coordinator || queue_context.is_coordinator(queue_arn),
        requested: container.map(requested_resources_from_container).unwrap_or_default(),
    })
}

/// [`requested_resources`] of a DescribeJobs `container` (the reconciler's view of the same block
/// the EventBridge event carries), so a reconciled terminal records the request the live event
/// would have.
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
                log_stream: Some("batch/default/abc".into()),
                instance_id: Some("ci-7".into()),
                queue: Some("q-spot".into()),
                is_coordinator: false,
                requested: RequestedResources { vcpus: Some(2.0), memory_mib: Some(4096) },
            })
        );
        let coordinator = |queue: &str, bound: bool| {
            described_terminal(&job(JobStatus::Succeeded, queue), &context, bound, 0)
                .unwrap()
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
}
