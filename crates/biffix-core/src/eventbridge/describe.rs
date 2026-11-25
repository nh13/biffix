//! AWS Batch start and terminal facts as AWS Batch `DescribeJobs` reports them.
//!
//! EventBridge remains the primary lifecycle feed. The ingest reconciler's backstop describes
//! stale jobs and converts the start and terminal it finds into the exact same normalized
//! `RunEvent` shape as [`decode::decode_batch`](super::decode::decode_batch), including the same
//! **timestamp-independent** dedup key (keyed on `(job, status)` plus, for a retry's start, the
//! attempts before it; never the event time; see `dedup::aws_batch`), so a reconciled fact
//! collapses onto the live EventBridge one for the same job. The reconciler enqueues those events
//! on the ordinary ingest queue; it never writes the event store directly. Reading the AWS SDK's
//! `JobDetail` into a [`DescribedStartedJob`] or [`DescribedTerminalJob`] stays in the ingest
//! crate.

use super::decode::RequestedResources;
use crate::event::RunEvent;
use crate::ids::BatchJobId;
use crate::time::Timestamp;
use crate::translate::from_aws_batch;
use crate::wire::aws_batch::{AwsBatchEvent, BatchStatus};

/// What a DescribeJobs start and terminal both carry about the job, read alike from one
/// `JobDetail` (by the ingest crate) and mapped alike onto the event, so the two cannot drift.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DescribedJobFacts {
    pub log_stream: Option<String>,
    /// The host's container-instance id (`container_instance_id` of `containerInstanceArn`, from
    /// [`super::decode`]), as the EventBridge path records it, so a reconciled fact attributes the
    /// attempt's host too.
    pub instance_id: Option<String>,
    /// The job queue's name (`job_queue` from DescribeJobs, via [`super::decode::queue_name`]).
    pub queue: Option<String>,
    pub is_coordinator: bool,
    /// What the job requested (its container's resource requirements), as the EventBridge path
    /// records it ([`super::decode::requested_resources`]).
    pub requested: RequestedResources,
}

impl DescribedJobFacts {
    /// The wire event for `batch_job_id` reaching `status` at `ts_millis`, carrying these facts;
    /// the status-specific fields (exit code, reason, attempt count) are left empty for the
    /// caller to set.
    fn wire(&self, batch_job_id: &str, ts_millis: i64, status: BatchStatus) -> AwsBatchEvent {
        AwsBatchEvent {
            ts: Timestamp::from_epoch_millis(ts_millis),
            batch_job_id: BatchJobId::new(batch_job_id),
            status,
            exit_code: None,
            status_reason: None,
            log_stream: self.log_stream.clone(),
            instance_id: self.instance_id.as_deref().map(crate::ids::InstanceId::new),
            queue: self.queue.clone(),
            is_coordinator: self.is_coordinator,
            requested_vcpus: self.requested.vcpus,
            requested_memory_mib: self.requested.memory_mib,
            prior_attempts: None,
        }
    }
}

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
    pub facts: DescribedJobFacts,
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
    // A terminal's dedup key ignores the attempt count, so it stays unset.
    from_aws_batch(&AwsBatchEvent {
        exit_code: job.exit_code,
        status_reason: job.status_reason.clone(),
        ..job.facts.wire(&job.batch_job_id, job.stopped_at_millis, status)
    })
}

/// A start AWS Batch `DescribeJobs` reports for a job: its newest attempt reached RUNNING.
#[derive(Clone, Debug, PartialEq)]
pub struct DescribedStartedJob {
    pub batch_job_id: String,
    /// When the attempt started running, in epoch milliseconds ([`start_time_millis`]).
    pub started_at_millis: i64,
    /// The attempts Batch lists before this start ([`prior_attempts_before`]): what the live
    /// RUNNING state change carries as `len(detail.attempts)`, so both get one dedup key.
    pub prior_attempts: u32,
    pub facts: DescribedJobFacts,
}

