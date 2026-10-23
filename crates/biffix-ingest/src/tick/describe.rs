//! The reconcile step: describe stale attempts and coordinators and enqueue
//! the terminals Batch reports, then reap the live children of every ended run.

use std::collections::{BTreeSet, HashSet};
use std::time::Duration;

use aws_sdk_batch::error::{DisplayErrorContext, ProvideErrorMetadata};
use biffix_core::clock::{micros, millis, since_epoch};
use biffix_core::eventbridge::describe::terminal_run_event;
use lambda_runtime::Error;

use super::{aws_error, enqueue_events, micros_before, note, State};
use crate::decode::described_terminal;
use crate::reconcile::{
    forgotten_children, plan_reap, reap_reason, reap_terminal_event, terminate_error_is_benign,
    DescribedChild, DESCRIBE_JOBS_BATCH_SIZE,
};
use crate::store::reconcile::{
    ended_runs_with_live_children, live_children_of_run, record_describe_results,
    run_id_for_coordinator, stale_attempts, stale_coordinators, touch_described, StaleWindow,
};

/// How long an attempt may go without news before the backstop describes it.
const STALE_AFTER: Duration = Duration::from_secs(900);

/// The most stale attempts one tick describes, and the reaper's page size.
const CANDIDATE_LIMIT: i64 = 1000;

/// How far back the ended-run sweep looks for runs with live children. Long enough to retry a
/// failed terminate for hours; short enough that finished history is not rescanned every poll.
const ENDED_RUN_SWEEP_WINDOW: Duration = Duration::from_secs(6 * 3600);

/// How long DescribeJobs may leave a stale candidate out before the backstop stops asking about
/// it. Batch keeps a finished job about a week and never forgets a live one, so an omission that
/// lasts a day of polls is Batch having forgotten the job, not a transient gap.
const GIVE_UP_AFTER_MISSING: Duration = Duration::from_secs(24 * 3600);

/// How far back the stale backstop looks for bound jobs. Batch keeps a finished job about a week,
/// so describing a job bound earlier can recover its missed terminal only if it is still running
/// after more than a week; past this window it is no longer asked about, which keeps every poll
/// proportional to the window rather than to the whole log.
const STALE_JOB_WINDOW: Duration = Duration::from_secs(14 * 24 * 3600);

/// How long DescribeJobs must have left out a child of an ended run before the reaper closes it
/// with a synthetic FAILED: well past any visibility lag of a just-submitted job, and well inside
/// [`ENDED_RUN_SWEEP_WINDOW`] so the sweep is still looking at the run when it elapses.
const REAP_FORGOTTEN_AFTER: Duration = Duration::from_secs(3600);

/// A `reconcile_describe` row for a job Batch still knew is dropped once it is this old: rotation
/// re-describes every live candidate far sooner, so the job's terminal has landed since.
const DESCRIBE_FORGET_AFTER: Duration = Duration::from_secs(30 * 24 * 3600);

/// DescribeJobs for at most [`DESCRIBE_JOBS_BATCH_SIZE`] ids.
async fn describe_jobs(
    state: &State,
    ids: &[String],
) -> Result<Vec<aws_sdk_batch::types::JobDetail>, Error> {
    let result = state
        .batch
        .describe_jobs()
        .set_jobs(Some(ids.to_vec()))
        .send()
        .await
        .map_err(|error| aws_error("DescribeJobs", error))?;
    Ok(result.jobs.unwrap_or_default())
}

