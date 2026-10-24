//! `runs` — the run list and one run's detail (served from the projection).

use serde::{Deserialize, Serialize};

use crate::SupportContact;

use biffix_core::ids::{BatchJobId, CoordinatorJobId, RunId, TaskKey, WorkflowVersionId};

use crate::{ReferenceUse, RunProvenance, SubmittedImage, WorkflowImage};

/// One run's summary row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct RunSummary {
    #[cfg_attr(feature = "openapi", schema(value_type = String))]
    pub run_id: RunId,
    pub state: String,
    /// Whether `state` is terminal (the run changes no more), as the server's run lifecycle
    /// decides. A client reads this rather than re-deriving it from `state`, an open string whose
    /// set of values may grow. Filled by the API handler; `false` only means "still in flight" when
    /// the server sent it.
    #[serde(default)]
    pub terminal: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub submitted_at: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub submitter: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub submitter_label: Option<String>,
    /// The run's human name, given at submit or by a later rename (`POST /runs/{id}/name`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(value_type = Option<String>))]
    pub workflow: Option<WorkflowVersionId>,
    /// The workflow's display name (joined from the version), for a human-readable label + link.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_name: Option<String>,
    /// The pipeline's friendly title (the version's form `title`, from `biffix.yaml`), joined from
    /// the version; absent when its author gave none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_title: Option<String>,
    /// Engine (snakemake/nextflow), joined from the workflow version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine: Option<String>,
    /// Owning project, joined from the workflow version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    /// Observed task-outcome counts over the projected task rows. Always present (0 when no tasks).
    /// The UI pairs these with `total`: a determinate bar when `total` is known, else an
    /// indeterminate indicator with these as outcome chips (done/running/failed).
    pub tasks_observed: i64,
    pub tasks_succeeded: i64,
    pub tasks_running: i64,
    /// Tasks whose live Batch attempts are all still queued (e.g. `RUNNABLE`, waiting on capacity)
    /// — none has started executing.
    #[serde(default)]
    pub tasks_queued: i64,
    pub tasks_failed: i64,
    /// Tasks the run's cancel ended: still open when it was cancelled, or failed only once the
    /// cancel was requested. 0 unless the run was cancelled.
    #[serde(default)]
    pub tasks_cancelled: i64,
    /// Of the succeeded tasks, how many were engine cache-hits (a subset of `tasks_succeeded`).
    pub tasks_cached: i64,
    /// Output objects the engine registered for this run, excluding work-dir intermediates. 0 when
    /// the engine registered none.
    #[serde(default)]
    pub outputs_total: i64,
    /// Of `outputs_total`, the objects satisfied from the engine cache rather than produced.
    #[serde(default)]
    pub outputs_reused: i64,
    /// The run resumed another (`--resume-from`), so it may have results it reused from the runs it
    /// resumed though `outputs_total` (what it registered itself) is 0. The run detail names that
    /// run (`resumed_from`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub resumed: bool,
    /// The input URIs declared at submit (provenance; empty when the submit declared none).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub input_uris: Vec<String>,
    /// How the run's samples stand (the Lab view's "9 of 12 samples done"); absent when no task
    /// names a sample.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub samples: Option<RunSampleCounts>,
    /// The experiment the run is in, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub experiment_id: Option<String>,
    /// The run's estimated cost so far (USD): its detail's `cost.estimated_usd`, every priced job
    /// ([`RunCost::priced_usd`]). Absent until a job is priced (even when the detail's `cost` is
    /// present with only pending jobs). It keeps growing
    /// until every job is priced, which can be after the run is `terminal`: the pricer prices
    /// finished jobs on its next reconcile tick, and the coordinator only after the run ends (see
    /// `cost_pending`). Also on the run detail, next to its `cost`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    /// Whether the run has jobs not priced yet that still can be (its detail's
    /// `cost.pending_jobs > 0`), so `cost_usd` (or its absence) is not final yet, even for a
    /// `terminal` run.
    #[serde(default)]
    pub cost_pending: bool,
}

/// How a run's samples stand (`biffix_core::sample_progress::SampleCounts` on the wire).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct RunSampleCounts {
    /// With a sheet (`declared`): the sheet's samples plus any other sample a task named; else the
    /// samples tasks have named so far.
    pub total: i64,
    pub done: i64,
    pub failed: i64,
    /// Whether the run has a sheet (`false`: `total` is the samples seen so far).
    pub declared: bool,
}

impl From<biffix_core::sample_progress::SampleCounts> for RunSampleCounts {
    fn from(c: biffix_core::sample_progress::SampleCounts) -> RunSampleCounts {
        let biffix_core::sample_progress::SampleCounts { total, done, failed, declared } = c;
        RunSampleCounts { total, done, failed, declared }
    }
}

/// A page of runs plus the total matching the filter/search (for the list's pagination controls).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct RunPage {
    pub runs: Vec<RunSummary>,
    pub total: i64,
    pub limit: i64,
    /// Keyset cursor bounding the newer edge of this page — pass as `before` to page back. `None`
    /// on an empty page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_cursor: Option<String>,
    /// Keyset cursor bounding the older edge — pass as `after` to page forward. `None` when empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_cursor: Option<String>,
}

