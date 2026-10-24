//! [`MemStore`]: the in-memory [`Store`].
//!
//! [`Store`]: crate::store::Store

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;

use crate::store::cost::CostReads;
use crate::store::event_log::EventLogReads;
use crate::store::experiments::ExperimentReads;
use crate::store::notifications::NotificationStore;
use crate::store::outputs::{ObjectMeta, OutputPage, OutputQuery, OutputRow, OutputStore};
use crate::store::pending::PendingLaunchStore;
use crate::store::provenance::ProvenanceStore;
use crate::store::runs::{RunPage, RunQuery, RunReads};
use crate::store::status::{StatusBucketView, StatusCarriedView, StatusComponentView, StatusStore};
use crate::store::tasks::TaskReads;
use crate::store::{RunDetailView, StoreError};
use async_trait::async_trait;
use biffix_core::event::RunEvent;
use biffix_core::ids::RunId;

/// In-memory [`Store`] double. Its runs are what a test seeds (or what a
/// [`MemEventSink::folding_into`] it folds); reads return the Postgres store's shapes.
///
/// [`Store`]: crate::store::Store
/// [`MemEventSink::folding_into`]: crate::testing::MemEventSink::folding_into
#[derive(Default)]
pub struct MemStore {
    pub runs: Mutex<BTreeMap<String, RunDetailView>>,
    /// Runs' sample specs (their anchors' `samples`), by run id.
    pub sample_specs: Mutex<BTreeMap<String, biffix_core::event::SampleSpec>>,
    /// Attempts keyed by (run id, task key), any order (`get_attempts` sorts them as Postgres
    /// does).
    pub attempts: Mutex<BTreeMap<(String, String), Vec<biffix_api::TaskAttempt>>>,
    /// Outputs keyed by run id (any order; `list_outputs` sorts by `s3_uri`).
    pub outputs: Mutex<BTreeMap<String, Vec<OutputRow>>>,
    /// The `s3_object_meta` cache, keyed by URI; joined onto output rows as they are read.
    pub object_meta: Mutex<BTreeMap<String, ObjectMeta>>,
    /// Past successful runs by (project, workflow name): estimate bases. Listed in the order
    /// Postgres returns them (`completed_at DESC, run_id`); a `PastRun` carries neither key, so
    /// `estimate_basis` keeps the test's order and only applies the limit.
    pub past_runs: Mutex<BTreeMap<(String, String), Vec<crate::runs::estimate::PastRun>>>,
    /// Past successful runs by (project, workflow name), any sample count: typical-run bases, in
    /// the order Postgres returns them (`typical_basis` only applies the limit).
    pub past_walls: Mutex<BTreeMap<(String, String), Vec<crate::runs::estimate::PastWall>>>,
    /// Status-probe samples, any order and any number per component (empty: the probe never
    /// ran); `status_components` reads the latest per component, as Postgres does.
    pub status_samples: Mutex<Vec<crate::store::status::StatusComponentView>>,
    /// What `status_history` returns, whatever window it is asked for.
    pub status_buckets: Mutex<Vec<StatusBucketView>>,
    /// What `status_carried_in` returns, whatever window it is asked for.
    pub status_carried_in: Mutex<Vec<StatusCarriedView>>,
    /// Every `status_history` call's `(window_secs, bucket_secs)`.
    pub status_history_asked: Mutex<Vec<(i64, i64)>>,
    /// Make `status_components`, `status_history` and `status_carried_in` fail (the samples cannot
    /// be read).
    pub fail_status: std::sync::atomic::AtomicBool,
    /// Make `health_check` fail (the database is unreachable).
    pub unhealthy: std::sync::atomic::AtomicBool,
    /// Each run's control-plane singletons (the `run_submitted` anchor, `run_preparing`, …) by
    /// (run id, kind), as `run_event`/`run_anchor` read them from the log.
    pub anchors: Mutex<BTreeMap<(String, String), RunEvent>>,
    /// What `provenance_due` returns: `(run, newest seq)` pairs, listed in the order Postgres
    /// would return them (`ORDER BY last_at, run_id`; the double has no arrival times to sort by).
    pub provenance_due: Mutex<Vec<(RunId, i64)>>,
    /// Every `mark_provenance` call: `(run, seq, uri)`.
    pub provenance_marked: Mutex<Vec<(RunId, i64, String)>>,
    /// The runs whose `mark_provenance` fails (a store error); other runs' marks succeed.
    pub fail_mark_provenance: Mutex<std::collections::BTreeSet<String>>,
    /// Each run whose provenance failed (`mark_provenance_failed`): `(attempts, retry_after)`,
    /// dropped by a `mark_provenance`.
    pub provenance_failures: Mutex<BTreeMap<String, (i64, i64)>>,
    /// Each run's priced jobs (`attempt_cost`), by run id: what `usage` sums. (A run's detail
    /// carries its own `cost`, which a test sets directly.)
    pub costs: Mutex<BTreeMap<String, Vec<crate::store::cost::CostRow>>>,
    /// The output prefixes a submit holds a claim on now.
    pub prefix_claims: std::sync::Arc<Mutex<std::collections::BTreeSet<String>>>,
    /// Parked launch requests (`pending_launch`), by run id.
    pub pending: Mutex<BTreeMap<String, crate::store::pending::PendingLaunch>>,
    /// The runs whose launch is claimed now.
    pub launch_claims: std::sync::Arc<Mutex<std::collections::BTreeSet<String>>>,
    /// Every `delete_pending_launch` call: the run, and whether its launch was claimed then.
    pub pending_deleted: Mutex<Vec<(String, bool)>>,
    /// Make `put_pending_launch` fail (the store is unavailable).
    pub fail_put_pending: std::sync::atomic::AtomicBool,
    /// Run once, by the next `list_pending_launches`, after it has taken its listing: what another
    /// launcher does between one sweep's listing and its claims.
    pub after_list_pending: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    /// Make `run_state` fail (the store is unavailable).
    pub fail_run_state: AtomicBool,
    /// Make `run_engine` fail (the store is unavailable).
    pub fail_run_engine: AtomicBool,
    /// Make `task_samples` fail (the store is unavailable).
    pub fail_task_samples: AtomicBool,
    /// Make `experiment_runs_of` fail (the store is unavailable).
    pub fail_experiment_runs_of: AtomicBool,
    /// The `notifications` projection, by notification id: (run id, the fold's notification). What
    /// a [`MemEventSink::folding_into`] folds, or a test seeds with
    /// [`MemStore::insert_notification`].
    ///
    /// [`MemEventSink::folding_into`]: crate::testing::MemEventSink::folding_into
    pub notifications: Mutex<BTreeMap<String, (String, biffix_core::reducer::RunNotification)>>,
    /// `notification_reads`: (user, notification id) pairs the user has read.
    pub notification_reads: Mutex<std::collections::BTreeSet<(String, String)>>,
    /// `notification_dismissals`: (user, notification id) → how it was dismissed (`via`).
    pub notification_dismissals: Mutex<BTreeMap<(String, String), &'static str>>,
    /// How many times `run_costs` was called (a list page reads its runs' costs in one call).
    pub run_costs_calls: AtomicUsize,
    /// Make `run_costs` fail (the store is unavailable).
    pub fail_run_costs: AtomicBool,
    /// Make `list_runs` fail (the store is unavailable).
    pub fail_list_runs: AtomicBool,
}

