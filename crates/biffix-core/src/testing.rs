//! Test builders for the contract types that tests construct everywhere, so a test names only the
//! fields its case is about and a new field gets its default here once, instead of in a struct
//! literal in every test that builds one. Also the one Batch job the execution-event tests of
//! every crate share (`RETRIED_*`, [`retried_job_detail`], [`retried_job_snapshot`]), and the
//! builders of one execution's events ([`execution_started`], [`execution_ended`]).
//!
//! Test tooling, not library API: compiled only for this crate's tests and behind the
//! `test-support` feature, which only the workspace's dev-dependencies turn on.

use crate::dedup::{aws_batch_execution_ended, aws_batch_execution_started};
use crate::event::{EventData, LogSeq, RunEvent, RunSubmitted, TerminationCategory};
use crate::eventbridge::execution::{BatchJobSnapshot, ExecutionFacts};
use crate::ids::{Attempt, BatchJobId, InstanceId, RunId, TaskKey};
use crate::source::EventSource;
use crate::time::Timestamp;
use crate::wire::aws_batch::BatchStatus;

/// A normalized event at `ts` (epoch seconds) with every correlation field unset, as the
/// production constructors start from. Set the fields a case needs with [`RunEventExt`].
pub fn event(
    ts: f64,
    source: EventSource,
    dedup_key: impl Into<String>,
    data: EventData,
) -> RunEvent {
    RunEvent::new(dedup_key.into(), Timestamp::from_epoch_seconds(ts), source, data)
}

/// A `run_submitted` anchor with placeholder required fields (version `wf`, sha `abc`, image
/// `sha256:x`, no params or inputs) and every optional field unset. Override what a case checks
/// with struct-update syntax: `RunSubmitted { run_name: Some(..), ..anchor() }`.
pub fn anchor() -> RunSubmitted {
    RunSubmitted::new("wf", "abc", "sha256:x", serde_json::json!({}), vec![])
}

/// Builder-style setters for a test event's correlation fields:
/// `event(..).with_run("r1").with_task("align").with_attempt(1).with_job("j1")`.
pub trait RunEventExt: Sized {
    /// Attributed to run `run`.
    fn with_run(self, run: &str) -> Self;
    /// Keyed on task `key`.
    fn with_task(self, key: &str) -> Self;
    /// Attempt number `n`.
    fn with_attempt(self, n: u32) -> Self;
    /// About Batch job `job`.
    fn with_job(self, job: &str) -> Self;
    /// Reported from the (container-instance) host `host`.
    fn with_instance(self, host: &str) -> Self;
    /// Run on the Batch queue `queue`.
    fn with_queue(self, queue: &str) -> Self;
    /// At log position `seq`.
    fn with_seq(self, seq: i64) -> Self;
}

impl RunEventExt for RunEvent {
    fn with_run(mut self, run: &str) -> Self {
        self.run_id = Some(RunId::new(run));
        self
    }

    fn with_task(mut self, key: &str) -> Self {
        self.task_key = Some(TaskKey::new(key));
        self
    }

    fn with_attempt(mut self, n: u32) -> Self {
        self.attempt = Some(Attempt(n));
        self
    }

    fn with_job(mut self, job: &str) -> Self {
        self.batch_job_id = Some(BatchJobId::new(job));
        self
    }

    fn with_instance(mut self, host: &str) -> Self {
        self.instance_id = Some(InstanceId::new(host));
        self
    }

    fn with_queue(mut self, queue: &str) -> Self {
        self.queue = Some(queue.into());
        self
    }

    fn with_seq(mut self, seq: i64) -> Self {
        self.seq = Some(LogSeq(seq));
        self
    }
}

/// The Batch job of the **retried case**: one job (one engine attempt) that AWS ran twice, the
/// first execution lost with its reclaimed host and the second finishing. Every test of execution
/// events shares these values, so the EventBridge and DescribeJobs paths are checked on one job.
pub const RETRIED_JOB_ID: &str = "job-retried";

/// The name of the queue the retried job ran on.
pub const RETRIED_QUEUE: &str = "biffix-dev-cpu-spot";

/// [`RETRIED_QUEUE`]'s ARN, as AWS names a job's queue.
pub const RETRIED_QUEUE_ARN: &str =
    "arn:aws:batch:us-east-1:123456789012:job-queue/biffix-dev-cpu-spot";

/// The `statusReason` Batch gives an execution lost with its host; it categorizes as
/// `host_terminated`.
pub const RETRIED_LOST_REASON: &str = "Host EC2 (instance i-0123456789abcdef0) terminated.";

/// The `statusReason` Batch gives an execution whose container exited by itself.
pub const RETRIED_EXITED_REASON: &str = "Essential container in task exited";

/// Retried jobs as `(lost_s, gap_s, run_s)`: how long the lost execution ran, how long the job
/// waited for its next host, and how long the second execution ran. The first is the retried
/// case [`retried_job_snapshot`] spells out; the gaps are measured ones, the rest made up.
pub const RETRIED_CASES: [(i64, i64, i64); 3] =
    [(681, 147, 1386), (95, 111, 640), (410, 132, 2210)];

