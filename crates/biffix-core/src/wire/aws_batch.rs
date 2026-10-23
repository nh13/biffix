//! The AWS Batch wire event (`source: aws-batch`).
//!
//! Mirrors the fields biffix reads from an EventBridge "Batch Job State Change" detail. The ingest
//! decoder builds it from the raw EventBridge JSON (`crate::eventbridge`); it is an internal decode
//! structure, not a published contract.

use serde::{Deserialize, Serialize};

use crate::ids::{BatchJobId, InstanceId};
use crate::time::Timestamp;

/// The Batch job status biffix keys the event `kind` off.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum BatchStatus {
    Submitted,
    Pending,
    Runnable,
    Starting,
    Running,
    Succeeded,
    Failed,
    /// A status not known to this build; preserved rather than dropped.
    #[serde(untagged)]
    Unknown(String),
}

impl BatchStatus {
    /// The AWS wire status string (SCREAMING_SNAKE_CASE). Used to build the deterministic
    /// `dedup_key`, which must be the contractual AWS form — never Rust's `Debug`
    /// representation, which is not a stable interface for a value persisted in a uniqueness index.
    pub fn as_wire(&self) -> &str {
        match self {
            BatchStatus::Submitted => "SUBMITTED",
            BatchStatus::Pending => "PENDING",
            BatchStatus::Runnable => "RUNNABLE",
            BatchStatus::Starting => "STARTING",
            BatchStatus::Running => "RUNNING",
            BatchStatus::Succeeded => "SUCCEEDED",
            BatchStatus::Failed => "FAILED",
            BatchStatus::Unknown(s) => s.as_str(),
        }
    }
}

/// A single AWS Batch Job State Change, as biffix consumes it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AwsBatchEvent {
    /// Source time of the state change.
    pub ts: Timestamp,
    /// `detail.jobId` — the attempt's Batch job id (and the join key downstream).
    pub batch_job_id: BatchJobId,
    /// `detail.status`.
    pub status: BatchStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_stream: Option<String>,
    /// The host the attempt ran on. The Batch event carries no raw EC2 instance id, so this is the
    /// ECS container-instance id (the last segment of `detail.container.containerInstanceArn`), the
    /// only host identifier present in-event. Absent for Fargate jobs and before the job starts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance_id: Option<InstanceId>,
    /// The job queue's name (the last segment of `detail.jobQueue`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue: Option<String>,
    /// Whether this job is the run's coordinator (told apart by queue/job-definition).
    #[serde(default)]
    pub is_coordinator: bool,
    /// How many of this job's attempts Batch had already finished when the state change happened
    /// (`len(detail.attempts)`). Batch's own `retryStrategy` re-runs a job under the *same* job id,
    /// taking it through RUNNABLE → STARTING → RUNNING again; this tells those edges apart from the
    /// first attempt's so they are distinct facts in the log. Absent ⇒ 0 (the first attempt, or a
    /// producer that cannot tell). Optional + additive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prior_attempts: Option<u32>,
    /// The vCPUs the job asked Batch for: its `VCPU` resource requirement (legacy
    /// `container.vcpus` when absent). What the task requested, not what its host has. Absent when
    /// the event carries neither (e.g. a multi-node or ECS-properties job). Optional + additive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_vcpus: Option<f64>,
    /// The memory (MiB) the job asked Batch for: its `MEMORY` resource requirement (legacy
    /// `container.memory` when absent). Optional + additive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_memory_mib: Option<i64>,
}

impl BatchStatus {
    /// A terminal status: the job reaches it once, however many attempts it took (a failed attempt
    /// that Batch retries goes back to RUNNABLE, not FAILED).
    pub fn is_terminal(&self) -> bool {
        matches!(self, BatchStatus::Succeeded | BatchStatus::Failed)
    }
}