impl MemStore {
    pub fn new() -> Self {
        Self::default()
    }
    /// Record a run's control-plane singleton (the anchor, `run_preparing`, …) under its kind.
    /// Faithful to the Postgres store: `run_event` reads the earliest (by `ts`, then `seq`) of a
    /// kind from the control plane, so a later one does not replace it and another source's is
    /// not kept.
    pub fn insert_anchor(&self, anchor: RunEvent) {
        if anchor.source != biffix_core::source::EventSource::BiffixControlPlane {
            return;
        }
        let run_id = anchor.run_id.clone().expect("an anchor names its run");
        let kind = anchor.data.kind().as_str().to_string();
        self.anchors
            .lock()
            .unwrap()
            .entry((run_id.to_string(), kind))
            .and_modify(|held| {
                if anchor.log_order() < held.log_order() {
                    *held = anchor.clone();
                }
            })
            .or_insert(anchor);
    }
    /// `row` with its cached S3 metadata joined on, as the Postgres store reads it.
    fn with_meta(&self, mut row: OutputRow) -> OutputRow {
        row.meta = self.object_meta.lock().unwrap().get(&row.s3_uri).cloned();
        row
    }
    /// `scope`'s rows, as Postgres reads them: the run's own rows, plus each resumed run's under
    /// the shared prefix, one per URI (the nearest run's), each with `run_id` set, ordered by URI.
    fn scope_rows(&self, scope: &crate::store::outputs::OutputScope) -> Vec<OutputRow> {
        let outputs = self.outputs.lock().unwrap();
        let mut by_uri: std::collections::BTreeMap<String, OutputRow> = Default::default();
        for (i, run) in scope.runs().iter().enumerate() {
            for row in outputs.get(run.as_str()).into_iter().flatten() {
                let counts =
                    i == 0 || scope.shared_prefix().is_some_and(|p| row.s3_uri.starts_with(p));
                if counts && !by_uri.contains_key(&row.s3_uri) {
                    let mut row = row.clone();
                    row.run_id = run.clone();
                    by_uri.insert(row.s3_uri.clone(), row);
                }
            }
        }
        by_uri.into_values().collect()
    }
    pub fn insert_outputs(&self, run_id: &str, outputs: Vec<OutputRow>) {
        self.outputs.lock().unwrap().insert(run_id.to_string(), outputs);
    }
    pub fn insert_attempts(
        &self,
        run_id: &str,
        task_key: &str,
        attempts: Vec<biffix_api::TaskAttempt>,
    ) {
        self.attempts.lock().unwrap().insert((run_id.to_string(), task_key.to_string()), attempts);
    }
    /// Seed the notification run `run_id` reaching `kind` at `at_micros` gives `user` (as the fold
    /// would: its id is `<run>:<kind>`).
    pub fn insert_notification(
        &self,
        run_id: &str,
        user: &str,
        kind: biffix_core::state::RunState,
        at_micros: i64,
    ) {
        let id = biffix_core::reducer::notification_id(run_id, kind);
        let n = biffix_core::reducer::RunNotification {
            id: id.clone(),
            user_sub: user.to_string(),
            kind,
            at_micros,
        };
        self.notifications.lock().unwrap().insert(id, (run_id.to_string(), n));
    }
    /// Make the run's notifications match its fold, as the event-log writer does: drop any the fold
    /// no longer produces, upsert the one it does.
    pub fn sync_notification(
        &self,
        run_id: &str,
        notification: Option<biffix_core::reducer::RunNotification>,
    ) {
        let mut all = self.notifications.lock().unwrap();
        let keep = notification.as_ref().map(|n| n.id.clone());
        all.retain(|id, (run, _)| run != run_id || Some(id) == keep.as_ref());
        if let Some(n) = notification {
            all.insert(n.id.clone(), (run_id.to_string(), n));
        }
    }
    /// The user's unread counts: (unread, unread of failed runs).
    fn unread_counts(&self, user: &str) -> biffix_api::UnreadCounts {
        let all = self.notifications.lock().unwrap();
        let reads = self.notification_reads.lock().unwrap();
        let unread: Vec<_> = all
            .iter()
            .filter(|(id, (_, n))| {
                n.user_sub == user && !reads.contains(&(user.to_string(), (*id).clone()))
            })
            .collect();
        biffix_api::UnreadCounts {
            unread: unread.len() as i64,
            unread_failed: unread
                .iter()
                .filter(|(_, (_, n))| n.kind == biffix_core::state::RunState::Failed)
                .count() as i64,
        }
    }
    pub fn insert(&self, detail: RunDetailView) {
        self.runs.lock().unwrap().insert(detail.run.run_id.to_string(), detail);
    }
}

