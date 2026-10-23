//! SQL rules that more than one crate's queries apply, defined once so they cannot drift apart:
//! what counts as a job's authoritative terminal, and which jobs the pricer can still price (the
//! reconcile tick's `unpriced_jobs` picks them up; the backend counts them as cost "pending").
//!
//! Every function returns an SQL fragment built only from code constants and the column or
//! placeholder names its caller passes (never a runtime value), so splicing it into a query with
//! `format!` is injection-safe.

use std::time::Duration;

use biffix_core::event::{Kind, COORDINATOR_TERMINAL_KINDS, TASK_TERMINAL_KINDS};
use biffix_core::source::EventSource;
use biffix_core::state::RunState;

/// How far back the pricer picks up an unpriced finished job. EC2 keeps 90 days of spot price
/// history, and a spot job's lookup reaches a day before the job started (see the reconcile tick's
/// spot-history lookup in `biffix_ingest`'s `tick/pricing.rs`); 85 days leaves that day plus a few days of slack, so a job picked up near the
/// boundary still finds its history. A job that finished earlier is never priced (the backend's
/// "pending" count reads this too, so a job that can no longer be priced is not shown as pending).
pub const PRICING_WINDOW: Duration = Duration::from_secs(85 * 24 * 3600);

/// The canonical strings of `kinds` ([`Kind::as_str`]), bound as a `text[]` for a
/// `kind = ANY($n)` (such as a [`batch_terminal_predicate`]'s).
pub fn kind_strings(kinds: &[Kind]) -> Vec<String> {
    kinds.iter().map(|kind| kind.as_str().to_owned()).collect()
}

/// The kinds of a Batch job's terminal fact, of either shape ([`kind_strings`] of
/// `TASK_TERMINAL_KINDS` and `COORDINATOR_TERMINAL_KINDS`), for a [`batch_terminal_predicate`]. The
/// translator shapes a job's events as a task's or a coordinator's by its queue
/// (`BIFFIX_COORDINATOR_QUEUES`), while the fold reads them by the job's binding, so a coordinator
/// on a queue the deploy does not list ends task-shaped (and a task on a listed one
/// coordinator-shaped). Either way the job has ended.
pub fn batch_job_terminal_kinds() -> Vec<String> {
    kind_strings(&[TASK_TERMINAL_KINDS, COORDINATOR_TERMINAL_KINDS].concat())
}

/// The authoritative terminal predicate: an AWS-Batch fact whose kind is one of the array bound at
/// `kinds_param` ([`batch_job_terminal_kinds`]). Built from the shared terminal lists and
/// [`EventSource::AwsBatch`] so it cannot drift from the fold's own terminal logic
/// (`AttemptState::is_terminal`), whichever shape the translator gave the fact, and every terminal
/// test shares one rule: an engine-reported terminal is never Batch's authoritative exit. `alias`
/// qualifies the columns for a correlated subquery.
pub fn batch_terminal_predicate(alias: Option<&str>, kinds_param: &str) -> String {
    let column = |name: &str| match alias {
        Some(alias) => format!("{alias}.{name}"),
        None => name.to_owned(),
    };
    format!(
        "{} = '{}' AND {} = ANY({kinds_param}::text[])",
        column("source"),
        EventSource::AwsBatch.as_str(),
        column("kind"),
    )
}

/// Which kind of Batch job a priced job is: `attempt_cost.role`, which the reconcile tick's pricer
/// writes and the backend's cost reads compare.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobRole {
    /// A task attempt (a child job the engine submitted).
    Task,
    /// A run's coordinator job.
    Coordinator,
}

impl JobRole {
    /// The `attempt_cost.role` value.
    pub fn as_str(self) -> &'static str {
        match self {
            JobRole::Task => "task",
            JobRole::Coordinator => "coordinator",
        }
    }
}

/// The `run_projection.state` values of an ended run ([`RunState::is_terminal`]), bound as a
/// `text[]` for a `state = ANY($n)`.
pub fn ended_run_states() -> Vec<String> {
    RunState::ALL
        .into_iter()
        .filter(|state| state.is_terminal())
        .map(|state| state.as_str().to_owned())
        .collect()
}

/// The job `job` of run `run` (SQL expressions) has no estimated cost (`attempt_cost`) and is not
/// recorded as one that can never be priced (`unpriceable_job`).
pub fn not_yet_priced(run: &str, job: &str) -> String {
    format!(
        "NOT EXISTS (SELECT 1 FROM attempt_cost c \
              WHERE c.run_id = {run} AND c.batch_job_id = {job}) \
         AND NOT EXISTS (SELECT 1 FROM unpriceable_job u \
              WHERE u.run_id = {run} AND u.batch_job_id = {job})"
    )
}

/// The run time and queue of the coordinator job `job` (an SQL expression: its run's
/// `coordinator_job_id`, the control plane's binding) from its own AWS-Batch facts, as a subquery
/// for a `LATERAL` join with columns `started` (its first start), `finished` (its newest terminal),
/// both epoch micros or NULL, and `queue` (its newest). Facts of either shape count, as for
/// [`batch_job_terminal_kinds`]: the fold reads the job by its binding, not by the shape the
/// translator gave its events. The pricer prices a coordinator from this window, and the backend
/// counts it as pending by the same one, so a coordinator the pricer can never price (its terminal
/// never reached the log) is not pending forever.
pub fn coordinator_window(job: &str) -> String {
    let kinds = |kinds: &[Kind]| {
        kinds.iter().map(|kind| format!("'{}'", kind.as_str())).collect::<Vec<_>>().join(", ")
    };
    let started = kinds(&[Kind::CoordinatorRunning, Kind::TaskStarted]);
    let terminal = kinds(&[TASK_TERMINAL_KINDS, COORDINATOR_TERMINAL_KINDS].concat());
    format!(
        "SELECT MIN(ts_micros) FILTER (WHERE kind IN ({started})) AS started, \
                MAX(ts_micros) FILTER (WHERE kind IN ({terminal})) AS finished, \
                (array_agg(payload->>'queue' ORDER BY ts_micros DESC, seq DESC) \
                   FILTER (WHERE payload->>'queue' IS NOT NULL))[1] AS queue \
           FROM run_events \
          WHERE batch_job_id = {job} AND source = '{aws_batch}'",
        aws_batch = EventSource::AwsBatch.as_str(),
    )
}

/// A finished job the pricer can price: it ran on a `host_registry` row (`host`, an alias) that
/// knows its instance type, and it finished (`finished`, epoch micros) at or after `window`, the
/// [`PRICING_WINDOW`]'s start. A job that never got a host, ran on one that predates the registry,
/// or finished before the window can never be priced. Callers that price from a run time also
/// require its start.
pub fn priceable(host: &str, finished: &str, window: &str) -> String {
    format!(
        "{host}.instance_type IS NOT NULL AND {finished} IS NOT NULL AND {finished} >= {window}"
    )
}