/// Which page of runs to return: the query of `GET /runs`, or the JSON body of `POST /runs/query`.
/// The body form keeps the search text (which can be a run name, and so name a sample) out of the
/// request URL, and so out of every access log.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema, utoipa::IntoParams))]
#[cfg_attr(feature = "openapi", into_params(parameter_in = Query))]
pub struct RunQueryRequest {
    /// Which runs by state (default all). A string here, parsed with `RunFilter::parse` once the
    /// caller may list runs: an unknown one is a 400.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(value_type = Option<crate::RunFilter>))]
    #[cfg_attr(feature = "openapi", param(value_type = Option<crate::RunFilter>))]
    pub filter: Option<String>,
    /// Free-text search: a case-insensitive substring of one of run id, run name, workflow name,
    /// the pipeline's title (its form's `title`), workflow-version id, submitter, submitter label,
    /// project or engine. A name the UI derives for an unnamed run is not searched as a whole, but
    /// its pipeline title is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub q: Option<String>,
    /// Exact frozen workflow-version id to scope to (runs of one registered version).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(value_type = Option<String>))]
    #[cfg_attr(feature = "openapi", param(value_type = Option<String>))]
    pub workflow: Option<WorkflowVersionId>,
    /// `true`: only runs the caller submitted ("my runs"). Default: everyone's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mine: Option<bool>,
    /// `true`: only the caller's failed runs they have not dismissed (the Lab home's *Needs
    /// attention*; opening or reading one leaves it there). Implies `mine`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attention: Option<bool>,
    /// Only the runs in this experiment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub experiment: Option<String>,
    /// Only these runs, by id (at most 200; an unknown id is left out): how a client names many
    /// runs it knows only by id at once (the run folders a Data page lists). Empty or absent ⇒ any
    /// run; a list whose ids are all blank names no run (an empty page, never every run). In the
    /// body of `POST /runs/query` only: `GET /runs?ids=…` is refused with a 400, since a query
    /// string carries no list.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "openapi", param(ignore))]
    #[cfg_attr(feature = "openapi", schema(value_type = Vec<String>))]
    pub ids: Vec<RunId>,
    /// Page size (default 50, clamped by the store).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<i64>,
    /// Keyset cursor: return the next (older) page after this one. From a prior `last_cursor`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<String>,
    /// Keyset cursor: return the previous (newer) page. From a prior `first_cursor`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<String>,
}

/// `POST /runs/{id}/name` — rename a run, or clear its name (`run_name: null`, or blank) so readers
/// show a derived one. Recorded as the run's `run_renamed` event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct RunRenameRequest {
    /// Required, even to clear the name (`null`): a body without it (say, a misspelled field) is
    /// refused rather than read as "clear". Trimmed; at most 200 characters.
    #[serde(deserialize_with = "crate::experiments::nullable")]
    #[cfg_attr(feature = "openapi", schema(required = true, nullable))]
    pub run_name: Option<String>,
}

/// A task row within a run's detail.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Task {
    #[cfg_attr(feature = "openapi", schema(value_type = String))]
    pub task_key: TaskKey,
    pub state: String,
    pub attempts: i64,
    /// Per-task display detail from the task's display attempt; omitted when absent (a local rule,
    /// or an attempt that has not terminated yet).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(value_type = Option<String>))]
    pub batch_job_id: Option<BatchJobId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_stream: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub termination_category: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance_id: Option<String>,
    /// Attempt start / finish as epoch seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<f64>,
    /// Whether this (succeeded) task was an engine cache-hit rather than work run this time.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub cached: bool,
    /// The DAG node (rule/process) this task rolls up onto, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    /// Task keys of the tasks in this run that this one depends on (produced its inputs). Its
    /// inverse is `feeds`, which only `POST /runs/{id}/tasks/query` fills: a reader of
    /// `GET /runs/{id}`'s whole task list derives it from the list.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deps: Vec<String>,
    /// The run's tasks that depend on this one (the inverse of `deps`). Filled by
    /// `POST /runs/{id}/tasks/query`, which pages tasks and so cannot leave the inverse to the
    /// client; empty in `GET /runs/{id}`'s task list.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub feeds: Vec<String>,
    /// Estimated cost of every priced attempt of this task (USD), retries and spot-lost attempts
    /// included; absent until one is priced.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    /// The host the display attempt ran on, when anything about it is known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<Host>,
    /// When EC2 warned that the display attempt's spot host would be reclaimed (epoch seconds), if
    /// it did while the attempt was running.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spot_warned_at: Option<f64>,
    /// The container image the task ran in (its display attempt's), as the engine reported it at
    /// submit; absent when the engine did not report one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    /// Why the workflow engine failed this task although its AWS Batch job did not fail (e.g. it
    /// did not write its declared outputs); absent otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verdict_reason: Option<String>,
    /// The vCPUs the display attempt's Batch job requested (not its host's capacity — see `host`); absent when
    /// Batch reported no requirement.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_vcpus: Option<f64>,
    /// The memory (MiB) the display attempt's Batch job requested; absent when unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_memory_mib: Option<i64>,
}