/// Translate a `DescribeJobs` start through the shared AWS Batch translator, as the live RUNNING
/// state change: `task_started` (or `coordinator_running`) at `started_at_millis`.
pub fn started_run_event(job: &DescribedStartedJob) -> RunEvent {
    // A retry's RUNNING carries the attempts before it in its dedup key, as the live one does.
    from_aws_batch(&AwsBatchEvent {
        prior_attempts: Some(job.prior_attempts),
        ..job.facts.wire(&job.batch_job_id, job.started_at_millis, BatchStatus::Running)
    })
}

/// The job-level time, else the newest attempt's: the fallback a DescribeJobs start and terminal
/// time share.
fn job_else_newest_attempt<I: IntoIterator<Item = Option<i64>>>(
    job: Option<i64>,
    attempts: I,
) -> Option<i64> {
    job.or_else(|| attempts.into_iter().flatten().max())
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
    job_else_newest_attempt(stopped_at, attempt_stopped_at).unwrap_or(poll_millis)
}

/// The start DescribeJobs reports, in epoch milliseconds: the job's `startedAt`, else the newest
/// attempt's. `None` when Batch reports no start; a time is never made up for one.
pub fn start_time_millis<I: IntoIterator<Item = Option<i64>>>(
    started_at: Option<i64>,
    attempt_started_at: I,
) -> Option<i64> {
    job_else_newest_attempt(started_at, attempt_started_at)
}

