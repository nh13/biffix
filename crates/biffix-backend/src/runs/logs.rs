//! A run's logs and its tasks' attempts, as the run page reads them: a page of a task's or the
//! coordinator's CloudWatch log stream ([`log_page`], [`task_log_page`]), the end of a log for a
//! failure summary ([`log_lines`]), and a task's Batch attempts ([`task_attempts`]).

use biffix_api::{LogLine, LogPage, Task, TaskAttempt, TaskAttemptList, TaskLogsRequest};
use biffix_core::ids::{BatchJobId, RunId};
use biffix_core::state::{AttemptState, TaskState};
use sha2::{Digest, Sha256};

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

/// A server-authorized log stream and whether its source has ended.
pub(crate) struct LogSource {
    pub stream: Option<String>,
    pub ended: bool,
}

/// The selected coordinator execution can outlive the engine's terminal workflow event.
/// Without execution facts, keep its ending conservative; a never-launched terminal run has no
/// coordinator left to write output.
pub(crate) async fn coordinator_source(
    state: &AppState,
    run: &RunDetailView,
) -> Result<LogSource, ServiceError> {
    let terminal = super::detail::state_is_terminal(&run.run.state);
    let mut source = LogSource { stream: run.coordinator_log_stream.clone(), ended: false };
    if let Some(job) = &run.coordinator_job_id {
        let executions = state
            .store
            .run_executions(&run.run.run_id, Some(job.as_str()))
            .await
            .map_err(ServiceError::status_only)?;
        if let Some(current) = executions.coordinator.last() {
            // The historical stream is min-latched; execution order identifies the current one.
            // Host loss or image-pull failure can retry even after the engine's terminal event.
            // Only a clean exit with its own stream proves completion without Batch's final
            // job-stop facts below; an end can fold before the start that supplies the stream.
            source.stream = current.log_stream.clone().or(source.stream);
            source.ended = terminal
                && current.log_stream.is_some()
                && current.ended
                && current.exit_code == Some(0)
                && current.interruption.is_none();
        }
    } else if source.stream.is_none() {
        source.ended = terminal;
    }
    // These are Batch's coordinator_failed facts, never the engine's workflow verdict. They also
    // prove a job that never produced an ECS execution/log (for example an image-pull failure) ended.
    source.ended |= run.coordinator_stop.exit_code.is_some()
        || run.coordinator_stop.status_reason.is_some()
        || run.coordinator_stop.termination_category.is_some();
    Ok(source)
}

