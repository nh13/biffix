//! `POST /events` — the engine's RunEvent delivery endpoint.
//!
//! Both engine plugins (the Snakemake logger plugin and nf-biffix) POST here with their run's
//! capability token, so task-level engine facts — `task_created`, the `task_submitted` binding
//! that joins the engine and AWS streams, `run_progress` — reach the log.
//!
//! Authorization is the capability token, NOT Cognito: the coordinator is a machine with no user
//! identity. The token is HMAC-signed and scoped to exactly one run, so this route accepts appends
//! only for that run — a token for run A cannot write to run B. Before anything is appended the
//! whole batch must pass these, or none of it lands:
//!
//! - every event names the token's run, and that run is not cancelled (cancel revokes the token);
//! - every kind is an engine kind: a known one, or an unknown one under the engine namespace
//!   (`translate::ENGINE_KIND_PREFIX`), so a token cannot plant a control-plane fact's name.
//!
//! Then every `task_submitted` must claim a job of THIS run ([`check_job_claims`]). A claim is how
//! a Batch job's AWS facts (attempt state, exit code, host, cost) reach a run, and the fold binds a
//! job to the first claim in `ts` order, from an immutable log, so a forged claim would hand run A
//! run B's facts for good. A claim is accepted only when the job's `biffix:run` Batch tag
//! ([`crate::aws::batch::RUN_TAG`], set by the engine's executor from the coordinator's env) names
//! the token's run, no other run already claims the job in the log, and its `ts` neither predates
//! the job's creation nor lies in the future (beyond clock skew), so it cannot be backdated ahead
//! of the owner's. A claim that fails one of these is refused on its own: it is left out, named in
//! the answer's `refused`, and the rest of the batch lands. A claim can fail without any forgery (a
//! job Batch no longer describes, during the end-of-run replay of a long run), and the batch it
//! came in carries the run's other facts, which must not be lost with it. When Batch or the log
//! cannot answer, the whole batch is refused with a 503 the plugin retries, never accepted
//! unchecked.
//!
//! The tag stops a coordinator that only holds its token. One that also calls the Batch API with
//! its coordinator role cannot move the tag either: IAM makes it write-once (a job is created with
//! it, the backend's coordinator jobs included, and no coordinator may re-tag a job that carries
//! one or untag it; `infra/biffix_infra/stacks/compute_stack.py` `grant_coordinator`). It can
//! create a job tagged with another run, but only that run could claim it.

use std::collections::BTreeSet;

use axum::extract::{DefaultBodyLimit, State};
use axum::http::{header, StatusCode};
use axum::Json;

use biffix_api::{IngestRequest, IngestSummary, RefusedEvent};
use biffix_core::event::{EventData, RunEvent};
use biffix_core::ids::{BatchJobId, RunId};
use biffix_core::ingest_limits::{MAX_BATCH_EVENTS, MAX_BODY_BYTES};
use biffix_core::source::EventSource;
use biffix_core::state::RunState;
use biffix_core::time::Timestamp;
use biffix_core::translate::from_engine_value;

use crate::api::ApiError;
use crate::aws::AwsError;
use crate::store::StoreError;
use crate::AppState;
use biffix_workflow::engines::Engine;

