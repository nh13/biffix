//! [`TaskReads`]: a run's tasks and their Batch attempts.

use async_trait::async_trait;
use biffix_api::{Task, TaskAttempt};
use biffix_core::ids::{RunId, TaskKey};

use super::runs::RunReads;
use super::StoreError;

/// A run's tasks: one, a page, their samples, and a task's Batch attempts. The defaults read
/// the run's detail ([`RunReads::get_run`]).
#[async_trait]
pub trait TaskReads: RunReads {
    /// One task of the run; `None` when the run has no such task.
    async fn get_task(&self, run_id: &RunId, task_key: &str) -> Result<Option<Task>, StoreError> {
        Ok(self.get_run(run_id).await?.tasks.into_iter().find(|t| t.task_key.as_str() == task_key))
    }

    /// One page of the run's tasks ([`crate::runs::task_page`]); `now` (epoch seconds) measures a
    /// running task's duration.
    async fn list_tasks(
        &self,
        run_id: &RunId,
        query: &crate::runs::task_page::TaskQuery,
        now: f64,
    ) -> Result<biffix_api::TaskPage, StoreError> {
        let detail = self.get_run(run_id).await?;
        Ok(crate::runs::task_page::page_tasks(&detail.tasks, query, now))
    }

    /// The sample each of `task_keys` belongs to (the per-sample attribution stored with the task),
    /// by task key. A whole-study task, or one not in the run, is absent.
    async fn task_samples(
        &self,
        run_id: &RunId,
        task_keys: &[String],
    ) -> Result<std::collections::BTreeMap<String, String>, StoreError>;

    /// Every Batch attempt of one task, ordered by attempt number (unnumbered last), then job id.
    /// `display` is left false; the caller marks the display attempt from the task's summary. An
    /// unknown task or a task with no Batch attempts (a local rule, a cache hit) yields an empty list.
    async fn get_attempts(
        &self,
        run_id: &RunId,
        task_key: &TaskKey,
    ) -> Result<Vec<TaskAttempt>, StoreError>;
}
