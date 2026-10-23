//! `POST /runs/{id}/tasks/query`: a run's tasks one page at a time, for the UI's Jobs view —
//! filtered by state, group and a search term, sorted, and paged, with each task's dependents
//! (`feeds`) filled in so the client need not hold every task to show them. The query semantics
//! live in [`crate::runs::task_page`]. The filters ride in a JSON body, so a search term or a task key
//! (which can name a sample) never reaches a request URL.

use axum::extract::{Path, State};
use axum::{Extension, Json};

use biffix_core::ids::RunId;

pub(crate) use biffix_api::{TaskFilterCounts, TaskPage, TaskQueryRequest};

use crate::runs::task_page::{TaskQuery, TaskSort, TaskStateFilter};

use crate::api::ApiError;
use crate::auth::actor::AuthedActor;
use crate::config::queue_capacity::fill_queue_capacity;
use crate::runs::access::load_visible_run;
use crate::AppState;

/// Default and maximum page sizes.
const DEFAULT_PAGE: i64 = 200;
const MAX_PAGE: i64 = 1000;

/// Parse the optional query value `name` with `f`: absent ⇒ the default, unrecognized ⇒ a 400 that
/// names the value and the `accepted` spellings.
pub(crate) fn parse_param<T: Default>(
    name: &str,
    value: Option<&str>,
    f: fn(&str) -> Option<T>,
    accepted: &str,
) -> Result<T, ApiError> {
    match value {
        None => Ok(T::default()),
        Some(v) => {
            f(v).ok_or_else(|| ApiError::BadRequest(format!("unknown {name} `{v}` ({accepted})")))
        }
    }
}

/// `POST /runs/{run_id}/tasks/query` — one page of the run's tasks, with its filters in a JSON
/// body: a search term or a task key can name a sample, and a body is not logged the way a request
/// URL is. 400 for an unknown `state`, `sort` or `order` (checked only once the caller may see the
/// run, so a bad parameter never tells an unauthorized caller anything).
#[utoipa::path(
    post, path = "/runs/{run_id}/tasks/query",
    params(("run_id" = String, Path, description = "biffix run id")),
    request_body = TaskQueryRequest,
    responses((status = 200, body = TaskPage), (status = 400), (status = 403), (status = 404))
)]
pub(crate) async fn query_tasks(
    State(state): State<AppState>,
    Extension(actor): Extension<AuthedActor>,
    Path(run_id): Path<RunId>,
    Json(q): Json<TaskQueryRequest>,
) -> Result<Json<TaskPage>, ApiError> {
    let run = load_visible_run(&state, &actor, &run_id).await?;
    let query = TaskQuery {
        state: parse_param(
            "state",
            q.state.as_deref(),
            TaskStateFilter::parse,
            TaskStateFilter::ACCEPTED,
        )?,
        group: q.group.filter(|g| !g.is_empty()),
        q: q.q,
        // An empty key is no filter, as an empty group is.
        task_key: q.task_key.map(|k| k.0).filter(|k| !k.is_empty()),
        sort: parse_param("sort", q.sort.as_deref(), TaskSort::parse, TaskSort::ACCEPTED)?,
        reverse: parse_param(
            "order",
            q.order.as_deref(),
            |o| match o {
                "asc" => Some(false),
                "desc" => Some(true),
                _ => None,
            },
            "asc or desc",
        )?,
        offset: q.offset.unwrap_or(0).max(0),
        limit: q.limit.unwrap_or(DEFAULT_PAGE).clamp(1, MAX_PAGE),
    };
    let mut page =
        state.store.list_tasks(&run_id, &query, biffix_core::clock::epoch_secs()).await?;
    for task in &mut page.tasks {
        if let Some(host) = task.host.as_mut() {
            fill_queue_capacity(host, &state.deploy.queue_capacities);
        }
    }
    page.log_group =
        crate::runs::logs::project_task_log_group(&*state.projects.current().await, &run);
    Ok(Json(page))
}

// ── routes ───────────────────────────────────────────────────────────────────────────────────────

/// A run's task table: the routes behind the Cognito middleware.
pub(super) fn routes() -> axum::Router<AppState> {
    axum::Router::new().route("/runs/{run_id}/tasks/query", axum::routing::post(query_tasks))
}

/// A run's task table: the OpenAPI operations, tagged `tasks` in [`super::ApiDoc`].
#[derive(utoipa::OpenApi)]
#[openapi(paths(query_tasks), components(schemas(TaskPage, TaskFilterCounts, TaskQueryRequest)))]
pub(super) struct Doc;