/// Return a copy of `detail` with the summary's observed task-outcome counts derived from its task
/// rows, whatever a test set on the literal. Postgres reads these counters from `run_projection`,
/// where the fold stored `RunProjection::task_counts` over the tasks' effective states; the task
/// rows it serves carry those same effective states, so counting a test's rows (which must be
/// seeded with effective states: no `running` task on an ended run) gives the same numbers.
/// `outputs_total` and `outputs_reused` are NOT derived: they are what the test set. `resumed` is
/// derived from `resumed_from`.
fn with_task_counts(mut detail: RunDetailView) -> RunDetailView {
    let count = |s: &str| detail.tasks.iter().filter(|t| t.state == s).count() as i64;
    let observed = detail.tasks.len() as i64;
    let (succeeded, running, failed) = (count("succeeded"), count("running"), count("failed"));
    let queued = count("queued");
    let cached = detail.tasks.iter().filter(|t| t.cached).count() as i64;
    detail.run.tasks_observed = observed;
    detail.run.tasks_succeeded = succeeded;
    detail.run.tasks_running = running;
    detail.run.tasks_queued = queued;
    detail.run.tasks_failed = failed;
    detail.run.tasks_cached = cached;
    // As Postgres reads it: `run_projection.resumed_from IS NOT NULL`.
    detail.run.resumed = detail.resumed_from.is_some();
    detail
}

/// A [`MemStore`] claim (on an output prefix, or a run's launch): released when dropped.
struct MemClaim {
    claims: std::sync::Arc<Mutex<std::collections::BTreeSet<String>>>,
    /// What is claimed: the prefix, or the run id.
    key: String,
}

impl Drop for MemClaim {
    fn drop(&mut self) {
        self.claims.lock().unwrap().remove(&self.key);
    }
}

#[async_trait]
impl RunReads for MemStore {
    async fn list_runs(&self, query: &RunQuery) -> Result<RunPage, StoreError> {
        use crate::store::runs::{decode_cursor, encode_cursor, run_sort_key};
        if self.fail_list_runs.load(Ordering::SeqCst) {
            return Err(StoreError::Unavailable("list_runs failed (test)".into()));
        }
        // The `RunQuery::q` contract, as the Pg store runs it: the whole term is a substring of one
        // field (never of fields run together), case-insensitively, with no wildcards.
        let q = query.q.as_deref().map(str::to_lowercase);
        let matches_q = |v: &biffix_api::RunSummary| -> bool {
            let Some(ref needle) = q else { return true };
            [
                Some(v.run_id.as_str()),
                v.run_name.as_deref(),
                v.workflow_name.as_deref(),
                v.workflow_title.as_deref(),
                v.workflow.as_ref().map(biffix_core::ids::WorkflowVersionId::as_str),
                v.submitter.as_deref(),
                v.submitter_label.as_deref(),
                v.project.as_deref(),
                v.engine.as_deref(),
            ]
            .into_iter()
            .flatten()
            .any(|field| field.to_lowercase().contains(needle.as_str()))
        };
        let matches_workflow = |v: &biffix_api::RunSummary| match &query.workflow {
            None => true,
            Some(id) => v.workflow.as_ref().is_some_and(|w| w.as_str() == id),
        };
        // The Pg store's order and cursor key: run_sort_key over submitted_at, then run_id, both
        // descending (run_id in byte order).
        let key = |v: &biffix_api::RunSummary| {
            (
                run_sort_key(v.submitted_at.map(|s| (s * 1e6).round() as i64)),
                v.run_id.as_str().to_string(),
            )
        };
        // (user, run) pairs with a `failed` notification not dismissed: the runs needing that
        // user's attention, as the Pg store's `undismissed_failed_of_run` finds them.
        let unread_failed: std::collections::BTreeSet<(String, String)> = {
            let dismissed = self.notification_dismissals.lock().unwrap();
            self.notifications
                .lock()
                .unwrap()
                .iter()
                .filter(|(id, (_, n))| {
                    n.kind == biffix_core::state::RunState::Failed
                        && !dismissed.contains_key(&(n.user_sub.clone(), (*id).clone()))
                })
                .map(|(_, (run, n))| (n.user_sub.clone(), run.clone()))
                .collect()
        };
        let mut all: Vec<_> = self
            .runs
            .lock()
            .unwrap()
            .values()
            .map(|d| with_task_counts(d.clone()).run)
            .filter(|v| {
                query.filter.matches(&v.state)
                    && matches_q(v)
                    && matches_workflow(v)
                    && query
                        .ids
                        .as_ref()
                        .is_none_or(|ids| ids.iter().any(|id| id == v.run_id.as_str()))
                    && query
                        .submitter
                        .as_ref()
                        .is_none_or(|s| v.submitter.as_deref() == Some(s.as_str()))
                    && query.attention.as_ref().is_none_or(|user| {
                        v.state == "failed"
                            && unread_failed.contains(&(user.clone(), v.run_id.to_string()))
                    })
                    && query
                        .experiment
                        .as_ref()
                        .is_none_or(|e| v.experiment_id.as_deref() == Some(e.as_str()))
            })
            .collect();
        all.sort_by_key(|v| std::cmp::Reverse(key(v)));
        let total = all.len() as i64;
        let limit = query.page_size() as usize;
        // Keyset paging as the Pg store does it: `after` wins, and a token that does not decode is
        // the first page.
        let cursor = match (&query.after, &query.before) {
            (Some(after), _) => decode_cursor(after),
            (None, Some(before)) => decode_cursor(before),
            (None, None) => None,
        };
        let runs: Vec<_> = match cursor {
            None => all.into_iter().take(limit).collect(),
            Some(c) if query.after.is_some() => {
                all.into_iter().filter(|v| key(v) < c).take(limit).collect()
            }
            Some(c) => {
                let newer: Vec<_> = all.into_iter().filter(|v| key(v) > c).collect();
                let skip = newer.len().saturating_sub(limit);
                newer.into_iter().skip(skip).collect()
            }
        };
        let cursor_of = |v: &biffix_api::RunSummary| {
            let (sort_key, run_id) = key(v);
            encode_cursor(sort_key, &run_id)
        };
        Ok(RunPage {
            first_cursor: runs.first().map(cursor_of),
            last_cursor: runs.last().map(cursor_of),
            runs,
            total,
        })
    }

