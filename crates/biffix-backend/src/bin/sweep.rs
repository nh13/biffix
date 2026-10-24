//! biffix backend sweep Lambda — the backend's scheduled work, invoked by an EventBridge schedule
//! and by every biffix CodeBuild "Build State Change" event. It writes each ended run's provenance
//! crate into its S3 prefix (`biffix_backend::runs::provenance::write_due`), moves forward what
//! registered versions need: their coordinator variants (`biffix_backend::images::variants::sweep`)
//! and image imports (`biffix_backend::images::sweep`), and launches the runs parked for their
//! submit-time images once those are runnable (`biffix_backend::runs::launch::sweep`).
//!
//! A build state change is folded first (`biffix_backend::images::build_follow::fold_build_event`),
//! so a build that just finished launches what waited on it now rather than at the next schedule,
//! and it runs only the sweeps a build can advance (coordinator variants, image imports and
//! derives, and the launches waiting on them; the provenance sweep is reported as `null`). The
//! schedule's payload folds nothing and runs every sweep. The payload `{"sweep": "variants"}`
//! (`infra/deploy.sh` invokes it after a deploy) runs only the coordinator-variants sweep and returns
//! its report, so the deploy can start the builds a new base image needs and wait for them.
//!
//! The schedule fires every 5 minutes, on the minutes that are multiples of 5. A scheduled
//! invocation first asks AWS Batch, not the database, whether a coordinator job is active or
//! recently stopped (`biffix_db::idle::check`) and skips its sweeps when none is, so an idle
//! Aurora cluster can pause; the hour's first tick always runs, and a build state change is never
//! skipped. The migration runs on the first invocation that touches the database, not at cold
//! start, and every such invocation ends by closing its connections
//! (`biffix_db::pool::release_idle`): a container frozen between invocations must hold none open.
//!
//! The sweeps are independent: one's failure never keeps the others from running. The invocation
//! still fails when any of them did, naming them, so the Lambda's `Errors` metric (and the API
//! stack's `SweepErrorsAlarm` on it) says the scheduled work is not getting done.
//!
//!   cargo lambda build --release --features aws --bin biffix-backend-sweep
//!
//! It composes the same AppState as the API Lambda (minus the auth context: it serves no HTTP
//! request), so the crate it writes is exactly the one
//! `GET /runs/{id}/provenance` serves. (`required-features` keeps Cargo from building it without
//! `aws`.)

use serde_json::Value;

/// The sweeps of one invocation: each one's outcome for its result, and which of them failed.
#[derive(Default)]
struct Sweeps {
    failed: Vec<&'static str>,
}

impl Sweeps {
    /// One sweep's outcome for the invocation's result: its report, or its error (also logged,
    /// and remembered as a failure).
    fn outcome<T: serde::Serialize, E: std::fmt::Display>(
        &mut self,
        what: &'static str,
        result: Result<T, E>,
    ) -> Value {
        match result {
            Ok(report) => {
                let report = serde_json::to_value(report).unwrap_or_default();
                tracing::info!(%report, "sweep: {what}");
                report
            }
            Err(e) => {
                tracing::warn!(error = %e, "sweep: {what} failed");
                self.failed.push(what);
                Value::String(e.to_string())
            }
        }
    }

    /// The invocation's result: the `report`, or an error naming every sweep that failed.
    fn finish(self, report: Value) -> Result<Value, lambda_runtime::Error> {
        if self.failed.is_empty() {
            return Ok(report);
        }
        tracing::error!(%report, "sweep: failed: {}", self.failed.join(", "));
        Err(format!("sweep failed: {}", self.failed.join(", ")).into())
    }
}

