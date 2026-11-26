//! Coordinator and task log reads through the authenticated API.

use std::io::Write;

use biffix_api::LogPage;

use super::Session;
use crate::api::ApiClient;
use crate::error::{CliError, CliResult};
use crate::render::{write_log_page, OutputMode};

/// Read the coordinator's or the task's display-attempt log through the user's biffix session.
pub async fn logs(
    session: &mut Session,
    run: &str,
    task: Option<&str>,
    _follow: bool,
) -> CliResult<()> {
    let client = session.authed_client().await?;
    read_to_end(&client, run, task, session.output, &mut std::io::stdout()).await
}

/// Read each forward page once, following advancing empty pages too.
async fn read_to_end<W: Write>(
    client: &ApiClient,
    run: &str,
    task: Option<&str>,
    mode: OutputMode,
    out: &mut W,
) -> CliResult<()> {
    let mut position = Position::default();
    loop {
        let page = fetch_page(client, run, task, position.cursor.as_deref()).await?;
        match position.advance(&page)? {
            Progress::Restart => {
                if mode == OutputMode::Human {
                    teprintln!("[biffix] log stream changed; restarting from its beginning");
                }
            }
            progress => {
                write_log_page(out, mode, &page.events)?;
                if progress == Progress::Idle {
                    return Ok(());
                }
            }
        }
    }
}

/// Keep the selected task key out of request URLs, and explain message-less log 404s to CLI users.
async fn fetch_page(
    client: &ApiClient,
    run: &str,
    task: Option<&str>,
    cursor: Option<&str>,
) -> CliResult<LogPage> {
    let result = match task {
        Some(task) => client.task_logs(run, task, cursor).await,
        None => client.coordinator_logs(run, cursor).await,
    };
    result.map_err(|e| match e {
        CliError::NotFound(_) => CliError::NotFound(
            "no such run or task, or its log is no longer in CloudWatch; check the run and task"
                .into(),
        ),
        other => other,
    })
}

/// Cursor progression shared by finite reads and following. Stream identity is never authorization.
#[derive(Default)]
struct Position {
    stream: Option<String>,
    cursor: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
enum Progress {
    Continue,
    Idle,
    Restart,
}

impl Position {
    /// Discard a continuation from a changed stream; retain identity across a no-stream answer.
    fn advance(&mut self, page: &LogPage) -> CliResult<Progress> {
        if page.log_stream.is_none() && !page.events.is_empty() {
            return Err(CliError::Unclassified(
                "log API returned events without a stream; check the backend version".into(),
            ));
        }
        if page.log_stream.is_none() {
            return Ok(Progress::Idle);
        }
        if let Some(stream) = &page.log_stream {
            let changed = self.stream.as_ref().is_some_and(|old| old != stream);
            self.stream = Some(stream.clone());
            if changed {
                self.cursor = None;
                return Ok(Progress::Restart);
            }
        }
        let advancing = page.next_forward_token.is_some()
            && page.next_forward_token.as_ref() != self.cursor.as_ref();
        if let Some(cursor) = &page.next_forward_token {
            self.cursor = Some(cursor.clone());
        }
        Ok(if advancing { Progress::Continue } else { Progress::Idle })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use biffix_api::LogPage;

    fn page(stream: Option<&str>, cursor: Option<&str>, messages: &[&str]) -> LogPage {
        LogPage {
            log_stream: stream.map(String::from),
            ended: Some(false),
            events: messages
                .iter()
                .map(|message| biffix_api::LogLine { ts_ms: 1, message: (*message).into() })
                .collect(),
            next_forward_token: cursor.map(String::from),
            next_backward_token: None,
        }
    }

    #[test]
    fn a_cursor_without_a_stream_cannot_replace_the_current_position() {
        let mut position = Position::default();
        position.advance(&page(Some("s"), Some("f1"), &["a"])).unwrap();
        assert_eq!(position.advance(&page(None, Some("unowned"), &[])).unwrap(), Progress::Idle);
        assert_eq!(position.cursor.as_deref(), Some("f1"));
        assert_eq!(position.stream.as_deref(), Some("s"));
    }

    #[test]
    fn empty_pages_advance_and_idle_pages_keep_the_last_usable_cursor() {
        let mut position = Position::default();
        assert_eq!(
            position.advance(&page(Some("s"), Some("f1"), &["a"])).unwrap(),
            Progress::Continue
        );
        assert_eq!(position.cursor.as_deref(), Some("f1"));
        assert_eq!(
            position.advance(&page(Some("s"), Some("f2"), &[])).unwrap(),
            Progress::Continue
        );
        assert_eq!(position.cursor.as_deref(), Some("f2"));
        assert_eq!(position.advance(&page(Some("s"), Some("f2"), &[])).unwrap(), Progress::Idle);
        assert_eq!(position.advance(&page(Some("s"), None, &[])).unwrap(), Progress::Idle);
        assert_eq!(position.cursor.as_deref(), Some("f2"));
    }

    #[test]
    fn stream_changes_restart_but_no_stream_does_not_discard_identity() {
        let mut position = Position::default();
        position.advance(&page(Some("old"), Some("f1"), &["a"])).unwrap();
        assert_eq!(
            position.advance(&page(Some("new"), Some("f2"), &["b"])).unwrap(),
            Progress::Restart
        );
        assert!(position.cursor.is_none());
        assert_eq!(position.stream.as_deref(), Some("new"));
        assert_eq!(
            position.advance(&page(Some("new"), Some("f2"), &["b"])).unwrap(),
            Progress::Continue
        );
        assert_eq!(position.advance(&page(None, None, &[])).unwrap(), Progress::Idle);
        assert_eq!(position.cursor.as_deref(), Some("f2"));
        assert_eq!(position.stream.as_deref(), Some("new"));
        assert!(position.advance(&page(None, None, &["malformed"])).is_err());
    }
}