/// The EC2 host an attempt ran on, joined from the ECS host registry, plus whether it was spot or
/// on-demand. Every field is optional: a host registered before the registry existed has only the
/// queue-derived capacity, and a still-queued attempt has no host at all.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Host {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ec2_instance_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub availability_zone: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_architecture: Option<String>,
    /// vCPUs ECS could allocate on the host.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registered_vcpus: Option<f64>,
    /// Memory (MiB) ECS could allocate on the host, after the agent's reservation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registered_memory_mib: Option<i64>,
    /// The Batch job queue (name) the attempt ran on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue: Option<String>,
    /// `spot`, `on_demand`, or another EC2 lifecycle (e.g. `capacity_block`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capacity: Option<String>,
    /// Where `capacity` came from: `instance` (EC2 reported it for this host) or `queue` (the job
    /// queue's configured capacity, used when the instance's own lifecycle is unknown).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capacity_source: Option<String>,
}

/// `GET /workflows/{id}/estimate` query.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::IntoParams))]
#[cfg_attr(feature = "openapi", into_params(parameter_in = Query))]
pub struct WorkflowEstimateQuery {
    /// Samples the launch is for; absent (or 0) asks for the typical run only.
    #[serde(default)]
    pub samples: u32,
}

/// `GET /workflows/{id}/estimate?samples=N`: a launch's cost and time from the workflow's recent
/// successful runs. `estimate` is absent until it has two runs with a cost and a sample count.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct WorkflowEstimate {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimate: Option<LaunchEstimate>,
    /// How long (and how much) one run of the workflow usually takes, whatever its sample count:
    /// from its recent successful runs, sample count or cost recorded or not. Absent until it has
    /// two. Answered with or without `samples`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub typical: Option<TypicalRun>,
}

/// A typical run of a workflow: the median wall time of its recent successful runs, and their
/// median cost when at least two of them are priced.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct TypicalRun {
    /// Median hours from launch to completion.
    pub hours_median: f64,
    /// Median USD of the priced ones; absent with fewer than two priced.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usd_median: Option<f64>,
    /// Past runs the times are from.
    pub runs: i64,
}

/// USD for the samples asked for (median, and the 25th–75th percentile range), and median hours.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct LaunchEstimate {
    pub usd_median: f64,
    pub usd_low: f64,
    pub usd_high: f64,
    pub usd_per_sample: f64,
    pub hours_median: f64,
    /// Past runs the estimate is from.
    pub runs: i64,
}

/// Why a failed run failed, in plain words (`GET /runs/{id}/failure-summary`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct FailureSummary {
    /// How many steps failed (the items cover the first few).
    pub failed_steps: i64,
    /// The first failed steps, earliest first; or the engine's own failure when no step failed.
    /// Empty for a run that did not fail.
    pub items: Vec<FailureItem>,
    /// Who to ask about this run: its workflow's `catalog.contact`, else the site's support
    /// contact; `None` when neither is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub support: Option<SupportContact>,
}

/// One failed step (or the engine), its cause and what to try.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct FailureItem {
    /// The failed task; absent for the engine's own failure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(value_type = Option<String>))]
    pub task_key: Option<TaskKey>,
    /// What to call it: the step (rule/process) name, else the task key, else `workflow engine`.
    pub step: String,
    /// `could_not_start`, `out_of_memory`, `spot_reclaimed`, `host_lost`, `timed_out`,
    /// `missing_input`, `bad_input`, `reference_mismatch`, `no_space`, `access_denied`,
    /// `missing_output`, `step_error`, `engine_error`, `launch_failed` or `unknown` (open: a reader
    /// treats a value it does not know as unknown).
    pub cause: String,
    /// The sample the failed task belongs to (per-sample progress), when it is attributed to one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sample: Option<String>,
    /// What *Try again* should do for this cause: `resume` (pick up where it stopped), `rerun`
    /// (start again with the same inputs), `fix_samples` or `fix_options` (retrying unchanged would
    /// fail the same way: open the launch form pre-filled at samples / at options). Open.
    pub try_again: String,
    /// Completes "`<step>` failed: …".
    pub message: String,
    pub advice: String,
    /// Attempts the step made.
    pub attempts: i64,
    /// The last lines of its log (or the engine's), oldest first.
    pub log_tail: Vec<String>,
    /// AWS's own reason its last attempt stopped, when it gave one (e.g. why a container could not
    /// start — there is no log then); or, when the workflow engine failed a step whose job
    /// succeeded, the engine's reason.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Set when the run failed before it launched: the control-plane stage it stopped at
    /// (`image_preparation` or `launch`; open, so a reader treats any value as "before launch").
    /// There is then no job or engine log to open. Absent for a failed step or the engine.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stage: Option<String>,
    /// The exit code of the failed task's last attempt, when AWS recorded one; for a Snakemake job
    /// whose log names its command's own exit status, that status (its job exits 1 whatever the
    /// command did).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i64>,
    /// The AWS Batch job of the failed task's last attempt (for a support report).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(value_type = Option<String>))]
    pub batch_job_id: Option<BatchJobId>,
}