#[tokio::main]
async fn main() -> Result<(), lambda_runtime::Error> {
    use lambda_runtime::{service_fn, LambdaEvent};

    biffix_core::telemetry::init_lambda();

    let config = biffix_backend::config::env::BackendConfig::from_process_env()?;
    let background = biffix_backend::config::compose::build_background_state(config)
        .await
        .map_err(|e| lambda_runtime::Error::from(e.to_string()))?;
    let background = std::sync::Arc::new(background);
    let sdk = aws_config::load_defaults(aws_config::BehaviorVersion::latest()).await;
    let batch = aws_sdk_batch::Client::new(&sdk);
    let migrated = std::sync::Arc::new(tokio::sync::OnceCell::new());
    lambda_runtime::run(service_fn(move |event: LambdaEvent<Value>| {
        let background = background.clone();
        let batch = batch.clone();
        let migrated = migrated.clone();
        async move {
            let queues = [background.state.deploy.coordinator_queue.clone()];
            if biffix_db::idle::check(&batch, &queues, &event.payload).await
                == biffix_db::idle::Tick::Idle
            {
                return Ok(serde_json::json!({ "skipped": "idle" }));
            }
            let result = match migrated
                .get_or_try_init(|| biffix_db::migrate::run(&background.pool))
                .await
            {
                Ok(()) => sweep(&background.state, &event.payload).await,
                Err(e) => Err(format!("migrate: {e}").into()),
            };
            biffix_db::pool::release_idle(&background.pool).await;
            result
        }
    }))
    .await
}

/// Whether `payload` asks for the coordinator-variants sweep alone (`{"sweep": "variants"}`).
fn variants_only(payload: &Value) -> bool {
    payload.get("sweep").and_then(Value::as_str) == Some("variants")
}

/// One invocation's sweeps for its `payload` (a build state change, the schedule's event, or a
/// request for the variants sweep alone).
async fn sweep(
    state: &biffix_backend::AppState,
    payload: &Value,
) -> Result<Value, lambda_runtime::Error> {
    if variants_only(payload) {
        let mut sweeps = Sweeps::default();
        let variants = sweeps
            .outcome("coordinator variants", biffix_backend::images::variants::sweep(state).await);
        return sweeps.finish(variants);
    }
    // A build state change: fold it before the sweeps read the builds. Its failure is logged and
    // reported; the sweeps run regardless (the ingest copy lands it anyway).
    let folded = biffix_backend::images::build_follow::fold_build_event(state, payload).await;
    // Only the schedule writes provenance: a build never ends a run.
    let scheduled =
        matches!(folded, Ok(biffix_backend::images::build_follow::BuildEventFold::NotABuildEvent));
    let mut sweeps = Sweeps::default();
    let build_event = sweeps.outcome("build event folded", folded);
    // The sweeps are independent: each one's failure is logged and reported, and never keeps the
    // others from running.
    let provenance = if scheduled {
        sweeps.outcome(
            "provenance crates written",
            biffix_backend::runs::provenance::write_due(state).await,
        )
    } else {
        Value::Null
    };
    // Coordinator variants: finish, rebuild (a new base image) and retry what registered versions
    // need (a parked run may wait on its coordinator variant's build).
    let variants = sweeps
        .outcome("coordinator variants", biffix_backend::images::variants::sweep(state).await);
    let images =
        sweeps.outcome("image imports and derives", biffix_backend::images::sweep(state).await);
    // After the images: a build this sweep finished launches its runs this sweep.
    let launches =
        sweeps.outcome("parked launches swept", biffix_backend::runs::launch::sweep(state).await);
    sweeps.finish(serde_json::json!({
        "build_event": build_event,
        "provenance_written": provenance,
        "variants": variants,
        "images": images,
        "launches": launches,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_variants_payload_runs_the_variants_sweep_alone() {
        assert!(variants_only(&serde_json::json!({ "sweep": "variants" })));
        for other in [
            serde_json::json!({ "detail-type": "Scheduled Event", "time": "2026-10-24T00:05:00Z" }),
            serde_json::json!({ "sweep": "images" }),
            serde_json::json!({ "sweep": true }),
            Value::Null,
        ] {
            assert!(!variants_only(&other), "{other}");
        }
    }

    #[test]
    fn an_invocation_fails_when_any_sweep_did_naming_them() {
        let mut ok = Sweeps::default();
        let report = ok.outcome("launches", Ok::<_, String>(3));
        assert_eq!(report, 3);
        assert_eq!(ok.finish(serde_json::json!({"x": 1})).unwrap()["x"], 1);

        let mut sweeps = Sweeps::default();
        let first = sweeps.outcome("variants", Err::<u8, _>("db down"));
        sweeps.outcome("images", Ok::<_, String>(0));
        sweeps.outcome("launches", Err::<u8, _>("db down"));
        // The failed sweep's report is its error; the others still ran.
        assert_eq!(first, "db down");
        let err = sweeps.finish(Value::Null).unwrap_err();
        assert_eq!(err.to_string(), "sweep failed: variants, launches");
    }
}
