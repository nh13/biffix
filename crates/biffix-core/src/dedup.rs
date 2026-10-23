//! Deterministic `dedup_key` construction.
//!
//! A retried delivery must yield the *same* key so the log's uniqueness constraint makes the
//! second write a no-op. Keys are producer-computed and deterministic.

use crate::ids::{BatchJobId, InstanceId, RunId, TaskKey};
use crate::wire::aws_batch::BatchStatus;

/// AWS Batch: `aws-batch:{batch_job_id}:{aws_status}`, plus a trailing `:{prior_attempts}` for a
/// non-terminal edge of a Batch-internal retry.
///
/// The key is deliberately **timestamp-independent**. Folding a timestamp
/// in would break dedup precisely when it matters: the live EventBridge path times a terminal from
/// `stoppedAt` but falls back to the envelope `time` when that detail is absent, while
/// reconciliation always uses `stoppedAt` — so one attempt's terminal could carry two different
/// micros and thus two keys, and the log would store two terminals for one attempt.
///
/// What identifies the fact instead:
/// - **Terminals** (`SUCCEEDED`/`FAILED`) are reached once per job id, however many attempts it
///   took (Batch sends a failed attempt it will retry back to RUNNABLE, not to FAILED), so
///   `(job, status)` identifies them — and collapses the live and reconciled terminal (and any
///   redelivery) to one no-op. Reconciliation sees only terminals, so it never needs the count.
/// - **Non-terminal edges** (SUBMITTED … RUNNING) recur when the job definition has a Batch
///   `retryStrategy` (the infrastructure retry layer, same `batch_job_id`). The
///   number of attempts Batch had already finished (`len(detail.attempts)`, carried on every
///   state change) tells the retry's edges from the first attempt's, so a retried attempt's
///   RUNNING is a new fact rather than a dropped "redelivery".
///
/// The first attempt (`prior_attempts` 0 or unknown) keeps the plain `(job, status)` key, so the
/// live EventBridge terminal and the reconciler's (which may not know the count) collapse onto one
/// fact. Only a retry's edges get the suffix.
pub fn aws_batch(job: &BatchJobId, status: &BatchStatus, prior_attempts: u32) -> String {
    let (terminal, status) = (status.is_terminal(), status.as_wire());
    if terminal || prior_attempts == 0 {
        format!("aws-batch:{}:{status}", job.as_str())
    } else {
        format!("aws-batch:{}:{status}:{prior_attempts}", job.as_str())
    }
}

/// CodeBuild: `aws-codebuild:{build_arn}:{status}`. Like [`aws_batch`], timestamp-independent: a
/// build passes through each status once, so the pair identifies the fact.
pub fn aws_codebuild(build_arn: &str, status: &str) -> String {
    format!("aws-codebuild:{build_arn}:{status}")
}

/// A build biffix requested: `biffix-control-plane:build:{build_arn}:requested` (one per build).
pub fn build_requested(build_arn: &str) -> String {
    format!("biffix-control-plane:build:{build_arn}:requested")
}

/// Another EC2 event (a kind this build does not know): `aws-ec2:{instance_id}:{kind}:{ts_micros}`.
pub fn aws_ec2(instance: &InstanceId, kind: &str, ts_micros: i64) -> String {
    format!("aws-ec2:{}:{kind}:{ts_micros}", instance.as_str())
}

/// EC2 spot: `aws-ec2:{instance_id}:interruption:{ts_micros}`.
pub fn aws_ec2_interruption(instance: &InstanceId, ts_micros: i64) -> String {
    aws_ec2(instance, "interruption", ts_micros)
}

/// Engine / control-plane: `{source}:{run_id}:{task_key}:{attempt}:{kind}[:{disc}]`.
/// `task_key`/`attempt` are omitted (empty) for run-level events.
///
/// `disc` is a per-fact discriminator appended for **repeatable** kinds — kinds that legitimately
/// recur for the same `(run, task, attempt)` and must NOT collapse under the log's uniqueness
/// constraint. Without it, e.g. every `run_progress` for a run would share one key and
/// all but the first would be silently dropped. It stays `None` for singleton kinds
/// (`run_started`, `run_succeeded`, …), whose stable key is exactly what makes redelivery a no-op.
///
/// INVARIANT (the `:` join is not escaped): two parts legitimately contain `:`. `task_key` does
/// (`rule:sorted(wildcards)`), and so does `disc` (the `completed:total` pair of `run_progress`, an execution id,
/// the `<ts micros>:<name>` discriminators `event/control_plane.rs` builds). The key reads back
/// unambiguously only because every other part is colon-free and positionally fixed (`source` a
/// fixed lowercase tag, `attempt` a decimal number, `kind` an enum tag, `run_id` biffix-minted
/// without `:`), `disc` is always last, and whether there is one is fixed by `kind`. Even so, a
/// `task_key` that itself ends in `:<digits>:<kind>` can meet an earlier key plus a colon-bearing
/// `disc`: no real key has that shape. A new part that can contain `:` goes after `kind` or is
/// escaped or length-prefixed, or two distinct facts could collide onto one key and one event would
/// be silently dropped.
pub fn engine(
    source: &str,
    run: &RunId,
    task: Option<&TaskKey>,
    attempt: Option<u32>,
    kind: &str,
    disc: Option<&str>,
) -> String {
    let task_s = task.map(super::ids::TaskKey::as_str).unwrap_or("");
    let attempt_s = attempt.map(|a| a.to_string()).unwrap_or_default();
    let mut key = format!("{source}:{}:{task_s}:{attempt_s}:{kind}", run.as_str());
    if let Some(disc) = disc {
        key.push(':');
        key.push_str(disc);
    }
    key
}
