//! [`RunReads`]: the run list, a run's detail and sample facts, and the queries they take.

use std::collections::HashMap;

use async_trait::async_trait;
use biffix_api::RunSummary;
use biffix_core::ids::RunId;

use super::{RunDetailView, StoreError};

// The runs list's state filter is the shared wire type, so the CLI's `--state` and the UI's chips
// name the same set this store filters by.
pub use biffix_api::RunFilter;

/// A page request for listing runs: a filter plus optional free-text search and paging window.
#[derive(Clone, Debug)]
pub struct RunQuery {
    pub filter: RunFilter,
    /// Case-insensitive substring search, when set. A run matches when one of these fields contains
    /// the whole term: run id, run name, workflow name, pipeline title (the frozen form's `title`,
    /// trimmed), workflow-version id, submitter, submitter label, project, engine. `%`, `_` and `\`
    /// in the term match literally.
    pub q: Option<String>,
    /// Exact frozen workflow-version id to scope to (the runs of one registered version), when set.
    /// Unlike `q` (a fuzzy search that conflates versions), this is an equality filter.
    pub workflow: Option<String>,
    /// Exact submitter to scope to (`user:<sub>`, as a run records it), when set: "my runs".
    pub submitter: Option<String>,
    /// Only runs that need this user's attention, when set (the user's id, the `sub` their
    /// notifications are addressed to): a failed run whose `failed` notification for them they have
    /// not dismissed ([`NotificationStore::dismiss_attention`]; reading it is not enough). The
    /// caller also scopes [`Self::submitter`] to them.
    ///
    /// [`NotificationStore::dismiss_attention`]:
    /// crate::store::notifications::NotificationStore::dismiss_attention
    pub attention: Option<String>,
    /// Only the runs in this experiment (the folded `run_projection.experiment_id`), when set.
    pub experiment: Option<String>,
    /// Only the runs with these ids, when set (an id no run has matches nothing, and an empty list
    /// matches no run).
    pub ids: Option<Vec<String>>,
    /// Max rows to return (the store clamps to a sane ceiling).
    pub limit: i64,
    /// Keyset cursor: return the page of rows ordered *after* this one (the next, older page, in
    /// [`run_sort_key`] order). An opaque token from a prior [`RunPage::last_cursor`]. Takes
    /// precedence over `before`; `None` on both means the first page, and so does a token that does
    /// not decode. Replaces OFFSET so deep pages stay O(limit), not O(offset).
    pub after: Option<String>,
    /// Keyset cursor for the previous (newer) page, from a prior [`RunPage::first_cursor`]. A token
    /// that does not decode gives the first page, in the normal order.
    pub before: Option<String>,
}

/// The most runs one page of [`RunReads::list_runs`] returns.
pub const MAX_RUN_PAGE: i64 = 200;

impl RunQuery {
    /// The page size a store actually uses: `limit` clamped to `1..=`[`MAX_RUN_PAGE`]. The one
    /// place the clamp lives, so the size a response reports is the size the store served.
    pub(crate) fn page_size(&self) -> i64 {
        self.limit.clamp(1, MAX_RUN_PAGE)
    }
}

impl Default for RunQuery {
    fn default() -> Self {
        Self {
            filter: RunFilter::All,
            q: None,
            workflow: None,
            submitter: None,
            attention: None,
            experiment: None,
            ids: None,
            limit: 50,
            after: None,
            before: None,
        }
    }
}

/// A page of runs plus the total count matching the filter/search (for pagination controls) and the
/// keyset cursors bounding this page. `first_cursor`/`last_cursor` are `None` when the page is
/// empty; a caller pages back with `first_cursor` (as `before`) and forward with `last_cursor` (as
/// `after`).
#[derive(Clone, Debug, PartialEq)]
pub struct RunPage {
    pub runs: Vec<RunSummary>,
    pub total: i64,
    pub first_cursor: Option<String>,
    pub last_cursor: Option<String>,
}

