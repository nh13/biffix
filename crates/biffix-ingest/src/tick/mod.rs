//! The scheduled reconcile tick: the reconcile Lambda's periodic work.
//!
//! The reconcile step reads stale non-terminal attempt IDs from Aurora, describes them in bounded
//! AWS batches, and enqueues any terminal facts onto the ordinary ingest SQS queue. It deliberately
//! does not append or fold directly: the single ingest writer remains the sole append path.
//!
//! [`run`] runs every step, one module each — `describe` (reconcile + reap), `probe` (status
//! probe), `hosts` (host lifecycles), `pricing`, `retention` and `cache` (cache expiry) —
//! even when an earlier one fails, and inside each step one failing job, group or bucket never
//! stops the rest; the tick then reports every failure, so the Lambda still errors (and alarms)
//! while the independent work was done. The decisions each step makes are pure functions in the
//! library (`reconcile`, `pricing`, `retention`, `cache_expiry`, `status`, `budget`), unit-tested
//! there; these modules
//! are the AWS and database wiring around them.

mod cache;
mod describe;
mod hosts;
mod pricing;
mod probe;
mod retention;

use std::sync::Mutex;
use std::time::Duration;

use aws_sdk_batch::error::DisplayErrorContext;
use biffix_core::clock::micros;
use biffix_core::eventbridge::decode::QueueContext;
use biffix_db::idle::Why;
use lambda_runtime::Error;

use crate::budget::Deadline;
use crate::config::ReconcileConfig;

/// Maximum messages in one SQS `SendMessageBatch` request.
const SQS_BATCH_SIZE: usize = 10;

/// The tick's clients and configuration (its [`ReconcileConfig`], read once at the Lambda's cold
/// start).
pub struct State {
    pub pool: sqlx::PgPool,
    batch: aws_sdk_batch::Client,
    ec2: aws_sdk_ec2::Client,
    sqs: aws_sdk_sqs::Client,
    cloudwatch: aws_sdk_cloudwatch::Client,
    s3: aws_sdk_s3::Client,
    /// The AWS Pricing API, for the list prices of a region other than the checked-in table's.
    pricing: aws_sdk_pricing::Client,
    /// The list prices read from it so far, kept for the life of the Lambda container.
    prices: Mutex<pricing::PriceBook>,
    config: ReconcileConfig,
    /// The coordinator queues (`BIFFIX_COORDINATOR_QUEUES`), which `config` hands over to it.
    queue_context: QueueContext,
}

impl State {
    /// The tick's state from the reconcile Lambda's `config`, with clients built from `sdk`.
    pub fn new(
        pool: sqlx::PgPool,
        sdk: &aws_config::SdkConfig,
        mut config: ReconcileConfig,
    ) -> Self {
        if config.coordinator_queues.is_empty() {
            // Mirror bin/lambda.rs. The backstop, the reaper and the pricer find a coordinator by
            // the control plane's binding, so what is lost is the probe's coordinator-queue check.
            tracing::warn!(
                "BIFFIX_COORDINATOR_QUEUES is empty: the status probe cannot check the coordinator \
                 queue"
            );
        }
        let queue_context =
            QueueContext { coordinator_queues: std::mem::take(&mut config.coordinator_queues) };
        State {
            pool,
            batch: aws_sdk_batch::Client::new(sdk),
            ec2: aws_sdk_ec2::Client::new(sdk),
            sqs: aws_sdk_sqs::Client::new(sdk),
            cloudwatch: aws_sdk_cloudwatch::Client::new(sdk),
            s3: aws_sdk_s3::Client::new(sdk),
            pricing: pricing::pricing_client(sdk),
            prices: Mutex::default(),
            config,
            queue_context,
        }
    }
}

/// Run one tick: every step, whatever an earlier one did. The tick fails at the end if any step
/// failed, so the error still reaches Lambda (and its alarm). Every step is idempotent, so the
/// retry that failure triggers is safe. The steps that drain a backlog (retention, cache expiry)
/// stop starting work once `deadline`, the invocation's, is near, and resume on the next tick.
/// `why` is the reason the tick runs in full, which the status probe's samples carry forward.
/// Returns `{"emitted": n}`, the reconciliation events enqueued.
pub async fn run(state: &State, deadline: Deadline, why: Why) -> Result<serde_json::Value, Error> {
    let reconciled = describe::reconcile(state).await;
    let mut failures = Vec::new();
    // Sample service-status health, including when reconcile failed: that is when the
    // status page most needs a fresh sample.
    record_step(&mut failures, "status probe", probe::sample(state, why).await, "status sampled");
    record_step(
        &mut failures,
        "host lifecycles",
        hosts::resolve_host_lifecycles(state).await,
        "resolved host lifecycles",
    );
    // After lifecycles, so a job is priced with its host's resolved spot/on-demand label (a job
    // whose host is still unresolved waits for a later tick).
    record_step(
        &mut failures,
        "pricing",
        pricing::price_finished_jobs(state, deadline).await,
        "priced finished jobs",
    );
    // Last: deleting intermediate files is the least urgent work of a tick.
    record_step(
        &mut failures,
        "retention",
        retention::sweep_retention(state, deadline).await,
        "retention: deleted intermediate files",
    );
    record_step(
        &mut failures,
        "cache expiry",
        cache::sweep_snakemake_cache(state, deadline).await,
        "cache: deleted expired Snakemake entries",
    );
    let emitted = match reconciled {
        Ok(emitted) => emitted,
        Err(error) => {
            failures.insert(0, format!("reconcile: {error}"));
            0
        }
    };
    if failures.is_empty() {
        Ok(serde_json::json!({ "emitted": emitted }))
    } else {
        Err(Error::from(format!(
            "reconcile tick: {} step(s) failed: {}",
            failures.len(),
            failures.join("; ")
        )))
    }
}