/// The ECS task ARN of the retried job's execution `n`: the execution's identity.
pub fn execution_task_arn(n: u32) -> String {
    format!("arn:aws:ecs:us-east-1:123456789012:task/AWSBatch-c/t{n}")
}

/// The container-instance ARN of the host execution `n` ran on; its id is `ci{n}`.
pub fn execution_container_instance_arn(n: u32) -> String {
    format!("arn:aws:ecs:us-east-1:123456789012:container-instance/AWSBatch-c/ci{n}")
}

/// Execution `n`'s CloudWatch log stream.
pub fn execution_log_stream(n: u32) -> String {
    format!("job-def/default/t{n}")
}

/// The `detail` of a Batch job state change for the retried job in `status`, shaped as
/// DescribeJobs returns a reclaimed-and-retried job: execution 1 starts at 3 s, runs `lost_s`
/// seconds and is lost with its host (no exit code); execution 2 starts `gap_s` seconds later,
/// runs `run_s` seconds and exits 0. The job level repeats execution 2: its `container`, its
/// times and its reason.
pub fn retried_job_detail(status: &str, lost_s: i64, gap_s: i64, run_s: i64) -> serde_json::Value {
    let lost_at = 3_000 + lost_s * 1_000;
    let restarted_at = lost_at + gap_s * 1_000;
    let stopped_at = restarted_at + run_s * 1_000;
    let container = |n: u32, exit_code: Option<i32>| {
        serde_json::json!({
            "taskArn": execution_task_arn(n),
            "containerInstanceArn": execution_container_instance_arn(n),
            "logStreamName": execution_log_stream(n),
            "exitCode": exit_code,
        })
    };
    serde_json::json!({
        "jobId": RETRIED_JOB_ID,
        "jobQueue": RETRIED_QUEUE_ARN,
        "status": status,
        "statusReason": RETRIED_EXITED_REASON,
        "startedAt": restarted_at,
        "stoppedAt": stopped_at,
        "container": container(2, Some(0)),
        "attempts": [
            {
                "startedAt": 3_000,
                "stoppedAt": lost_at,
                "statusReason": RETRIED_LOST_REASON,
                "container": container(1, None),
            },
            {
                "startedAt": restarted_at,
                "stoppedAt": stopped_at,
                "statusReason": RETRIED_EXITED_REASON,
                "container": container(2, Some(0)),
            },
        ],
    })
}

/// The retried case as a finished job's snapshot, written out by hand: execution 1 ran from 3 s
/// to 684 s and was lost; execution 2 ran from 831 s to 2217 s and exited 0. What a reader makes
/// of `retried_job_detail("SUCCEEDED", 681, 147, 1386)`.
pub fn retried_job_snapshot() -> BatchJobSnapshot {
    let container = |n: u32, exit_code: Option<i32>| ExecutionFacts {
        task_arn: Some(execution_task_arn(n)),
        container_instance_arn: Some(execution_container_instance_arn(n)),
        log_stream: Some(execution_log_stream(n)),
        exit_code,
        ..ExecutionFacts::default()
    };
    BatchJobSnapshot {
        batch_job_id: RETRIED_JOB_ID.into(),
        queue: Some(RETRIED_QUEUE_ARN.into()),
        status: BatchStatus::Succeeded,
        started_at_millis: Some(831_000),
        stopped_at_millis: Some(2_217_000),
        status_reason: Some(RETRIED_EXITED_REASON.into()),
        current: container(2, Some(0)),
        attempts: vec![
            ExecutionFacts {
                status_reason: Some(RETRIED_LOST_REASON.into()),
                started_at_millis: Some(3_000),
                stopped_at_millis: Some(684_000),
                ..container(1, None)
            },
            ExecutionFacts {
                status_reason: Some(RETRIED_EXITED_REASON.into()),
                started_at_millis: Some(831_000),
                stopped_at_millis: Some(2_217_000),
                ..container(2, Some(0))
            },
        ],
    }
}

/// The `batch_execution_started` event of execution `execution_id` (its ECS task ARN) of Batch
/// job `job`, started at `started_s` (epoch seconds): the event the builder
/// ([`crate::eventbridge::execution::execution_events`]) makes, with its dedup key and its `ts`
/// at the start, but no host, queue or log stream. Set the host and queue with [`RunEventExt`].
pub fn execution_started(job: &str, execution_id: &str, started_s: f64) -> RunEvent {
    let data = EventData::BatchExecutionStarted {
        execution_id: execution_id.to_owned(),
        started_at: Timestamp::from_epoch_seconds(started_s),
        log_stream: None,
    };
    let key = aws_batch_execution_started(execution_id);
    event(started_s, EventSource::AwsBatch, key, data).with_job(job)
}

