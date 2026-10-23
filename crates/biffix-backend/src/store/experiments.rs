//! Experiments: the stored experiments themselves ([`ExperimentStore`], with [`ExperimentRecord`] and
//! [`ExperimentChange`]), and [`ExperimentReads`]: the runs of an experiment, as its list and detail
//! read them.

use std::collections::{HashMap, HashSet};

use async_trait::async_trait;
use biffix_api::RunSummary;
use biffix_core::ids::RunId;

use super::cost::CostReads;
use super::runs::{RunPage, RunQuery, RunReads, RunStats, MAX_RUN_PAGE};
use super::StoreError;

/// One stored experiment.
#[derive(Clone, Debug, PartialEq)]
pub struct ExperimentRecord {
    pub id: String,
    pub project: String,
    pub name: String,
    pub description: Option<String>,
    /// Who created it (the actor id), and a label for them when one was known.
    pub created_by: Option<String>,
    pub created_by_label: Option<String>,
    /// Epoch micros.
    pub created_at_micros: i64,
    /// When it was archived (epoch micros); `None` while it is live.
    pub archived_at_micros: Option<i64>,
}

impl ExperimentRecord {
    /// The wire view, with its runs' `stats` (none: no runs), whether it is the caller's
    /// ([`Self::is_mine`]) and how many of the caller's runs in it need their attention.
    pub(crate) fn to_view(
        &self,
        stats: Option<RunStats>,
        mine: bool,
        needs_attention: i64,
    ) -> biffix_api::Experiment {
        let secs =
            |micros: i64| biffix_core::time::Timestamp::from_micros(micros).as_epoch_seconds();
        let stats = stats.unwrap_or_default();
        biffix_api::Experiment {
            id: self.id.clone(),
            project: self.project.clone(),
            name: self.name.clone(),
            description: self.description.clone(),
            created_by: self.created_by.clone(),
            created_by_label: self.created_by_label.clone(),
            created_at: secs(self.created_at_micros),
            archived_at: self.archived_at_micros.map(secs),
            run_count: stats.count,
            last_run_at: stats.last_submitted_micros.map(secs),
            needs_attention,
            mine,
        }
    }

    /// Whether it is `actor_id`'s, for `GET /experiments?mine=true`: they created it, or `runs`
    /// (their runs per experiment,
    /// [`ExperimentReads::experiment_runs_of`]) has one of theirs in it.
    pub(crate) fn is_mine(&self, actor_id: &str, runs: &HashMap<String, SubmitterRuns>) -> bool {
        self.created_by.as_deref() == Some(actor_id)
            || runs.get(&self.id).is_some_and(|r| r.runs > 0)
    }

    /// When it last saw activity, for the list's order (`biffix_api::ExperimentList`): the later of
    /// its newest run's submit and its own creation. Its creation counts so a new experiment filled
    /// with runs submitted before it existed sorts as new, not below older, empty ones.
    pub(crate) fn activity_micros(&self, stats: Option<&RunStats>) -> i64 {
        stats
            .and_then(|s| s.last_submitted_micros)
            .map_or(self.created_at_micros, |last| last.max(self.created_at_micros))
    }
}

/// A partial update ([`ExperimentStore::update`]): `None` keeps a field.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ExperimentChange {
    /// A new name (already checked by
    /// [`crate::runs::experiments::normalize_name`]).
    pub name: Option<String>,
    /// A new description; `Some(None)` clears it.
    pub description: Option<Option<String>>,
    /// Archive (`true`) or restore (`false`). Archiving an archived experiment keeps its time.
    pub archived: Option<bool>,
    /// The time an archive takes (epoch micros).
    pub now_micros: i64,
}

/// Read and write experiments.
#[async_trait]
pub trait ExperimentStore: Send + Sync {
    /// The experiments, of `project` only when given, archived ones too when `include_archived`.
    /// In no particular order: the caller orders them by activity
    /// ([`ExperimentRecord::activity_micros`], which needs the runs' stats from the projection).
    async fn list(
        &self,
        project: Option<&str>,
        include_archived: bool,
    ) -> Result<Vec<ExperimentRecord>, StoreError>;
    /// One experiment, or [`StoreError::NotFound`].
    async fn get(&self, id: &str) -> Result<ExperimentRecord, StoreError>;
    /// Add a new experiment. [`StoreError::Conflict`] when the id is taken, or a live experiment of
    /// its project has its name (ignoring case).
    async fn create(&self, record: &ExperimentRecord) -> Result<(), StoreError>;
    /// Apply `change` and return the stored experiment. [`StoreError::NotFound`] when absent;
    /// [`StoreError::Conflict`] when the result would share a live experiment's name in its project
    /// (a rename, or a restore whose name was taken meanwhile).
    async fn update(
        &self,
        id: &str,
        change: &ExperimentChange,
    ) -> Result<ExperimentRecord, StoreError>;
}

/// The conflict a name clash is reported as (by the Postgres store and the in-memory double): it
/// names the name and the project.
#[cfg(any(test, feature = "aws", feature = "test-support"))]
pub(crate) fn name_conflict(project: &str, name: &str) -> StoreError {
    StoreError::Conflict(format!("project {project} already has an experiment named `{name}`"))
}

