//! A Batch job's executions, as events: the one builder every producer feeds.
//!
//! An **execution** is one AWS run of a Batch job on a host (one ECS task). Batch's own
//! `retryStrategy` runs a job again under the same job id, on another host, when an execution is
//! lost (a reclaimed spot instance), so one job, which is one engine attempt, can have several.
//! Each is recorded as its own immutable facts, `batch_execution_started` and
//! `batch_execution_ended`, identified by the execution's ECS task ARN and nothing else.
//!
//! Every reader of a Batch job (the EventBridge state change, [`super::decode::batch_snapshot`];
//! `DescribeJobs` in the ingest reconciler) fills a [`BatchJobSnapshot`] with what AWS said, raw,
//! and [`execution_events`] alone decides what that means. So a live and a reconciled sighting of
//! one execution share its dedup key, and are the same event only when both saw the same AWS
//! fields. The log's dedup is first-write-wins: it keeps whichever sighting arrived first, even a
//! thin one (an `attempts[]` entry with a task ARN and a stop but no start, host or reason), and a
//! fuller later sighting does not replace it.

use std::collections::{HashMap, HashSet};

use crate::dedup::{aws_batch_execution_ended, aws_batch_execution_started};
use crate::event::{EventData, RunEvent};
use crate::ids::{BatchJobId, InstanceId};
use crate::source::EventSource;
use crate::time::Timestamp;
use crate::translate::{batch_reason, category_from_reason};
use crate::wire::aws_batch::BatchStatus;

use super::decode::{container_instance_id, queue_name};

/// What AWS reports about one execution: an `attempts[]` entry, or the job-level `container`
/// block (the job's current or final execution).
///
/// A reader stores AWS's values as it got them (an empty string, a zero time); only
/// [`execution_events`] decides what counts as absent, so two readers cannot disagree.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ExecutionFacts {
    /// `container.taskArn`: the execution's identity. Without one the execution is not recorded.
    pub task_arn: Option<String>,
    /// `container.containerInstanceArn`: the host the execution ran on.
    pub container_instance_arn: Option<String>,
    /// `container.logStreamName`.
    pub log_stream: Option<String>,
    /// `container.exitCode`; absent when the container never exited by itself (a lost host).
    pub exit_code: Option<i32>,
    /// `container.reason`: the container's own account of why it stopped.
    pub container_reason: Option<String>,
    /// The entry's `statusReason`. Always `None` on [`BatchJobSnapshot::current`], whose reason is
    /// the job's ([`BatchJobSnapshot::status_reason`]).
    pub status_reason: Option<String>,
    /// The entry's `startedAt`, in epoch milliseconds. Always `None` on
    /// [`BatchJobSnapshot::current`], whose times are the job's.
    pub started_at_millis: Option<i64>,
    /// The entry's `stoppedAt`, in epoch milliseconds. Always `None` on
    /// [`BatchJobSnapshot::current`].
    pub stopped_at_millis: Option<i64>,
}

/// One Batch job as one reader saw it at one moment: everything [`execution_events`] needs, and
/// nothing reader-specific.
#[derive(Clone, Debug, PartialEq)]
pub struct BatchJobSnapshot {
    /// `jobId`.
    pub batch_job_id: String,
    /// `jobQueue`, as AWS gave it (an ARN or a name); the builder takes its name
    /// ([`super::decode::queue_name`]).
    pub queue: Option<String>,
    /// `status`.
    pub status: BatchStatus,
    /// The job-level `startedAt`, in epoch milliseconds: the current execution's start, unless it
    /// is stale (see [`execution_events`]).
    pub started_at_millis: Option<i64>,
    /// The job-level `stoppedAt`, in epoch milliseconds.
    pub stopped_at_millis: Option<i64>,
    /// The job-level `statusReason`.
    pub status_reason: Option<String>,
    /// The job-level `container` block: the job's current execution, or its final one.
    pub current: ExecutionFacts,
    /// `attempts[]`, in AWS's order: the executions Batch lists for the job.
    pub attempts: Vec<ExecutionFacts>,
}

impl BatchJobSnapshot {
    /// How many `attempts[]` entries name no task. Such an entry yields no event (no identity is
    /// made up for it), so the caller, which can log, warns with this count.
    pub fn entries_without_task_arn(&self) -> usize {
        self.attempts.iter().filter(|entry| present(&entry.task_arn).is_none()).count()
    }
}