/// One AWS Batch attempt of a task. A task retried after a spot reclaim has several; `Task`
/// summarizes only the display attempt (the succeeded one, else the highest-numbered).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct TaskAttempt {
    #[cfg_attr(feature = "openapi", schema(value_type = String))]
    pub batch_job_id: BatchJobId,
    /// The engine's attempt number (1-based); absent until the engine's submit is observed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt: Option<i64>,
    /// The attempt's own last observed state: created, submitted, queued, running, succeeded or
    /// failed.
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub termination_category: Option<String>,
    /// Batch's human-readable reason for a failed attempt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_stream: Option<String>,
    /// The Batch job queue (name) the attempt ran on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue: Option<String>,
    /// The ECS container-instance id of the host (not an EC2 instance id).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance_id: Option<String>,
    /// Attempt start / finish as epoch seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<f64>,
    /// Whether this is the task's display attempt (the one `Task` summarizes).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub display: bool,
    /// The host the attempt ran on, when anything about it is known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<Host>,
    /// When EC2 warned that the attempt's spot host would be reclaimed (epoch seconds), if it did
    /// while the attempt was running — i.e. the attempt was lost to a spot reclaim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spot_warned_at: Option<f64>,
    /// The attempt's estimated cost, once priced.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<JobCost>,
    /// The vCPUs the attempt's Batch job requested (not its host's capacity — see `host`); absent when
    /// Batch reported no requirement.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_vcpus: Option<f64>,
    /// The memory (MiB) the attempt's Batch job requested; absent when unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_memory_mib: Option<i64>,
}

/// One job's estimated cost (an attempt, or the coordinator).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct JobCost {
    /// Compute + root volume, USD.
    pub usd: f64,
    pub compute_usd: f64,
    pub ebs_usd: f64,
    pub hours: f64,
    /// The average compute price over the job, USD/hour.
    pub price_per_hour: f64,
    /// Where the compute price came from: `on_demand` (list price), `spot_history` (EC2 spot price
    /// history for the host's type and AZ — what spot bills), `spot_at_on_demand` (spot, history
    /// unavailable: an upper bound), `unknown_at_on_demand` (lifecycle unknown).
    pub price_source: String,
}

/// A run's estimated AWS cost: compute and root volume for the time each job ran, from its host's
/// type and AZ and EC2 prices. An estimate, not the bill — it excludes host boot/idle time, credits
/// and Savings Plans; same-region S3 transfer is free.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct RunCost {
    /// Every priced job — all task attempts (retries and spot-lost ones too) and the coordinator.
    pub estimated_usd: f64,
    pub compute_usd: f64,
    pub ebs_usd: f64,
    /// The coordinator's share of `estimated_usd`.
    pub coordinator_usd: f64,
    /// Only each task's final attempt, plus the coordinator: what the run would have cost without
    /// retries.
    pub winning_usd: f64,
    pub priced_jobs: i64,
    /// Jobs not priced yet (still running, or priced on the next reconcile tick).
    pub pending_jobs: i64,
    /// The distinct price sources behind the estimate (see [`JobCost::price_source`]).
    pub price_sources: Vec<String>,
    pub by_instance_type: Vec<CostByInstanceType>,
}

impl RunCost {
    /// The run's cost as one number: its estimate (`estimated_usd`, every priced job) once a job
    /// is priced; `None` while none is (only pending jobs). The rule every place that shows a
    /// run's cost follows (the run list, the run detail's `cost_usd`, an experiment's total).
    pub fn priced_usd(&self) -> Option<f64> {
        (self.priced_jobs > 0).then_some(self.estimated_usd)
    }
}

/// The estimate grouped by the host type (and lifecycle) the jobs ran on.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct CostByInstanceType {
    pub instance_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lifecycle: Option<String>,
    pub jobs: i64,
    pub hours: f64,
    pub usd: f64,
}

/// One object a run produced (`POST /runs/{id}/outputs/query`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct RunOutput {
    /// Opaque, stable id; the per-object routes take this, never a key.
    pub id: String,
    pub s3_uri: String,
    /// Relative to the run's output prefix when inside it, else the engine's own path, else the
    /// bucket-relative key.
    pub path: String,
    /// The basename.
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(value_type = Option<String>))]
    pub task_key: Option<TaskKey>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt: Option<i64>,
    /// output | published | intermediate | log | benchmark (open set).
    pub role: String,
    /// The registering run (this one, or the one named by `reused_from`) satisfied it from the
    /// engine's cache (a Nextflow `-resume` cache hit) rather than producing it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub reused: bool,
    /// Set when this run registered no output for the file and a run it resumed did: the nearest
    /// run in its resume chain (directly or through others) that registered it, sharing this run's
    /// output prefix. That run is not necessarily the one that wrote the bytes (it may itself have
    /// had it from the cache: see `reused`). Distinct from `reused`: a Snakemake resume skips a
    /// finished task and registers nothing for it, so its file comes from the earlier run's record;
    /// a Nextflow resume registers a cache hit on the resumed run itself, with `reused` set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(value_type = Option<String>))]
    pub reused_from: Option<RunId>,
    /// Bytes: S3's answer once checked, else what the engine reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub etag: Option<String>,
    /// Epoch seconds S3 last modified the object.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_modified: Option<f64>,
    /// `None` until S3 has been checked; `Some(false)` when the object is gone (a `temp()` file
    /// removed, or deleted since).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exists: Option<bool>,
    /// The object's etag no longer matches the one its producer reported: something (often a later
    /// run sharing the output prefix) has overwritten it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub modified_since_produced: bool,
    /// Whether biffix will issue a download link for it. False ⇒ listed only (outside the project
    /// bucket, or under a reserved prefix).
    pub servable: bool,
    /// How the UI can show it in the browser: `table`, `text`, `image`, `pdf` or `html`, from the
    /// file name. Absent ⇒ download only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub viewer: Option<String>,
}

