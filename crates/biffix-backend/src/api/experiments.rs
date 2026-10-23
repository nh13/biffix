//! Experiment endpoints: list, create, read and change experiments, and put a run
//! in one (`POST /runs/{id}/experiment`, recorded as the run's `experiment_set` event).
//!
//! An experiment lives in a project and grants nothing: reading one needs `ViewRuns`; creating,
//! renaming, describing, archiving it and adding or removing runs need `LaunchWorkflow`.
//!
//! Visibility seam: the experiment reads (the list, the detail's runs, samples and cost, and the
//! samples CSV) read the projection directly, not run by run through `runs::access::load_run`, the
//! seam every run-scoped read goes through. That is sound under the current flat RBAC, where
//! `ViewRuns` sees every run. When run visibility becomes project-scoped, the same scoping must
//! also apply here: check the experiment's project in `list_experiments`, `get_experiment` and
//! `experiment_samples_csv` (which reads every run, not only the newest page), as `load_run` will
//! check a run's.

use std::collections::{BTreeMap, HashMap};

use tokio::time::Instant;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::Response;
use axum::{Extension, Json};

use biffix_api::{
    Experiment, ExperimentCreateRequest, ExperimentDetail, ExperimentList, ExperimentListQuery,
    ExperimentSample, ExperimentSamplesCsvQuery, ExperimentUpdateRequest, RunExperimentSetRequest,
    RunSummary,
};
use biffix_core::ids::RunId;

use axum::extract::rejection::JsonRejection;

use crate::api::error::not_found_as;
use crate::api::ApiError;
use crate::auth::actor::{authorize_msg, AuthedActor};
use crate::auth::rbac::Capability;
use crate::runs::experiments::{
    joinable_experiment, new_id, normalize_description, normalize_name,
};
use crate::runs::samples_csv::{
    samples_csv_body, samples_csv_filename, NameZone, SamplesCsvLimits,
};
use crate::store::experiments::SubmitterRuns;
use crate::store::experiments::{ExperimentChange, ExperimentRecord};
use crate::store::runs::RunStats;
use crate::store::StoreError;
use crate::AppState;

/// The most runs `GET /experiments/{id}` returns (the newest); `runs_total` has the true count and
/// the run list's `?experiment=` pages through them all. Bounds a read the UI re-polls while a run
/// is live. One run-list page, so the store serves it in one read.
pub(crate) const MAX_DETAIL_RUNS: i64 = crate::store::runs::MAX_RUN_PAGE;

/// The 400 for a `POST /runs/{id}/experiment` body that is not `{"experiment_id": <id or null>}`
/// (the field absent or misspelled, or not a string or null).
pub(crate) const RUN_EXPERIMENT_BODY_ERROR: &str =
    "the body must name `experiment_id`: an experiment id, or null to take the run out of its \
     experiment";

/// A store failure reading experiment `id`: a 404 naming it when it is not there.
fn experiment_store_error(id: &str) -> impl FnOnce(StoreError) -> ApiError + '_ {
    not_found_as(move || format!("unknown experiment {id}"))
}

/// The experiment `id`, or a 404 naming it.
async fn experiment_or_404(state: &AppState, id: &str) -> Result<ExperimentRecord, ApiError> {
    state.experiments.get(id).await.map_err(experiment_store_error(id))
}

/// The caller's runs per experiment, and how many need their attention
/// ([`crate::store::experiments::ExperimentReads::experiment_runs_of`]), in every experiment or
/// (`only`) in one: for `?mine=`, `mine` and `needs_attention`.
async fn caller_runs(
    state: &AppState,
    actor_id: &str,
    only: Option<&str>,
) -> Result<HashMap<String, SubmitterRuns>, StoreError> {
    state
        .store
        .experiment_runs_of(&biffix_core::event::user_submitter(actor_id), actor_id, only)
        .await
}

