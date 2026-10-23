//! A run that waits for its submit-time images: parked for the launch sweep ([`park`]).
//!
//! Submit writes a run's samplesheets (`biffix/runs/<run>/inputs/`) before it parks or launches
//! the run. When parking (or the launch) fails before the run is recorded, those sheets stay with
//! no run that owns them, and nothing sweeps them: a resubmit is a new run with its own.

use biffix_api::SubmittedRun;
use biffix_core::ids::RunId;
use biffix_workflow::formschema::FormSchema;

use crate::error::ServiceError;
use crate::runs::launch::LaunchPlan;
use crate::AppState;

/// Park a run whose submit-time images are still being prepared: its launch request goes to
/// `pending_launch` (params unredacted, for the coordinator), then `run_preparing` records the run
/// (params redacted) so it is listed, as `preparing_images`, at once. The sweep launches it once
/// its images are runnable. If recording the run fails its request stays parked: the record may
/// have landed (a lost reply), and if it did not the sweep drops the request of a run never
/// recorded once it is old enough not to be a submit still recording.
pub(super) async fn park(
    state: &AppState,
    run_id: &RunId,
    plan: &LaunchPlan,
    form: &FormSchema,
    effective_output_prefix: Option<String>,
    reasons: Vec<String>,
) -> Result<SubmittedRun, ServiceError> {
    let now = biffix_core::clock::epoch_micros();
    let parked = crate::store::pending::PendingLaunch {
        run_id: run_id.clone(),
        created_at_micros: now,
        plan: serde_json::to_value(plan).expect("a launch plan always serializes"),
    };
    state.store.put_pending_launch(&parked).await.map_err(|e| {
        ServiceError::reworded(e, |e| format!("parking the run's launch failed: {e}"))
    })?;
    let preparing = biffix_core::event::RunPreparing {
        workflow_version_id: plan.version_id.clone(),
        params: super::redaction::redact_secret_params(&form.fields, plan.params.clone()),
        input_uris: plan.input_uris.clone(),
        submitter: Some(biffix_core::event::user_submitter(&plan.actor)),
        submitter_label: plan.submitter_label.clone(),
        run_name: plan.run_name.clone(),
        output_prefix: effective_output_prefix,
        resumed_from: plan.resumed_from.as_ref().map(std::string::ToString::to_string),
        images: plan.images.clone(),
        experiment_id: plan.experiment_id.clone(),
    };
    let event = biffix_core::event::run_preparing(
        run_id,
        biffix_core::time::Timestamp::from_micros(now),
        preparing,
    );
    if let Err(e) = state.events.record(&event).await {
        tracing::warn!(
            run_id = %run_id,
            error = %e,
            "submit: recording a parked run failed; its launch stays parked for the sweep"
        );
        return Err(ServiceError::reworded(e, |e| format!("failed to persist run: {e}")));
    }
    tracing::info!(run_id = %run_id, waiting = ?reasons, "submit: run waits for its images");
    Ok(SubmittedRun {
        run_id: run_id.clone(),
        state: biffix_core::state::RunState::PreparingImages.as_str().to_string(),
        coordinator_job_id: None,
        assumed_role_arn: None,
    })
}