/// One report a run wrote (`GET /runs/{id}/reports`): a report its workflow declared, or a MultiQC
/// report found among its outputs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct RunReport {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// `declared` (the workflow's `reports:`), `multiqc`: the run's MultiQC report, the one
    /// `/qc/general-stats` reads (found by name; a declared report that is it keeps its name), or
    /// `detected`: a FastQC result the workflow did not declare, found by name (for the QC
    /// previews; a client listing only what the workflow declared leaves these out). Open.
    pub kind: String,
    /// The output that is the report (open it with the output view/preview endpoints).
    pub output: RunOutput,
    /// How to show it: `html`, `table`, `image`, `pdf`, `alignment`, `reads`, `fastqc` (a FastQC
    /// `_fastqc.html` or `_fastqc.zip`; its data comes from `/fastqc`) or `file` — the author's
    /// `kind:`, else from the file's name. Open.
    pub format: String,
    /// For a `per_sample` report, the sample this one is (its `{sample}` in the declared path).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sample: Option<String>,
    /// The 0-based index, in the workflow's `reports:` (`biffix.yaml`), of the declaration this
    /// report came from: the same for every file and sample one declaration produced, so a client
    /// can group by it (two declarations may share a name). `None` for a report no declaration
    /// produced: the undeclared MultiQC report and `detected` FastQC results.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub declaration: Option<u32>,
}

/// A run's reports: declared ones first, in the workflow's order (one declaration's reports
/// together: a per-sample one sample by sample in the run's sample order, each sample's files back
/// to back in path order), then the run's MultiQC report, then the FastQC results the workflow did
/// not declare (`kind` `detected`, `format` `fastqc`, grouped by sample the same way, then any no
/// sample owns).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct RunReportList {
    pub reports: Vec<RunReport>,
}

/// `GET /usage` query: the period and how to group it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::IntoParams))]
#[cfg_attr(feature = "openapi", into_params(parameter_in = Query))]
pub struct UsageQuery {
    /// First day, `YYYY-MM-DD` (default: the first of this month).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    /// The day after the period, `YYYY-MM-DD` (default: the first of next month).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    /// `month` (default), `workflow`, `submitter`, `instance_type` or `project`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_by: Option<String>,
}

/// Estimated spend over a period (`GET /usage`), from EC2 prices.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct UsageReport {
    /// The period's first day (`YYYY-MM-DD`, UTC).
    pub from: String,
    /// The day after the period (exclusive).
    pub to: String,
    /// `month`, `workflow`, `submitter`, `instance_type` or `project`.
    pub group_by: String,
    pub rows: Vec<UsageRow>,
    /// The whole period (each run counted once).
    pub total: UsageRow,
}

/// One group's estimated spend.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct UsageRow {
    pub key: String,
    /// Runs with a cost estimate submitted in the period (by instance type, a run on several types
    /// counts in each).
    pub runs: i64,
    pub compute_usd: f64,
    pub ebs_usd: f64,
    pub total_usd: f64,
}

/// A run's MultiQC General Statistics as data (`GET /runs/{id}/qc/general-stats`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct QcGeneralStats {
    /// The MultiQC report these come from (an output of the run).
    pub report_output_id: String,
    /// The columns in the report's order.
    pub columns: Vec<QcColumn>,
    pub samples: Vec<QcSample>,
}

/// One General Statistics column, with MultiQC's display hints.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct QcColumn {
    /// The key of this column in each sample's `values`.
    pub id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The tool the column comes from (`FastQC`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suffix: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    /// A color scale name (`RdYlGn`, `Blues`, …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scale: Option<String>,
    /// Decimals to show.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decimals: Option<u8>,
    /// Hidden by default in the report.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub hidden: bool,
    /// MultiQC's `{pass|warn|fail: [{gt|lt|eq|…: value}]}` rules, when the column has them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(value_type = Option<Object>))]
    pub cond_formatting_rules: Option<serde_json::Value>,
}

/// One sample's General Statistics values, by column id (a missing value is absent).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct QcSample {
    pub name: String,
    pub values: std::collections::BTreeMap<String, f64>,
}