/// `record`'s wire view for the caller `actor_id`, with its runs' `stats` and the caller's runs per
/// experiment (`caller`, [`caller_runs`]).
fn view_for(
    record: &ExperimentRecord,
    stats: Option<RunStats>,
    actor_id: &str,
    caller: &HashMap<String, SubmitterRuns>,
) -> Experiment {
    let attention = caller.get(&record.id).map_or(0, |r| r.attention);
    record.to_view(stats, record.is_mine(actor_id, caller), attention)
}

/// List experiments, newest activity first: the later of the newest run's submit and the
/// experiment's creation ([`ExperimentRecord::activity_micros`]), ties by id. `?mine=true` keeps
/// the caller's: those they created or submitted a run into. Each carries whether it is the
/// caller's (`mine`) and how many of the caller's runs in it need their attention.
#[utoipa::path(
    get, path = "/experiments",
    params(ExperimentListQuery),
    responses((status = 200, body = ExperimentList), (status = 403))
)]
pub(crate) async fn list_experiments(
    State(state): State<AppState>,
    Extension(actor): Extension<AuthedActor>,
    Query(q): Query<ExperimentListQuery>,
) -> Result<Json<ExperimentList>, ApiError> {
    let actor_id = authorize_msg(&state, &actor, Capability::ViewRuns)?;
    let project = crate::api::non_blank(q.project.as_deref());
    let (mut records, caller) = tokio::try_join!(
        state.experiments.list(project, q.archived.unwrap_or(false)),
        caller_runs(&state, &actor_id, None),
    )?;
    if q.mine.unwrap_or(false) {
        records.retain(|e| e.is_mine(&actor_id, &caller));
    }
    let ids: Vec<String> = records.iter().map(|e| e.id.clone()).collect();
    let stats = state.store.experiment_run_stats(&ids).await?;
    // Most recently active first, then by id (descending).
    records.sort_by_cached_key(|e| {
        std::cmp::Reverse((e.activity_micros(stats.get(&e.id)), e.id.clone()))
    });
    Ok(Json(ExperimentList {
        experiments: records
            .iter()
            .map(|e| view_for(e, stats.get(&e.id).copied(), &actor_id, &caller))
            .collect(),
    }))
}

/// Create an experiment in a project (one a registered workflow is in).
#[utoipa::path(
    post, path = "/experiments",
    request_body = ExperimentCreateRequest,
    responses(
        (status = 201, body = Experiment),
        (status = 400, description = "a bad name or description"),
        (status = 403),
        (status = 404, description = "no registered workflow is in the project"),
        (status = 409, description = "the project already has a live experiment of that name"),
    )
)]
pub(crate) async fn create_experiment(
    State(state): State<AppState>,
    Extension(actor): Extension<AuthedActor>,
    Json(req): Json<ExperimentCreateRequest>,
) -> Result<(StatusCode, Json<Experiment>), ApiError> {
    let actor_id = authorize_msg(&state, &actor, Capability::LaunchWorkflow)?;
    let bad = |m: String| ApiError::BadRequest(m);
    let name = normalize_name(&req.name).map_err(bad)?;
    let description = normalize_description(req.description.as_deref()).map_err(bad)?;
    let project = req.project.trim().to_string();
    let known = state.registry.has_project(&project).await?;
    if !known {
        return Err(ApiError::NotFound(format!(
            "unknown project {project}: no registered workflow is in it"
        )));
    }
    let record = ExperimentRecord {
        id: new_id(),
        project,
        name,
        description,
        created_by: Some(actor_id),
        created_by_label: crate::auth::actor::actor_label(&state, &actor).await,
        created_at_micros: biffix_core::clock::epoch_micros(),
        archived_at_micros: None,
    };
    state.experiments.create(&record).await?;
    Ok((StatusCode::CREATED, Json(record.to_view(None, true, 0))))
}