pub(super) async fn reconcile(state: &State) -> Result<usize, Error> {
    let now = since_epoch();
    let now_micros = micros(now);
    let poll_millis = millis(now);
    let cutoff_micros = micros_before(now, STALE_AFTER);
    let forget_before = micros_before(now, DESCRIBE_FORGET_AFTER);
    // Backstop BOTH child tasks and the coordinator: a missed coordinator terminal otherwise
    // strands the whole run at "submitted" forever (child-only reconciliation can't advance it).
    let window = StaleWindow {
        cutoff_micros,
        bound_since_micros: micros_before(now, STALE_JOB_WINDOW),
        give_up_before_micros: micros_before(now, GIVE_UP_AFTER_MISSING),
        limit: CANDIDATE_LIMIT,
    };
    let mut candidates = stale_attempts(&state.pool, window).await?;
    let coordinators = stale_coordinators(&state.pool, window).await?;
    // The control plane bound these as coordinators, whatever queue they ran on: a terminal found
    // for one is the coordinator's (and its run's children are reaped), as the fold reads it.
    let bound_coordinators: HashSet<String> =
        coordinators.iter().map(|c| c.batch_job_id.clone()).collect();
    // Union, de-duplicated by batch_job_id (a job is never both a task and a coordinator).
    let known: HashSet<String> = candidates.iter().map(|c| c.batch_job_id.clone()).collect();
    candidates.extend(coordinators.into_iter().filter(|c| !known.contains(&c.batch_job_id)));
    let ids: Vec<String> = candidates.into_iter().map(|c| c.batch_job_id).collect();

    let mut emitted = 0usize;
    // Coordinators found terminal this poll; after enqueuing terminals we reap their live children
    // so a crashed coordinator does not strand orphaned, still-billing Batch jobs.
    let mut terminal_coordinators: Vec<String> = Vec::new();
    // A describe/enqueue failure in one chunk must not abort the poll: record the first error, keep
    // going, run the reaps, then surface it so the Lambda reports the failure (every append and
    // enqueue is idempotent, and the next poll retries what failed).
    let mut first_error: Option<Error> = None;
    for chunk in ids.chunks(DESCRIBE_JOBS_BATCH_SIZE) {
        let jobs = match describe_jobs(state, chunk).await {
            Ok(jobs) => jobs,
            Err(error) => {
                tracing::warn!(error = %error, "reconcile: DescribeJobs chunk failed (will retry)");
                note(&mut first_error, error);
                // Rotate the chunk to the back, so a request Batch always rejects does not pin
                // these candidates at the head of every poll.
                if let Err(error) = touch_described(&state.pool, chunk, now_micros).await {
                    note(&mut first_error, error.into());
                }
                continue;
            }
        };
        let present: BTreeSet<String> =
            jobs.iter().filter_map(|job| job.job_id().map(str::to_owned)).collect();
        let missing: Vec<String> =
            chunk.iter().filter(|id| !present.contains(*id)).cloned().collect();
        let present: Vec<String> = present.into_iter().collect();
        if let Err(error) =
            record_describe_results(&state.pool, &present, &missing, now_micros, forget_before)
                .await
        {
            note(&mut first_error, error.into());
        }

        let mut events = Vec::new();
        for terminal in jobs.iter().filter_map(|job| {
            let bound = job.job_id().is_some_and(|id| bound_coordinators.contains(id));
            described_terminal(job, &state.queue_context, bound, poll_millis)
        }) {
            if terminal.is_coordinator {
                terminal_coordinators.push(terminal.batch_job_id.clone());
            }
            events.push(terminal_run_event(&terminal));
        }
        match enqueue_events(state, &events).await {
            Ok(count) => emitted += count,
            Err(error) => {
                tracing::warn!(error = %error, "reconcile: enqueue chunk failed (will retry)");
                note(&mut first_error, error);
            }
        }
    }

    // Targeted reap: the live children of every coordinator whose missed terminal was
    // found above — reaped regardless of whether some enqueue failed (the coordinator is confirmed
    // dead). The ended-run sweep below would reach the same run on a later poll once that terminal
    // is folded; reaping here saves that poll's latency, and still covers a coordinator whose
    // terminal is older than the sweep window (a reconciler outage longer than it).
    let mut reaped = 0usize;
    for coordinator_job_id in &terminal_coordinators {
        match reap_orphaned_children(state, coordinator_job_id).await {
            Ok(count) => reaped += count,
            Err(error) => {
                tracing::warn!(coordinator = coordinator_job_id, error = %error, "reap failed (will retry next poll)");
                note(&mut first_error, error);
            }
        }
    }

    // General sweep: a coordinator that ended normally — cancelled via biffix, or failed with an
    // executor that leaves its children running (Nextflow does on SIGTERM) — has its terminal
    // delivered by EventBridge and never becomes stale, so its live children would bill to
    // completion. Reap every recently-ended run that still has live children; a run is retried on
    // every poll while it is inside the window and still has any.
    let since_micros = micros_before(now, ENDED_RUN_SWEEP_WINDOW);
    match ended_runs_with_live_children(&state.pool, since_micros, CANDIDATE_LIMIT).await {
        Ok(runs) => {
            for (run_id, coordinator_job_id) in &runs {
                match reap_run_children(state, run_id, coordinator_job_id).await {
                    Ok(count) => reaped += count,
                    Err(error) => {
                        tracing::warn!(run_id, error = %error, "ended-run reap failed (will retry next poll)");
                        note(&mut first_error, error);
                    }
                }
            }
        }
        Err(error) => {
            tracing::warn!(error = %error, "ended-run sweep query failed (will retry next poll)");
            note(&mut first_error, error.into());
        }
    }

    tracing::info!(candidates = ids.len(), emitted, reaped, "reconciliation poll complete");
    first_error.map_or(Ok(emitted), Err)
}

