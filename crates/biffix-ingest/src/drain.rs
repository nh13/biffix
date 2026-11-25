//! SQS → batched writer. AWS-gated (`feature = "aws"`).
//!
//! The queue carries raw EventBridge events (Batch job, ECS container instance, EC2 spot
//! interruption and CodeBuild state changes) and the reconcile tick's `biffix_ingest` envelopes
//! (`biffix_core::eventbridge::route`); this drains a batch of SQS records, decodes each to its
//! RunEvents (a Batch job state change is the job's event plus its executions') or a host fact
//! for the host registry, appends idempotently, then reconciles the projection once per batch.
//!
//! A record's events are appended together or not at all, so a redelivered record never finds
//! itself half-recorded. One bad record never poisons the batch:
//! - a record that fails to decode or route is logged (with its `messageId`) and dropped. It is not
//!   retried and does not reach the DLQ, because no retry can make it decode.
//! - when the batch's one transaction fails (a hard DB error), each decoded record is retried in a
//!   transaction of its own, and only the records that still fail are reported back to SQS as
//!   partial batch failures ([`SqsBatchResponse`]; the event source mapping must enable
//!   `ReportBatchItemFailures`). SQS redelivers just those, and after `maxReceiveCount` the DLQ
//!   holds just those; the healthy records are committed.

use std::ops::Range;

use aws_lambda_events::sqs::{BatchItemFailure, SqsBatchResponse, SqsEvent};
use sqlx::PgPool;

use biffix_core::event::RunEvent;
use biffix_core::eventbridge::decode::{batch_snapshot, HostFact, QueueContext};
use biffix_core::eventbridge::execution::BatchJobSnapshot;
use biffix_core::eventbridge::{route, Ingested};
use biffix_db::log::ingest_batch;
use biffix_db::DbError;

/// The outcome of one SQS batch.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct BatchOutcome {
    /// Events newly appended to the log (a redelivery appends nothing).
    pub appended: usize,
    /// The records SQS must redeliver: their own append or fold failed.
    pub response: SqsBatchResponse,
}

/// A batch's decoded records: the events and host facts to ingest, in arrival order, and for
/// each record its `messageId` and where its items sit, so a record can be retried alone.
#[derive(Default)]
struct Decoded<'a> {
    events: Vec<RunEvent>,
    hosts: Vec<HostFact>,
    records: Vec<(Option<&'a str>, Slot)>,
}

/// Where one decoded record's items sit in [`Decoded`]: a record's events are contiguous, as
/// are its host facts, because each record is pushed whole before the next.
struct Slot {
    events: Range<usize>,
    hosts: Range<usize>,
}

impl<'a> Decoded<'a> {
    /// Add one record's items, keeping them together under its `messageId`.
    fn push(&mut self, message_id: Option<&'a str>, items: Vec<Ingested>) {
        let (first_event, first_host) = (self.events.len(), self.hosts.len());
        for item in items {
            match item {
                Ingested::Event(run_event) => self.events.push(run_event),
                Ingested::Host(host) => self.hosts.push(host),
            }
        }
        let slot =
            Slot { events: first_event..self.events.len(), hosts: first_host..self.hosts.len() };
        self.records.push((message_id, slot));
    }

    /// The events and host facts of one record, alone.
    fn one(&self, slot: &Slot) -> (&[RunEvent], &[HostFact]) {
        (&self.events[slot.events.clone()], &self.hosts[slot.hosts.clone()])
    }
}

