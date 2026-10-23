//! Run endpoints: list runs, get one run's detail, its task logs, attempts and failure
//! summary — served from the projection — and cancel a run.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::{Extension, Json};

use biffix_core::ids::RunId;

// The run wire types are the shared contract (biffix-api); how a run is shown in them is
// `crate::runs::detail`'s.
pub(crate) use biffix_api::{
    CoordinatorLogsQuery, DagNode, FailureItem, FailureSummary, Host, LogLine, LogPage,
    OutputPrefixKind, RunDag, RunDetail, RunDetailQuery, RunPage, RunQueryRequest,
    RunRenameRequest, RunSummary, Task, TaskAttempt, TaskAttemptList, TaskAttemptsRequest,
    TaskLogsRequest,
};

use crate::api::ApiError;
use crate::auth::actor::{authorize, AuthedActor};
use crate::auth::rbac::Capability;
use crate::error::ServiceError;
use crate::runs::access::{load_run, load_visible_run, RunLoad};
use crate::runs::detail::{sample_grid_of, summary_with_cost, with_list_facts};
use crate::store::runs::{RunFilter, RunQuery};
use crate::store::RunDetailView;
use crate::AppState;

/// Parse the `filter` param. Absent means "all"; an *unrecognized* value is a client error that
/// names the accepted spellings, not a silent fall-through to all.
fn parse_filter(s: Option<&str>) -> Result<RunFilter, ApiError> {
    crate::api::tasks::parse_param("filter", s, RunFilter::parse, RunFilter::ACCEPTED)
}

/// List runs, filterable by state, searchable, and paginated.
#[utoipa::path(
    get, path = "/runs",
    params(RunQueryRequest),
    responses((status = 200, body = RunPage), (status = 400), (status = 403))
)]
pub(crate) async fn list_runs(
    State(state): State<AppState>,
    Extension(actor): Extension<AuthedActor>,
    Query(q): Query<RunQueryRequest>,
) -> Result<Json<RunPage>, ApiError> {
    run_list(&state, &actor, q).await
}

/// `POST /runs/query` — `GET /runs` with its parameters in a JSON body, the form the UI uses: the
/// search text can be a run name, which can name a sample, and a body is not logged the way a
/// request URL is. Same answers.
#[utoipa::path(
    post, path = "/runs/query",
    request_body = RunQueryRequest,
    responses((status = 200, body = RunPage), (status = 400), (status = 403))
)]
pub(crate) async fn query_runs(
    State(state): State<AppState>,
    Extension(actor): Extension<AuthedActor>,
    Json(q): Json<RunQueryRequest>,
) -> Result<Json<RunPage>, ApiError> {
    run_list(&state, &actor, q).await
}

/// A page of runs for `actor`, however the request carried its parameters.
async fn run_list(
    state: &AppState,
    actor: &AuthedActor,
    q: RunQueryRequest,
) -> Result<Json<RunPage>, ApiError> {
    let actor_id = authorize(state, actor, Capability::ViewRuns)?;
    let filter = parse_filter(q.filter.as_deref())?;
    let attention = q.attention.unwrap_or(false);
    // A list of ids names a page's worth of runs at most: more is refused, not cut short.
    let ids: Vec<String> =
        q.ids.iter().map(|id| id.as_str().trim().to_string()).filter(|id| !id.is_empty()).collect();
    let max = crate::store::runs::MAX_RUN_PAGE;
    if ids.len() > max as usize {
        return Err(ApiError::BadRequest(format!(
            "`ids` names {} runs; at most {max} per request",
            ids.len()
        )));
    }
    let query = RunQuery {
        filter,
        q: q.q.filter(|s| !s.trim().is_empty()),
        workflow: q.workflow.map(|w| w.0).filter(|s| !s.trim().is_empty()),
        // Needing the caller's attention is about their own runs, so it implies `mine`.
        submitter: (q.mine.unwrap_or(false) || attention)
            .then(|| biffix_core::event::user_submitter(&actor_id)),
        attention: attention.then(|| actor_id.clone()),
        // Trimmed, as submit and `POST /runs/{id}/experiment` trim the id they record.
        experiment: q.experiment.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()),
        // A list of ids, even of blank ones only, names those runs: never "any run".
        ids: (!q.ids.is_empty()).then_some(ids),
        limit: q.limit.unwrap_or(50),
        after: q.after.filter(|s| !s.is_empty()),
        before: q.before.filter(|s| !s.is_empty()),
    };
    let page = state.store.list_runs(&query).await.map_err(ApiError::status_only)?;
    Ok(Json(RunPage {
        runs: with_list_facts(state, page.runs).await,
        total: page.total,
        limit: query.page_size(),
        first_cursor: page.first_cursor,
        last_cursor: page.last_cursor,
    }))
}