/// Reap the still-live child jobs of a coordinator found terminal this poll.
async fn reap_orphaned_children(state: &State, coordinator_job_id: &str) -> Result<usize, Error> {
    let Some(run_id) = run_id_for_coordinator(&state.pool, coordinator_job_id).await? else {
        return Ok(0);
    };
    reap_run_children(state, &run_id, coordinator_job_id).await
}

/// Reap every still-"live" child job of `run_id` (whose coordinator is `coordinator_job_id`).
///
/// Each page of children is described first, and what happens to a child follows Batch's own
/// answer ([`plan_reap`]): a child Batch reports finished gets its real terminal enqueued (a
/// SUCCEEDED the log missed stays a success, and a real FAILED keeps its exit code and reason); a
/// child still active is terminated, and its real FAILED arrives through EventBridge or a later
/// poll's describe; a child Batch has omitted for [`REAP_FORGOTTEN_AFTER`] gets a synthetic FAILED
/// ([`reap_terminal_event`]), since no real terminal can come. Nothing is ever fabricated for a
/// job Batch still knows. Returns how many children were terminated; errors are collected so one
/// child or page never stops the rest, and the first is returned at the end.
async fn reap_run_children(
    state: &State,
    run_id: &str,
    coordinator_job_id: &str,
) -> Result<usize, Error> {
    let now = since_epoch();
    let now_micros = micros(now);
    let poll_millis = millis(now);
    let forgotten_before = micros_before(now, REAP_FORGOTTEN_AFTER);
    let forget_before = micros_before(now, DESCRIBE_FORGET_AFTER);
    let mut first_error: Option<Error> = None;
    let (mut terminated, mut recovered, mut forgotten) = (0usize, 0usize, 0usize);
    // Page through ALL live children by `batch_job_id` keyset, not just the first
    // `CANDIDATE_LIMIT`: what this enqueues is not folded synchronously, so a re-query would return
    // the same still-"live" children; only the cursor makes progress within a poll.
    let reason = reap_reason(coordinator_job_id);
    let mut after: Option<String> = None;
    loop {
        let children =
            live_children_of_run(&state.pool, run_id, after.as_deref(), CANDIDATE_LIMIT).await?;
        if children.is_empty() {
            break;
        }
        for chunk in children.chunks(DESCRIBE_JOBS_BATCH_SIZE) {
            let jobs = match describe_jobs(state, chunk).await {
                Ok(jobs) => jobs,
                Err(error) => {
                    // Never terminate or close a child whose state is unknown; the next poll
                    // retries.
                    note(&mut first_error, error);
                    continue;
                }
            };
            let described: Vec<DescribedChild> = jobs
                .iter()
                .filter_map(|job| {
                    let id = job.job_id()?.to_owned();
                    // A run's live children are its task-bound jobs.
                    Some((id, described_terminal(job, &state.queue_context, false, poll_millis)))
                })
                .collect();
            let present: Vec<String> = described.iter().map(|(id, _)| id.clone()).collect();
            let plan = plan_reap(chunk, described);
            let mut events: Vec<biffix_core::event::RunEvent> =
                plan.terminals.iter().map(terminal_run_event).collect();
            recovered += plan.terminals.len();
            for child in &plan.terminate {
                match state.batch.terminate_job().job_id(child).reason(&reason).send().await {
                    Ok(_) => terminated += 1,
                    Err(error) if terminate_error_is_benign(error.code(), error.message()) => {
                        tracing::info!(child, error = %DisplayErrorContext(&error), "reap: child already gone");
                    }
                    Err(error) => {
                        let error = aws_error(&format!("TerminateJob {child}"), error);
                        tracing::warn!(child, error = %error, "reap: terminate_job failed (will retry next poll)");
                        note(&mut first_error, error);
                    }
                }
            }
            match record_describe_results(
                &state.pool,
                &present,
                &plan.missing,
                now_micros,
                forget_before,
            )
            .await
            {
                Ok(missing_since) => {
                    for child in forgotten_children(&plan.missing, &missing_since, forgotten_before)
                    {
                        events.push(reap_terminal_event(&child, coordinator_job_id, poll_millis));
                        forgotten += 1;
                    }
                }
                Err(error) => note(&mut first_error, error.into()),
            }
            if let Err(error) = enqueue_events(state, &events).await {
                note(&mut first_error, error);
            }
        }
        // Advance the keyset cursor past this page; a short page means we reached the end.
        let page_len = children.len() as i64;
        after = children.into_iter().last();
        if page_len < CANDIDATE_LIMIT {
            break;
        }
    }
    if terminated + recovered + forgotten > 0 {
        tracing::info!(
            run_id,
            coordinator = coordinator_job_id,
            terminated,
            recovered,
            forgotten,
            "reaped the live children of an ended run"
        );
    }
    first_error.map_or(Ok(terminated), Err)
}
