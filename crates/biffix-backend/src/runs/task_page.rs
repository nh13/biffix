//! A run's tasks as one filtered, sorted page — the Jobs view's query
//! (`POST /runs/{id}/tasks/query`).
//!
//! [`page_tasks`] is the reference semantics: a pure function over a run's full task list. The
//! Postgres store answers the same query in SQL (so a big run is not loaded whole to show one
//! page), and its tests check it against this function.

use std::collections::BTreeMap;

use biffix_api::{Task, TaskFilterCounts, TaskPage};
use biffix_core::state::TaskState;

// The Jobs view's state chips are the shared wire type.
pub use biffix_api::TaskStateFilter;

/// The order of a page.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TaskSort {
    /// By task key.
    #[default]
    Key,
    /// Failed first, then running, queued, succeeded; ties by key.
    State,
    /// Longest first (a running task's duration so far); unstarted last.
    Duration,
    /// Most recently started first; unstarted last.
    Started,
}

impl TaskSort {
    /// The accepted spellings, for a caller's error message.
    pub(crate) const ACCEPTED: &'static str = "key, state, duration or started";

    pub(crate) fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "key" => Self::Key,
            "state" => Self::State,
            "duration" => Self::Duration,
            "started" => Self::Started,
            _ => return None,
        })
    }
}

/// Filters, order and paging for [`page_tasks`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TaskQuery {
    pub state: TaskStateFilter,
    /// Only this group (rule/process).
    pub group: Option<String>,
    /// Case-insensitive substring of the task key.
    pub q: Option<String>,
    /// Only the task with exactly this key (a linked task, which a substring search can bury under
    /// the many keys that contain it).
    pub task_key: Option<String>,
    pub sort: TaskSort,
    /// Reverse the sort's natural order.
    pub reverse: bool,
    pub offset: i64,
    pub limit: i64,
}

/// One page of `tasks` for `query`, with `feeds` filled in from the whole list. `now` (epoch
/// seconds) measures a running task's duration.
pub fn page_tasks(tasks: &[Task], query: &TaskQuery, now: f64) -> TaskPage {
    let needle = query.q.as_deref().map(str::trim).filter(|q| !q.is_empty()).map(str::to_lowercase);
    // Group and search narrow everything, including the chip counts; the state filter is the
    // chips' own choice, so it applies to the rows only.
    let narrowed: Vec<&Task> = tasks
        .iter()
        .filter(|t| query.group.is_none() || t.group == query.group)
        .filter(|t| query.task_key.as_deref().is_none_or(|k| t.task_key.as_str() == k))
        .filter(|t| {
            needle.as_deref().is_none_or(|n| t.task_key.as_str().to_lowercase().contains(n))
        })
        .collect();
    let counts = TaskFilterCounts {
        all: narrowed.len() as i64,
        active: count(&narrowed, TaskStateFilter::Active),
        succeeded: count(&narrowed, TaskStateFilter::Succeeded),
        failed: count(&narrowed, TaskStateFilter::Failed),
    };
    let mut rows: Vec<&Task> =
        narrowed.into_iter().filter(|t| query.state.matches(&t.state)).collect();
    sort_tasks(&mut rows, query.sort, now);
    if query.reverse {
        rows.reverse();
    }
    let total = rows.len() as i64;
    let feeds = feeds_by_task(tasks);
    let page = rows
        .into_iter()
        .skip(query.offset.max(0) as usize)
        .take(query.limit.max(0) as usize)
        .map(|t| {
            let mut t = t.clone();
            t.feeds = feeds.get(t.task_key.as_str()).cloned().unwrap_or_default();
            t
        })
        .collect();
    TaskPage {
        tasks: page,
        total,
        offset: query.offset.max(0),
        limit: query.limit,
        counts,
        log_group: None,
    }
}

fn count(tasks: &[&Task], filter: TaskStateFilter) -> i64 {
    tasks.iter().filter(|t| filter.matches(&t.state)).count() as i64
}