/// The execution events `job` proves, sorted by `(ts, dedup_key)`. Pure and deterministic: the
/// same snapshot always yields the same events, whoever read it.
///
/// A time of zero or less and an empty string are absent. No time, identity or exit code is made
/// up where AWS gave none. In order:
///
/// 1. An `attempts[]` entry with a task ARN and a stop time is that execution's **end**, at the
///    stop time, with the entry's own start, reason, exit code, log stream and host.
/// 2. An entry with a task ARN and a start but no stop is that execution's **start**.
/// 3. A RUNNING job's `container` names its current execution, which `attempts[]` does not list
///    yet: its **start**, at the job-level start. Only when that start is later than every start
///    and stop `attempts[]` lists for another execution: after a retry the job-level start can
///    still be the lost execution's for a while, and that time is not the new execution's.
/// 4. A terminal job's `container` names its final execution. When no entry ended it (a job that
///    failed before `attempts[]` listed it, or a reader that saw only the earlier entries or an
///    entry not yet stopped), its **end**, at the job-level stop, with the job-level reason. Its
///    start is its entry's own when the entry has one, else the job-level start under rule 3's
///    guard, else absent; a host, log stream, exit code or container reason `container` omits is
///    the entry's. A start rule 2 made of that entry is dropped. A terminal job's only entry, when
///    it names no task and has the job's own start and stop, is that final execution, so it does
///    not make the job-level start stale.
///
/// An entry without a task ARN yields nothing, and neither does a second entry for an execution
/// an earlier entry already yielded an event for. No snapshot yields both the start and the end
/// of one execution: the end says everything the start does, and the fold needs no start.
pub fn execution_events(job: &BatchJobSnapshot) -> Vec<RunEvent> {
    let mut events = Vec::new();
    // The executions an entry started (with that start), and those an entry ended.
    let mut started_by_entry = HashMap::new();
    let mut ended_by_entry = HashSet::new();

    for entry in &job.attempts {
        let Some(task_arn) = present(&entry.task_arn) else { continue };
        if started_by_entry.contains_key(task_arn) || ended_by_entry.contains(task_arn) {
            continue;
        }
        let started = present_time(entry.started_at_millis);
        match (started, present_time(entry.stopped_at_millis)) {
            (_, Some(stopped)) => {
                let reason = entry.status_reason.as_deref();
                events.push(ended_event(job, task_arn, entry, started, stopped, reason));
                ended_by_entry.insert(task_arn);
            }
            (Some(started), None) => {
                events.push(started_event(job, task_arn, entry, started));
                started_by_entry.insert(task_arn, started);
            }
            (None, None) => {}
        }
    }

    if let Some(task_arn) = present(&job.current.task_arn) {
        // The job-level start, when it is this execution's: later than every start and stop
        // `attempts[]` lists for another execution. Otherwise it is still an earlier execution's.
        let own_start = present_time(job.started_at_millis).filter(|&started| {
            job.attempts
                .iter()
                .filter(|entry| present(&entry.task_arn) != Some(task_arn))
                .filter(|entry| !is_sole_final_entry(job, entry))
                .flat_map(|entry| [entry.started_at_millis, entry.stopped_at_millis])
                .filter_map(present_time)
                .all(|other| started > other)
        });
        let entry_start = started_by_entry.get(task_arn).copied();
        let ended = ended_by_entry.contains(task_arn);
        if job.status == BatchStatus::Running {
            if let Some(started) = own_start.filter(|_| entry_start.is_none() && !ended) {
                events.push(started_event(job, task_arn, &job.current, started));
            }
        } else if job.status.is_terminal() && !ended {
            if let Some(stopped) = present_time(job.stopped_at_millis) {
                // The end stands for the execution: a start its entry yielded above is dropped,
                // and its time is carried on the end instead.
                let start_key = aws_batch_execution_started(task_arn);
                events.retain(|event| event.dedup_key != start_key);
                let started = entry_start.or(own_start);
                let reason = job.status_reason.as_deref();
                let facts = with_entry_facts(&job.current, job, task_arn);
                events.push(ended_event(job, task_arn, &facts, started, stopped, reason));
            }
        }
    }

    events.sort_by(|a, b| (a.ts, &a.dedup_key).cmp(&(b.ts, &b.dedup_key)));
    events
}

