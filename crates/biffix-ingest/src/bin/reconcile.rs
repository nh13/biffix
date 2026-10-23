//! Scheduled reconcile Lambda (AWS-gated): runs one `biffix_ingest::tick` per scheduled invoke,
//! or one page of a manual refold.
//!
//!   cargo build -p biffix-ingest --features aws --bin biffix-reconcile-lambda
//!
//! Config comes from env (see `biffix_ingest::config::ReconcileConfig`): DATABASE_URL (Aurora) or
//! the RDS secret, BIFFIX_INGEST_QUEUE_URL, BIFFIX_COORDINATOR_QUEUES and the tick's optional
//! settings. The invoke payload is read by `biffix_ingest::config::ReconcileInvoke`: the schedule's
//! event runs the tick, and `{"action": "refold", "after": "<run id>", "limit": <1..=200>}` (both
//! fields optional) re-folds one page of runs and answers `{"refolded": n, "last_run_id": …}`.
//!
//! The schedule fires every 5 minutes, on the minutes that are multiples of 5. A scheduled tick
//! first asks AWS Batch, not the database, whether any coordinator job is active or recently
//! stopped (`biffix_db::idle::check`) and skips its work when none is, so an idle Aurora cluster
//! can pause; the hour's first tick always runs. The reason a tick runs reaches the status probe,
//! whose samples say how soon the next one is due. The migration runs on the first invocation that
//! touches the database, not at cold start, and every such invocation ends by closing its
//! connections (`biffix_db::pool::release_idle`): a container frozen between invocations must
//! hold none open.

use biffix_db::idle::{Tick, Why};
use biffix_ingest::budget::Deadline;
use biffix_ingest::config::{ReconcileConfig, ReconcileInvoke};
use biffix_ingest::tick;
use lambda_runtime::{service_fn, Error, LambdaEvent};
use std::sync::Arc;
use tokio::sync::OnceCell;

#[tokio::main]
async fn main() -> Result<(), Error> {
    biffix_core::telemetry::init_lambda();

    let config = ReconcileConfig::from_process_env()?;
    let coordinator_queues = Arc::new(config.coordinator_queues.clone());
    let db_url = biffix_db::url::resolve(&config.db).await?;
    // Connect lazily + Aurora-tuned so a resume-from-zero waits rather than erroring at boot.
    let pool = biffix_db::pool::tuned_pool_options(2).connect_lazy(&db_url)?;

    let sdk = aws_config::load_defaults(aws_config::BehaviorVersion::latest()).await;
    let batch = aws_sdk_batch::Client::new(&sdk);
    let state = Arc::new(tick::State::new(pool, &sdk, config));
    let migrated = Arc::new(OnceCell::new());

    lambda_runtime::run(service_fn(move |event: LambdaEvent<serde_json::Value>| {
        let state = state.clone();
        let batch = batch.clone();
        let coordinator_queues = coordinator_queues.clone();
        let migrated = migrated.clone();
        async move {
            let invoke = ReconcileInvoke::from_payload(&event.payload).map_err(Error::from)?;
            let why = match invoke {
                ReconcileInvoke::Tick => {
                    match biffix_db::idle::check(&batch, &coordinator_queues, &event.payload).await
                    {
                        Tick::Full(why) => why,
                        Tick::Idle => return Ok(serde_json::json!({ "skipped": "idle" })),
                    }
                }
                // A refold runs no tick, so no status sample carries this reason.
                ReconcileInvoke::Refold { .. } => Why::NotScheduled,
            };
            let result = with_db(&state, &migrated, invoke, why, &event).await;
            biffix_db::pool::release_idle(&state.pool).await;
            result
        }
    }))
    .await
}

/// Run `invoke`, which touches the database, migrating it first on the container's first such
/// invocation. `why` is the reason a tick runs in full.
async fn with_db(
    state: &tick::State,
    migrated: &OnceCell<()>,
    invoke: ReconcileInvoke,
    why: Why,
    event: &LambdaEvent<serde_json::Value>,
) -> Result<serde_json::Value, Error> {
    migrated.get_or_try_init(|| biffix_db::migrate::run(&state.pool)).await?;
    match invoke {
        ReconcileInvoke::Tick => {
            tick::run(state, Deadline::at_epoch_millis(event.context.deadline), why).await
        }
        ReconcileInvoke::Refold { after, limit } => {
            let page =
                biffix_db::projection::refold_runs(&state.pool, after.as_deref(), limit).await?;
            tracing::info!(refolded = page.refolded, last = ?page.last_run_id, "manual refold");
            Ok(serde_json::json!({
                "refolded": page.refolded,
                "last_run_id": page.last_run_id,
            }))
        }
    }
}