/// The runs of one workflow version ([`RunReads::run_stats_by_workflow`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RunStats {
    /// How many runs were submitted against it.
    pub count: i64,
    /// When the newest of them was submitted, epoch micros; `None` when none records a time.
    pub last_submitted_micros: Option<i64>,
}

/// What a run's samples grid is built from ([`RunReads::runs_sample_facts`]): its sample spec
/// (`None` before an anchor folded), its tasks with the sample each belongs to, and its DAG.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RunSampleFacts {
    pub spec: Option<biffix_core::event::SampleSpec>,
    pub tasks: Vec<biffix_core::sample_progress::TaskFact>,
    pub dag_nodes: Vec<String>,
    pub dag_edges: Vec<(String, String)>,
}

/// The run list's sort key for a run submitted at `submitted_at_micros`. The list is newest
/// submitted first, ties broken by `run_id` descending in byte order. A run whose anchor is not
/// folded yet has no `submitted_at` and sorts first, as the newest.
///
/// The key is fold-derived and set once (from the anchor), unlike the row's write time. So a run
/// that advances while a caller pages does not cross the cursor, and a replay does not reshuffle
/// the list. The one move is a not-yet-anchored run dropping from the top to its submit time when
/// its anchor lands, which can show it twice but never hides it. The Postgres store computes the
/// same key in SQL (`RUN_SORT_KEY` in pg/runs.rs).
// In-memory stores are test doubles; the Postgres store computes this in SQL.
#[cfg_attr(not(any(test, feature = "test-support")), allow(dead_code))]
pub(crate) fn run_sort_key(submitted_at_micros: Option<i64>) -> i64 {
    submitted_at_micros.unwrap_or(i64::MAX)
}

/// Encode a keyset cursor from a run's [`run_sort_key`] and `run_id`. Opaque and URL-safe (base64)
/// so callers treat it as a token; `|` never appears in an integer, so the split is unambiguous.
pub(crate) fn encode_cursor(sort_key: i64, run_id: &str) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(format!("{sort_key}|{run_id}"))
}

/// Decode a keyset cursor into `(sort_key, run_id)`. `None` if it is not a cursor this service
/// issues: bad base64, no delimiter, a key that is not an integer (which includes the
/// timestamp-keyed cursors earlier versions issued), or a run id holding a NUL (no run id does).
/// Callers treat `None` as "no cursor" and serve the first page, never an error. The key is bound
/// as an integer and the run id as text Postgres accepts, so no decoded token reaches the database
/// as a value it could fail to cast or store.
pub(crate) fn decode_cursor(cursor: &str) -> Option<(i64, String)> {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(cursor).ok()?;
    let decoded = String::from_utf8(bytes).ok()?;
    let (key, run_id) = decoded.split_once('|')?;
    if run_id.contains('\0') {
        return None;
    }
    Some((key.parse().ok()?, run_id.to_string()))
}

/// One run a run resumed, directly or through others ([`RunReads::resume_origins`]).
#[derive(Clone, Debug, PartialEq)]
pub struct ResumeOrigin {
    pub run_id: RunId,
    /// Its recorded output prefix (`RunDetailView::output_prefix`).
    pub output_prefix: Option<String>,
}

/// The runs: the list, a run's detail, its sample facts, state and engine, the runs it resumed,
/// and the per-workflow run counts.
#[async_trait]
pub trait RunReads: Send + Sync {
    /// One page of runs matching `query`'s filter, search and workflow scope, in [`run_sort_key`]
    /// order (newest submitted first, then `run_id` descending), with the total match count and the
    /// page's keyset cursors.
    async fn list_runs(&self, query: &RunQuery) -> Result<RunPage, StoreError>;

    /// The run's full detail, every task included.
    async fn get_run(&self, run_id: &RunId) -> Result<RunDetailView, StoreError>;

    /// The run's detail without having to load its tasks: `tasks` may be empty (a store that can
    /// do better than [`Self::get_run`] leaves them out), but `task_groups` and `cost` are filled.
    /// For every read that needs the run but not its task table.
    async fn get_run_light(&self, run_id: &RunId) -> Result<RunDetailView, StoreError> {
        self.get_run(run_id).await
    }