/// How many of the listed `attempts` (each `(startedAt, stoppedAt)`) came before the attempt that
/// started at `started_at_millis`: the count the live RUNNING state change carries as
/// `len(detail.attempts)`, so a reconciled start and the live one get one dedup key.
///
/// Every listed attempt counts except the started attempt's own, which is any attempt that started
/// or stopped at or after `started_at_millis`. DescribeJobs lists that attempt once it has stopped
/// (a finished job) and may list it while it runs; the live RUNNING lists neither. An earlier
/// attempt counts whatever it carries, even one that never ran (no start, no stop), as the live
/// length counts it. No exact-millisecond match is needed.
///
/// This assumes `started_at_millis` (the job-level `startedAt`, from [`start_time_millis`]) is the
/// newest execution's start, as the live RUNNING's `startedAt` is: Batch overwrites it each time
/// it runs the job again. That was observed in DescribeJobs on three spot-reclaimed jobs once
/// they had finished (issue #75); while a retry is RUNNING it may still briefly be the lost
/// execution's, which [`super::execution`]'s rule 3 guards against. Were it an earlier
/// execution's, the start would count fewer attempts before it and take that execution's dedup
/// key, a duplicate of a start the log may already hold. Every execution is recorded in its own
/// right by [`super::execution`].
pub fn prior_attempts_before<I: IntoIterator<Item = (Option<i64>, Option<i64>)>>(
    started_at_millis: i64,
    attempts: I,
) -> u32 {
    let is_own = |time: Option<i64>| time.is_some_and(|time| time >= started_at_millis);
    let before =
        attempts.into_iter().filter(|&(started, stopped)| !is_own(started) && !is_own(stopped));
    u32::try_from(before.count()).unwrap_or(u32::MAX)
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
            facts: DescribedJobFacts {
                log_stream: Some("batch/job-1".into()),
                instance_id: Some("ci-1".into()),
                queue: None,
                is_coordinator: false,
                requested: Default::default(),
            },
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
            facts: DescribedJobFacts {
                log_stream: None,
                instance_id: None,
                queue: None,
                is_coordinator: true,
                requested: Default::default(),
            },
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
                                facts: DescribedJobFacts {
                                    log_stream: log_stream.map(str::to_owned),
                                    instance_id: None,
                                    queue: None,
                                    is_coordinator,
                                    requested: Default::default(),
                                },
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

    fn started(is_coordinator: bool) -> DescribedStartedJob {
        DescribedStartedJob {
            batch_job_id: "job-1".into(),
            started_at_millis: 1_789_186_351_123,
            prior_attempts: 0,
            facts: DescribedJobFacts {
                log_stream: Some("batch/job-1".into()),
                instance_id: Some("ci-1".into()),
                queue: Some("biffix-dev-cpu-spot".into()),
                is_coordinator,
                requested: RequestedResources { vcpus: Some(2.0), memory_mib: Some(7680) },
            },
        }
    }

    #[test]
    fn a_described_start_is_the_running_fact_at_its_start_time() {
        let event = started_run_event(&started(false));
        assert_eq!(event.source, EventSource::AwsBatch);
        assert_eq!(event.kind(), Kind::TaskStarted);
        assert_eq!(event.ts.as_micros(), 1_789_186_351_123_000);
        assert_eq!(event.batch_job_id.as_ref().unwrap().as_str(), "job-1");
        assert_eq!(event.instance_id.as_ref().unwrap().as_str(), "ci-1");
        assert_eq!(event.queue.as_deref(), Some("biffix-dev-cpu-spot"));
        assert_eq!(event.requested_vcpus, Some(2.0));
        assert_eq!(event.requested_memory_mib, Some(7680));
        assert_eq!(event.dedup_key, "aws-batch:job-1:RUNNING");
        match &event.data {
            EventData::TaskStarted { started_at, log_stream } => {
                assert_eq!(*started_at, Some(event.ts));
                assert_eq!(log_stream.as_deref(), Some("batch/job-1"));
            }
            other => panic!("expected task_started, got {other:?}"),
        }

        let coordinator = started_run_event(&started(true));
        assert_eq!(coordinator.kind(), Kind::CoordinatorRunning);
        assert_eq!(coordinator.ts, event.ts);
        assert_eq!(coordinator.instance_id, event.instance_id);
    }

    #[test]
    fn the_start_time_is_the_jobs_else_the_newest_attempts_else_none() {
        assert_eq!(start_time_millis(Some(5), [Some(9)]), Some(5));
        assert_eq!(start_time_millis(None, [Some(3), None, Some(7)]), Some(7));
        // Not started yet (SUBMITTED … STARTING), or ended without running: no start is made up.
        assert_eq!(start_time_millis(None, [None]), None);
        assert_eq!(start_time_millis(None, []), None);
    }

    #[test]
    fn prior_attempts_are_every_listed_attempt_but_the_started_ones_own() {
        // The first attempt, running: Batch lists no attempts yet.
        assert_eq!(prior_attempts_before(100, []), 0);
        // The first attempt, listed while it runs (no stop yet): its own start excludes it.
        assert_eq!(prior_attempts_before(100, [(Some(100), None)]), 0);
        // The first attempt, finished: it started (and stopped) at or after the start.
        assert_eq!(prior_attempts_before(100, [(Some(100), Some(200))]), 0);
        // A retry, running: the earlier attempt is listed, the running one is not.
        assert_eq!(prior_attempts_before(100, [(Some(10), Some(50))]), 1);
        // A retry, finished: the earlier attempt counts, its own does not.
        assert_eq!(prior_attempts_before(100, [(Some(10), Some(50)), (Some(100), Some(200))]), 1);
        // An earlier attempt that never ran (no start, no stop) counts, as the live
        // `len(detail.attempts)` counts it; so does one that ran and has no stop.
        assert_eq!(prior_attempts_before(100, [(None, None)]), 1);
        assert_eq!(prior_attempts_before(100, [(Some(10), None)]), 1);
        assert_eq!(prior_attempts_before(100, [(None, None), (Some(100), Some(200))]), 1);
        // The started attempt listed with no start of its own is still known by its stop.
        assert_eq!(prior_attempts_before(100, [(Some(10), Some(50)), (None, Some(200))]), 1);
    }
}