/// Append engine-reported RunEvents for one run.
#[utoipa::path(
    post, path = "/events",
    request_body = IngestRequest,
    responses(
        (status = 202, body = IngestSummary, description = "the batch landed, but for the events in `refused` (a task_submitted claiming a Batch job the run cannot be shown to own, or whose ts lies outside the job's lifetime)"),
        (status = 400, description = "malformed batch: an event without a run id, a known kind with a malformed body, a kind no engine may send, or a value the log cannot store (a NUL in text)"),
        (status = 401, description = "missing/invalid/expired capability token"),
        (status = 403, description = "token does not authorize the run these events claim, or the run is cancelled (token revoked)"),
        (status = 413, description = "batch too large (events or bytes)"),
        (status = 503, description = "log or AWS Batch unavailable (retry)"),
    )
)]
pub(crate) async fn ingest(
    State(state): State<AppState>,
    request: axum::extract::Request,
) -> Result<(StatusCode, Json<IngestSummary>), ApiError> {
    let Some(verifier) = state.capability.as_ref() else {
        // Fail closed: no configured secret means no authenticated ingest, never open ingest.
        return Err(ApiError::Unavailable("ingest is not configured".to_string()));
    };
    // Same bearer-token extraction as the Cognito middleware (`auth::bearer_token`), just surfaced
    // as an ingest-shaped error instead of the middleware's bare status.
    let header = request.headers().get(header::AUTHORIZATION).and_then(|value| value.to_str().ok());
    let token = crate::auth::bearer_token(header)
        .map_err(|_| ApiError::Unauthorized("missing capability token".to_string()))?;
    let authorized_run =
        verifier.verify(token).map_err(|e| ApiError::Unauthorized(e.to_string()))?;
    // The body is read (and parsed) only now the token verifies (this route is outside the Cognito
    // middleware): an unauthenticated caller costs no read or parse and learns nothing of one. The
    // `Json` extractor's own checks and answers (a 413, 415, 400 or 422), the route's body limit
    // included.
    let Json(req) = {
        use axum::extract::FromRequest;
        Json::<IngestRequest>::from_request(request, &state).await?
    };

    // A bounded batch: an unbounded one is a cheap way to exhaust the writer and the DB.
    // The engine plugins' posters are pinned to the same cap.
    if req.events.len() > MAX_BATCH_EVENTS {
        return Err(ApiError::PayloadTooLarge(format!(
            "batch of {} exceeds {MAX_BATCH_EVENTS}",
            req.events.len()
        )));
    }
    if req.events.is_empty() {
        return Ok((
            StatusCode::ACCEPTED,
            Json(IngestSummary { appended: 0, duplicates: 0, refused: Vec::new() }),
        ));
    }

    // Every event must belong to the run this token authorizes — whatever its kind, known or not.
    // Checked BEFORE any append, so a batch that mixes runs is rejected whole rather than partially
    // applied.
    for (i, event) in req.events.iter().enumerate() {
        let Some(run_id) = event.get("run_id").and_then(|r| r.as_str()) else {
            return Err(ApiError::BadRequest(format!("event {i} has no string `run_id`")));
        };
        if run_id != authorized_run.as_str() {
            return Err(ApiError::Forbidden(
                "capability token does not authorize the run these events claim".to_string(),
            ));
        }
    }

    // Revocation-by-recheck: a cancelled run's capability token no longer authorizes
    // appends — cancel == revoke, so no separate revocation list is needed. Only CANCELLED is
    // rejected (not succeeded/failed), so the coordinator's at-least-once events.jsonl replay
    // backstop for a normally-terminated run still lands; the reaper's synthetic terminals and AWS
    // Batch terminals arrive on the (non-token) ingest queue and are unaffected. An unprojected run
    // (None) is treated as not-revoked — the anchor may not have folded yet.
    match state.store.run_state(&authorized_run).await {
        Ok(Some(s)) if s == RunState::Cancelled.as_str() => {
            return Err(ApiError::Forbidden(
                "run is cancelled; capability token revoked".to_string(),
            ));
        }
        Ok(_) => {}
        Err(e) => {
            tracing::warn!(error = %e, "ingest: run-state revocation check failed");
            return Err(ApiError::from(e).with_message("run-state check failed"));
        }
    }

    // Record these engine events under the run's actual engine source, not always Snakemake. The
    // engine wire shape is engine-agnostic (the Nextflow observer POSTs the same shape), so a
    // Nextflow run's events would otherwise be mislabelled `EventSource::Snakemake`. One point
    // lookup per batch (all events in a batch belong to one run == one engine); an
    // unknown/unprojected engine falls back to Snakemake, the default.
    let source = match state.store.run_engine(&authorized_run).await {
        Ok(Some(engine)) if engine == Engine::Nextflow.as_str() => EventSource::Nextflow,
        Ok(_) => EventSource::Snakemake,
        Err(e) => {
            tracing::warn!(error = %e, "ingest: run-engine lookup failed");
            return Err(ApiError::from(e).with_message("run-engine lookup failed"));
        }
    };

    // Translate the whole batch, then append it in ONE lock-holding transaction — a single fold
    // pass — instead of a transaction plus a full re-fold of the run's history per event, which
    // made a buffered 500-event flush O(batch × run_history). Translation is the same pure function
    // the AWS path uses. A kind this build does not know is preserved (stored, not folded) rather
    // than rejected (an event's kind is an open enum), so a newer plugin cannot break us; a known
    // kind with a malformed body is still a 400 for the whole batch, before anything is appended.
    // `record_all` returns how many rows were newly appended; the rest are dedup no-ops (a plugin
    // retry).
    let mut events: Vec<RunEvent> = Vec::with_capacity(req.events.len());
    for (i, event) in req.events.into_iter().enumerate() {
        let translated = from_engine_value(source.clone(), event)
            .map_err(|e| ApiError::BadRequest(format!("event {i}: {e}")))?;
        // `from_engine_value` attributes the event to its own `run_id`, checked above.
        debug_assert_eq!(translated.run_id.as_ref(), Some(&authorized_run));
        events.push(translated);
    }
    let refused = check_job_claims(&state, &authorized_run, &events).await?;
    let events: Vec<RunEvent> = events
        .into_iter()
        .enumerate()
        .filter(|(i, _)| !refused.iter().any(|r| r.index == *i))
        .map(|(_, e)| e)
        .collect();
    let appended = state.events.record_all(&events).await.map_err(|e| {
        tracing::warn!(error = %e, "ingest: append failed");
        // A value the log refused is the batch's (a 400 the plugin isolates the event by), and
        // says which; an outage says only that the append failed.
        let message = match &e {
            StoreError::Rejected(_) => format!("append failed: {e}"),
            _ => "append failed".to_string(),
        };
        ApiError::from(e).with_message(message)
    })?;
    let duplicates = events.len() - appended;

    Ok((StatusCode::ACCEPTED, Json(IngestSummary { appended, duplicates, refused })))
}