/// Process one SQS batch: decode → append → advance.
///
/// Returns `Err` only when a failure cannot be pinned on specific records: the fold fails with
/// nothing to append, or a failing record has no `messageId` to report. Then the whole batch is
/// retried, which is safe because every append is idempotent on `dedup_key`.
pub async fn handle_sqs_batch(
    pool: &PgPool,
    ctx: &QueueContext,
    event: &SqsEvent,
) -> Result<BatchOutcome, DbError> {
    let mut decoded = Decoded::default();
    for record in &event.records {
        let message_id = record.message_id.as_deref();
        let shown = message_id.unwrap_or("?");
        let Some(body) = &record.body else {
            tracing::warn!(message_id = shown, "skipping SQS record with no body");
            continue;
        };
        let value: serde_json::Value = match serde_json::from_str(body) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(message_id = shown, error = %e, "skipping undecodable SQS body");
                continue;
            }
        };
        match route(&value, ctx) {
            Ok(items) if items.is_empty() => {
                tracing::debug!(message_id = shown, "ignoring non-ingest event");
            }
            Ok(items) => {
                for item in &items {
                    log_routed(shown, item);
                }
                warn_of_unidentified_batch_executions(shown, &value);
                decoded.push(message_id, items);
            }
            Err(e) => tracing::warn!(message_id = shown, error = %e, "skipping malformed event"),
        }
    }

    // Append the batch AND advance the projection in one lock-holding transaction (the single
    // production writer). Run it every batch even when nothing new is appended: if a prior batch
    // appended events but its fold failed transiently, the redelivery arrives as all-duplicate
    // appends (appended == 0) — gating on new appends would strand those events unfolded forever.
    // The fold is a full, idempotent re-fold, so running it unconditionally self-heals.
    let outcome = match ingest_batch(pool, &decoded.events, &decoded.hosts).await {
        Ok(appended) => BatchOutcome { appended, response: SqsBatchResponse::default() },
        // Nothing to blame a record for: fail the invocation so the batch is retried.
        Err(e) if decoded.records.is_empty() => return Err(e),
        Err(e) => {
            tracing::warn!(
                error = %e,
                records = decoded.records.len(),
                "ingest: batch transaction failed; retrying each record on its own"
            );
            one_by_one(pool, &decoded).await?
        }
    };
    tracing::info!(
        records = event.records.len(),
        appended = outcome.appended,
        hosts = decoded.hosts.len(),
        failed = outcome.response.batch_item_failures.len(),
        "ingest: batch processed"
    );
    Ok(outcome)
}

/// Retry each record in a transaction of its own, collecting the ones that still fail. All of a
/// record's events share that transaction, so a record one of whose events is rejected commits
/// none of them and is redelivered whole. A failing record without a `messageId` cannot be
/// reported alone, so it fails the whole invocation.
async fn one_by_one(pool: &PgPool, decoded: &Decoded<'_>) -> Result<BatchOutcome, DbError> {
    let mut outcome = BatchOutcome::default();
    for (message_id, slot) in &decoded.records {
        let (events, hosts) = decoded.one(slot);
        match ingest_batch(pool, events, hosts).await {
            Ok(appended) => outcome.appended += appended,
            Err(e) => {
                let Some(message_id) = *message_id else {
                    return Err(e);
                };
                tracing::error!(
                    message_id,
                    error = %e,
                    "ingest: record failed on its own; returning it to SQS for redelivery"
                );
                // `#[non_exhaustive]`: built from its default, not a struct literal.
                let mut failure = BatchItemFailure::default();
                failure.item_identifier = message_id.to_string();
                outcome.response.batch_item_failures.push(failure);
            }
        }
    }
    Ok(outcome)
}

/// Warn when a Batch job state change lists executions without a task ARN.
///
/// The decoders are pure and cannot log, and what they return has no place for the count, so
/// this reads the job's `detail` a second time.
fn warn_of_unidentified_batch_executions(message_id: &str, body: &serde_json::Value) {
    if body.get("source").and_then(|source| source.as_str()) != Some("aws.batch") {
        return;
    }
    if let Some(snapshot) = body.get("detail").and_then(batch_snapshot) {
        let _span = tracing::warn_span!("record", message_id).entered();
        warn_of_unidentified_executions(&snapshot, "ingest");
    }
}

/// Warn when `snapshot`'s job lists executions without a task ARN (`attempts[]` entries with no
/// `container.taskArn`), on the ingest `path` that read it (the live drain or the reconcile
/// backstop). An execution's identity is its task ARN, so such an entry is recorded as nothing
/// rather than under a made-up identity, and this warning (the job's id and how many) is the
/// only trace of it.
pub(crate) fn warn_of_unidentified_executions(snapshot: &BatchJobSnapshot, path: &'static str) {
    let unidentified = snapshot.entries_without_task_arn();
    if unidentified > 0 {
        tracing::warn!(
            path,
            batch_job_id = snapshot.batch_job_id.as_str(),
            unidentified,
            "Batch reported executions without a task ARN; they are not recorded"
        );
    }
}

/// Log each routed item, so a stuck projection (a coordinator terminal never folding a run) can be
/// traced from the logs to the record that should have moved it.
fn log_routed(message_id: &str, item: &Ingested) {
    match item {
        Ingested::Host(host) => tracing::info!(
            message_id,
            container_instance_id = host.container_instance_id.as_str(),
            ec2_instance_id = host.ec2_instance_id.as_str(),
            "ingest: routed host fact"
        ),
        Ingested::Event(run_event) => tracing::info!(
            message_id,
            kind = ?run_event.kind(),
            batch_job_id = run_event.batch_job_id.as_ref().map(biffix_core::ids::BatchJobId::as_str).unwrap_or(""),
            run_id = run_event.run_id.as_ref().map(biffix_core::ids::RunId::as_str).unwrap_or(""),
            "ingest: routed batch event"
        ),
    }
}
