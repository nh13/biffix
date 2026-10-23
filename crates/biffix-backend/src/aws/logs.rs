//! CloudWatch Logs: task and coordinator logs, served through biffix's own auth ([`LogReader`]).

use async_trait::async_trait;

use super::AwsError;

/// The fleet-wide CloudWatch log group Batch jobs log into unless their job definition names
/// another: every coordinator, and the tasks of a Snakemake runtime whose executor cannot name a
/// log group (the others log to their project's,
/// `crate::config::projects::ProjectContext::task_log_group`). Matches the UI's `links.ts`
/// deep-link target; the single source of truth for the log-group name.
pub(crate) const BATCH_LOG_GROUP: &str = "/aws/batch/job";

/// Which way to page a log stream. `Backward` (tail) reads from the newest events toward older;
/// `Forward` reads oldest-first and is used to live-tail a running task.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogDirection {
    Forward,
    Backward,
}

/// One CloudWatch log event (aws-boundary type; the handler maps it to the wire `LogLine`).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CloudLogEvent {
    /// Epoch milliseconds (CloudWatch's native unit).
    pub(crate) timestamp_ms: i64,
    pub(crate) message: String,
}

/// A page of log events plus CloudWatch's opaque paging cursors (return of
/// [`LogReader::get_log_events`]).
#[derive(Clone, Debug, PartialEq, Default)]
pub struct CloudLogPage {
    pub(crate) events: Vec<CloudLogEvent>,
    pub(crate) next_forward_token: Option<String>,
    pub(crate) next_backward_token: Option<String>,
}

/// Reads a Batch job's log stream (see the module docs).
#[async_trait]
pub trait LogReader: Send + Sync {
    /// Fetch a page of a Batch job's CloudWatch log stream (in the [`BATCH_LOG_GROUP`] group), so
    /// biffix can serve task/coordinator logs through its own auth rather than sending the caller to
    /// the AWS console. `cursor` is an opaque CloudWatch token from a prior page (`None` ⇒ start at
    /// the tail for `Backward`, or the head for `Forward`); `limit` caps the events returned. The
    /// caller derives `log_stream` server-side from a run/task it has authorized — it is never
    /// client-supplied. A missing stream yields [`AwsError::NotFound`].
    async fn get_log_events(
        &self,
        log_stream: &str,
        cursor: Option<&str>,
        direction: LogDirection,
        limit: i32,
    ) -> Result<CloudLogPage, AwsError>;

    /// [`Self::get_log_events`] from `log_group` (a project's task log group,
    /// `crate::config::projects::ProjectContext::task_log_group`: where its Nextflow task jobs
    /// log), rather than [`BATCH_LOG_GROUP`]. A stream (or group) that is not there is
    /// [`AwsError::NotFound`], so the caller falls back to [`BATCH_LOG_GROUP`].
    async fn get_log_events_in(
        &self,
        log_group: &str,
        log_stream: &str,
        cursor: Option<&str>,
        direction: LogDirection,
        limit: i32,
    ) -> Result<CloudLogPage, AwsError>;
}