/// A page of a run's outputs, ordered by `s3_uri`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct RunOutputPage {
    pub outputs: Vec<RunOutput>,
    /// How many outputs match the filter (across all pages).
    pub total: i64,
    /// The run's effective output prefix, which `path`s are relative to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_prefix: Option<String>,
    /// Keyset cursor for the next page; absent on the last page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    /// How many matching outputs S3 has not been checked for yet (sizes still unknown). Each list
    /// request checks a bounded number, so re-requesting converges to zero.
    #[serde(default)]
    pub enrichment_pending: i64,
}

/// `POST /runs/{id}/outputs/query` request: the output-list filters in a JSON body. A task key, a
/// directory and the cursor (an output's S3 URI) can all name a sample or a file, so a client
/// sends them here rather than in a request URL, which proxies and access logs record.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct RunOutputQueryRequest {
    /// Only this task's outputs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    /// Roles to include; absent or empty ⇒ every role except `intermediate`; `*` ⇒ every role.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub roles: Vec<String>,
    /// Only outputs whose path (relative to the run's output prefix) starts with this: a directory
    /// of the output-prefix tree (`qc/`), or a file's own path (`qc/S1.bam`, which also matches
    /// files named after it, such as `qc/S1.bam.bai`: how a client finds one file's output).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefix: Option<String>,
    /// Keyset cursor from a prior page's `next_cursor`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<String>,
    /// Page size (default 500, max 2000).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<i64>,
}

/// A short-lived URL for one output (`GET /runs/{id}/outputs/{output_id}/download`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct OutputUrl {
    pub url: String,
    /// Epoch seconds after which the URL no longer works.
    pub expires_at: f64,
    pub filename: String,
    /// The content type the URL serves the object as (view URLs only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
}

/// The start of one output as text (`GET /runs/{id}/outputs/{output_id}/preview`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct OutputPreview {
    /// `text`, or `binary` when the bytes are not text (then `text` is empty).
    pub kind: String,
    pub text: String,
    /// More of the file exists than `text` shows.
    pub truncated: bool,
    /// Bytes read from S3.
    pub bytes_read: i64,
    /// The object's size, when S3 reported it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_size: Option<i64>,
    /// The object was gzip (or bgzip) and `text` is its decompressed start.
    pub decompressed: bool,
}

/// `POST /runs/{id}/attempts/query`: every attempt of one task, ordered by attempt number
/// (unnumbered last), then Batch job id.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct TaskAttemptList {
    #[cfg_attr(feature = "openapi", schema(value_type = String))]
    pub task_key: TaskKey,
    pub attempts: Vec<TaskAttempt>,
}

/// A node in the run's group-level DAG, with its task-instance state rollup.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct DagNode {
    pub id: String,
    pub pending: i64,
    pub running: i64,
    pub succeeded: i64,
    pub failed: i64,
    pub cached: i64,
}

/// One log line from a task attempt's CloudWatch stream.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct LogLine {
    /// Event time, epoch milliseconds (CloudWatch's native unit).
    pub ts_ms: i64,
    pub message: String,
}

/// `POST /runs/{id}/task-logs/query`: a page of a task's log stream. Serves the logs *through*
/// biffix (gated by the caller's ViewRuns capability), so viewing a task's logs needs a biffix
/// login, not an AWS console account. The stream name is derived server-side from the task the caller is
/// authorized to see — never supplied by the client.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct LogPage {
    /// The CloudWatch stream these events came from. `None` ⇒ the task has no stream yet (still
    /// queued, a cache-hit, or a local rule that never became a Batch job) — render "no logs yet".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_stream: Option<String>,
    pub events: Vec<LogLine>,
    /// Opaque CloudWatch cursors — pass one back as `cursor` to page. `next_backward_token` walks
    /// toward older events ("load earlier"); `next_forward_token` toward newer ("live tail").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_forward_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_backward_token: Option<String>,
}

/// The run's group-level dependency graph for the DAG view: nodes (with rollups) + directed edges.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct RunDag {
    pub nodes: Vec<DagNode>,
    /// Directed edges as `[from, to]` node-id pairs.
    pub edges: Vec<[String; 2]>,
}

/// Task counts by the Jobs tab's filter chips, over the tasks matching the other filters.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct TaskFilterCounts {
    pub all: i64,
    /// Running or queued.
    pub active: i64,
    pub succeeded: i64,
    pub failed: i64,
}

/// Which page of one task's log to return: the JSON body of `POST /runs/{id}/task-logs/query`. A
/// body keeps the task key (whose wildcards name a sample) out of the request URL, and so out of
/// every access log.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct TaskLogsRequest {
    /// The task key.
    #[cfg_attr(feature = "openapi", schema(value_type = String))]
    pub task: TaskKey,
    /// Opaque CloudWatch cursor from a prior page (`next_forward_token`/`next_backward_token`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    /// `forward` (live-tail toward newer events) or `backward`/absent (tail — newest first).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direction: Option<String>,
    /// Max events to return (default 200, clamped to 1..=1000).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<i32>,
    /// One attempt's Batch job id (from the task's attempts); absent ⇒ the task's display attempt.
    /// Must be an attempt of this task of this run, else 404.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(value_type = Option<String>))]
    pub job: Option<BatchJobId>,
}