/// Get one run's detail (status + task table).
#[utoipa::path(
    get, path = "/runs/{run_id}",
    params(
        ("run_id" = String, Path, description = "biffix run id"),
        RunDetailQuery,
    ),
    responses((status = 200, body = RunDetail), (status = 403), (status = 404))
)]
pub(crate) async fn get_run(
    State(state): State<AppState>,
    Extension(actor): Extension<AuthedActor>,
    Path(run_id): Path<RunId>,
    Query(q): Query<RunDetailQuery>,
) -> Result<Json<RunDetail>, ApiError> {
    let with_tasks = q.tasks != Some(false);
    let load = if with_tasks { RunLoad::Full } else { RunLoad::Light };
    let d = load_run(&state, &actor, &run_id, load).await?;
    Ok(Json(crate::runs::detail::run_detail(&state, d, with_tasks).await))
}

/// `POST /runs/{run_id}/task-logs/query` — a page of one task's CloudWatch log stream, served
/// through biffix's own auth (ViewRuns) so a scientist needs a biffix login, not an AWS account, to
/// read why their job failed. The stream is derived server-side from the authorized task; the client
/// supplies only the run id, task key, and an opaque paging cursor. They ride in a JSON body: the
/// task key names a sample, and a body is not logged the way a request URL is.
#[utoipa::path(
    post, path = "/runs/{run_id}/task-logs/query",
    params(("run_id" = String, Path, description = "biffix run id")),
    request_body = TaskLogsRequest,
    responses((status = 200, body = LogPage), (status = 403), (status = 404))
)]
pub(crate) async fn query_task_logs(
    State(state): State<AppState>,
    Extension(actor): Extension<AuthedActor>,
    Path(run_id): Path<RunId>,
    Json(q): Json<TaskLogsRequest>,
) -> Result<Json<LogPage>, ApiError> {
    let run = load_visible_run(&state, &actor, &run_id).await?;
    Ok(Json(crate::runs::logs::task_log_page(&state, &run, q).await?))
}

/// `GET /runs/{run_id}/samples` — the run's samples × steps (the Lab view's per-sample progress):
/// each sample's state and its cell per step, the whole-study steps, in pipeline order.
#[utoipa::path(
    get, path = "/runs/{run_id}/samples",
    params(("run_id" = String, Path, description = "biffix run id")),
    responses(
        (status = 200, body = Object, description = "biffix_core::sample_progress::SampleGrid"),
        (status = 403),
        (status = 404)
    )
)]
pub(crate) async fn run_samples(
    State(state): State<AppState>,
    Extension(actor): Extension<AuthedActor>,
    Path(rid): Path<RunId>,
) -> Result<Json<biffix_core::sample_progress::SampleGrid>, ApiError> {
    // The light load reads no task rows (only per-group counts), so `sample_facts` is not a
    // second read of them.
    let run = load_visible_run(&state, &actor, &rid).await?;
    sample_grid_of(&state, &run).await.map(Json).map_err(ApiError::status_only)
}

