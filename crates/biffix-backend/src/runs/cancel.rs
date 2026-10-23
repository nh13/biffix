//! Cancelling a run ([`cancel_run`]): the request recorded, then its coordinator and live task jobs
//! terminated, and the run ended when no terminal would otherwise arrive.

use biffix_core::ids::RunId;
use biffix_core::state::{RunState, TaskState};

use crate::auth::actor::AuthedActor;
use crate::error::ServiceError;
use crate::runs::access::{load_run, RunLoad};
use crate::store::{RunDetailView, StoreError};
use crate::AppState;

/// The most child jobs a cancel terminates inline; any beyond are left to the reconcile Lambda's
/// ended-run sweep (within ~5 min), keeping the request well inside its Lambda timeout.
const MAX_INLINE_CHILD_TERMINATES: usize = 200;

/// A user who cancels their own run is not told it was cancelled: once their request is recorded,
/// the `cancelled` notification the run's fold will produce (`<run>:cancelled`, deterministic) is
/// marked read for them ahead of it. Reads are user state, so writing one ahead keeps
/// `projection == fold`; should the run end another way (it succeeds or fails first) that read
/// names a notification that never exists and the real one still notifies. Another user's cancel
/// of the run notifies its submitter as usual. Best-effort: the cancel is already recorded.
async fn read_own_cancel_ahead(state: &AppState, detail: &RunDetailView, actor_id: &str) {
    let own_run = detail.run.submitter.as_deref()
        == Some(biffix_core::event::user_submitter(actor_id).as_str());
    if !own_run {
        return;
    }
    let id = biffix_core::reducer::notification_id(detail.run.run_id.as_str(), RunState::Cancelled);
    if let Err(e) = state.store.read_notification_ahead(actor_id, &id).await {
        tracing::warn!(run_id = %detail.run.run_id, error = %e, "cancel: marking the user's own cancel read failed");
    }
}

/// How long a cancel waits for a launch in progress (the sweep holding the run's launch claim)
/// before it asks to be retried: a launch is a few AWS calls. Inside the API Lambda's timeout
/// ([`crate::config::REQUEST_WAIT_BUDGET`]), with room for the cancel's own work after it, so a cancel
/// that waits it out gets its 503 rather than the gateway's timeout.
const CANCEL_LAUNCH_WAIT: std::time::Duration = crate::config::REQUEST_WAIT_BUDGET;

/// How often a waiting cancel tries the claim again.
const CANCEL_LAUNCH_POLL: std::time::Duration = std::time::Duration::from_millis(500);

/// The run's launch claim for a cancel: tried (never blocking a pooled connection) every
/// [`CANCEL_LAUNCH_POLL`] for up to [`CANCEL_LAUNCH_WAIT`], then 503 so the caller retries.
async fn claim_launch_for_cancel(
    state: &AppState,
    rid: &RunId,
) -> Result<crate::store::Claim, ServiceError> {
    let tries = CANCEL_LAUNCH_WAIT.as_millis() / CANCEL_LAUNCH_POLL.as_millis();
    for attempt in 0..=tries {
        if attempt > 0 {
            tokio::time::sleep(CANCEL_LAUNCH_POLL).await;
        }
        let claim = state.store.claim_run_launch(rid).await.map_err(|e| {
            ServiceError::reworded(e, |e| format!("claiming the run's launch: {e}"))
        })?;
        if let Some(claim) = claim {
            return Ok(claim);
        }
    }
    Err(ServiceError::Unavailable("the run is being launched; retry the cancel in a moment".into()))
}

/// A run's full detail for a cancel (the live child jobs to terminate come from the task table).
async fn load_for_cancel(
    state: &AppState,
    actor: &AuthedActor,
    rid: &RunId,
) -> Result<RunDetailView, ServiceError> {
    load_run(state, actor, rid, RunLoad::Full).await.map_err(|e| {
        let msg = match e {
            ServiceError::NotFound(_) => "unknown run",
            _ => e.reason(),
        };
        e.with_message(msg)
    })
}