/// `GET /runs/{id}/coordinator-logs` query: which page of the coordinator's log.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::IntoParams))]
#[cfg_attr(feature = "openapi", into_params(parameter_in = Query))]
pub struct CoordinatorLogsQuery {
    /// Opaque CloudWatch cursor from a prior page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    /// `forward` (from the given cursor toward newer events, or from the start of the stream when
    /// no cursor is given) or `backward`/absent (tail — newest first).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direction: Option<String>,
    /// Max events to return (default 200, clamped to 1..=1000).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<i32>,
}

/// Which task's attempts to return: the JSON body of `POST /runs/{id}/attempts/query` (the task key
/// kept out of the request URL).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct TaskAttemptsRequest {
    /// The task key.
    #[cfg_attr(feature = "openapi", schema(value_type = String))]
    pub task: TaskKey,
}

/// Which page of a run's tasks to return: the JSON body of `POST /runs/{id}/tasks/query`. A body
/// keeps a search term or a task key (which names a sample through its wildcards) out of the
/// request URL, and so out of every access log.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct TaskQueryRequest {
    /// Which tasks by state (default all). A string here, parsed with `TaskStateFilter::parse`
    /// once the caller may see the run: an unknown one is a 400.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(value_type = Option<crate::TaskStateFilter>))]
    pub state: Option<String>,
    /// Only this group (rule/process).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    /// A substring of the task key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub q: Option<String>,
    /// Only the task with exactly this key (e.g. a linked task, which a substring search can bury
    /// under the many keys that contain it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(value_type = Option<String>))]
    pub task_key: Option<TaskKey>,
    /// key | state | duration | started (default key).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sort: Option<String>,
    /// asc (default) | desc: `desc` reverses the sort's natural order.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order: Option<String>,
    /// Rows to skip.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset: Option<i64>,
    /// Page size (default 200, max 1000).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<i64>,
}

/// `POST /runs/{id}/tasks/query`: one page of a run's tasks.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct TaskPage {
    pub tasks: Vec<Task>,
    /// Tasks matching every filter, across all pages.
    pub total: i64,
    pub offset: i64,
    pub limit: i64,
    /// Counts per state filter, with the group and search filters applied.
    pub counts: TaskFilterCounts,
    /// The CloudWatch log group the tasks' `log_stream`s are in, when it is not the fleet-wide
    /// `/aws/batch/job`: a run's tasks log to their project's task log group. Omitted ⇒
    /// `/aws/batch/job`. (A run launched before its project had that group logged to
    /// `/aws/batch/job`; the log routes fall back to it, a console link built from this does not.)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_group: Option<String>,
}

/// `GET /runs/{id}` query.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::IntoParams))]
#[cfg_attr(feature = "openapi", into_params(parameter_in = Query))]
pub struct RunDetailQuery {
    /// `false` leaves out the task table (an empty `tasks`): a client that pages tasks through
    /// `POST /runs/{id}/tasks/query` polls the run without re-fetching every task. Default `true`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tasks: Option<bool>,
}