/// How far a `task_submitted` claim's `ts` may sit before its job's creation, or past now: the
/// clock skew between the coordinator's host and AWS, with margin. A legitimate claim is emitted
/// after `SubmitJob` returns, so it never predates its job by more than that; a late one (a plugin
/// retry, the end-of-run replay) is at most as old as its job, whatever its age.
const CLAIM_SKEW_MICROS: i64 = 5 * 60 * 1_000_000;

/// The `task_submitted` events of the batch that do not claim a Batch job of `run` (see the
/// module docs), by index, each with why; an `Err` refuses the whole batch (the log or Batch could
/// not answer). Jobs the log already binds to `run` (a plugin retry, the coordinator's end-of-run
/// replay) are not described again; each other job is described once, whatever the number of
/// claims on it.
async fn check_job_claims(
    state: &AppState,
    run: &RunId,
    events: &[RunEvent],
) -> Result<Vec<RefusedEvent>, ApiError> {
    let claims: Vec<(usize, &BatchJobId, Timestamp)> = events
        .iter()
        .enumerate()
        .filter(|(_, e)| matches!(e.data, EventData::TaskSubmitted { .. }))
        .filter_map(|(i, e)| e.batch_job_id.as_ref().map(|job| (i, job, e.ts)))
        .collect();
    let mut refused = Vec::new();
    if claims.is_empty() {
        return Ok(refused);
    }
    let mut seen = BTreeSet::new();
    let jobs: Vec<BatchJobId> = claims
        .iter()
        .filter(|(_, job, _)| seen.insert(job.as_str()))
        .map(|(_, job, _)| (*job).clone())
        .collect();

    let held = state.events.job_claims(&jobs).await.map_err(|e| {
        tracing::warn!(error = %e, "ingest: job-claim lookup failed");
        ApiError::from(e).with_message("job-claim lookup failed")
    })?;
    let mut ours = BTreeSet::new();
    let mut theirs = BTreeSet::new();
    for (job, holder) in held {
        if holder == *run {
            ours.insert(job);
        } else {
            tracing::warn!(%job, claimant = %run.as_str(), holder = %holder.as_str(),
                "ingest: refused a claim on another run's Batch job");
            theirs.insert(job);
        }
    }
    let unverified: Vec<BatchJobId> = jobs
        .into_iter()
        .filter(|job| !ours.contains(job.as_str()) && !theirs.contains(job.as_str()))
        .collect();

    let owners = if unverified.is_empty() {
        Default::default()
    } else {
        state.cloud.batch_job_owners(&unverified).await.map_err(|e| {
            tracing::warn!(error = %e, "ingest: describing claimed Batch jobs failed");
            claim_check_error(&e)
                .with_message("could not check which run the claimed Batch jobs belong to")
        })?
    };
    let now = biffix_core::clock::epoch_micros();
    for (index, job, ts) in claims {
        let job = job.as_str();
        if ours.contains(job) {
            continue;
        }
        let mut refuse = |reason: String| refused.push(RefusedEvent { index, reason });
        if theirs.contains(job) {
            refuse(format!("Batch job {job} is already claimed by another run"));
            continue;
        }
        let owner = owners.get(job).filter(|owner| owner.run.as_ref() == Some(run));
        let Some(owner) = owner else {
            tracing::warn!(%job, claimant = %run.as_str(),
                "ingest: refused a claim on a Batch job not tagged for the claiming run");
            refuse(format!("Batch job {job} is not a job of this run"));
            continue;
        };
        let ts = ts.as_micros();
        let predates_job = owner
            .created_at_micros
            .is_some_and(|created| ts < created.saturating_sub(CLAIM_SKEW_MICROS));
        if predates_job || ts > now.saturating_add(CLAIM_SKEW_MICROS) {
            refuse(format!(
                "task_submitted for Batch job {job} has a ts outside the job's lifetime"
            ));
        }
    }
    Ok(refused)
}