/// Terminate the run's live task jobs (the current attempt of every running or queued task),
/// concurrently and best-effort: a failure is logged, and the ended-run sweep retries it.
async fn terminate_live_children(state: &AppState, detail: &RunDetailView, reason: &str) {
    let live: Vec<String> = detail
        .tasks
        .iter()
        .filter(|t| [TaskState::Running, TaskState::Queued].iter().any(|s| s.as_str() == t.state))
        .filter_map(|t| t.batch_job_id.as_ref().map(|j| j.0.clone()))
        .take(MAX_INLINE_CHILD_TERMINATES)
        .collect();
    let mut calls = tokio::task::JoinSet::new();
    for job in live {
        let cloud = state.cloud.clone();
        let reason = reason.to_string();
        calls.spawn(async move {
            if let Err(e) = cloud.terminate_job(&job, &reason).await {
                tracing::warn!(job, error = %e, "cancel: terminating a child job failed (the sweep will retry)");
            }
        });
    }
    while calls.join_next().await.is_some() {}
}

/// Cancel run `rid` for `actor` (`actor_id`, whom the caller has authorized to launch): record the
/// request, terminate the coordinator and the live task jobs, and end the run when no terminal
/// would otherwise arrive. See `POST /runs/{id}/cancel` for the rules; an already finished run is a
/// no-op.
pub(crate) async fn cancel_run(
    state: &AppState,
    actor: &AuthedActor,
    actor_id: &str,
    rid: &RunId,
) -> Result<(), ServiceError> {
    let mut detail = load_for_cancel(state, actor, rid).await?;
    // No coordinator yet: hold the run's launch (held to the end of the cancel), and see where the
    // run stands now that no sweep can be launching it.
    let mut _launch_claim = None;
    if detail.coordinator_job_id.is_none()
        && !crate::runs::detail::state_is_terminal(&detail.run.state)
    {
        _launch_claim = Some(claim_launch_for_cancel(state, rid).await?);
        detail = load_for_cancel(state, actor, rid).await?;
    }
    let state_now = detail.run.state.parse::<RunState>().ok();
    if state_now.is_some_and(RunState::is_terminal) {
        return Ok(());
    }
    let end_now = state_now != Some(RunState::Running);
    let record_err = |e: StoreError| {
        ServiceError::reworded(e, |e| format!("failed to record cancellation: {e}"))
    };
    let now = || biffix_core::time::Timestamp::from_epoch_seconds(biffix_core::clock::epoch_secs());
    // Record the request BEFORE terminating, so the coordinator's resulting FAILED is stamped after
    // it and folds to CANCELLED whichever arrives first. (A retry's `cancel_requested` dedups onto the first.)
    // A human label for who asked, snapshotted onto the request like a run's submitter label.
    let actor_label = crate::auth::actor::actor_label(state, actor).await;
    let mut events =
        vec![biffix_core::event::cancel_requested(rid, now(), actor_id, actor_label.as_deref())];
    let Some(job) = &detail.coordinator_job_id else {
        // No coordinator to terminate (and none to report a terminal): end the run now.
        if end_now {
            events.push(biffix_core::event::run_cancelled(rid, now()));
        }
        state.events.record_all(&events).await.map_err(record_err)?;
        read_own_cancel_ahead(state, &detail, actor_id).await;
        // A run waiting for its images has a parked launch: drop it while the claim is held (if
        // this fails, the launch sweep drops the launch of a cancelled run).
        if let Err(e) = state.store.delete_pending_launch(rid).await {
            tracing::warn!(run_id = %rid, error = %e, "cancel: dropping the run's parked launch failed (the sweep will)");
        }
        return Ok(());
    };
    state.events.record_all(&events).await.map_err(record_err)?;
    read_own_cancel_ahead(state, &detail, actor_id).await;

    let reason = format!("cancelled via biffix by {actor_id}");
    state.cloud.terminate_job(job.as_str(), &reason).await.map_err(|e| {
        ServiceError::Upstream(format!(
            "cancel recorded, but terminating the coordinator failed (retry to re-attempt): {e}"
        ))
    })?;
    if end_now {
        // Only now, with the coordinator terminated: a failed terminate leaves the run
        // non-terminal, so a retry reaches the terminate again.
        state
            .events
            .record_all(&[biffix_core::event::run_cancelled(rid, now())])
            .await
            .map_err(record_err)?;
    }
    terminate_live_children(state, &detail, &reason).await;
    Ok(())
}
