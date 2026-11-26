//! Coordinator and task log reads through the authenticated API.

use std::io::Write;
use std::time::Duration;

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
    follow: bool,
) -> CliResult<()> {
    let client = session.authed_client().await?;
    read_with(
        &client,
        run,
        task,
        session.output,
        follow,
        Duration::from_secs(3),
        &mut std::io::stdout(),
    )
    .await
}

/// Drain advancing pages immediately; poll idle sources and tolerate transient follow failures.
async fn read_with<W: Write>(
    client: &ApiClient,
    run: &str,
    task: Option<&str>,
    mode: OutputMode,
    follow: bool,
    interval: Duration,
    out: &mut W,
) -> CliResult<()> {
    read_with_wait(
        client,
        run,
        task,
        mode,
        follow,
        Poll { interval, wait: tokio::time::sleep },
        out,
    )
    .await
}

/// The interval and its wait boundary travel together; production uses Tokio's timer.
struct Poll<F> {
    interval: Duration,
    wait: F,
}

/// The same reader with an injectable wait, so HTTP latency cannot stand in for polling delays.
async fn read_with_wait<W, F, Wait>(
    client: &ApiClient,
    run: &str,
    task: Option<&str>,
    mode: OutputMode,
    follow: bool,
    poll: Poll<F>,
    out: &mut W,
) -> CliResult<()>
where
    W: Write,
    F: FnMut(Duration) -> Wait,
    Wait: std::future::Future<Output = ()>,
{
    let Poll { interval, mut wait } = poll;
    let mut position = Position::default();
    let mut transient = 0;
    let mut ended_idle = 0;
    let mut printed = false;
    let mut restarting = false;
    let mut resume = format!("biffix runs logs {} --follow", shell_quote(run));
    if let Some(task) = task {
        resume.push_str(&format!(" --task {}", shell_quote(task)));
    }
    loop {
        let page = match fetch_page(client, run, task, position.cursor.as_deref()).await {
            Ok(page) => {
                transient = 0;
                page
            }
            Err(e) if follow => {
                ended_idle = 0;
                if let Some(stop) = super::runs::poll_failed(run, e, &mut transient, &resume) {
                    return Err(stop);
                }
                wait(interval).await;
                continue;
            }
            Err(e) => return Err(e),
        };
        let progress = position.advance(&page)?;
        if progress == Progress::Restart {
            ended_idle = 0;
            if mode == OutputMode::Human {
                teprintln!("[biffix] log stream changed; restarting from its beginning");
            }
            if restarting {
                wait(interval).await;
            }
            restarting = true;
            continue;
        }
        restarting = false;
        write_log_page(out, mode, &page.events)?;
        printed |= !page.events.is_empty();
        if progress == Progress::Continue {
            ended_idle = 0;
            continue;
        }
        if !follow {
            return Ok(());
        }
        if !page.events.is_empty() {
            return Err(CliError::Unclassified(
                "log API supplied no advancing cursor to continue from; update the backend or read without --follow".into(),
            ));
        }
        ended_idle = if page.ended == Some(true) { ended_idle + 1 } else { 0 };
        if ended_idle >= 2 {
            if !printed {
                teprintln!("[biffix] no log output was recorded");
            }
            return Ok(());
        }
        wait(interval).await;
    }
}