    /// What per-sample progress reads of a run: its sample spec (from the anchor; `None` before
    /// one folded) and its tasks with the sample each belongs to. Empty for an unknown run.
    async fn sample_facts(
        &self,
        run_id: &RunId,
    ) -> Result<
        (Option<biffix_core::event::SampleSpec>, Vec<biffix_core::sample_progress::TaskFact>),
        StoreError,
    >;

    /// [`Self::sample_facts`] and the DAG of each of `run_ids`, by run id, for a read over many
    /// runs (an experiment's samples). An unknown run is absent. The default reads run by run;
    /// Postgres reads them all in two queries.
    async fn runs_sample_facts(
        &self,
        run_ids: &[RunId],
    ) -> Result<HashMap<String, RunSampleFacts>, StoreError> {
        let mut out = HashMap::new();
        for run_id in run_ids {
            let Some(run) = super::found(self.get_run_light(run_id).await)? else {
                continue;
            };
            let (spec, tasks) = self.sample_facts(run_id).await?;
            out.insert(
                run_id.to_string(),
                RunSampleFacts { spec, tasks, dag_nodes: run.dag_nodes, dag_edges: run.dag_edges },
            );
        }
        Ok(out)
    }

    /// The run's sample ids in the samples grid's order
    /// ([`biffix_core::sample_progress::sample_ids`]): its samplesheet's ids, then any other sample
    /// a task was matched to or that it carried over from the run it resumed
    /// ([`biffix_core::event::SampleSpec::reused`]: a resume skips a finished sample's tasks). The
    /// default reads every task through [`Self::sample_facts`]; Postgres reads only the distinct
    /// samples.
    async fn run_sample_ids(&self, run_id: &RunId) -> Result<Vec<String>, StoreError> {
        let (spec, tasks) = self.sample_facts(run_id).await?;
        let spec = spec.unwrap_or_default();
        Ok(biffix_core::sample_progress::sample_ids(
            &spec.ids,
            tasks
                .iter()
                .filter_map(|t| t.sample.as_deref())
                .chain(spec.reused.iter().map(|r| r.id.as_str())),
        ))
    }

    /// The run's current derived state string (`RunState::as_str`), or `None` if the run is not yet
    /// projected. A single-column point lookup: the `POST /events` hot path uses it to enforce
    /// capability revocation-by-recheck without loading the run's full detail — a cancelled run's
    /// token no longer authorizes appends (cancel == revoke), so no separate revocation list is
    /// needed. `None` (unprojected) is treated as not-revoked; only an explicit CANCELLED is.
    async fn run_state(&self, run_id: &RunId) -> Result<Option<String>, StoreError>;

    /// The run's engine (`snakemake` / `nextflow`), joined from its workflow version, or `None` if
    /// the run/version is not yet projected. The ingest path uses it to record engine events under
    /// the correct `EventSource` (the engine wire shape is engine-agnostic, so the run's registered
    /// engine is the source of truth); an unknown engine falls back to the Snakemake default.
    async fn run_engine(&self, run_id: &RunId) -> Result<Option<String>, StoreError>;

    /// The runs `run_id` resumed, nearest first: its `resumed_from`, that run's, and so on, at most
    /// `max_hops` of them. The walk ends at a run that resumed nothing, one not found, or one
    /// already seen (a cycle, which submit cannot make but a hand-edited log could).
    /// The default walks the runs one [`Self::get_run_light`] at a time; Postgres walks them in
    /// one recursive query.
    async fn resume_origins(
        &self,
        run_id: &RunId,
        max_hops: usize,
    ) -> Result<Vec<ResumeOrigin>, StoreError> {
        let mut seen = std::collections::HashSet::from([run_id.clone()]);
        let mut origins = Vec::new();
        let mut next = super::found(self.get_run_light(run_id).await)?.and_then(|d| d.resumed_from);
        while let Some(id) = next.take() {
            if origins.len() >= max_hops || !seen.insert(id.clone()) {
                break;
            }
            let Some(detail) = super::found(self.get_run_light(&id).await)? else {
                break;
            };
            next = detail.resumed_from;
            origins.push(ResumeOrigin { run_id: id, output_prefix: detail.output_prefix });
        }
        Ok(origins)
    }