/// The samples across `runs` (newest first): one row per sample id, in first-seen order over the
/// runs oldest first, with its state in each run that has it.
async fn experiment_samples(
    state: &AppState,
    runs: &[RunSummary],
) -> Result<Vec<ExperimentSample>, ApiError> {
    let ids: Vec<RunId> = runs.iter().map(|r| r.run_id.clone()).collect();
    let facts = state.store.runs_sample_facts(&ids).await?;
    let mut order: Vec<String> = Vec::new();
    let mut by_sample: HashMap<String, BTreeMap<String, String>> = HashMap::new();
    for run in runs.iter().rev() {
        let Some(facts) = facts.get(run.run_id.as_str()) else {
            continue;
        };
        let grid = crate::runs::detail::grid_from_facts(facts, &run.state);
        for row in grid.samples {
            let cells = by_sample.entry(row.id.clone()).or_insert_with(|| {
                order.push(row.id.clone());
                BTreeMap::new()
            });
            cells.insert(run.run_id.to_string(), row.state.as_str().to_string());
        }
    }
    Ok(order
        .into_iter()
        .map(|id| ExperimentSample { by_run: by_sample.remove(&id).unwrap_or_default(), id })
        .collect())
}

/// One experiment with its newest runs (newest first, at most [`MAX_DETAIL_RUNS`]), the samples
/// across those runs, and the summed cost of all of its runs. A bounded read whatever the
/// experiment's size: its run count and newest run (one aggregate), one page of runs (no count),
/// that page's per-run costs (one grouped read, as the run list's), their sample facts, and one
/// cost aggregate.
#[utoipa::path(
    get, path = "/experiments/{id}",
    params(("id" = String, Path, description = "experiment id")),
    responses((status = 200, body = ExperimentDetail), (status = 403), (status = 404))
)]
pub(crate) async fn get_experiment(
    State(state): State<AppState>,
    Extension(actor): Extension<AuthedActor>,
    Path(id): Path<String>,
) -> Result<Json<ExperimentDetail>, ApiError> {
    let actor_id = authorize_msg(&state, &actor, Capability::ViewRuns)?;
    let record = experiment_or_404(&state, &id).await?;
    let (stats, runs, total_cost_usd, caller) = tokio::try_join!(
        state.store.experiment_run_stats(std::slice::from_ref(&record.id)),
        state.store.experiment_runs(&id, MAX_DETAIL_RUNS),
        state.store.experiment_priced_usd(&id),
        caller_runs(&state, &actor_id, Some(&record.id)),
    )?;
    let runs: Vec<RunSummary> = crate::runs::detail::with_list_facts(&state, runs).await;
    let samples = experiment_samples(&state, &runs).await?;
    let stats = stats.get(&record.id).copied();
    // The count and the page are separate reads, so a run that joins between them is in one only:
    // the total is never below what is shown.
    let runs_total = stats.map_or(0, |s| s.count).max(runs.len() as i64);
    Ok(Json(ExperimentDetail {
        experiment: view_for(&record, stats, &actor_id, &caller),
        runs,
        runs_total,
        samples,
        total_cost_usd,
    }))
}