/// Quote a selected run/task for the suggested POSIX-shell resume command.
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
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
    /// Requests and waits observed at the reader's external boundaries, not inferred from time.
    struct FollowTrace {
        cursors: Vec<Option<String>>,
        waits: Vec<(usize, Duration)>,
    }

    async fn follow_replies(
        replies: Vec<(u16, LogPage)>,
        limit: Duration,
    ) -> (Result<CliResult<()>, tokio::time::error::Elapsed>, Vec<u8>, FollowTrace) {
        use std::collections::VecDeque;
        use std::sync::{Arc, Mutex};
        let fallback = replies.last().unwrap().clone();
        let queue = Arc::new(Mutex::new(VecDeque::from(replies)));
        let cursors = Arc::new(Mutex::new(Vec::new()));
        let requests = cursors.clone();
        let app = axum::Router::new().fallback(
            move |axum::Json(query): axum::Json<biffix_api::TaskLogsRequest>| {
                requests.lock().unwrap().push(query.cursor);
                let (status, page) =
                    queue.lock().unwrap().pop_front().unwrap_or_else(|| fallback.clone());
                async move { (axum::http::StatusCode::from_u16(status).unwrap(), axum::Json(page)) }
            },
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let serving = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = ApiClient::new(
            reqwest::Client::new(),
            &base,
            crate::secret::Secret::new("tok".into()),
            false,
        );
        let waits = Arc::new(Mutex::new(Vec::new()));
        let observed = waits.clone();
        let requests = cursors.clone();
        let wait = move |interval| {
            observed.lock().unwrap().push((requests.lock().unwrap().len(), interval));
            tokio::time::sleep(interval)
        };
        let mut out = Vec::new();
        let result = tokio::time::timeout(
            limit,
            read_with_wait(
                &client,
                "r1",
                Some("task-A"),
                OutputMode::Human,
                true,
                Poll { interval: Duration::from_millis(10), wait },
                &mut out,
            ),
        )
        .await;
        serving.abort();
        let trace = FollowTrace {
            cursors: cursors.lock().unwrap().clone(),
            waits: waits.lock().unwrap().clone(),
        };
        (result, out, trace)
    }

    fn expected_cursors(values: &[Option<&str>]) -> Vec<Option<String>> {
        values.iter().map(|v| v.map(String::from)).collect()
    }

    fn assert_waits(trace: &FollowTrace, after_requests: &[usize]) {
        assert_eq!(trace.waits.iter().map(|(n, _)| *n).collect::<Vec<_>>(), after_requests);
        assert!(trace.waits.iter().all(|(_, interval)| *interval == Duration::from_millis(10)));
    }

    fn ended(mut page: LogPage, value: Option<bool>) -> LogPage {
        page.ended = value;
        page
    }

    #[tokio::test]
    async fn an_error_between_ended_polls_requires_two_fresh_idle_polls() {
        let idle = (200, ended(page(Some("s"), Some("f1"), &[]), Some(true)));
        let replies =
            vec![idle.clone(), idle.clone(), (500, page(None, None, &[])), idle.clone(), idle];
        let (result, _, trace) = follow_replies(replies, Duration::from_secs(2)).await;
        result.unwrap().unwrap();
        assert_eq!(trace.cursors.len(), 5);
        assert_waits(&trace, &[2, 3, 4]);
    }

    #[tokio::test]
    async fn repeated_stream_changes_are_rate_limited() {
        let replies = vec![
            (200, page(Some("a"), Some("f1"), &["a"])),
            (200, page(Some("b"), Some("f2"), &["discard"])),
            (200, page(Some("c"), Some("f3"), &["discard"])),
            (200, page(Some("d"), Some("f4"), &["discard"])),
            (200, ended(page(Some("d"), Some("f4"), &["d"]), Some(true))),
            (200, ended(page(Some("d"), Some("f4"), &[]), Some(true))),
            (200, ended(page(Some("d"), Some("f4"), &[]), Some(true))),
        ];
        let (result, out, trace) = follow_replies(replies, Duration::from_secs(2)).await;
        result.unwrap().unwrap();
        assert_eq!(out, b"a\nd\n");
        assert_eq!(trace.cursors.len(), 7);
        assert_waits(&trace, &[3, 4, 6]);
    }

    #[tokio::test]
    async fn following_drains_empty_pages_and_waits_again_after_late_delivery() {
        let replies = vec![
            (200, page(Some("s"), Some("f1"), &["a"])),
            (200, page(Some("s"), Some("f2"), &[])),
            (200, ended(page(Some("s"), Some("f3"), &["b"]), Some(true))),
            (200, ended(page(Some("s"), Some("f3"), &[]), Some(true))),
            (200, ended(page(Some("s"), Some("f4"), &["late"]), Some(true))),
            (200, ended(page(Some("s"), Some("f4"), &[]), Some(true))),
            (200, ended(page(Some("s"), Some("f4"), &[]), Some(true))),
        ];
        let (result, out, trace) = follow_replies(replies, Duration::from_secs(2)).await;
        result.unwrap().unwrap();
        assert_eq!(out, b"a\nb\nlate\n");
        assert_eq!(
            trace.cursors,
            expected_cursors(&[
                None,
                Some("f1"),
                Some("f2"),
                Some("f3"),
                Some("f3"),
                Some("f4"),
                Some("f4")
            ])
        );
        assert_waits(&trace, &[4, 6]);
    }

    #[tokio::test]
    async fn following_waits_for_a_stream_and_preserves_tokenless_idle_position() {
        let replies = vec![
            (200, page(None, None, &[])),
            (200, page(Some("s"), Some("f1"), &["a"])),
            (200, page(Some("s"), None, &[])),
            (200, ended(page(Some("s"), Some("f1"), &[]), Some(true))),
            (200, ended(page(Some("s"), Some("f1"), &[]), Some(true))),
        ];
        let (result, out, trace) = follow_replies(replies, Duration::from_secs(2)).await;
        result.unwrap().unwrap();
        assert_eq!(out, b"a\n");
        assert_eq!(
            trace.cursors,
            expected_cursors(&[None, None, Some("f1"), Some("f1"), Some("f1")])
        );
        assert_waits(&trace, &[1, 3, 4]);
    }

    #[tokio::test]
    async fn old_servers_and_live_sources_do_not_end_following() {
        for end in [None, Some(false)] {
            let (result, out, trace) = follow_replies(
                vec![(200, ended(page(Some("s"), Some("f1"), &[]), end))],
                Duration::from_secs(1),
            )
            .await;
            assert!(result.is_err(), "a source without an ending stopped following");
            assert!(out.is_empty());
            assert!(trace.cursors.len() >= 3);
            assert_eq!(trace.cursors[0], None);
            assert!(trace.cursors[1..].iter().all(|c| c.as_deref() == Some("f1")));
            assert!(!trace.waits.is_empty());
            assert!(trace.waits.iter().all(|(_, d)| *d == Duration::from_millis(10)));
        }
    }

    #[tokio::test]
    async fn a_nonempty_page_without_progress_ends_following_without_replay() {
        for (replies, want) in [
            (vec![(200, page(Some("s"), None, &["a"]))], &b"a\n"[..]),
            (
                vec![
                    (200, page(Some("s"), Some("f1"), &["a"])),
                    (200, page(Some("s"), None, &["b"])),
                ],
                &b"a\nb\n"[..],
            ),
            (
                vec![
                    (200, page(Some("s"), Some("f1"), &["a"])),
                    (200, page(Some("s"), Some("f1"), &["b"])),
                ],
                &b"a\nb\n"[..],
            ),
        ] {
            let count = replies.len();
            let (result, out, trace) = follow_replies(replies, Duration::from_secs(2)).await;
            assert!(result.unwrap().unwrap_err().to_string().contains("cursor"));
            assert_eq!(trace.cursors.len(), count);
            assert_eq!(out, want);
            assert_waits(&trace, &[]);
        }
    }

    #[tokio::test]
    async fn follow_restarts_on_a_new_stream_and_discards_its_continuation() {
        let replies = vec![
            (200, page(Some("old"), Some("f1"), &["old"])),
            (200, page(Some("new"), Some("f2"), &["discard"])),
            (200, ended(page(Some("new"), Some("f2"), &["new"]), Some(true))),
            (200, ended(page(Some("new"), Some("f2"), &[]), Some(true))),
            (200, ended(page(Some("new"), Some("f2"), &[]), Some(true))),
        ];
        let (result, out, trace) = follow_replies(replies, Duration::from_secs(2)).await;
        result.unwrap().unwrap();
        assert_eq!(out, b"old\nnew\n");
        assert_eq!(
            trace.cursors,
            expected_cursors(&[None, Some("f1"), None, Some("f2"), Some("f2")])
        );
        assert_waits(&trace, &[4]);
    }

    #[tokio::test]
    async fn refusals_stop_following_without_retries() {
        for (status, exit) in [(400, 6), (401, 3), (403, 4), (404, 5)] {
            let (result, _, trace) =
                follow_replies(vec![(status, page(None, None, &[]))], Duration::from_secs(2)).await;
            assert_eq!(result.unwrap().unwrap_err().exit_code(), exit);
            assert_eq!(trace.cursors, vec![None]);
            assert_waits(&trace, &[]);
        }
    }

    #[tokio::test]
    async fn transient_follow_failures_have_a_budget_reset_by_success() {
        let fail = (500, page(None, None, &[]));
        let idle = (200, ended(page(Some("s"), Some("f1"), &[]), Some(true)));
        let mut replies = vec![fail.clone(); 20];
        replies.push((200, page(Some("s"), Some("f1"), &["a"])));
        replies.extend(vec![fail.clone(); 20]);
        replies.extend([idle.clone(), idle]);
        let (result, out, trace) = follow_replies(replies, Duration::from_secs(3)).await;
        result.unwrap().unwrap();
        assert_eq!(out, b"a\n");
        let mut want = vec![None; 21];
        want.extend(vec![Some("f1".to_string()); 22]);
        assert_eq!(trace.cursors, want);
        assert_waits(&trace, &(1..=20).chain(22..=42).collect::<Vec<_>>());
        let (result, _, trace) = follow_replies(vec![fail], Duration::from_secs(2)).await;
        let error = result.unwrap().unwrap_err();
        assert!(error.to_string().contains("biffix runs logs"), "{error}");
        assert!(error.to_string().contains("--task"), "{error}");
        assert_eq!(trace.cursors, vec![None; 21]);
        assert_waits(&trace, &(1..=20).collect::<Vec<_>>());
    }
}
