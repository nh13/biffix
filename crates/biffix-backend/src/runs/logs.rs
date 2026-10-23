//! A run's logs and its tasks' attempts, as the run page reads them: a page of a task's or the
//! coordinator's CloudWatch log stream ([`log_page`], [`task_log_page`]), the end of a log for a
//! failure summary ([`log_lines`]), and a task's Batch attempts ([`task_attempts`]).

use biffix_api::{LogLine, LogPage, Task, TaskAttemptList, TaskLogsRequest};
use biffix_core::ids::{BatchJobId, RunId};

use crate::aws::logs::LogDirection;
use crate::config::queue_capacity::fill_queue_capacity;
use crate::error::ServiceError;
use crate::store::RunDetailView;
use crate::AppState;

/// Where `run`'s task log streams are, besides [`crate::aws::logs::BATCH_LOG_GROUP`]: its
/// project's task log group (the coordinator's `aws.batch.logsGroup` / `--aws-batch-log-group`), unless the run
/// ran on a Snakemake runtime whose executor has no such setting; those streams are in the shared
/// group, which [`read_log_events`] falls back to. Every coordinator logs to the shared group only.
pub(crate) fn project_task_log_group(
    projects: &crate::config::projects::Projects,
    run: &RunDetailView,
) -> Option<String> {
    Some(projects.for_run(run).task_log_group.clone()).filter(|g| !g.is_empty())
}

/// A page of `stream`: from `project_group` (see [`project_task_log_group`]) when given, else, or
/// when the stream is not there, from the shared Batch group.
/// A stream is in exactly one group (its name carries its ECS task id), so the fallback never reads
/// another job's stream.
async fn read_log_events(
    state: &AppState,
    project_group: Option<&str>,
    stream: &str,
    cursor: Option<&str>,
    direction: LogDirection,
    limit: i32,
) -> Result<crate::aws::logs::CloudLogPage, crate::aws::AwsError> {
    if let Some(group) = project_group {
        match state.cloud.get_log_events_in(group, stream, cursor, direction, limit).await {
            Err(crate::aws::AwsError::NotFound) => {}
            other => return other,
        }
    }
    state.cloud.get_log_events(stream, cursor, direction, limit).await
}

/// The last `limit` lines of a log stream, oldest first; empty when there is none or it cannot be
/// read (a summary without its log is still worth showing).
pub(crate) async fn log_lines(
    state: &AppState,
    project_group: Option<&str>,
    stream: Option<&str>,
    limit: i32,
) -> Vec<String> {
    let Some(stream) = stream else {
        return Vec::new();
    };
    match read_log_events(state, project_group, stream, None, LogDirection::Backward, limit).await {
        Ok(page) => page.events.into_iter().map(|e| e.message).collect(),
        Err(e) => {
            tracing::warn!(error = %e, "failure summary: log read failed");
            Vec::new()
        }
    }
}

/// One page of a CloudWatch log stream, shared by the task and coordinator logs. The caller
/// derives `stream` server-side from what the viewer is authorized to see — never from the client
/// — and `project_group` as for [`read_log_events`].
/// No stream ⇒ an empty page (`log_stream: None`), not a 404; a stream CloudWatch no longer has
/// (rotated/expired) ⇒ 404; any other CloudWatch error ⇒ 502; a `direction` other than `forward`
/// or `backward` ⇒ 400.
pub(crate) async fn log_page(
    state: &AppState,
    project_group: Option<&str>,
    stream: Option<String>,
    cursor: Option<&str>,
    direction: Option<&str>,
    limit: Option<i32>,
) -> Result<LogPage, ServiceError> {
    // A spelling biffix does not know is the client's bug, not a request for the end of the log.
    let direction = match direction {
        Some("forward") => LogDirection::Forward,
        None | Some("backward") => LogDirection::Backward,
        Some(other) => {
            return Err(ServiceError::Invalid(format!(
                "unknown direction {other:?}: `forward` or `backward`"
            )))
        }
    };
    let Some(stream) = stream else {
        return Ok(LogPage {
            log_stream: None,
            events: vec![],
            next_forward_token: None,
            next_backward_token: None,
        });
    };
    let limit = limit.unwrap_or(200).clamp(1, 1000);
    match read_log_events(state, project_group, &stream, cursor, direction, limit).await {
        Ok(page) => Ok(LogPage {
            log_stream: Some(stream),
            events: page
                .events
                .into_iter()
                .map(|e| LogLine { ts_ms: e.timestamp_ms, message: e.message })
                .collect(),
            next_forward_token: page.next_forward_token,
            next_backward_token: page.next_backward_token,
        }),
        // Stream expired/rotated/not-yet-created ⇒ 404 (not a server error).
        Err(crate::aws::AwsError::NotFound) => Err(ServiceError::NotFound(String::new())),
        Err(e) => {
            tracing::warn!(error = %e, "log page: cloudwatch error");
            Err(ServiceError::status_only(e))
        }
    }
}

/// A page of one task's log (`q.task`, or `q.job`, one of its attempts) of `run`. The stream is
/// derived from the task, never named by the client; a task with no stream yet is an empty page.
pub(crate) async fn task_log_page(
    state: &AppState,
    run: &RunDetailView,
    q: TaskLogsRequest,
) -> Result<LogPage, ServiceError> {
    let rid = &run.run.run_id;
    // Derive the stream from a task the caller is authorized to see — never trust a client-supplied
    // stream name (that would let a viewer read any stream in the log group).
    let task = task_or_404(state, rid, q.task.as_str()).await?;
    let stream = match q.job.as_ref().map(BatchJobId::as_str) {
        None => task.log_stream.clone(),
        // A specific attempt: only one recorded for THIS task of THIS run, so a viewer can never
        // name another run's (or task's) job to read its stream.
        Some(job) => {
            state
                .store
                .get_attempts(rid, &task.task_key)
                .await
                .map_err(ServiceError::status_only)?
                .into_iter()
                .find(|a| a.batch_job_id.as_str() == job)
                .ok_or(ServiceError::NotFound(String::new()))?
                .log_stream
        }
    };
    // Queued / cache-hit / local rule: no Batch job, so no stream yet — an empty page, not 404.
    let group = project_task_log_group(&*state.projects.current().await, run);
    log_page(state, group.as_deref(), stream, q.cursor.as_deref(), q.direction.as_deref(), q.limit)
        .await
}

/// Task `key` of run `rid`, or a bare 404 when the run has no such task.
async fn task_or_404(state: &AppState, rid: &RunId, key: &str) -> Result<Task, ServiceError> {
    state
        .store
        .get_task(rid, key)
        .await
        .map_err(ServiceError::status_only)?
        .ok_or(ServiceError::NotFound(String::new()))
}

/// Every Batch attempt of `task` of run `rid`, each marked whether it is the task's display attempt
/// and with its host's queue capacity. 404 for a task the run does not have.
pub(crate) async fn task_attempts(
    state: &AppState,
    rid: &RunId,
    task: &str,
) -> Result<TaskAttemptList, ServiceError> {
    let task = task_or_404(state, rid, task).await?;
    let mut attempts =
        state.store.get_attempts(rid, &task.task_key).await.map_err(ServiceError::status_only)?;
    for attempt in &mut attempts {
        attempt.display = task.batch_job_id.as_ref() == Some(&attempt.batch_job_id);
        if let Some(host) = attempt.host.as_mut() {
            fill_queue_capacity(host, &state.deploy.queue_capacities);
        }
    }
    Ok(TaskAttemptList { task_key: task.task_key.clone(), attempts })
}
