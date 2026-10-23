//! biffix ingest Lambda (AWS-gated). Drains SQS batches → RunEvent log → projection.
//!
//!   cargo build -p biffix-ingest --features aws --bin biffix-ingest-lambda
//!
//! Deployed by `infra/` (`biffix_infra/constructs/ingest_pipeline.py`) as the batched writer behind
//! the ingest SQS queue, with partial batch responses (`ReportBatchItemFailures`) enabled on its
//! event source. Config comes from env (see
//! `biffix_ingest::config::IngestConfig`): DATABASE_URL (Aurora) or the RDS secret,
//! BIFFIX_COORDINATOR_QUEUES (comma-separated, for worker/coordinator classification).

use aws_lambda_events::sqs::SqsEvent;
use biffix_core::eventbridge::decode::QueueContext;
use biffix_ingest::config::IngestConfig;
use biffix_ingest::drain::handle_sqs_batch;
use lambda_runtime::{service_fn, Error, LambdaEvent};
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Error> {
    biffix_core::telemetry::init_lambda();

    let config = IngestConfig::from_process_env()?;
    // Prefer DATABASE_URL; otherwise assemble it from the RDS secret (BIFFIX_DB_SECRET_ARN).
    let db_url = biffix_db::url::resolve(&config.db).await?;
    // Connect lazily + Aurora-tuned so a resume-from-zero waits rather than erroring at boot.
    let pool = Arc::new(biffix_db::pool::tuned_pool_options(4).connect_lazy(&db_url)?);
    biffix_db::migrate::run(&pool).await.map_err(|e| Error::from(e.to_string()))?;
    // No connection may outlive an invocation (nor the init phase): a container frozen between
    // invocations cannot run the pool's idle reaper, so a connection left open would keep the
    // cluster from pausing after a run ends.
    biffix_db::pool::release_idle(&pool).await;

    let coordinator_queues = config.coordinator_queues;
    if coordinator_queues.is_empty() {
        tracing::warn!(
            "BIFFIX_COORDINATOR_QUEUES is empty: every Batch job's events are stored task-shaped \
             (the fold still reads a coordinator's through the control plane's binding)"
        );
    }
    let ctx = Arc::new(QueueContext { coordinator_queues });

    lambda_runtime::run(service_fn(move |event: LambdaEvent<SqsEvent>| {
        let pool = pool.clone();
        let ctx = ctx.clone();
        async move {
            // The partial-batch response: SQS redelivers only the records listed as failed (the
            // event source mapping enables `ReportBatchItemFailures`).
            let outcome = handle_sqs_batch(&pool, &ctx, &event.payload).await;
            biffix_db::pool::release_idle(&pool).await;
            Ok::<_, Error>(outcome.map_err(|e| Error::from(e.to_string()))?.response)
        }
    }))
    .await
}
