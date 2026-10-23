//! The same AWS Batch terminal facts as AWS Batch `DescribeJobs` reports them.
//!
//! EventBridge remains the primary lifecycle feed. The ingest reconciler's backstop describes
//! stale jobs and converts their terminal facts into the exact same normalized `RunEvent` shape
//! as [`decode::decode_batch`](super::decode::decode_batch), including the same
//! **timestamp-independent** dedup key (keyed on `(job, status)`, not the event time — see
//! `dedup::aws_batch`), so a reconciled terminal collapses onto the live EventBridge one for the
//! same job. The reconciler enqueues that event on the ordinary ingest queue; it never writes the
//! event store directly. Reading the AWS SDK's `JobDetail` into a [`DescribedTerminalJob`] stays in
//! the ingest crate.

use super::decode::RequestedResources;
use crate::event::RunEvent;
use crate::ids::BatchJobId;
use crate::time::Timestamp;
use crate::translate::from_aws_batch;
use crate::wire::aws_batch::{AwsBatchEvent, BatchStatus};

/// A terminal AWS Batch fact returned by `DescribeJobs`.
#[derive(Clone, Debug, PartialEq)]
pub struct DescribedTerminalJob {
    pub batch_job_id: String,
    pub status: TerminalStatus,
    /// When the job reached its terminal state, in epoch milliseconds: AWS `stoppedAt`, or the
    /// fallback [`terminal_time_millis`] picks when Batch reports none. It is only the event's
    /// source time; the dedup key ignores it (it is keyed on `(job, status)`).
    pub stopped_at_millis: i64,
    pub exit_code: Option<i32>,
    pub status_reason: Option<String>,
    pub log_stream: Option<String>,
    /// The host's container-instance id (`container_instance_id` of `containerInstanceArn`, from
    /// [`super::decode`]), as the EventBridge path records it, so a reconciled terminal attributes
    /// the attempt's host too.
    pub instance_id: Option<String>,
    /// The job queue's name (`job_queue` from DescribeJobs, via [`super::decode::queue_name`]).
    pub queue: Option<String>,
    pub is_coordinator: bool,
    /// What the job requested (its container's resource requirements), as the EventBridge path
    /// records it ([`super::decode::requested_resources`]).
    pub requested: RequestedResources,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TerminalStatus {
    Succeeded,
    Failed,
}

/// Translate a `DescribeJobs` terminal result through the shared AWS Batch translator.
pub fn terminal_run_event(job: &DescribedTerminalJob) -> RunEvent {
    let status = match job.status {
        TerminalStatus::Succeeded => BatchStatus::Succeeded,
        TerminalStatus::Failed => BatchStatus::Failed,
    };
    let wire = AwsBatchEvent {
        ts: Timestamp::from_epoch_millis(job.stopped_at_millis),
        batch_job_id: BatchJobId::new(&job.batch_job_id),
        status,
        exit_code: job.exit_code,
        status_reason: job.status_reason.clone(),
        log_stream: job.log_stream.clone(),
        instance_id: job.instance_id.as_deref().map(crate::ids::InstanceId::new),
        queue: job.queue.clone(),
        is_coordinator: job.is_coordinator,
        requested_vcpus: job.requested.vcpus,
        requested_memory_mib: job.requested.memory_mib,
        // Reconciliation only ever sees terminals, whose dedup key ignores the attempt count.
        prior_attempts: None,
    };
    from_aws_batch(&wire)
}

/// The source time of a terminal DescribeJobs result, in epoch milliseconds.
///
/// AWS documents `stoppedAt` as the moment the job went from RUNNING to a terminal state, so a job
/// that failed or was cancelled before it ran (from SUBMITTED/RUNNABLE) has none — and never gets
/// one, so waiting for it would defer that terminal forever. The fallback is the latest attempt's
/// `stoppedAt` (a retried job's earlier attempt may have run), else `poll_millis`, the moment the
/// reconciler saw the job terminal. `createdAt`/`startedAt` are not used: they precede the job's
/// own lifecycle facts in the log, and a terminal sorted before them would fold out of order. The
/// dedup key ignores the time, so a later poll's different fallback collapses onto the same event.
pub fn terminal_time_millis(
    stopped_at: Option<i64>,
    attempt_stopped_at: impl IntoIterator<Item = Option<i64>>,
    poll_millis: i64,
) -> i64 {
    stopped_at.or_else(|| attempt_stopped_at.into_iter().flatten().max()).unwrap_or(poll_millis)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::EventData;
    use crate::event::Kind;
    use crate::source::EventSource;

    #[test]
    fn failed_describe_result_preserves_terminal_details() {
        let event = terminal_run_event(&DescribedTerminalJob {
            batch_job_id: "job-1".into(),
            status: TerminalStatus::Failed,
            stopped_at_millis: 1_789_186_351_123,
            exit_code: Some(137),
            status_reason: Some("Host EC2 instance terminated (spot)".into()),
            log_stream: Some("batch/job-1".into()),
            instance_id: Some("ci-1".into()),
            queue: None,
            is_coordinator: false,
            requested: Default::default(),
        });

        assert_eq!(event.source, EventSource::AwsBatch);
        assert_eq!(event.kind(), Kind::TaskFailed);
        assert_eq!(event.ts.as_micros(), 1_789_186_351_123_000);
        assert_eq!(event.batch_job_id.unwrap().as_str(), "job-1");
        assert_eq!(event.instance_id.unwrap().as_str(), "ci-1");
        match event.data {
            EventData::TaskFailed { exit_code, status_reason, .. } => {
                assert_eq!(exit_code, 137);
                assert_eq!(status_reason.as_deref(), Some("Host EC2 instance terminated (spot)"));
            }
            other => panic!("expected task_failed, got {other:?}"),
        }
    }

    #[test]
    fn coordinator_result_uses_coordinator_terminal_kind() {
        let event = terminal_run_event(&DescribedTerminalJob {
            batch_job_id: "coord-1".into(),
            status: TerminalStatus::Succeeded,
            stopped_at_millis: 1_000,
            exit_code: Some(0),
            status_reason: None,
            log_stream: None,
            instance_id: None,
            queue: None,
            is_coordinator: true,
            requested: Default::default(),
        });
        assert_eq!(event.kind(), Kind::CoordinatorSucceeded);
    }

    #[test]
    fn property_repeated_reconciliation_is_deterministic_for_all_terminal_shapes() {
        // Property: for every terminal status and representative optional-field combination,
        // arbitrarily many reconciliation deliveries produce byte-identical events/dedup keys.
        // This is the D11 idempotency invariant; the unique run_events index makes repeats no-op.
        let statuses = [TerminalStatus::Succeeded, TerminalStatus::Failed];
        let exits = [None, Some(0), Some(1), Some(137)];
        let reasons = [None, Some("application error"), Some("spot interruption")];
        let logs = [None, Some("batch/log")];
        for status in statuses {
            for exit_code in exits {
                for status_reason in reasons {
                    for log_stream in logs {
                        for is_coordinator in [false, true] {
                            let job = DescribedTerminalJob {
                                batch_job_id: "job-property".into(),
                                status,
                                stopped_at_millis: 1_789_186_351_123,
                                exit_code,
                                status_reason: status_reason.map(str::to_owned),
                                log_stream: log_stream.map(str::to_owned),
                                instance_id: None,
                                queue: None,
                                is_coordinator,
                                requested: Default::default(),
                            };
                            let first = terminal_run_event(&job);
                            for _ in 0..5 {
                                assert_eq!(terminal_run_event(&job), first);
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn a_terminal_without_stopped_at_falls_back_to_an_attempt_then_the_poll() {
        assert_eq!(terminal_time_millis(Some(5), [Some(9)], 100), 5);
        assert_eq!(terminal_time_millis(None, [Some(3), None, Some(7)], 100), 7);
        // Cancelled from RUNNABLE: it never ran, so no stoppedAt ever appears; not deferred.
        assert_eq!(terminal_time_millis(None, [None], 100), 100);
        assert_eq!(terminal_time_millis(None, [], 100), 100);
    }
}