/// `GET /runs/{run_id}/failure-summary` — why a failed run failed, in plain words: its first failed
/// steps (earliest first), each with its cause, what to try, and the end of its log; or the workflow
/// engine's own failure when no step failed. Empty for a run that did not fail.
#[utoipa::path(
    get, path = "/runs/{run_id}/failure-summary",
    params(("run_id" = String, Path, description = "biffix run id")),
    responses((status = 200, body = FailureSummary), (status = 403), (status = 404))
)]
pub(crate) async fn failure_summary(
    State(state): State<AppState>,
    Extension(actor): Extension<AuthedActor>,
    Path(run_id): Path<RunId>,
) -> Result<Json<FailureSummary>, ApiError> {
    let run = load_visible_run(&state, &actor, &run_id).await?;
    Ok(Json(crate::runs::failure::failure_summary(&state, &run).await?))
}

/// `POST /runs/{run_id}/attempts/query` — every AWS Batch attempt of one task (e.g. a
/// spot-reclaimed attempt and its successful retry), each with its own exit code, reason, log
/// stream and host. Empty for a task that never became a Batch job (a local rule, a cache hit). The
/// task key rides in a JSON body: it names a sample, and must not reach a request URL.
#[utoipa::path(
    post, path = "/runs/{run_id}/attempts/query",
    params(("run_id" = String, Path, description = "biffix run id")),
    request_body = TaskAttemptsRequest,
    responses((status = 200, body = TaskAttemptList), (status = 403), (status = 404))
)]
pub(crate) async fn query_task_attempts(
    State(state): State<AppState>,
    Extension(actor): Extension<AuthedActor>,
    Path(run_id): Path<RunId>,
    Json(q): Json<TaskAttemptsRequest>,
) -> Result<Json<TaskAttemptList>, ApiError> {
    load_visible_run(&state, &actor, &run_id).await?;
    Ok(Json(crate::runs::logs::task_attempts(&state, &run_id, q.task.as_str()).await?))
}

/// `GET /runs/{run_id}/coordinator-logs` — a page of the run's coordinator (workflow driver) log,
/// served through biffix like task logs. Where a run that failed before scheduling any task (clone,
/// staging, or DAG errors) explains itself. Empty page when no stream was recorded: the coordinator
/// has not started, or the run predates coordinator log tracking.
#[utoipa::path(
    get, path = "/runs/{run_id}/coordinator-logs",
    params(
        ("run_id" = String, Path, description = "biffix run id"),
        CoordinatorLogsQuery,
    ),
    responses((status = 200, body = LogPage), (status = 403), (status = 404))
)]
pub(crate) async fn coordinator_logs(
    State(state): State<AppState>,
    Extension(actor): Extension<AuthedActor>,
    Path(run_id): Path<RunId>,
    Query(q): Query<CoordinatorLogsQuery>,
) -> Result<Json<LogPage>, ApiError> {
    let detail = load_visible_run(&state, &actor, &run_id).await?;
    // Coordinators always log to the shared group.
    let page = crate::runs::logs::log_page(
        &state,
        None,
        detail.coordinator_log_stream,
        q.cursor.as_deref(),
        q.direction.as_deref(),
        q.limit,
    )
    .await?;
    Ok(Json(page))
}