/// Every sample of every run in an experiment, as a CSV file for a spreadsheet.
///
/// One row per sample per run (a sample in several runs has a row in each), runs oldest first and
/// each run's samples in its samples grid's order. Unlike the experiment's detail, which holds only
/// its newest runs, this reads all of them; it is read on asking, not polled. A run that names no
/// sample yet (no samplesheet and no task matched to a sample, such as one still preparing its
/// images) has no rows.
///
/// Columns: Sample; Sample status (Waiting, Running, Done, Failed or Stopped); Reused (`yes` when
/// the sample finished in that run and every one of its steps there came from an earlier run's
/// results, an engine cache hit or carried from the run it resumed; else `no`); Analysis (the run's
/// name as the UI shows it: its own, else `<pipeline> · <start> · <N samples>`, the start time in
/// the time zone `tz` the UI sends, else UTC); Run ID; Analysis status (the run's state in
/// words: Preparing images, Submitted, Running, Succeeded, Failed or Cancelled); Pipeline; Pipeline
/// version; Engine; Submitted by; Submitted (UTC) and Finished (UTC), as `YYYY-MM-DD HH:MM:SS`.
///
/// UTF-8 with a byte-order mark (for Excel), CRLF line ends and RFC 4180 quoting. A cell a
/// spreadsheet could run as a formula gets a leading `'`: one that starts with `=`, `+`, `-`, `@`
/// or their full-width forms after any leading whitespace, or with a tab or line break (numbers
/// such as `-5` included). So does a cell that already starts with `'`, so a program reading the
/// file gets every original value back by removing one leading `'` from each cell that has one.
///
/// One download covers at most 1,000 runs, about 5 MiB of CSV and 20 s of reading. Past any of
/// these the answer is a 413 whose message says which limit and what to do, never a file cut short.
#[utoipa::path(
    get, path = "/experiments/{id}/samples.csv",
    params(
        ("id" = String, Path, description = "experiment id"),
        ExperimentSamplesCsvQuery,
    ),
    responses(
        (status = 200, description = "One row per sample per run, with a header row",
         content_type = "text/csv", body = String),
        (status = 400, description = "An unknown time zone", content_type = "text/plain",
         body = String),
        (status = 403),
        (status = 404),
        (status = 413, description = "Too much to download at once (too many runs, too large a \
         file, or too slow to read); the message says which and what to do",
         content_type = "text/plain", body = String),
    )
)]
pub(crate) async fn experiment_samples_csv(
    State(state): State<AppState>,
    Extension(actor): Extension<AuthedActor>,
    Path(id): Path<String>,
    Query(q): Query<ExperimentSamplesCsvQuery>,
) -> Result<Response, ApiError> {
    let started = Instant::now();
    authorize_msg(&state, &actor, Capability::ViewRuns)?;
    let zone = NameZone::named(q.tz.as_deref(), biffix_core::clock::epoch_micros())
        .map_err(ApiError::BadRequest)?;
    let (record, stats) = tokio::try_join!(experiment_or_404(&state, &id), async {
        state.store.experiment_run_stats(std::slice::from_ref(&id)).await.map_err(ApiError::from)
    })?;
    let runs_total = stats.get(&id).map_or(0, |s| s.count);
    let body = samples_csv_body(
        state.store.as_ref(),
        &id,
        runs_total,
        SamplesCsvLimits::DEPLOYED,
        started,
        zone,
    )
    .await?;
    Ok(crate::api::attachment("text/csv; charset=utf-8", &samples_csv_filename(&record.name), body))
}

/// Rename, describe, archive or restore an experiment (a partial update).
#[utoipa::path(
    put, path = "/experiments/{id}",
    params(("id" = String, Path, description = "experiment id")),
    request_body = ExperimentUpdateRequest,
    responses(
        (status = 200, body = Experiment),
        (status = 400, description = "a bad name or description"),
        (status = 403),
        (status = 404),
        (status = 409, description = "the project already has a live experiment of that name"),
    )
)]
pub(crate) async fn update_experiment(
    State(state): State<AppState>,
    Extension(actor): Extension<AuthedActor>,
    Path(id): Path<String>,
    Json(req): Json<ExperimentUpdateRequest>,
) -> Result<Json<Experiment>, ApiError> {
    let actor_id = authorize_msg(&state, &actor, Capability::LaunchWorkflow)?;
    let bad = |m: String| ApiError::BadRequest(m);
    let change = ExperimentChange {
        name: req.name.as_deref().map(normalize_name).transpose().map_err(bad)?,
        description: req
            .description
            .as_ref()
            .map(|d| normalize_description(d.as_deref()))
            .transpose()
            .map_err(bad)?,
        archived: req.archived,
        now_micros: biffix_core::clock::epoch_micros(),
    };
    let record =
        state.experiments.update(&id, &change).await.map_err(experiment_store_error(&id))?;
    let (stats, caller) = tokio::join!(
        state.store.experiment_run_stats(std::slice::from_ref(&record.id)),
        caller_runs(&state, &actor_id, Some(&record.id)),
    );
    let stats = stats?;
    // The change is saved: a failed read of the caller's runs in it must not report the save as
    // failed. It shows as not needing their attention (and theirs only if they made it) until the
    // next read.
    let caller = caller.unwrap_or_else(|e| {
        tracing::warn!(experiment = %record.id, error = %e, "reading the caller's runs in an updated experiment failed");
        HashMap::new()
    });
    Ok(Json(view_for(&record, stats.get(&record.id).copied(), &actor_id, &caller)))
}