/// `GET /runs/{id}` detail: the run's summary (flattened onto the wire) plus its task table, DAG,
/// and resolved (secret-redacted) params.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct RunDetail {
    #[serde(flatten)]
    pub summary: RunSummary,
    pub tasks: Vec<Task>,
    /// Resolved params at submit, already secret-redacted. `{}` when none/unknown.
    pub params: serde_json::Value,
    /// The run's S3 output prefix (`s3://<project-bucket>/biffix/runs/<run_id>/`), filled by the
    /// handler which knows the deploy's project bucket.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_prefix: Option<String>,
    /// The coordinator (head) job's Batch id — links to the coordinator job/logs, the only visible
    /// artifact when a run fails before scheduling any child task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(value_type = Option<String>))]
    pub coordinator_job_id: Option<CoordinatorJobId>,
    /// The coordinator's CloudWatch log stream (read it through `GET /runs/{id}/coordinator-logs`).
    /// Absent before the coordinator starts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coordinator_log_stream: Option<String>,
    /// The host the coordinator ran on, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coordinator_host: Option<Host>,
    /// The engine session the run executed as — for Nextflow, the key of its resume cache under
    /// `<output_prefix>.nextflow/cache/<engine_session_id>/`. Absent for Snakemake runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine_session_id: Option<String>,
    /// True when the engine reported a different session than the one biffix launched it with (so
    /// `engine_session_id` is the engine's own). Omitted when false.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub session_mismatch: bool,
    /// The run this one resumed (`--resume-from`): it shares that run's output prefix, work dir and
    /// engine session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(value_type = Option<String>))]
    pub resumed_from: Option<RunId>,
    /// When the run's workflow version (`workflow`) was registered, epoch microseconds: with the
    /// git sha the version id starts with, a version label a person can tell apart. Absent when the
    /// registry no longer has the version (or could not be read).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_registered_at_micros: Option<i64>,
    /// When the run's coordinator was launched (its first launch), epoch microseconds: where its
    /// run time starts, as a typical run's (`WorkflowEstimate.typical`) does, so a wait for
    /// submit-time images or in the queue is not counted. Absent before launch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launched_at_micros: Option<i64>,
    /// Samplesheets biffix wrote for this run from rows given at submit, by param:
    /// `{uri, etag, rows: [{column: value}]}`. Absent when none were.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(value_type = Option<Object>))]
    pub input_tables: Option<serde_json::Value>,
    /// Who asked to cancel the run (their label), when it was cancelled on request and a label is
    /// known: never an opaque user id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancelled_by: Option<String>,
    /// The reference bundle the run was launched with and the URIs it filled, by bundle key:
    /// `{id, uris}`. Absent when none was chosen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference_bundle: Option<ReferenceUse>,
    /// What happens to the run's intermediate files: `default`, `delete_on_success` or `keep`.
    /// Absent ⇒ `default`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(value_type = Option<String>))]
    pub retention: Option<biffix_core::event::Retention>,
    /// When the retention sweep deleted the run's intermediate files (a Nextflow run's `work/`),
    /// epoch microseconds. Its results are kept, but it can no longer be resumed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_deleted_at_micros: Option<i64>,
    /// The run's estimated cost; absent until a job of the run has run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<RunCost>,
    /// What the run ran with (source commit, images, engine runtime), from its submit record.
    /// Absent when the store keeps no event log.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<RunProvenance>,
    /// The images given at submit (`--image`), as pinned: from the run's anchor once it launched,
    /// else from when it was accepted (so a run that failed while preparing them has them too).
    /// Absent when the submit named none, or the store keeps no event log.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub submit_images: Option<Vec<SubmittedImage>>,
    /// Where the run's submit-time images are, while it waits for them (`preparing_images`).
    /// Absent otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preparing: Option<RunPreparation>,
    /// The group-level DAG (nodes + edges) with per-node task rollups. Empty when the engine
    /// reported no graph (an engine that does not emit `run_dag`).
    pub dag: RunDag,
    /// Whether a `--resume-from` of this run passes the submit path's resume rules (finished,
    /// intermediate files kept, a Nextflow run's session recorded, a prefix to resume in) — the
    /// same check, so a client offers resume exactly when a submit would accept it. The version
    /// being launched must still be of the same workflow and engine.
    #[serde(default)]
    pub resumable: bool,
    /// A Nextflow run's resume cache, `<output_prefix>.nextflow/cache/<engine_session_id>/` (the
    /// coordinator's `cloudcache.path`, keyed by session). Absent without a prefix and a session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume_cache_uri: Option<String>,
    /// How `output_prefix` relates to this run. Absent without a prefix.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefix_kind: Option<OutputPrefixKind>,
}

/// How a run's output prefix relates to the run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum OutputPrefixKind {
    /// The run's own per-run default, `biffix/runs/<run_id>/`.
    Isolated,
    /// The per-run prefix of the run it resumed (shared with it by design).
    Resumed,
    /// A caller-chosen `--output-prefix`, possibly shared with other runs.
    Custom,
}

/// A run waiting for its submit-time images: each image and where its import is, and why the run
/// cannot launch yet.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct RunPreparation {
    pub images: Vec<WorkflowImage>,
    /// One line per image not yet runnable.
    pub not_ready: Vec<String>,
    /// When the run was submitted (epoch microseconds).
    pub since_micros: i64,
}

#[cfg(test)]
mod tests {
    /// The backticked values in the doc comment right above `field`'s declaration in this file.
    fn documented_values(field: &str) -> Vec<String> {
        let source = include_str!("runs.rs");
        let lines: Vec<&str> = source.lines().collect();
        let at = lines
            .iter()
            .position(|l| l.trim() == field)
            .unwrap_or_else(|| panic!("`{field}` is not declared in runs.rs"));
        let doc: Vec<&str> = lines[..at]
            .iter()
            .rev()
            .map(|l| l.trim())
            .take_while(|l| l.starts_with("///") || l.starts_with("#["))
            .filter(|l| l.starts_with("///"))
            .collect();
        doc.iter().rev().flat_map(|l| l.split('`').skip(1).step_by(2)).map(str::to_string).collect()
    }

    #[test]
    fn the_failure_cause_doc_lists_exactly_the_causes() {
        // `FailureItem.cause` carries `FailureCause::as_str`; its doc (and so the OpenAPI
        // description) is hand-written, so pin it to the enum: same values, same order.
        let documented = documented_values("pub cause: String,");
        let causes: Vec<String> = biffix_core::failure::FailureCause::ALL
            .iter()
            .map(|c| c.as_str().to_string())
            .collect();
        assert_eq!(documented, causes);
    }

    #[test]
    fn the_try_again_doc_lists_exactly_the_modes() {
        let documented = documented_values("pub try_again: String,");
        let modes: Vec<String> =
            biffix_core::failure::TryAgain::ALL.iter().map(|m| m.as_str().to_string()).collect();
        assert_eq!(documented, modes);
    }
}