/// Cancel a run: record who asked (`cancel_requested`, which makes a failure terminal stamped after
/// it fold to CANCELLED), then terminate its coordinator Batch job and its live task jobs.
/// The engine does not reliably stop its children when its head job is terminated (Nextflow leaves
/// them running), so the children are terminated here too, best-effort and concurrently; the
/// reconcile Lambda's ended-run sweep terminates any that remain — so cancel stops the spend, not
/// just the tracking.
///
/// A run with no coordinator yet (one waiting for its submit-time images) may be one the launch
/// sweep is launching at this moment, so its cancel takes the run's launch claim first (waiting up
/// to 20 s for a launch in progress, else 503: retry), and re-reads the run under it: a
/// coordinator bound meanwhile is stopped as any other; otherwise the run ends and its parked
/// launch is dropped while the claim is held, so no sweep launches it after.
///
/// A run not yet confirmed running (`submitted`) is also marked cancelled (`run_cancelled`): its
/// coordinator may never have started (or never will report), so no terminal would otherwise
/// arrive. That terminal is recorded only once the coordinator's terminate succeeded (or there is
/// no coordinator to terminate), so a run is never shown cancelled while its coordinator may still
/// start. An already finished run is an idempotent no-op. If terminating the coordinator fails, the
/// request stays recorded but the run is not ended, and the call returns 502 so the caller knows
/// the run may still be running; a retry re-attempts the terminate.
#[utoipa::path(
    post, path = "/runs/{run_id}/cancel",
    params(("run_id" = String, Path, description = "biffix run id")),
    responses(
        (status = 202, description = "cancellation recorded"),
        (status = 403),
        (status = 404),
        (status = 502, description = "the cancel is recorded, but terminating the coordinator failed; retry"),
        (status = 503, description = "the run is being launched; retry")
    )
)]
pub(crate) async fn cancel_run(
    State(state): State<AppState>,
    Extension(actor): Extension<AuthedActor>,
    Path(run_id): Path<RunId>,
) -> Result<StatusCode, ApiError> {
    let actor_id = authorize(&state, &actor, Capability::LaunchWorkflow)?;
    crate::runs::cancel::cancel_run(&state, &actor, &actor_id, &run_id).await?;
    Ok(StatusCode::ACCEPTED)
}

/// The 400 for a rename body that does not name `run_name` (or is not JSON).
pub(crate) const RUN_RENAME_BODY_ERROR: &str =
    "the body must be JSON naming `run_name`: a name, or null to clear it";