    async fn get_run(&self, run_id: &RunId) -> Result<RunDetailView, StoreError> {
        self.runs
            .lock()
            .unwrap()
            .get(run_id.as_str())
            .cloned()
            .map(with_task_counts)
            .map(|mut d| {
                // Like Postgres: the DAG's per-step counts are derived from the task rows.
                d.task_groups = crate::store::task_group_counts(&d.tasks);
                d
            })
            .ok_or(StoreError::NotFound)
    }

    async fn get_run_light(&self, run_id: &RunId) -> Result<RunDetailView, StoreError> {
        // Like Postgres: the groups (and cost), never the task table.
        let mut d = self.get_run(run_id).await?;
        d.tasks.clear();
        Ok(d)
    }

    async fn sample_facts(
        &self,
        run_id: &RunId,
    ) -> Result<
        (Option<biffix_core::event::SampleSpec>, Vec<biffix_core::sample_progress::TaskFact>),
        StoreError,
    > {
        // As the Pg store reads it: the anchor's spec, and each task's sample attributed from it.
        let spec = self.sample_specs.lock().unwrap().get(run_id.as_str()).cloned();
        let default = biffix_core::event::SampleSpec::default();
        let matcher =
            biffix_core::sample_progress::SampleMatcher::new(spec.as_ref().unwrap_or(&default));
        // The tasks credited with an output, as the Pg store joins `run_output`.
        let with_outputs: std::collections::BTreeSet<String> = self
            .outputs
            .lock()
            .unwrap()
            .get(run_id.as_str())
            .into_iter()
            .flatten()
            .filter_map(|o| o.task_key.clone())
            .collect();
        let tasks = self
            .runs
            .lock()
            .unwrap()
            .get(run_id.as_str())
            .map(|d| {
                d.tasks
                    .iter()
                    .map(|t| biffix_core::sample_progress::TaskFact {
                        sample: matcher.attribute(t.task_key.as_str(), t.group.as_deref()),
                        group: t.group.clone(),
                        state: t.state.clone(),
                        cached: t.cached,
                        attempts: t.attempts,
                        outputs: with_outputs.contains(t.task_key.as_str()),
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok((spec, tasks))
    }

    async fn run_state(&self, run_id: &RunId) -> Result<Option<String>, StoreError> {
        if self.fail_run_state.load(Ordering::SeqCst) {
            return Err(StoreError::Unavailable("run_projection: timed out".into()));
        }
        Ok(self.runs.lock().unwrap().get(run_id.as_str()).map(|d| d.run.state.clone()))
    }

    async fn run_engine(&self, run_id: &RunId) -> Result<Option<String>, StoreError> {
        if self.fail_run_engine.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(StoreError::Unavailable("run_projection is down".into()));
        }
        Ok(self.runs.lock().unwrap().get(run_id.as_str()).and_then(|d| d.run.engine.clone()))
    }

    async fn run_stats_by_workflow(
        &self,
    ) -> Result<HashMap<String, crate::store::runs::RunStats>, StoreError> {
        // Faithful: count the double's runs per workflow-version id, and keep the newest submit.
        let mut stats: HashMap<String, crate::store::runs::RunStats> = HashMap::new();
        for d in self.runs.lock().unwrap().values() {
            if let Some(wf) = d.run.workflow.as_ref() {
                let s = stats.entry(wf.as_str().to_string()).or_default();
                s.count += 1;
                let submitted = d
                    .run
                    .submitted_at
                    .map(|t| biffix_core::time::Timestamp::from_epoch_seconds(t).as_micros());
                s.last_submitted_micros = s.last_submitted_micros.max(submitted);
            }
        }
        Ok(stats)
    }
}

#[async_trait]
impl TaskReads for MemStore {
    async fn task_samples(
        &self,
        run_id: &RunId,
        task_keys: &[String],
    ) -> Result<BTreeMap<String, String>, StoreError> {
        if self.fail_task_samples.load(Ordering::SeqCst) {
            return Err(StoreError::Unavailable("task_projection: timed out".into()));
        }
        // As the Pg store reads it: each task's sample, attributed from the run's spec.
        let (_, facts) = self.sample_facts(run_id).await?;
        let tasks = self
            .runs
            .lock()
            .unwrap()
            .get(run_id.as_str())
            .map(|d| d.tasks.clone())
            .unwrap_or_default();
        Ok(tasks
            .iter()
            .zip(facts)
            .filter(|(t, _)| task_keys.iter().any(|k| k == t.task_key.as_str()))
            .filter_map(|(t, f)| Some((t.task_key.as_str().to_string(), f.sample?)))
            .collect())
    }

    async fn get_attempts(
        &self,
        run_id: &RunId,
        task_key: &biffix_core::ids::TaskKey,
    ) -> Result<Vec<biffix_api::TaskAttempt>, StoreError> {
        let mut attempts = self
            .attempts
            .lock()
            .unwrap()
            .get(&(run_id.to_string(), task_key.to_string()))
            .cloned()
            .unwrap_or_default();
        // Like Postgres: `ORDER BY attempt NULLS LAST, batch_job_id`.
        attempts.sort_by_key(|a| (a.attempt.is_none(), a.attempt, a.batch_job_id.0.clone()));
        Ok(attempts)
    }
}

#[async_trait]
impl ExperimentReads for MemStore {
    async fn experiment_run_stats(
        &self,
        experiment_ids: &[String],
    ) -> Result<HashMap<String, crate::store::runs::RunStats>, StoreError> {
        // Faithful: count the double's runs per experiment asked about, and keep the newest submit.
        let mut stats: HashMap<String, crate::store::runs::RunStats> = HashMap::new();
        for d in self.runs.lock().unwrap().values() {
            let Some(e) = d.run.experiment_id.as_ref() else {
                continue;
            };
            if !experiment_ids.contains(e) {
                continue;
            }
            let s = stats.entry(e.clone()).or_default();
            s.count += 1;
            let submitted = d
                .run
                .submitted_at
                .map(|t| biffix_core::time::Timestamp::from_epoch_seconds(t).as_micros());
            s.last_submitted_micros = s.last_submitted_micros.max(submitted);
        }
        Ok(stats)
    }

    async fn experiment_runs_of(
        &self,
        submitter: &str,
        user: &str,
        experiment: Option<&str>,
    ) -> Result<HashMap<String, crate::store::experiments::SubmitterRuns>, StoreError> {
        if self.fail_experiment_runs_of.load(Ordering::SeqCst) {
            return Err(StoreError::Unavailable("run_projection: timed out".into()));
        }
        // Faithful: the submitter's runs per experiment, and those of them that are failed with
        // `user`'s `failed` notification not dismissed (as `list_runs`'s attention reads it).
        let unread_failed: std::collections::BTreeSet<String> = {
            let dismissed = self.notification_dismissals.lock().unwrap();
            self.notifications
                .lock()
                .unwrap()
                .iter()
                .filter(|(id, (_, n))| {
                    n.user_sub == user
                        && n.kind == biffix_core::state::RunState::Failed
                        && !dismissed.contains_key(&(n.user_sub.clone(), (*id).clone()))
                })
                .map(|(_, (run, _))| run.clone())
                .collect()
        };
        let mut out: HashMap<String, crate::store::experiments::SubmitterRuns> = HashMap::new();
        for d in self.runs.lock().unwrap().values() {
            let Some(e) = d.run.experiment_id.as_ref() else {
                continue;
            };
            if d.run.submitter.as_deref() != Some(submitter)
                || experiment.is_some_and(|only| only != e.as_str())
            {
                continue;
            }
            let r = out.entry(e.clone()).or_default();
            r.runs += 1;
            if d.run.state == "failed" && unread_failed.contains(d.run.run_id.as_str()) {
                r.attention += 1;
            }
        }
        Ok(out)
    }
}

#[async_trait]
impl CostReads for MemStore {
    async fn run_costs(
        &self,
        run_ids: &[RunId],
    ) -> Result<HashMap<String, crate::store::cost::ListCost>, StoreError> {
        // Faithful: each run's detail cost as the list shows it, read in one pass (counted).
        self.run_costs_calls.fetch_add(1, Ordering::SeqCst);
        if self.fail_run_costs.load(Ordering::SeqCst) {
            return Err(StoreError::Unavailable("attempt_cost: timed out".into()));
        }
        let runs = self.runs.lock().unwrap();
        Ok(run_ids
            .iter()
            .filter_map(|id| {
                let cost = runs.get(id.as_str())?.cost.as_ref()?;
                let listed = crate::store::cost::ListCost::from_run_cost(Some(cost));
                Some((id.to_string(), listed))
            })
            .collect())
    }

    async fn usage(
        &self,
        from_micros: i64,
        to_micros: i64,
        group: crate::runs::usage::UsageGroup,
    ) -> Result<crate::runs::usage::UsageReport, StoreError> {
        use crate::runs::usage::{UsageGroup, UsageReport, UsageTotals};
        use biffix_core::time::civil_from_days;
        // Faithful to the Postgres query: the priced jobs of the runs submitted in the period,
        // grouped by `group`'s key, each group counting its distinct runs.
        let runs = self.runs.lock().unwrap();
        let costs = self.costs.lock().unwrap();
        let mut groups: BTreeMap<String, (std::collections::BTreeSet<String>, f64, f64)> =
            BTreeMap::new();
        let mut total = (std::collections::BTreeSet::new(), 0.0, 0.0);
        for (run_id, rows) in costs.iter() {
            let Some(run) = runs.get(run_id).map(|d| &d.run) else {
                continue;
            };
            let Some(submitted) = run.submitted_at.map(|s| (s * 1e6).round() as i64) else {
                continue;
            };
            if submitted < from_micros || submitted >= to_micros {
                continue;
            }
            let unknown = || "(unknown)".to_string();
            for row in rows {
                let key = match group {
                    UsageGroup::Month => {
                        let (y, m, _) = civil_from_days(submitted.div_euclid(86_400_000_000));
                        format!("{y:04}-{m:02}")
                    }
                    UsageGroup::Workflow => match (&run.project, &run.workflow_name) {
                        (Some(p), Some(n)) => format!("{p}/{n}"),
                        _ => unknown(),
                    },
                    UsageGroup::Submitter => run
                        .submitter_label
                        .clone()
                        .or_else(|| run.submitter.clone())
                        .unwrap_or_else(unknown),
                    UsageGroup::InstanceType => row.instance_type.clone(),
                    UsageGroup::Project => run.project.clone().unwrap_or_else(unknown),
                };
                let entry = groups.entry(key).or_default();
                entry.0.insert(run_id.clone());
                entry.1 += row.compute_usd;
                entry.2 += row.ebs_usd;
                total.0.insert(run_id.clone());
                total.1 += row.compute_usd;
                total.2 += row.ebs_usd;
            }
        }
        let totals = |key: String,
                      (runs, compute_usd, ebs_usd): (
            std::collections::BTreeSet<String>,
            f64,
            f64,
        )| {
            UsageTotals { key, runs: runs.len() as i64, compute_usd, ebs_usd }
        };
        let mut rows: Vec<UsageTotals> = groups.into_iter().map(|(k, g)| totals(k, g)).collect();
        // By month oldest first (the BTreeMap order); otherwise largest first, ties by key.
        if group != UsageGroup::Month {
            rows.sort_by(|a, b| {
                (b.compute_usd + b.ebs_usd)
                    .total_cmp(&(a.compute_usd + a.ebs_usd))
                    .then_with(|| a.key.cmp(&b.key))
            });
        }
        Ok(UsageReport { rows, total: totals("total".into(), total) })
    }

    async fn estimate_basis(
        &self,
        project: &str,
        workflow_name: &str,
        limit: i64,
    ) -> Result<Vec<crate::runs::estimate::PastRun>, StoreError> {
        let past = self.past_runs.lock().unwrap();
        Ok(past
            .get(&(project.to_string(), workflow_name.to_string()))
            .map(|runs| runs.iter().take(limit as usize).cloned().collect())
            .unwrap_or_default())
    }

    async fn typical_basis(
        &self,
        project: &str,
        workflow_name: &str,
        limit: i64,
    ) -> Result<Vec<crate::runs::estimate::PastWall>, StoreError> {
        let past = self.past_walls.lock().unwrap();
        Ok(past
            .get(&(project.to_string(), workflow_name.to_string()))
            .map(|runs| runs.iter().take(limit as usize).cloned().collect())
            .unwrap_or_default())
    }

    async fn typical_bases(
        &self,
        limit: i64,
    ) -> Result<
        std::collections::HashMap<(String, String), Vec<crate::runs::estimate::PastWall>>,
        StoreError,
    > {
        let past = self.past_walls.lock().unwrap();
        Ok(past
            .iter()
            .filter(|(_, runs)| !runs.is_empty())
            .map(|(key, runs)| (key.clone(), runs.iter().take(limit as usize).cloned().collect()))
            .collect())
    }
}

#[async_trait]
impl OutputStore for MemStore {
    async fn list_outputs_in(
        &self,
        scope: &crate::store::outputs::OutputScope,
        query: &OutputQuery,
    ) -> Result<OutputPage, StoreError> {
        // `effective` orders by URI.
        let rows: Vec<OutputRow> = self
            .scope_rows(scope)
            .into_iter()
            .filter(|r| query.matches(r))
            .map(|r| self.with_meta(r))
            .collect();
        let total = rows.len() as i64;
        let now = biffix_core::clock::epoch_micros();
        let unchecked = rows
            .iter()
            .filter(|r| {
                crate::store::outputs::needs_head(r.meta.as_ref(), now)
                    && query
                        .checkable_bucket
                        .as_deref()
                        .is_some_and(|b| crate::store::outputs::checkable(&r.s3_uri, b))
            })
            .count() as i64;
        let after = query.after.clone().unwrap_or_default();
        let rows = rows
            .into_iter()
            .filter(|r| r.s3_uri > after)
            .take(query.limit.max(0) as usize)
            .collect();
        Ok(OutputPage { rows, total, unchecked })
    }

    async fn get_output_in(
        &self,
        scope: &crate::store::outputs::OutputScope,
        id: &str,
    ) -> Result<Option<OutputRow>, StoreError> {
        Ok(self.scope_rows(scope).into_iter().find(|r| r.id == id).map(|r| self.with_meta(r)))
    }

    async fn output_at_in(
        &self,
        scope: &crate::store::outputs::OutputScope,
        uri: &str,
    ) -> Result<Option<OutputRow>, StoreError> {
        Ok(self.scope_rows(scope).into_iter().find(|r| r.s3_uri == uri).map(|r| self.with_meta(r)))
    }

    async fn put_object_meta(&self, metas: &[(String, ObjectMeta)]) -> Result<(), StoreError> {
        // As Postgres: an older check never replaces a newer one.
        let mut cache = self.object_meta.lock().unwrap();
        for (uri, meta) in metas {
            if cache.get(uri).is_none_or(|held| held.checked_at <= meta.checked_at) {
                cache.insert(uri.clone(), meta.clone());
            }
        }
        Ok(())
    }

    async fn active_run_in_prefix(&self, output_prefix: &str) -> Result<Option<RunId>, StoreError> {
        // Faithful: the first (by id) run not yet ended whose recorded prefix is exactly this one.
        Ok(self
            .runs
            .lock()
            .unwrap()
            .values()
            .filter(|d| d.output_prefix.as_deref() == Some(output_prefix))
            .filter(|d| !crate::runs::detail::state_is_terminal(&d.run.state))
            .map(|d| d.run.run_id.clone())
            .next())
    }

    async fn claim_output_prefix(
        &self,
        output_prefix: &str,
    ) -> Result<Option<crate::store::Claim>, StoreError> {
        // Faithful: one holder per prefix until its claim is dropped.
        if !self.prefix_claims.lock().unwrap().insert(output_prefix.to_string()) {
            return Ok(None);
        }
        Ok(Some(crate::store::Claim::new(MemClaim {
            claims: self.prefix_claims.clone(),
            key: output_prefix.to_string(),
        })))
    }
}

#[async_trait]
impl EventLogReads for MemStore {
    async fn run_event(
        &self,
        run_id: &RunId,
        kind: &biffix_core::event::Kind,
    ) -> Result<Option<RunEvent>, StoreError> {
        Ok(self
            .anchors
            .lock()
            .unwrap()
            .get(&(run_id.to_string(), kind.as_str().to_string()))
            .cloned())
    }
}

#[async_trait]
impl ProvenanceStore for MemStore {
    async fn provenance_due(
        &self,
        _settle_secs: i64,
        limit: i64,
    ) -> Result<Vec<(RunId, i64)>, StoreError> {
        // The runs a test made due, less those already marked up to their newest seq (as the
        // Postgres store compares `max(seq)` with the recorded `written_seq`) and those waiting
        // after a failure; those that have failed after the rest (a stable sort keeps the test's
        // order within each count).
        let due = self.provenance_due.lock().unwrap();
        let marked = self.provenance_marked.lock().unwrap();
        let failures = self.provenance_failures.lock().unwrap();
        let now = biffix_core::clock::epoch_micros();
        let mut due: Vec<_> = due
            .iter()
            .filter(|(run, seq)| !marked.iter().any(|(r, s, _)| r == run && s >= seq))
            .filter(|(run, _)| failures.get(run.as_str()).is_none_or(|(_, at)| *at <= now))
            .cloned()
            .collect();
        due.sort_by_key(|(run, _)| failures.get(run.as_str()).map_or(0, |(n, _)| *n));
        Ok(due.into_iter().take(limit.max(0) as usize).collect())
    }

    async fn mark_provenance_failed(
        &self,
        run_id: &RunId,
        at_micros: i64,
    ) -> Result<(), StoreError> {
        let mut failures = self.provenance_failures.lock().unwrap();
        let attempts = failures.get(run_id.as_str()).map_or(0, |(n, _)| *n) + 1;
        let retry_after = crate::store::provenance::provenance_retry_after(attempts, at_micros);
        failures.insert(run_id.to_string(), (attempts, retry_after));
        Ok(())
    }

    async fn mark_provenance(
        &self,
        run_id: &RunId,
        seq: i64,
        uri: &str,
        _at_micros: i64,
    ) -> Result<(), StoreError> {
        if self.fail_mark_provenance.lock().unwrap().contains(run_id.as_str()) {
            return Err(StoreError::Unavailable("mark denied".into()));
        }
        self.provenance_marked.lock().unwrap().push((run_id.clone(), seq, uri.to_string()));
        self.provenance_failures.lock().unwrap().remove(run_id.as_str());
        Ok(())
    }
}

#[async_trait]
impl PendingLaunchStore for MemStore {
    async fn put_pending_launch(
        &self,
        pending: &crate::store::pending::PendingLaunch,
    ) -> Result<(), StoreError> {
        if self.fail_put_pending.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(StoreError::Unavailable("pending_launch is down".into()));
        }
        self.pending.lock().unwrap().insert(pending.run_id.to_string(), pending.clone());
        Ok(())
    }

    async fn list_pending_launches(
        &self,
    ) -> Result<Vec<crate::store::pending::PendingLaunch>, StoreError> {
        // Faithful: oldest first, then by run id.
        let mut rows: Vec<_> = self.pending.lock().unwrap().values().cloned().collect();
        rows.sort_by(|a, b| {
            (a.created_at_micros, a.run_id.as_str()).cmp(&(b.created_at_micros, b.run_id.as_str()))
        });
        // What runs between this listing and the lister's next step (another sweep), if a test
        // set it.
        let after = self.after_list_pending.lock().unwrap().take();
        if let Some(after) = after {
            after();
        }
        Ok(rows)
    }

    async fn get_pending_launch(
        &self,
        run_id: &RunId,
    ) -> Result<Option<crate::store::pending::PendingLaunch>, StoreError> {
        Ok(self.pending.lock().unwrap().get(run_id.as_str()).cloned())
    }

    async fn delete_pending_launch(&self, run_id: &RunId) -> Result<(), StoreError> {
        let claimed = self.launch_claims.lock().unwrap().contains(run_id.as_str());
        self.pending_deleted.lock().unwrap().push((run_id.to_string(), claimed));
        self.pending.lock().unwrap().remove(run_id.as_str());
        Ok(())
    }

    async fn mark_pending_launch_orphan(
        &self,
        run_id: &RunId,
        job_id: &str,
    ) -> Result<(), StoreError> {
        if let Some(row) = self.pending.lock().unwrap().get_mut(run_id.as_str()) {
            row.plan[crate::store::pending::PENDING_ORPHAN_KEY] = job_id.into();
        }
        Ok(())
    }

    async fn claim_run_launch(
        &self,
        run_id: &RunId,
    ) -> Result<Option<crate::store::Claim>, StoreError> {
        // Faithful: one holder per run until its claim is dropped.
        if !self.launch_claims.lock().unwrap().insert(run_id.to_string()) {
            return Ok(None);
        }
        Ok(Some(crate::store::Claim::new(MemClaim {
            claims: self.launch_claims.clone(),
            key: run_id.to_string(),
        })))
    }
}

#[async_trait]
impl NotificationStore for MemStore {
    async fn list_notifications(
        &self,
        user: &str,
        limit: i64,
    ) -> Result<biffix_api::NotificationList, StoreError> {
        // As the Pg store: the user's, newest (`at`, then id) first, joined to their reads and the
        // run's name, workflow name and sample counts.
        let limit = usize::try_from(limit).unwrap_or(0);
        let mut mine: Vec<(String, biffix_core::reducer::RunNotification)> = self
            .notifications
            .lock()
            .unwrap()
            .values()
            .filter(|(_, n)| n.user_sub == user)
            .cloned()
            .collect();
        mine.sort_by(|(_, a), (_, b)| (b.at_micros, &b.id).cmp(&(a.at_micros, &a.id)));
        let reads = self.notification_reads.lock().unwrap().clone();
        let runs = self.runs.lock().unwrap();
        let items = mine
            .into_iter()
            .take(limit)
            .map(|(run_id, n)| {
                let run = runs.get(&run_id).map(|d| &d.run);
                biffix_api::Notification {
                    read: reads.contains(&(user.to_string(), n.id.clone())),
                    id: n.id,
                    run_name: run.and_then(|r| r.run_name.clone()),
                    workflow_name: run.and_then(|r| r.workflow_name.clone()),
                    workflow_title: run.and_then(|r| r.workflow_title.clone()),
                    submitted_at: run.and_then(|r| r.submitted_at),
                    samples_total: run.and_then(|r| r.samples.as_ref()).map(|s| s.total),
                    samples_failed: run.and_then(|r| r.samples.as_ref()).map(|s| s.failed),
                    run_id: run_id.into(),
                    kind: n.kind.as_str().to_string(),
                    at: n.at_micros as f64 / 1e6,
                }
            })
            .collect();
        drop(runs);
        let unread = self.unread_counts(user);
        Ok(biffix_api::NotificationList {
            items,
            unread: unread.unread,
            unread_failed: unread.unread_failed,
        })
    }

    async fn mark_notifications_read(
        &self,
        user: &str,
        selection: &crate::store::notifications::NotificationSelection,
    ) -> Result<biffix_api::UnreadCounts, StoreError> {
        use crate::store::notifications::NotificationSelection;
        let picked: Vec<String> = self
            .notifications
            .lock()
            .unwrap()
            .iter()
            .filter(|(id, (run, n))| {
                n.user_sub == user
                    && match selection {
                        NotificationSelection::Ids(ids) => ids.contains(id),
                        NotificationSelection::Runs(runs) => runs.contains(run),
                        NotificationSelection::All => true,
                    }
            })
            .map(|(id, _)| id.clone())
            .collect();
        let mut reads = self.notification_reads.lock().unwrap();
        for id in picked {
            reads.insert((user.to_string(), id));
        }
        drop(reads);
        Ok(self.unread_counts(user))
    }

    async fn read_notification_ahead(
        &self,
        user: &str,
        notification_id: &str,
    ) -> Result<(), StoreError> {
        self.notification_reads
            .lock()
            .unwrap()
            .insert((user.to_string(), notification_id.to_string()));
        Ok(())
    }

    async fn dismiss_attention(
        &self,
        user: &str,
        run_ids: &[String],
        via: crate::store::notifications::DismissVia,
    ) -> Result<(), StoreError> {
        // Faithful: the user's own `failed` notifications of those runs; the first dismissal kept.
        let picked: Vec<String> = self
            .notifications
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, (run, n))| {
                n.user_sub == user
                    && n.kind == biffix_core::state::RunState::Failed
                    && run_ids.contains(run)
            })
            .map(|(id, _)| id.clone())
            .collect();
        let mut dismissed = self.notification_dismissals.lock().unwrap();
        for id in picked {
            dismissed.entry((user.to_string(), id)).or_insert(via.as_str());
        }
        Ok(())
    }
}

