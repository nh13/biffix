//! Reading one run for a caller: the one seam every run-scoped read goes through.

use biffix_core::ids::RunId;

use crate::auth::actor::{authorize, AuthedActor};
use crate::auth::rbac::Capability;
use crate::error::ServiceError;
use crate::store::RunDetailView;
use crate::AppState;

/// How much of a run a read loads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RunLoad {
    /// The run without its task table (`tasks` may be empty; `task_groups` and `cost` are filled).
    /// Most reads need the run, not its (possibly thousands of) tasks.
    Light,
    /// The run with every task.
    Full,
}

/// Authorize `ViewRuns` and load a run's detail — the one seam every run-scoped read goes through
/// (get-run, cancel, task-logs, failure summary, outputs, provenance, …), so project-scoped
/// visibility becomes a change here, not at each call site. Every AWS coordinate a downstream
/// handler touches (a task's `log_stream`, the output prefix) is derived from what this returns,
/// never from client input.
pub(crate) async fn load_run(
    state: &AppState,
    actor: &AuthedActor,
    run_id: &RunId,
    load: RunLoad,
) -> Result<RunDetailView, ServiceError> {
    authorize(state, actor, Capability::ViewRuns)?;
    let loaded = match load {
        RunLoad::Light => state.store.get_run_light(run_id).await,
        RunLoad::Full => state.store.get_run(run_id).await,
    };
    loaded.map_err(ServiceError::status_only)
}

/// [`load_run`] without the task table — what most run-scoped reads need.
pub(crate) async fn load_visible_run(
    state: &AppState,
    actor: &AuthedActor,
    run_id: &RunId,
) -> Result<RunDetailView, ServiceError> {
    load_run(state, actor, run_id, RunLoad::Light).await
}
