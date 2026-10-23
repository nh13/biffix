//! `experiments` — named groups of runs (the Lab view's *Experiments*). An experiment lives in a
//! project and adds no permissions; a run is in at most one, recorded on the run as an event.

use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer, Serialize};

use crate::RunSummary;

/// One experiment, with a summary of its runs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Experiment {
    pub id: String,
    /// The project it lives in (its runs are this project's).
    pub project: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Who created it (their id), and how to show them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_by_label: Option<String>,
    /// Epoch seconds.
    pub created_at: f64,
    /// Set once archived (hidden from the default list); epoch seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived_at: Option<f64>,
    /// How many runs are in it.
    pub run_count: i64,
    /// When its newest run was submitted (epoch seconds).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run_at: Option<f64>,
    /// How many of the caller's own runs in it need their attention: failed, and not dismissed
    /// from their *Needs attention* (as the run list's `?attention=true` reads it). Per caller; 0 for one
    /// with no runs in it.
    #[serde(default)]
    pub needs_attention: i64,
    /// Whether it is the caller's: they created it, or submitted a run into it (what `?mine=true`
    /// keeps). Per caller, so a list of everyone's can be split into Mine and All without a second
    /// read.
    #[serde(default)]
    pub mine: bool,
}

/// `GET /experiments` query.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::IntoParams))]
#[cfg_attr(feature = "openapi", into_params(parameter_in = Query))]
pub struct ExperimentListQuery {
    /// Only this project's experiments.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    /// `true`: include archived experiments.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived: Option<bool>,
    /// `true`: only the caller's experiments: those they created or submitted a run into.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mine: Option<bool>,
}

/// `GET /experiments/{id}/samples.csv` query.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::IntoParams))]
#[cfg_attr(feature = "openapi", into_params(parameter_in = Query))]
pub struct ExperimentSamplesCsvQuery {
    /// The downloader's time zone, an IANA name (`America/New_York`), for the start times in
    /// unnamed analyses' names: each read with the zone's rules for that instant, daylight saving
    /// included, as the UI shows it. Absent or blank: UTC. A name the server does not know is a
    /// 400.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tz: Option<String>,
}

/// `GET /experiments` — newest activity first: an experiment's activity is the later of its newest
/// run's submit and its own creation (so a new experiment filled with older runs sorts as new), ties
/// broken by id. `?project=` scopes to one project; `?archived=true` includes archived ones;
/// `?mine=true` keeps only the caller's: those they created or submitted a run into (a run in it
/// whose submitter is them, as the run list's `?mine=true` reads a run's submitter).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ExperimentList {
    pub experiments: Vec<Experiment>,
}

/// `POST /experiments` — create one. The name is trimmed, 1..=200 characters, unique (ignoring
/// case) among the project's experiments that are not archived.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ExperimentCreateRequest {
    pub project: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// `PUT /experiments/{id}` — a partial update: an absent field keeps its value; `description:
/// null` (or blank) clears it; `archived` archives (`true`) or restores (`false`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ExperimentUpdateRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::settings::present"
    )]
    #[cfg_attr(feature = "openapi", schema(value_type = Option<String>, nullable))]
    pub description: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived: Option<bool>,
}

/// One sample across an experiment's runs: its state in each run that has it, by run id. A state is
/// the samples grid's row state for that sample in that run: `waiting`, `running`, `done`, `failed`
/// or `stopped` (an open set: a client shows one it does not know as is).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ExperimentSample {
    pub id: String,
    pub by_run: BTreeMap<String, String>,
}

/// `GET /experiments/{id}` — the experiment, its newest runs (newest first, at most 200), the
/// samples across those runs and one total cost over all of its runs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ExperimentDetail {
    pub experiment: Experiment,
    /// Its newest runs, newest first: all of them up to 200, else the 200 newest (see
    /// `runs_total`; the run list's `?experiment=` pages through every one).
    pub runs: Vec<RunSummary>,
    /// How many runs it holds in all (`runs.len()` unless the list was cut short).
    pub runs_total: i64,
    /// Samples named by any of `runs`, in first-seen order (runs oldest first).
    pub samples: Vec<ExperimentSample>,
    /// The sum of the estimated costs of all of its runs (not only `runs`); absent when none has
    /// a priced job.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_cost_usd: Option<f64>,
}

/// `POST /runs/{id}/experiment` — put a run in an experiment (of its project), or take it out
/// (`experiment_id: null`, or blank). Recorded as the run's `experiment_set` event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct RunExperimentSetRequest {
    /// Required, even to take the run out (`null`): a body without it (say, a misspelled field)
    /// is refused rather than read as "out of any experiment".
    #[serde(deserialize_with = "nullable")]
    #[cfg_attr(feature = "openapi", schema(required = true, nullable))]
    pub experiment_id: Option<String>,
}

/// A field that must be present but may be `null`. Unlike a plain `Option` field, which serde reads
/// as `None` when absent, a field with a `deserialize_with` and no `default` is required.
pub(crate) fn nullable<'de, D: Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    Option::<String>::deserialize(d)
}
