//! Why a failed run failed, in plain words ([`failure_summary`]).

use biffix_api::{FailureItem, FailureSummary, Task};
use biffix_core::event::{EventData, Kind};
use biffix_core::ids::RunId;

use crate::error::ServiceError;
use crate::runs::logs::{log_lines, project_task_log_group};
use crate::store::RunDetailView;
use crate::AppState;

/// Failed steps a failure summary explains, and log lines it shows for each.
const SUMMARY_STEPS: usize = 3;

const SUMMARY_LOG_LINES: i32 = 20;

/// How much of a Snakemake job's log a failure summary reads to find the step's own last lines
/// above its Snakemake's failure epilogue, and its command's failure in it
/// ([`biffix_core::failure::snakemake_job_log`]).
const SNAKEMAKE_JOB_SCAN_LINES: i32 = 200;

/// The last lines of a log stream, oldest first; empty when there is none or it cannot be read (a
/// summary without its log is still worth showing). `project_group` as for [`log_lines`].
async fn log_tail(
    state: &AppState,
    project_group: Option<&str>,
    stream: Option<&str>,
) -> Vec<String> {
    log_lines(state, project_group, stream, SUMMARY_LOG_LINES).await
}

/// What [`SNAKEMAKE_JOB_SCAN_LINES`] of a Snakemake job's log's end say about its step: its own
/// last lines, cut above the job's own Snakemake's failure epilogue (which otherwise fills the
/// whole tail), and its command's failure from that epilogue.
async fn snakemake_job_log(
    state: &AppState,
    project_group: Option<&str>,
    stream: Option<&str>,
) -> biffix_core::failure::SnakemakeJobLog {
    let lines = log_lines(state, project_group, stream, SNAKEMAKE_JOB_SCAN_LINES).await;
    biffix_core::failure::snakemake_job_log(&lines, SUMMARY_LOG_LINES as usize)
}

/// The exit code Snakemake's AWS Batch job exits with when its rule fails, whatever the rule's
/// command exited with.
const SNAKEMAKE_FAILED_JOB_EXIT: i64 = 1;

/// Failed tasks a failure summary reads per store page.
const FAILED_TASK_PAGE: i64 = 1000;

/// Order failed tasks earliest-finished first (unfinished last), ties by task key.
fn earliest_first(a: &Task, b: &Task) -> std::cmp::Ordering {
    a.finished_at
        .unwrap_or(f64::MAX)
        .total_cmp(&b.finished_at.unwrap_or(f64::MAX))
        .then_with(|| a.task_key.as_str().cmp(b.task_key.as_str()))
}

/// The run's first [`SUMMARY_STEPS`] failed tasks by [`earliest_first`], and how many failed in
/// all. The store has no finish-time order, so this pages through every failed task (in key order)
/// keeping the earliest seen so far — a key-ordered first page alone could miss the first failure.
async fn earliest_failed_tasks(
    state: &AppState,
    rid: &RunId,
) -> Result<(Vec<Task>, i64), ServiceError> {
    let now = biffix_core::clock::epoch_secs();
    let mut earliest: Vec<Task> = Vec::new();
    let mut offset = 0;
    loop {
        let query = crate::runs::task_page::TaskQuery {
            state: crate::runs::task_page::TaskStateFilter::Failed,
            offset,
            limit: FAILED_TASK_PAGE,
            ..Default::default()
        };
        let page =
            state.store.list_tasks(rid, &query, now).await.map_err(ServiceError::status_only)?;
        let read = page.tasks.len() as i64;
        earliest.extend(page.tasks);
        earliest.sort_by(earliest_first);
        earliest.truncate(SUMMARY_STEPS);
        offset += read;
        if read == 0 || offset >= page.total {
            return Ok((earliest, page.total));
        }
    }
}

/// Who to ask about `run`: its workflow version's `catalog.contact`, else the site's support
/// contact. A display nicety: a version or settings read that fails is logged and reads as none.
async fn support_contact(
    state: &AppState,
    run: &RunDetailView,
) -> Option<biffix_api::SupportContact> {
    if let Some(version) = &run.run.workflow {
        match state.registry.get_version(version.as_str()).await {
            Ok(rec) => {
                if let Some(c) =
                    crate::register::catalog::frozen_catalog(&rec.version.form_schema).contact
                {
                    return Some(c.into());
                }
            }
            Err(crate::store::StoreError::NotFound) => {}
            Err(e) => {
                tracing::warn!(error = %e, "failure_summary: reading the run's version failed");
            }
        }
    }
    match state.settings.get_settings().await {
        Ok(settings) => settings.support,
        Err(e) => {
            tracing::warn!(error = %e, "failure_summary: reading the site settings failed");
            None
        }
    }
}