/// The error kind when Batch cannot say who owns the claimed jobs. Only a request Batch refused as
/// malformed (e.g. a job id it cannot parse) is the plugin's fault: a 400, on which the plugin
/// halves the batch until the bad claim is alone. Anything else — Batch down, or this deploy
/// lacking `batch:DescribeJobs` — is a 503 the plugin retries, so the check fails closed without
/// making the plugin drop the batch as if its token were bad.
fn claim_check_error(err: &AwsError) -> ApiError {
    match err {
        AwsError::Invalid(_) => ApiError::BadRequest(String::new()),
        _ => ApiError::Unavailable(String::new()),
    }
}

// ── routes ───────────────────────────────────────────────────────────────────────────────────────

/// Engine event ingest: the routes open before a caller has a token.
pub(super) fn open_routes() -> axum::Router<AppState> {
    // The body cap is `ingest_limits::MAX_BODY_BYTES`, which says why it is this size.
    axum::Router::new()
        .route("/events", axum::routing::post(ingest).layer(DefaultBodyLimit::max(MAX_BODY_BYTES)))
}

/// Engine event ingest: the OpenAPI operations, tagged `events` in [`super::ApiDoc`].
#[derive(utoipa::OpenApi)]
#[openapi(paths(ingest), components(schemas(IngestRequest, IngestSummary, RefusedEvent)))]
pub(super) struct Doc;