#[async_trait]
impl StatusStore for MemStore {
    async fn status_components(&self) -> Result<Vec<StatusComponentView>, StoreError> {
        if self.fail_status.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(StoreError::Unavailable("status samples: timed out".into()));
        }
        // Empty unless a test records samples (the probe never ran). Like Postgres's `DISTINCT ON
        // (component) ... ORDER BY component, sampled_at DESC`: the newest sample per component,
        // in component order. (Postgres's `sampled_at` is never NULL; a test's sample without one
        // sorts first, as a NULL would.)
        let mut samples = self.status_samples.lock().unwrap().clone();
        samples.sort_by(|a, b| {
            a.name.cmp(&b.name).then_with(|| match (a.sampled_at, b.sampled_at) {
                (None, None) => std::cmp::Ordering::Equal,
                (None, Some(_)) => std::cmp::Ordering::Less,
                (Some(_), None) => std::cmp::Ordering::Greater,
                (Some(x), Some(y)) => y.total_cmp(&x),
            })
        });
        samples.dedup_by(|later, first| later.name == first.name);
        Ok(samples)
    }

    async fn status_history(
        &self,
        window_secs: i64,
        bucket_secs: i64,
    ) -> Result<Vec<StatusBucketView>, StoreError> {
        if self.fail_status.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(StoreError::Unavailable("status samples: timed out".into()));
        }
        self.status_history_asked.lock().unwrap().push((window_secs, bucket_secs));
        Ok(self.status_buckets.lock().unwrap().clone())
    }

    async fn status_carried_in(
        &self,
        _window_secs: i64,
    ) -> Result<Vec<StatusCarriedView>, StoreError> {
        if self.fail_status.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(StoreError::Unavailable("status samples: timed out".into()));
        }
        Ok(self.status_carried_in.lock().unwrap().clone())
    }

    async fn health_check(&self) -> Result<(), StoreError> {
        if self.unhealthy.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(StoreError::Unavailable("connection refused".into()));
        }
        Ok(())
    }
}