    /// Runs per frozen workflow-version id (keyed by `run_projection.workflow_version`): how many,
    /// and when the newest was submitted. Powers the "Runs" column on the workflows list, the
    /// per-version counts on the Versions tab, and the catalog's *last run*. One query.
    ///
    /// No default: an empty default silently applied to every store, so a `MemStore`-backed test
    /// saw zero counts and an empty status page and whole features were invisible to unit tests.
    /// Each impl must provide one — a faithful fake or an explicit, commented empty.
    async fn run_stats_by_workflow(&self) -> Result<HashMap<String, RunStats>, StoreError>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use biffix_core::state::RunState;

    #[test]
    fn cursor_round_trips_and_rejects_garbage() {
        use base64::Engine;
        let token = |raw: &str| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw);
        let run = "run-6f1c2e00-0000-0000-0000-000000000000";
        for key in [1_758_254_751_123_456, 0, -1, i64::MAX, i64::MIN] {
            assert_eq!(decode_cursor(&encode_cursor(key, run)), Some((key, run.to_string())));
        }
        // Not a token this service issues, so "no cursor" (first page), never a panic or an error.
        assert!(decode_cursor("not base64!!!").is_none());
        // Valid base64 with no `|` delimiter.
        assert!(decode_cursor(&token("nopipe")).is_none());
        // A run id with a NUL, which Postgres cannot bind as text.
        assert!(decode_cursor(&token("5|run\0x")).is_none());
        // A key that is not an integer: empty, garbage, out of range, and the timestamp-text cursor
        // earlier versions issued (still in old bookmarks).
        for key in ["", "abc", "1-1", "9223372036854775808", "2026-09-19 04:12:31.123456+00"] {
            assert!(decode_cursor(&token(&format!("{key}|run-x"))).is_none(), "{key:?}");
        }
    }

    #[test]
    fn a_run_without_a_submit_time_sorts_first() {
        assert_eq!(run_sort_key(Some(5)), 5);
        assert!(run_sort_key(None) > run_sort_key(Some(i64::MAX - 1)));
    }

    #[test]
    fn run_state_string_round_trips() {
        for state in RunState::ALL {
            assert_eq!(state.as_str().parse::<RunState>(), Ok(state));
        }
        assert!("not_a_state".parse::<RunState>().is_err());
    }

    #[test]
    fn each_run_filter_admits_exactly_its_states() {
        use RunState::{Cancelled, Failed, PreparingImages, Running, Submitted, Succeeded};
        // Written out, not derived from `states()`: this pins which states each filter admits
        // (e.g. that Completed includes Cancelled), for `states()`, `matches` and so the SQL.
        let table: [(RunFilter, &[RunState]); 6] = [
            (RunFilter::All, &[PreparingImages, Submitted, Running, Succeeded, Failed, Cancelled]),
            (RunFilter::Active, &[PreparingImages, Submitted, Running]),
            (RunFilter::Submitted, &[Submitted]),
            (RunFilter::Completed, &[Succeeded, Failed, Cancelled]),
            (RunFilter::Succeeded, &[Succeeded]),
            (RunFilter::Failed, &[Failed]),
        ];
        // Every filter: a new one fails here until the table says what it admits.
        assert_eq!(table.map(|(f, _)| f).as_slice(), RunFilter::ALL);
        for (filter, admitted) in table {
            if filter != RunFilter::All {
                assert_eq!(filter.states(), Some(admitted), "{filter:?}");
            }
            for state in RunState::ALL {
                assert_eq!(
                    filter.matches(state.as_str()),
                    admitted.contains(&state),
                    "{filter:?} vs {state:?}"
                );
            }
        }
        assert_eq!(RunFilter::All.states(), None);
        // An unrecognized stored string matches only `All`.
        assert!(RunFilter::All.matches("bogus"));
        assert!(!RunFilter::Active.matches("bogus"));
    }
}