/// Sort in `sort`'s natural order, ties by key, so every order is total and paging is stable.
fn sort_tasks(rows: &mut [&Task], sort: TaskSort, now: f64) {
    // Borrowed keys: no allocation per comparison.
    fn key(t: &Task) -> &str {
        t.task_key.as_str()
    }
    match sort {
        TaskSort::Key => rows.sort_by(|a, b| key(a).cmp(key(b))),
        TaskSort::State => {
            rows.sort_by(|a, b| {
                (state_rank(&a.state), key(a)).cmp(&(state_rank(&b.state), key(b)))
            });
        }
        TaskSort::Duration => rows.sort_by(|a, b| {
            // Longest first; a task that never started sorts after every one that did.
            let (da, db) = (duration(a, now), duration(b, now));
            db.is_some()
                .cmp(&da.is_some())
                .then(db.partial_cmp(&da).unwrap_or(std::cmp::Ordering::Equal))
                .then_with(|| key(a).cmp(key(b)))
        }),
        TaskSort::Started => rows.sort_by(|a, b| {
            b.started_at
                .is_some()
                .cmp(&a.started_at.is_some())
                .then(b.started_at.partial_cmp(&a.started_at).unwrap_or(std::cmp::Ordering::Equal))
                .then_with(|| key(a).cmp(key(b)))
        }),
    }
}

/// Where a state sorts under [`TaskSort::State`]: what needs attention first, and a state no
/// [`TaskState`] spells last. Exhaustive, so a new state must be ranked to compile.
pub(crate) fn state_rank(state: &str) -> u8 {
    match state.parse::<TaskState>() {
        Ok(TaskState::Failed) => 0,
        Ok(TaskState::Running) => 1,
        Ok(TaskState::Queued) => 2,
        Ok(TaskState::Succeeded) => 3,
        Err(_) => 4,
    }
}

/// How long a task ran (or has been running), in seconds; `None` if it never started.
fn duration(t: &Task, now: f64) -> Option<f64> {
    let started = t.started_at?;
    Some(t.finished_at.unwrap_or(now) - started)
}

/// Each task's dependents: the inverse of `deps`, sorted, over the whole run.
pub(crate) fn feeds_by_task(tasks: &[Task]) -> BTreeMap<&str, Vec<String>> {
    let mut feeds: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    for t in tasks {
        for dep in &t.deps {
            if dep != t.task_key.as_str() {
                feeds.entry(dep.as_str()).or_default().push(t.task_key.as_str().to_string());
            }
        }
    }
    for list in feeds.values_mut() {
        list.sort();
        list.dedup();
    }
    feeds
}

#[cfg(test)]
mod tests {
    use super::*;
    use biffix_core::ids::TaskKey;

    fn task(
        key: &str,
        state: &str,
        group: &str,
        started: Option<f64>,
        finished: Option<f64>,
    ) -> Task {
        Task {
            task_key: TaskKey::new(key),
            state: state.into(),
            attempts: 1,
            exit_code: None,
            batch_job_id: None,
            log_stream: None,
            termination_category: None,
            instance_id: None,
            started_at: started,
            finished_at: finished,
            cached: false,
            group: Some(group.into()),
            deps: vec![],
            feeds: vec![],
            cost_usd: None,
            host: None,
            spot_warned_at: None,
            image: None,
            verdict_reason: None,
            requested_vcpus: None,
            requested_memory_mib: None,
        }
    }

    fn tasks() -> Vec<Task> {
        let mut call = task("call:s=A", "running", "call", Some(100.0), None);
        call.deps = vec!["align:s=A".into()];
        let mut joint = task("joint", "queued", "joint", None, None);
        joint.deps = vec!["call:s=A".into(), "call:s=B".into()];
        vec![
            task("align:s=B", "failed", "align", Some(0.0), Some(50.0)),
            task("align:s=A", "succeeded", "align", Some(0.0), Some(300.0)),
            call,
            task("call:s=B", "succeeded", "call", Some(10.0), Some(20.0)),
            joint,
        ]
    }

    fn keys(page: &TaskPage) -> Vec<&str> {
        page.tasks.iter().map(|t| t.task_key.as_str()).collect()
    }

    fn query() -> TaskQuery {
        TaskQuery { limit: 100, ..Default::default() }
    }

    #[test]
    fn states_are_read_through_task_state() {
        for (state, active, rank) in [
            ("failed", false, 0),
            ("running", true, 1),
            ("queued", true, 2),
            ("succeeded", false, 3),
            // A state no TaskState spells: shown only under All, and sorted last.
            ("paused", false, 4),
        ] {
            assert_eq!(TaskStateFilter::Active.matches(state), active, "{state}");
            assert!(TaskStateFilter::All.matches(state), "{state}");
            assert_eq!(state_rank(state), rank, "{state}");
        }
        assert!(
            TaskStateFilter::Failed.matches("failed") && !TaskStateFilter::Failed.matches("paused")
        );
        assert!(TaskStateFilter::Succeeded.matches("succeeded"));
    }