/// One submitter's runs in one experiment ([`ExperimentReads::experiment_runs_of`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SubmitterRuns {
    /// How many of the experiment's runs they submitted.
    pub runs: i64,
    /// How many of those need the user's attention: failed, with the `failed` notification not
    /// dismissed (as [`RunQuery::attention`] reads it).
    pub attention: i64,
}

/// An experiment's runs (the folded `run_projection.experiment_id`): a page, all of them,
/// their counts and their summed cost. The defaults read the run list
/// ([`RunReads::list_runs`]) and the runs' costs ([`CostReads::run_costs`]).
#[async_trait]
pub trait ExperimentReads: RunReads + CostReads {
    /// The newest `limit` (clamped as [`RunQuery::page_size`] clamps it) runs in experiment
    /// `experiment_id`, newest first, as the run list builds them: the list's first page for
    /// `?experiment=`, without its total (the caller has the count from
    /// [`Self::experiment_run_stats`]). The default reads that page through
    /// [`RunReads::list_runs`]; Postgres skips the list's `COUNT(*)`.
    async fn experiment_runs(
        &self,
        experiment_id: &str,
        limit: i64,
    ) -> Result<Vec<RunSummary>, StoreError> {
        let page = self
            .list_runs(&RunQuery {
                experiment: Some(experiment_id.to_string()),
                limit,
                ..Default::default()
            })
            .await?;
        Ok(page.runs)
    }

    /// One page ([`MAX_RUN_PAGE`] runs) of experiment `experiment_id`'s runs, in [`run_sort_key`]
    /// order, after the `after` cursor (`None`: the first page), for a read that pages through them
    /// all. Its `total` is not counted (0): the caller has the count from
    /// [`Self::experiment_run_stats`]. The default reads the page through [`RunReads::list_runs`];
    /// Postgres skips the list's `COUNT(*)`.
    ///
    /// [`run_sort_key`]: super::runs::run_sort_key
    async fn experiment_run_page(
        &self,
        experiment_id: &str,
        after: Option<String>,
    ) -> Result<RunPage, StoreError> {
        let page = self
            .list_runs(&RunQuery {
                experiment: Some(experiment_id.to_string()),
                limit: MAX_RUN_PAGE,
                after,
                ..Default::default()
            })
            .await?;
        Ok(RunPage { total: 0, ..page })
    }

    /// Every run in experiment `experiment_id`, newest first, each once:
    /// [`Self::experiment_run_page`] after page. A run whose anchor lands while it pages moves down
    /// the order and can come back on a later page ([`run_sort_key`]); only its first copy is kept.
    ///
    /// [`run_sort_key`]: super::runs::run_sort_key
    async fn all_experiment_runs(
        &self,
        experiment_id: &str,
    ) -> Result<Vec<RunSummary>, StoreError> {
        let mut runs: Vec<RunSummary> = Vec::new();
        let mut seen = HashSet::new();
        let mut after = None;
        loop {
            let page = self.experiment_run_page(experiment_id, after.take()).await?;
            let full = page.runs.len() as i64 >= MAX_RUN_PAGE;
            runs.extend(page.runs.into_iter().filter(|r| seen.insert(r.run_id.to_string())));
            match page.last_cursor {
                Some(cursor) if full => after = Some(cursor),
                _ => return Ok(runs),
            }
        }
    }

    /// How many runs each of `experiment_ids` holds and when the newest was submitted (the folded
    /// `run_projection.experiment_id`), in one read. An experiment with no runs is absent. The one
    /// source of an experiment's run count and newest run, for the list and the detail alike.
    async fn experiment_run_stats(
        &self,
        experiment_ids: &[String],
    ) -> Result<HashMap<String, RunStats>, StoreError>;

    /// The runs `submitter` (`user:<sub>`, as a run records it) has in each experiment, and how
    /// many of those need `user`'s attention (`user` is the `sub` notifications are addressed to;
    /// see [`RunQuery::attention`]), in one read. An experiment they have no run in is absent.
    /// `experiment` scopes the read to that one experiment (a detail or an update needs only its
    /// own); `None` covers every experiment. Powers `GET /experiments?mine=true` and each
    /// experiment's `mine` and `needs_attention`.
    async fn experiment_runs_of(
        &self,
        submitter: &str,
        user: &str,
        experiment: Option<&str>,
    ) -> Result<HashMap<String, SubmitterRuns>, StoreError>;

    /// The summed estimated cost (`RunCost::estimated_usd`: every priced job) of every run in
    /// experiment `experiment_id`; `None` when none of them has a priced job. The default reads
    /// [`Self::all_experiment_runs`] and their [`CostReads::run_costs`], summed in run order;
    /// Postgres sums in one aggregate query.
    async fn experiment_priced_usd(&self, experiment_id: &str) -> Result<Option<f64>, StoreError> {
        let ids: Vec<RunId> = self
            .all_experiment_runs(experiment_id)
            .await?
            .into_iter()
            .map(|run| run.run_id)
            .collect();
        let costs = self.run_costs(&ids).await?;
        Ok(ids
            .iter()
            .filter_map(|id| costs.get(id.as_str())?.usd)
            .fold(None, |total, usd| Some(total.unwrap_or(0.0) + usd)))
    }
}