/// Put a run in an experiment of its project, move it to another, or take it out (`null`, or a
/// blank id). The body must name `experiment_id` (a 400 otherwise, so a misspelled field is not
/// read as "take it out"). Recorded as the run's `experiment_set` event; returns the run's summary
/// after.
#[utoipa::path(
    post, path = "/runs/{run_id}/experiment",
    params(("run_id" = String, Path, description = "biffix run id")),
    request_body = RunExperimentSetRequest,
    responses(
        (status = 200, body = RunSummary),
        (status = 400, description = "no `experiment_id` in the body, the run has no project, or the experiment is archived or in another project"),
        (status = 403),
        (status = 404, description = "unknown run or experiment"),
    )
)]
pub(crate) async fn set_run_experiment(
    State(state): State<AppState>,
    Extension(actor): Extension<AuthedActor>,
    Path(rid): Path<RunId>,
    body: Result<Json<RunExperimentSetRequest>, JsonRejection>,
) -> Result<Json<RunSummary>, ApiError> {
    let actor_id = authorize_msg(&state, &actor, Capability::LaunchWorkflow)?;
    let req = crate::api::runs::json_body_or_400(body, RUN_EXPERIMENT_BODY_ERROR)?;
    let run = crate::api::runs::visible_run_or_msg(&state, &actor, &rid).await?;
    let target = crate::api::non_blank(req.experiment_id.as_deref());
    if let Some(id) = target {
        let Some(project) = run.run.project.as_deref() else {
            return Err(ApiError::BadRequest(format!(
                "run {rid} has no project, so it cannot be put in an experiment"
            )));
        };
        joinable_experiment(&state, id, project, true).await?;
    }
    let event = biffix_core::event::experiment_set(
        &rid,
        biffix_core::time::Timestamp::from_micros(biffix_core::clock::epoch_micros()),
        target,
        &actor_id,
    );
    crate::api::runs::record_then_summary(&state, &rid, &event).await
}

// ── routes ───────────────────────────────────────────────────────────────────────────────────────

/// Experiments, and moving a run between them: the routes behind the Cognito middleware.
pub(super) fn routes() -> axum::Router<AppState> {
    axum::Router::new()
        .route("/runs/{run_id}/experiment", axum::routing::post(set_run_experiment))
        .route("/experiments", axum::routing::get(list_experiments).post(create_experiment))
        .route("/experiments/{id}", axum::routing::get(get_experiment).put(update_experiment))
        .route("/experiments/{id}/samples.csv", axum::routing::get(experiment_samples_csv))
}

/// Experiments, and moving a run between them: the OpenAPI operations, tagged `experiments` in
/// [`super::ApiDoc`].
#[derive(utoipa::OpenApi)]
#[openapi(
    paths(
        set_run_experiment,
        list_experiments,
        create_experiment,
        get_experiment,
        experiment_samples_csv,
        update_experiment
    ),
    components(schemas(
        biffix_api::Experiment,
        biffix_api::ExperimentList,
        biffix_api::ExperimentCreateRequest,
        biffix_api::ExperimentUpdateRequest,
        biffix_api::ExperimentSample,
        biffix_api::ExperimentDetail,
        biffix_api::RunExperimentSetRequest
    ))
)]
pub(super) struct Doc;