/// The `batch_execution_ended` event of execution `execution_id` of Batch job `job`: started at
/// `started_s` (`None` for an execution AWS gave no start), stopped at `stopped_s` (the event's
/// `ts`) and categorized `category`, with no exit code, reason, log stream, host or queue. As
/// [`execution_started`].
pub fn execution_ended(
    job: &str,
    execution_id: &str,
    started_s: Option<f64>,
    stopped_s: f64,
    category: Option<TerminationCategory>,
) -> RunEvent {
    let data = EventData::BatchExecutionEnded {
        execution_id: execution_id.to_owned(),
        started_at: started_s.map(Timestamp::from_epoch_seconds),
        stopped_at: Timestamp::from_epoch_seconds(stopped_s),
        exit_code: None,
        status_reason: None,
        termination_category: category,
        log_stream: None,
    };
    let key = aws_batch_execution_ended(execution_id);
    event(stopped_s, EventSource::AwsBatch, key, data).with_job(job)
}

/// The environment the deploy's CDK (Python) sets on a Lambda, read as text, for the tests that
/// hold each binary's config to it: a binary must read every variable its Lambda is given, and its
/// Lambda must be given every variable the binary requires.
pub mod cdk {
    use std::collections::BTreeSet;

    /// The variable names `python` sets: each quoted `"BIFFIX_…"` literal, and each f-string name
    /// `f"BIFFIX_…_{…}"` as the pattern `BIFFIX_…_*`.
    pub fn env_names(python: &str) -> BTreeSet<String> {
        let mut names = BTreeSet::new();
        for (at, _) in python.match_indices("\"BIFFIX_") {
            let rest = &python[at + 1..];
            let end = rest
                .find(|c: char| !(c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_'))
                .unwrap_or(rest.len());
            match rest[end..].chars().next() {
                Some('"') => names.insert(rest[..end].to_owned()),
                Some('{') if python[..at].ends_with('f') => {
                    names.insert(format!("{}*", &rest[..end]))
                }
                _ => false,
            };
        }
        names
    }

    /// The block of `python` that starts at the first line containing `start`: that line and the
    /// lines after it indented deeper (a `def`'s body, a multi-line argument).
    pub fn block<'a>(python: &'a str, start: &str) -> &'a str {
        let at = python.find(start).unwrap_or_else(|| panic!("no {start:?} in the CDK source"));
        let line_start = python[..at].rfind('\n').map_or(0, |i| i + 1);
        let indent = |line: &str| line.len() - line.trim_start().len();
        let depth = indent(&python[line_start..]);
        let mut end = python.len();
        let mut offset = line_start;
        for (i, line) in python[line_start..].split_inclusive('\n').enumerate() {
            if i > 0 && !line.trim().is_empty() && indent(line) <= depth {
                end = offset;
                break;
            }
            offset += line.len();
        }
        &python[line_start..end]
    }

    /// Assert the contract between a Lambda's CDK environment `set` and the variables its binary
    /// `read`: every one set is read (a pattern `X*` by some name starting with `X`), and of each
    /// group in `required` (one name, or names one of which suffices) one is set.
    pub fn assert_contract(
        lambda: &str,
        set: &BTreeSet<String>,
        read: &BTreeSet<String>,
        required: &[Vec<String>],
    ) {
        for name in set {
            let is_read = match name.strip_suffix('*') {
                Some(prefix) => read.iter().any(|r| r.starts_with(prefix)),
                None => read.contains(name),
            };
            assert!(
                is_read,
                "the CDK sets {name} on the {lambda} Lambda, but its binary never reads it"
            );
        }
        for group in required {
            assert!(
                group.iter().any(|name| set.contains(name)),
                "the {lambda} Lambda's binary requires {}, but the CDK sets none of it",
                group.join(" or ")
            );
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        const PY: &str = r#"
class X:
    def a(self, fn):
        fn.add_environment("BIFFIX_A", "1")
        names = {f"BIFFIX_B_{arch.upper()}": 1, "NOT_OURS": 2}
        """`BIFFIX_DOC` in a docstring."""

    def b(self, fn):
        fn.add_environment("BIFFIX_C", "1")
"#;

        #[test]
        fn names_and_blocks_are_read_from_the_python() {
            assert_eq!(
                env_names(PY),
                ["BIFFIX_A", "BIFFIX_B_*", "BIFFIX_C"].map(String::from).into()
            );
            assert_eq!(
                env_names(block(PY, "def a(")),
                ["BIFFIX_A", "BIFFIX_B_*"].map(String::from).into()
            );
            let read: BTreeSet<String> = ["BIFFIX_A", "BIFFIX_B_AMD64"].map(String::from).into();
            let required = [vec!["BIFFIX_A".to_owned()]];
            assert_contract("x", &env_names(block(PY, "def a(")), &read, &required);
        }

        #[test]
        #[should_panic(expected = "never reads it")]
        fn a_variable_set_but_never_read_breaks_the_contract() {
            assert_contract("x", &env_names(PY), &BTreeSet::new(), &[]);
        }

        #[test]
        #[should_panic(expected = "the CDK sets none of it")]
        fn a_required_variable_the_cdk_never_sets_breaks_the_contract() {
            let required = [vec!["BIFFIX_D".to_owned()]];
            assert_contract("x", &BTreeSet::new(), &BTreeSet::new(), &required);
        }
    }
}