/// Rename a run, or clear its name (`null`, or a blank name) so readers show a derived one. Allowed
/// to whoever may control the run, as for cancelling it or moving it between experiments
/// (`LaunchWorkflow` on a run they can see). The body must name `run_name` (a 400 otherwise, so a
/// misspelled field is not read as "clear"). Recorded as the run's `run_renamed` event, so the name
/// folds into the projection and a replay keeps it; returns the run's summary after.
#[utoipa::path(
    post, path = "/runs/{run_id}/name",
    params(("run_id" = String, Path, description = "biffix run id")),
    request_body = RunRenameRequest,
    responses(
        (status = 200, body = RunSummary),
        (status = 400, description = "no `run_name` in the body, or the name is too long"),
        (status = 403),
        (status = 404, description = "unknown run"),
    )
)]
pub(crate) async fn rename_run(
    State(state): State<AppState>,
    Extension(actor): Extension<AuthedActor>,
    Path(rid): Path<RunId>,
    body: Result<Json<RunRenameRequest>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<RunSummary>, ApiError> {
    let actor_id = crate::auth::actor::authorize_msg(&state, &actor, Capability::LaunchWorkflow)?;
    let req = json_body_or_400(body, RUN_RENAME_BODY_ERROR)?;
    let name = crate::api::non_blank(req.run_name.as_deref());
    let max = crate::runs::submit::request::MAX_RUN_NAME_CHARS;
    if name.is_some_and(|n| n.chars().count() > max) {
        return Err(ApiError::BadRequest(format!("run_name must be at most {max} characters")));
    }
    visible_run_or_msg(&state, &actor, &rid).await?;
    let event = biffix_core::event::run_renamed(
        &rid,
        biffix_core::time::Timestamp::from_micros(biffix_core::clock::epoch_micros()),
        name,
        &actor_id,
    );
    record_then_summary(&state, &rid, &event).await
}

/// A control-plane run mutation's JSON body, or a 400 with `msg` when it is not JSON or not the
/// expected shape (a missing or mistyped field); any other rejection (say, no JSON content type)
/// keeps its own status and text.
pub(crate) fn json_body_or_400<T>(
    body: Result<Json<T>, axum::extract::rejection::JsonRejection>,
    msg: &str,
) -> Result<T, ApiError> {
    use axum::extract::rejection::JsonRejection;
    body.map(|Json(t)| t).map_err(|rejection| match rejection {
        JsonRejection::JsonDataError(_) | JsonRejection::JsonSyntaxError(_) => {
            ApiError::BadRequest(msg.to_string())
        }
        other => ApiError::Rejection(other),
    })
}

/// [`load_visible_run`] with a message: `unknown run <id>` for a 404, else `could not load run
/// <id>`.
pub(crate) async fn visible_run_or_msg(
    state: &AppState,
    actor: &AuthedActor,
    rid: &RunId,
) -> Result<RunDetailView, ApiError> {
    load_visible_run(state, actor, rid).await.map_err(|e| {
        let msg = match e {
            ServiceError::NotFound(_) => format!("unknown run {rid}"),
            _ => format!("could not load run {rid}"),
        };
        e.with_message(msg).into()
    })
}

/// Record a control-plane event about run `rid`, then answer with the run's summary after it.
pub(crate) async fn record_then_summary(
    state: &AppState,
    rid: &RunId,
    event: &biffix_core::event::RunEvent,
) -> Result<Json<RunSummary>, ApiError> {
    state.events.record(event).await?;
    let after = state.store.get_run_light(rid).await?;
    Ok(Json(summary_with_cost(after.run, after.cost.as_ref())))
}

// ── routes ───────────────────────────────────────────────────────────────────────────────────────

/// Runs: list, detail, logs, attempts, failure summary, samples, cancel and rename: the routes
/// behind the Cognito middleware.
pub(super) fn routes() -> axum::Router<AppState> {
    axum::Router::new()
        .route("/runs", axum::routing::get(list_runs))
        .route("/runs/query", axum::routing::post(query_runs))
        .route("/runs/{run_id}", axum::routing::get(get_run))
        .route("/runs/{run_id}/task-logs/query", axum::routing::post(query_task_logs))
        .route("/runs/{run_id}/attempts/query", axum::routing::post(query_task_attempts))
        .route("/runs/{run_id}/failure-summary", axum::routing::get(failure_summary))
        .route("/runs/{run_id}/samples", axum::routing::get(run_samples))
        .route("/runs/{run_id}/coordinator-logs", axum::routing::get(coordinator_logs))
        .route("/runs/{run_id}/cancel", axum::routing::post(cancel_run))
        .route("/runs/{run_id}/name", axum::routing::post(rename_run))
}

/// Runs: list, detail, logs, attempts, failure summary, samples, cancel and rename: the OpenAPI
/// operations, tagged `runs` in [`super::ApiDoc`].
#[derive(utoipa::OpenApi)]
#[openapi(
    paths(
        list_runs,
        query_runs,
        get_run,
        query_task_logs,
        query_task_attempts,
        failure_summary,
        run_samples,
        coordinator_logs,
        cancel_run,
        rename_run
    ),
    components(schemas(
        RunSummary,
        biffix_api::RunSampleCounts,
        RunPage,
        RunDetail,
        OutputPrefixKind,
        Task,
        RunDag,
        DagNode,
        LogPage,
        TaskLogsRequest,
        TaskAttemptsRequest,
        RunQueryRequest,
        TaskAttemptList,
        TaskAttempt,
        Host,
        biffix_api::JobCost,
        biffix_api::RunCost,
        biffix_api::CostByInstanceType,
        LogLine,
        FailureSummary,
        FailureItem,
        biffix_api::RunProvenance,
        biffix_api::SubmittedImage,
        biffix_api::RunPreparation,
        biffix_api::RuntimeUse,
        biffix_api::RunRenameRequest
    ))
)]
pub(super) struct Doc;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_filter_is_all_when_absent_and_refused_when_unknown() {
        assert_eq!(parse_filter(None).ok(), Some(RunFilter::All));
        assert_eq!(parse_filter(Some("active")).ok(), Some(RunFilter::Active));
        assert_eq!(parse_filter(Some("failed")).ok(), Some(RunFilter::Failed));
        assert_eq!(parse_filter(Some("all")).ok(), Some(RunFilter::All));
        assert!(parse_filter(Some("bogus")).is_err());
        assert!(parse_filter(Some("")).is_err());
    }
}