    #[test]
    fn default_is_every_task_by_key_with_counts() {
        let page = page_tasks(&tasks(), &query(), 1000.0);
        assert_eq!(keys(&page), ["align:s=A", "align:s=B", "call:s=A", "call:s=B", "joint"]);
        assert_eq!(page.total, 5);
        assert_eq!(page.counts, TaskFilterCounts { all: 5, active: 2, succeeded: 2, failed: 1 });
    }

    #[test]
    fn state_filters_rows_but_not_counts() {
        let page =
            page_tasks(&tasks(), &TaskQuery { state: TaskStateFilter::Active, ..query() }, 1000.0);
        assert_eq!(keys(&page), ["call:s=A", "joint"]);
        assert_eq!(page.total, 2);
        assert_eq!(page.counts.all, 5, "the chips still count every state");
    }

    #[test]
    fn an_exact_key_finds_a_task_that_many_others_contain() {
        // `align:s=A` is a substring of 60 other keys: a substring search pages it away, the
        // exact-key filter finds only it.
        let mut all: Vec<Task> = (0..60)
            .map(|i| task(&format!("align:s=A{i:02}"), "succeeded", "align", None, None))
            .collect();
        all.push(task("align:s=A", "failed", "align", None, None));
        let search = page_tasks(
            &all,
            &TaskQuery { q: Some("align:s=A".into()), limit: 50, ..query() },
            1000.0,
        );
        assert_eq!(search.total, 61);
        let page = page_tasks(
            &all,
            &TaskQuery { task_key: Some("align:s=A".into()), limit: 1, ..query() },
            1000.0,
        );
        assert_eq!(keys(&page), ["align:s=A"]);
        assert_eq!((page.total, page.counts.all), (1, 1));
        let none =
            page_tasks(&all, &TaskQuery { task_key: Some("align:s=a".into()), ..query() }, 1000.0);
        assert_eq!(none.total, 0, "exact, so case-sensitive");
    }

    #[test]
    fn group_and_search_narrow_rows_and_counts() {
        let page =
            page_tasks(&tasks(), &TaskQuery { group: Some("align".into()), ..query() }, 1000.0);
        assert_eq!(keys(&page), ["align:s=A", "align:s=B"]);
        assert_eq!(page.counts.all, 2);
        assert_eq!(page.counts.failed, 1);

        let page = page_tasks(&tasks(), &TaskQuery { q: Some("  S=B ".into()), ..query() }, 1000.0);
        assert_eq!(keys(&page), ["align:s=B", "call:s=B"], "trimmed, case-insensitive");
    }

    #[test]
    fn sorts_are_total_and_reversible() {
        let by = |sort, reverse| {
            keys(&page_tasks(&tasks(), &TaskQuery { sort, reverse, ..query() }, 1000.0))
                .into_iter()
                .map(str::to_string)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            by(TaskSort::State, false),
            ["align:s=B", "call:s=A", "joint", "align:s=A", "call:s=B"]
        );
        // call:s=A has run 900 s so far (now = 1000); joint never started.
        assert_eq!(
            by(TaskSort::Duration, false),
            ["call:s=A", "align:s=A", "align:s=B", "call:s=B", "joint"]
        );
        assert_eq!(
            by(TaskSort::Started, false),
            ["call:s=A", "call:s=B", "align:s=A", "align:s=B", "joint"]
        );
        let mut reversed = by(TaskSort::Key, false);
        reversed.reverse();
        assert_eq!(by(TaskSort::Key, true), reversed);
    }

    #[test]
    fn pages_by_offset_and_limit() {
        let page = page_tasks(&tasks(), &TaskQuery { offset: 3, limit: 10, ..query() }, 0.0);
        assert_eq!(keys(&page), ["call:s=B", "joint"]);
        assert_eq!((page.total, page.offset), (5, 3));
        let past = page_tasks(&tasks(), &TaskQuery { offset: 50, ..query() }, 0.0);
        assert!(past.tasks.is_empty());
        assert_eq!(past.total, 5);
    }

    #[test]
    fn feeds_come_from_the_whole_run_not_the_page() {
        // Only align:s=A is on the page, but call:s=A (off the page) depends on it.
        let page = page_tasks(&tasks(), &TaskQuery { q: Some("align:s=A".into()), ..query() }, 0.0);
        assert_eq!(page.tasks[0].feeds, ["call:s=A"]);
        let all = page_tasks(&tasks(), &query(), 0.0);
        let call_a = all.tasks.iter().find(|t| t.task_key.as_str() == "call:s=A").unwrap();
        assert_eq!(call_a.feeds, ["joint"]);
        assert!(all
            .tasks
            .iter()
            .find(|t| t.task_key.as_str() == "joint")
            .unwrap()
            .feeds
            .is_empty());
    }
}