/// The reason (and stage) the control plane recorded when it failed the run before it launched (a
/// `run_failed` carrying a reason). `None` otherwise, or when it cannot be read.
async fn control_plane_failure(state: &AppState, rid: &RunId) -> Option<(String, Option<String>)> {
    let ev = match state.store.run_event(rid, &Kind::RunFailed).await {
        Ok(ev) => ev?,
        Err(e) => {
            tracing::warn!(error = %e, "failure_summary: reading the run's run_failed failed");
            return None;
        }
    };
    match ev.data {
        EventData::RunFailed { reason, stage } => reason.map(|r| (r, stage)),
        _ => None,
    }
}

/// Why `run` failed, in plain words (`GET /runs/{id}/failure-summary`): its first failed steps
/// (earliest first), each with its cause, what to try, and the end of its log; or the control
/// plane's or the workflow engine's own failure when no step failed. Empty for a run that did not
/// fail.
pub(crate) async fn failure_summary(
    state: &AppState,
    run: &RunDetailView,
) -> Result<FailureSummary, ServiceError> {
    use biffix_core::failure::{
        diagnose, diagnose_coordinator, FailedAttempt, FailureCause, TryAgain,
    };

    let rid = &run.run.run_id;
    if run.run.state != biffix_core::state::RunState::Failed.as_str() {
        return Ok(FailureSummary { failed_steps: 0, items: vec![], support: None });
    }
    let support = support_contact(state, run).await;
    // The submit path's own resume check (what `GET /runs/{id}` serves as `resumable`): a cause
    // that would resume re-runs instead when this run cannot be resumed.
    let projects = state.projects.current().await;
    let resumable = crate::runs::submit::resume::resumable_now(run, &projects).is_ok();
    let try_again = |cause: FailureCause| {
        match cause.try_again() {
            TryAgain::Resume if !resumable => TryAgain::Rerun,
            mode => mode,
        }
        .as_str()
        .to_string()
    };
    let (failed, failed_steps) = earliest_failed_tasks(state, rid).await?;
    let task_group = project_task_log_group(&projects, run);
    // An unknown engine is Snakemake's, as the rest of the backend reads it.
    let snakemake = run
        .run
        .engine
        .as_deref()
        .is_none_or(|e| e == biffix_workflow::engines::Engine::Snakemake.as_str());
    let engine = if snakemake {
        biffix_workflow::engines::Engine::Snakemake
    } else {
        biffix_workflow::engines::Engine::Nextflow
    };
    let keys: Vec<String> = failed.iter().map(|t| t.task_key.as_str().to_string()).collect();
    // Which sample each failed step was is a display nicety: a read that fails is logged and the
    // items name no sample, rather than failing the whole summary.
    let mut samples = match state.store.task_samples(rid, &keys).await {
        Ok(samples) => samples,
        Err(e) => {
            tracing::warn!(error = %e, "failure_summary: reading the failed steps' samples failed");
            Default::default()
        }
    };

    let mut items = Vec::new();
    for task in &failed {
        let attempts = state
            .store
            .get_attempts(rid, &task.task_key)
            .await
            .map_err(ServiceError::status_only)?;
        let mut recorded: Vec<FailedAttempt> = if attempts.is_empty() {
            // No Batch attempt recorded (a local rule): what the task itself says.
            vec![FailedAttempt {
                exit_code: task.exit_code,
                termination_category: task.termination_category.clone(),
                ..Default::default()
            }]
        } else {
            attempts
                .iter()
                .map(|a| {
                    // The attempt the workflow engine failed although its job did not (an engine
                    // verdict, on the task's display attempt): what the engine said is its ending,
                    // so its category and reason stand in for the ones a failed job carries.
                    let rejected = task.verdict_reason.is_some()
                        && task.batch_job_id.as_ref() == Some(&a.batch_job_id);
                    FailedAttempt {
                        exit_code: a.exit_code,
                        termination_category: a.termination_category.clone().or_else(|| {
                            rejected.then(|| task.termination_category.clone()).flatten()
                        }),
                        status_reason: if rejected {
                            task.verdict_reason.clone()
                        } else {
                            a.status_reason.clone()
                        },
                        spot_warned: a.spot_warned_at.is_some(),
                    }
                })
                .collect()
        };
        let stream =
            attempts.last().and_then(|a| a.log_stream.clone()).or_else(|| task.log_stream.clone());
        // A Snakemake job's log is its step's output wrapped in the job's own Snakemake run, whose
        // failure epilogue would be all a plain tail shows (an OOM-killed job included: its log is
        // its own, not the coordinator's): show the step's lines above it, then the epilogue's
        // line naming its command's failure. That line's exit status is the step's own: the job
        // exits 1 whatever its command did, so it replaces the last attempt's 1 (or none). The
        // cause is read from the step's lines alone, never the command text the error quotes.
        let (tail, step_lines) = if snakemake {
            let log = snakemake_job_log(state, task_group.as_deref(), stream.as_deref()).await;
            if let (Some(code), Some(last)) = (log.step_exit_code, recorded.last_mut()) {
                if last.exit_code.is_none_or(|c| c == SNAKEMAKE_FAILED_JOB_EXIT) {
                    last.exit_code = Some(code);
                }
            }
            (log.tail(SUMMARY_LOG_LINES as usize), log.step_lines)
        } else {
            let tail = log_tail(state, task_group.as_deref(), stream.as_deref()).await;
            (tail.clone(), tail)
        };
        let d = diagnose(&recorded, &step_lines);
        let reason = recorded.last().and_then(|a| a.status_reason.clone());
        items.push(FailureItem {
            // The last attempt's (the task's own when no Batch attempt was recorded).
            exit_code: recorded.last().and_then(|a| a.exit_code),
            batch_job_id: attempts
                .last()
                .map(|a| a.batch_job_id.clone())
                .or_else(|| task.batch_job_id.clone()),
            task_key: Some(task.task_key.clone()),
            // The step's rule or process, from its key when no group was recorded (never the
            // key itself: a Snakemake key carries its wildcard values).
            step: crate::runs::provenance::task_group(task, Some(engine)),
            cause: d.cause.as_str().to_string(),
            sample: samples.remove(task.task_key.as_str()),
            try_again: try_again(d.cause),
            message: d.message,
            advice: d.advice,
            attempts: task.attempts.max(recorded.len() as i64),
            log_tail: tail,
            reason,
            stage: None,
        });
    }
    if items.is_empty() {
        // Failed before it launched: the control plane's reason, and at which step.
        if let Some((reason, stage)) = control_plane_failure(state, rid).await {
            let (step, advice) =
                if stage.as_deref() == Some(biffix_core::event::FAILURE_STAGE_IMAGE_PREPARATION) {
                    (
                    "image preparation",
                    "Fix the image reference or its registry, or rebuild it from the workflow's \
                     page, then submit again.",
                )
                } else {
                    // `launch`, and any stage this build does not know (or none).
                    (
                    "launch",
                    "The run could not be launched; check the deploy and the workflow version's \
                     engine runtime, then submit again.",
                )
                };
            items.push(FailureItem {
                exit_code: None,
                batch_job_id: None,
                task_key: None,
                step: step.into(),
                cause: FailureCause::LaunchFailed.as_str().into(),
                sample: None,
                // Nothing ran, so there is nothing to resume.
                try_again: TryAgain::Rerun.as_str().into(),
                message: reason.clone(),
                advice: advice.into(),
                attempts: 0,
                log_tail: vec![],
                reason: Some(reason),
                // The recorded stage as is (an open value), `launch` when none was recorded.
                stage: Some(
                    stage.unwrap_or_else(|| biffix_core::event::FAILURE_STAGE_LAUNCH.to_string()),
                ),
            });
            return Ok(FailureSummary { failed_steps, items, support });
        }
        // Coordinators always log to the shared group.
        let tail = log_tail(state, None, run.coordinator_log_stream.as_deref()).await;
        // Batch's account of the coordinator's stop outranks its log (which a coordinator that
        // never started does not have).
        let stop = &run.coordinator_stop;
        let d = diagnose_coordinator(stop, &tail);
        items.push(FailureItem {
            exit_code: stop.exit_code,
            batch_job_id: None,
            task_key: None,
            step: "workflow engine".into(),
            cause: d.cause.as_str().to_string(),
            sample: None,
            try_again: try_again(d.cause),
            message: d.message,
            advice: d.advice,
            attempts: 1,
            log_tail: tail,
            reason: stop.status_reason.clone(),
            stage: None,
        });
    }
    Ok(FailureSummary { failed_steps, items, support })
}