/// Log one tick step's outcome, and keep its error for the tick's result.
fn record_step(failures: &mut Vec<String>, step: &str, outcome: Result<usize, Error>, done: &str) {
    match outcome {
        Ok(0) => {}
        Ok(count) => tracing::info!(count, step, "{done}"),
        Err(error) => {
            tracing::warn!(step, error = %error, "tick step failed (the other steps still ran)");
            failures.push(format!("{step}: {error}"));
        }
    }
}

/// An AWS SDK error with its full cause chain: the SDK's own `Display` says only "service error",
/// never the code and message AWS returned.
fn aws_error(context: &str, error: impl std::error::Error) -> Error {
    Error::from(format!("{context}: {}", DisplayErrorContext(&error)))
}

/// Keep the first error of a step while letting it go on.
fn note(first_error: &mut Option<Error>, error: Error) {
    first_error.get_or_insert(error);
}

/// `at` minus `ago`, in epoch micros (zero before the epoch).
fn micros_before(at: Duration, ago: Duration) -> i64 {
    micros(at.saturating_sub(ago))
}

/// Enqueue `events` on the ingest queue, in `SendMessageBatch` requests of at most
/// [`SQS_BATCH_SIZE`]; returns how many were sent.
async fn enqueue_events(
    state: &State,
    events: &[biffix_core::event::RunEvent],
) -> Result<usize, Error> {
    let mut emitted = 0usize;
    for chunk in events.chunks(SQS_BATCH_SIZE) {
        let entries = chunk
            .iter()
            .enumerate()
            .map(|(index, event)| {
                Ok(aws_sdk_sqs::types::SendMessageBatchRequestEntry::builder()
                    .id(format!("event-{index}"))
                    // Wrap in the typed ingest envelope so the writer routes on the explicit
                    // `biffix_ingest` tag, not on which RunEvent fields happen to be present.
                    .message_body(serde_json::to_string(
                        &biffix_core::eventbridge::NormalizedIngestEnvelope::new(event.clone()),
                    )?)
                    .build()?)
            })
            .collect::<Result<Vec<_>, Error>>()?;
        let result = state
            .sqs
            .send_message_batch()
            .queue_url(&state.config.ingest_queue_url)
            .set_entries(Some(entries))
            .send()
            .await
            .map_err(|error| aws_error("SQS SendMessageBatch", error))?;
        if let Some(failed) = result.failed().first() {
            return Err(Error::from(format!(
                "SQS rejected {} reconciliation events (first: {} {})",
                result.failed().len(),
                failed.code(),
                failed.message().unwrap_or_default()
            )));
        }
        emitted += result.successful().len();
    }
    Ok(emitted)
}

/// Delete `keys` from `bucket` in one DeleteObjects call; any per-key failure is an error.
async fn delete_keys(
    state: &State,
    bucket: &str,
    keys: Vec<aws_sdk_s3::types::ObjectIdentifier>,
) -> Result<usize, Error> {
    let n = keys.len();
    let delete =
        aws_sdk_s3::types::Delete::builder().set_objects(Some(keys)).quiet(true).build()?;
    let out = state
        .s3
        .delete_objects()
        .bucket(bucket)
        .delete(delete)
        .send()
        .await
        .map_err(|error| aws_error(&format!("DeleteObjects in s3://{bucket}"), error))?;
    if let Some(err) = out.errors().first() {
        return Err(Error::from(format!(
            "DeleteObjects in s3://{bucket}: {} of {n} keys failed, first {} ({} {})",
            out.errors().len(),
            err.key().unwrap_or("?"),
            err.code().unwrap_or("?"),
            err.message().unwrap_or("unknown error")
        )));
    }
    Ok(n)
}

/// The DeleteObjects identifiers of listed `objects` (those with a key).
fn object_ids<'a>(
    objects: impl Iterator<Item = &'a aws_sdk_s3::types::Object>,
) -> Vec<aws_sdk_s3::types::ObjectIdentifier> {
    objects
        .filter_map(|o| aws_sdk_s3::types::ObjectIdentifier::builder().key(o.key()?).build().ok())
        .collect()
}