/// One page of a server-derived stream, from `project_group` as for [`read_log_events`].
/// No stream, or one not created yet while its source is live, is an empty page; a missing
/// stream of an ended source is a bare 404. Cursors are bound to their stream, never authorization.
pub(crate) async fn log_page(
    state: &AppState,
    project_group: Option<&str>,
    source: LogSource,
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
    let LogSource { stream, ended } = source;
    let empty = |stream| LogPage {
        log_stream: stream,
        ended: Some(ended),
        events: vec![],
        next_forward_token: None,
        next_backward_token: None,
    };
    let Some(stream) = stream else {
        return Ok(empty(None));
    };
    let tag = cursor_tag(&stream);
    let cursor = read_cursor(&tag, cursor);
    let limit = limit.unwrap_or(200).clamp(1, 1000);
    match read_log_events(state, project_group, &stream, cursor, direction, limit).await {
        Ok(page) => Ok(LogPage {
            log_stream: Some(stream),
            ended: Some(ended),
            events: page
                .events
                .into_iter()
                .map(|e| LogLine { ts_ms: e.timestamp_ms, message: e.message })
                .collect(),
            next_forward_token: page.next_forward_token.map(|token| format!("{tag}:{token}")),
            next_backward_token: page.next_backward_token.map(|token| format!("{tag}:{token}")),
        }),
        Err(crate::aws::AwsError::NotFound) if !ended => Ok(empty(Some(stream))),
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
    let job = q.job.as_ref().map(BatchJobId::as_str);
    let attempts = if job.is_some() || q.execution.is_some() || task.batch_job_id.is_some() {
        state.store.get_attempts(rid, &task.task_key).await.map_err(ServiceError::status_only)?
    } else {
        Vec::new()
    };
    let source = if job.is_none() && q.execution.is_none() {
        // Effective task cancellation can precede the worker stopping. Its Batch attempt keeps
        // the physical lifecycle, unaffected by the enclosing run's outcome.
        let ended = task
            .batch_job_id
            .as_ref()
            .and_then(|job| attempts.iter().find(|a| a.batch_job_id == *job))
            .map(|a| a.state.parse::<AttemptState>().is_ok_and(AttemptState::is_terminal))
            .unwrap_or_else(|| {
                task.state.parse::<TaskState>().is_ok_and(|s| {
                    matches!(s, TaskState::Succeeded | TaskState::Failed)
                        || (s == TaskState::Cancelled && task.batch_job_id.is_none())
                })
            });
        LogSource { stream: task.log_stream.clone(), ended }
    } else {
        // An explicit source must belong to THIS task of THIS run; never fall back to another.
        picked_source(&attempts, job, q.execution.as_deref())
            .ok_or(ServiceError::NotFound(String::new()))?
    };
    // Queued / cache-hit / local rule: no Batch job, so no stream yet — an empty page, not 404.
    let group = project_task_log_group(&*state.projects.current().await, run);
    log_page(state, group.as_deref(), source, q.cursor.as_deref(), q.direction.as_deref(), q.limit)
        .await
}

/// A task's own selected attempt/execution and its ending; no match is a 404, never a fallback
/// to another source's stream.
fn picked_source(
    attempts: &[TaskAttempt],
    job: Option<&str>,
    execution: Option<&str>,
) -> Option<LogSource> {
    let mut attempts = attempts.iter().filter(|a| job.is_none_or(|j| a.batch_job_id.as_str() == j));
    match execution {
        None => attempts.next().map(|a| LogSource {
            stream: a.log_stream.clone(),
            ended: a.state.parse::<AttemptState>().is_ok_and(AttemptState::is_terminal),
        }),
        Some(id) => attempts
            .flat_map(|a| &a.executions)
            .find(|x| x.execution_id == id)
            .map(|x| LogSource { stream: x.log_stream.clone(), ended: x.ended }),
    }
}

/// A short stream identity, not an authorization token.
fn cursor_tag(stream: &str) -> String {
    hex::encode(&Sha256::digest(stream.as_bytes())[..8])
}

/// Accept pre-deploy raw tokens; unwrap tagged ones only for the stream they came from.
fn read_cursor<'a>(tag: &str, cursor: Option<&'a str>) -> Option<&'a str> {
    let cursor = cursor?;
    match cursor.split_once(':') {
        Some((prefix, token))
            if prefix.len() == 16 && prefix.bytes().all(|b| b.is_ascii_hexdigit()) =>
        {
            (prefix == tag).then_some(token)
        }
        _ => Some(cursor),
    }
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
/// and whether AWS Batch is finding it a new host, and with its hosts' queue capacity. 404 for a
/// task the run does not have.
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
        attempt.awaiting_host =
            super::executions::awaiting_host(&attempt.state, &task.state, &attempt.executions);
        let hosts = attempt.executions.iter_mut().filter_map(|x| x.host.as_mut());
        for host in attempt.host.as_mut().into_iter().chain(hosts) {
            fill_queue_capacity(host, &state.deploy.queue_capacities);
        }
    }
    Ok(TaskAttemptList { task_key: task.task_key.clone(), attempts })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_tags_preserve_raw_tokens_and_colons_without_crossing_streams() {
        let tag = cursor_tag("stream-A");
        assert_eq!(tag, "5415e242576d6009");
        assert_eq!(read_cursor(&tag, Some("5415e242576d6009:f/a:b")), Some("f/a:b"));
        assert_eq!(read_cursor(&tag, Some("81ec3a39c3389f74:f/a:b")), None);
        assert_eq!(read_cursor(&tag, Some("f/a:b")), Some("f/a:b"));
        assert_eq!(read_cursor(&tag, Some("raw-token")), Some("raw-token"));
        assert_eq!(read_cursor(&tag, None), None);
    }

    /// job-1 (stream s1) ran twice (x1 with stream x1-s, x2 with none yet); job-2 (s2) once (x3).
    fn attempts() -> Vec<TaskAttempt> {
        let attempt = |job: &str, stream: &str, executions: serde_json::Value| {
            serde_json::from_value(serde_json::json!({
                "batch_job_id": job, "state": "failed", "log_stream": stream,
                "executions": executions,
            }))
            .unwrap()
        };
        let execution = |id: &str, stream: Option<&str>| {
            serde_json::json!({"execution_id": id, "ordinal": 1, "ended": true,
                               "final_execution": true, "log_stream": stream})
        };
        vec![
            attempt(
                "job-1",
                "s1",
                serde_json::json!([execution("x1", Some("x1-s")), execution("x2", None)]),
            ),
            attempt("job-2", "s2", serde_json::json!([execution("x3", Some("x3-s"))])),
        ]
    }

    #[test]
    fn a_log_is_picked_only_from_the_task_s_own_attempts_and_executions() {
        let a = attempts();
        let picked_stream = |a: &[TaskAttempt], job, execution| {
            picked_source(a, job, execution).map(|source| source.stream)
        };
        let some = |s: &str| Some(Some(s.to_string()));
        assert_eq!(picked_stream(&a, Some("job-2"), None), some("s2"));
        assert_eq!(picked_stream(&a, None, Some("x1")), some("x1-s"));
        assert_eq!(picked_stream(&a, Some("job-2"), Some("x3")), some("x3-s"));
        // Found, but no stream yet: its own (none), never the attempt's.
        assert_eq!(picked_stream(&a, None, Some("x2")), Some(None));
        // Unknown job or execution, or an execution of another attempt than `job`: a 404.
        assert_eq!(picked_stream(&a, Some("job-9"), None), None);
        assert_eq!(picked_stream(&a, None, Some("x9")), None);
        assert_eq!(picked_stream(&a, Some("job-2"), Some("x1")), None);
    }
}