/// Whether `entry` is a terminal job's only `attempts[]` entry, naming no task, with the job's
/// own start and stop: then it is the execution the job-level `container` names (`container` is
/// optional on an entry), not another one, so it does not make the job-level start stale. Never
/// for a job that is not terminal: a RUNNING retry's stale start equals its lost entry's.
/// `described_start` in the ingest crate applies the same test to a DescribeJobs result.
fn is_sole_final_entry(job: &BatchJobSnapshot, entry: &ExecutionFacts) -> bool {
    job.status.is_terminal()
        && job.attempts.len() == 1
        && present(&entry.task_arn).is_none()
        && present_time(entry.started_at_millis).is_some()
        && entry.started_at_millis == job.started_at_millis
        && present_time(entry.stopped_at_millis).is_some()
        && entry.stopped_at_millis == job.stopped_at_millis
}

/// `current` (the job-level `container` of execution `task_arn`) with what it omits taken from
/// that execution's own `attempts[]` entry, when one is listed: each container field is optional,
/// and an end built from `current` alone would drop a host or log stream the entry named.
fn with_entry_facts(
    current: &ExecutionFacts,
    job: &BatchJobSnapshot,
    task_arn: &str,
) -> ExecutionFacts {
    let Some(entry) = job.attempts.iter().find(|entry| present(&entry.task_arn) == Some(task_arn))
    else {
        return current.clone();
    };
    let or_entry = |own: &Option<String>, entry: &Option<String>| {
        present(own).or(present(entry)).map(String::from)
    };
    ExecutionFacts {
        container_instance_arn: or_entry(
            &current.container_instance_arn,
            &entry.container_instance_arn,
        ),
        log_stream: or_entry(&current.log_stream, &entry.log_stream),
        exit_code: current.exit_code.or(entry.exit_code),
        container_reason: or_entry(&current.container_reason, &entry.container_reason),
        ..current.clone()
    }
}

/// A string AWS gave: `None` for a missing or empty one.
fn present(value: &Option<String>) -> Option<&str> {
    value.as_deref().filter(|value| !value.is_empty())
}

/// A time AWS gave: `None` for a missing one and for zero or less, which AWS writes for "not yet".
fn present_time(millis: Option<i64>) -> Option<i64> {
    millis.filter(|&millis| millis > 0)
}

/// Execution `task_arn`'s start at `started_at_millis`, with the log stream and host `facts` name.
fn started_event(
    job: &BatchJobSnapshot,
    task_arn: &str,
    facts: &ExecutionFacts,
    started_at_millis: i64,
) -> RunEvent {
    let started_at = Timestamp::from_epoch_millis(started_at_millis);
    let data = EventData::BatchExecutionStarted {
        execution_id: task_arn.to_owned(),
        started_at,
        log_stream: present(&facts.log_stream).map(String::from),
    };
    envelope(job, facts, aws_batch_execution_started(task_arn), started_at, data)
}

/// Execution `task_arn`'s end at `stopped_at_millis`, with the exit code, container reason, log
/// stream and host `facts` name. The start and `status_reason` are the caller's, since they are
/// the entry's for a listed execution and the job's for the one its `container` names.
fn ended_event(
    job: &BatchJobSnapshot,
    task_arn: &str,
    facts: &ExecutionFacts,
    started_at_millis: Option<i64>,
    stopped_at_millis: i64,
    status_reason: Option<&str>,
) -> RunEvent {
    let stopped_at = Timestamp::from_epoch_millis(stopped_at_millis);
    let status_reason = batch_reason(status_reason, facts.container_reason.as_deref());
    let data = EventData::BatchExecutionEnded {
        execution_id: task_arn.to_owned(),
        started_at: started_at_millis.map(Timestamp::from_epoch_millis),
        stopped_at,
        exit_code: facts.exit_code,
        termination_category: category_from_reason(status_reason.as_deref()),
        status_reason,
        log_stream: present(&facts.log_stream).map(String::from),
    };
    envelope(job, facts, aws_batch_execution_ended(task_arn), stopped_at, data)
}

/// An execution event's envelope: about the job, on the host `facts` names and the job's queue,
/// at the execution's own time `ts`. `run_id`, `task_key` and `attempt` stay unset: an execution
/// is attributed through its job, and is not an engine attempt.
fn envelope(
    job: &BatchJobSnapshot,
    facts: &ExecutionFacts,
    dedup_key: String,
    ts: Timestamp,
    data: EventData,
) -> RunEvent {
    let host = present(&facts.container_instance_arn)
        .map(container_instance_id)
        .filter(|id| !id.is_empty());
    RunEvent {
        batch_job_id: Some(BatchJobId::new(job.batch_job_id.as_str())),
        instance_id: host.map(InstanceId::new),
        queue: present(&job.queue)
            .map(queue_name)
            .filter(|name| !name.is_empty())
            .map(String::from),
        ..RunEvent::new(dedup_key, ts, EventSource::AwsBatch, data)
    }
}
