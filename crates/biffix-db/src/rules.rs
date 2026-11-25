//! SQL rules that more than one crate's queries apply, defined once so they cannot drift apart:
//! what counts as a job's authoritative terminal (and its start), and which jobs and executions the
//! pricer can still price (the reconcile tick's `unpriced_jobs` and `unpriced_executions` pick them
//! up; the backend counts them as cost "pending").
//!
//! A job is priced one of two ways. A job with an execution that ran on a host of a known type
//! ([`prices_by_execution`]) is priced per execution, each on its own host (`execution_cost`). Any
//! other job is priced the legacy way, as one job-level line over its span (`attempt_cost`).
//!
//! Every function returns an SQL fragment built only from code constants and the column or
//! placeholder names its caller passes (never a runtime value), so splicing it into a query with
//! `format!` is injection-safe.

use std::time::Duration;

use biffix_core::event::{
    Kind, BATCH_JOB_PRE_TERMINAL_KINDS, BATCH_JOB_START_KINDS, COORDINATOR_TERMINAL_KINDS,
    TASK_TERMINAL_KINDS,
};
use biffix_core::source::EventSource;
use biffix_core::state::RunState;

/// How far back the pricer picks up an unpriced finished job. EC2 keeps 90 days of spot price
/// history, and a spot job's lookup reaches a day before the job started (see the reconcile tick's
/// spot-history lookup in `biffix_ingest`'s `tick/pricing.rs`); 85 days leaves that day plus a few days of slack, so a job picked up near the
/// boundary still finds its history. A job that finished earlier is never priced (the backend's
/// "pending" count reads this too, so a job that can no longer be priced is not shown as pending).
pub const PRICING_WINDOW: Duration = Duration::from_secs(85 * 24 * 3600);

/// The canonical strings of `kinds` ([`Kind::as_str`]), bound as a `text[]` for a
/// `kind = ANY($n)` (such as a [`batch_fact_predicate`]'s).
pub fn kind_strings(kinds: &[Kind]) -> Vec<String> {
    kinds.iter().map(|kind| kind.as_str().to_owned()).collect()
}

/// The kinds of a Batch job's terminal fact, of either shape ([`kind_strings`] of
/// `TASK_TERMINAL_KINDS` and `COORDINATOR_TERMINAL_KINDS`), for a [`batch_fact_predicate`]. The
/// translator shapes a job's events as a task's or a coordinator's by its queue
/// (`BIFFIX_COORDINATOR_QUEUES`), while the fold reads them by the job's binding, so a coordinator
/// on a queue the deploy does not list ends task-shaped (and a task on a listed one
/// coordinator-shaped). Either way the job has ended.
pub fn batch_job_terminal_kinds() -> Vec<String> {
    kind_strings(&[TASK_TERMINAL_KINDS, COORDINATOR_TERMINAL_KINDS].concat())
}

/// [`kind_strings`] of `BATCH_JOB_START_KINDS` (a Batch job's start fact, of either shape, as
/// [`batch_job_terminal_kinds`]), for a [`batch_fact_predicate`].
pub fn batch_job_start_kinds() -> Vec<String> {
    kind_strings(&BATCH_JOB_START_KINDS)
}

/// The kinds of the facts Batch reports about a job before its terminal, which the reconcile
/// backstop enqueues once each: [`kind_strings`] of `BATCH_JOB_PRE_TERMINAL_KINDS` (the job's
/// start, of either shape, as [`batch_job_terminal_kinds`], and each execution's start and end),
/// for a [`batch_fact_predicate`]. The backstop reads the dedup keys the log holds under these
/// kinds, and enqueues a described fact only when its key is not among them.
pub fn batch_job_known_fact_kinds() -> Vec<String> {
    kind_strings(&BATCH_JOB_PRE_TERMINAL_KINDS)
}

/// An authoritative AWS Batch fact: an [`EventSource::AwsBatch`] event whose kind is one of the
/// array bound at `kinds_param` ([`batch_job_terminal_kinds`] for a terminal,
/// [`batch_job_start_kinds`] for a start, [`batch_job_known_fact_kinds`] for the facts the
/// backstop deduplicates: a start, or an execution's start or end). An engine-reported fact never
/// counts: an engine's terminal is never Batch's authoritative exit, nor its start Batch's. Bound
/// to the shared kind lists, the terminal test cannot drift from the fold's own terminal logic
/// (`AttemptState::is_terminal`), whichever shape the translator gave the fact. `alias` qualifies
/// the columns for a correlated subquery.
pub fn batch_fact_predicate(alias: Option<&str>, kinds_param: &str) -> String {
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

/// Which kind of Batch job a job is: `attempt_cost.role` and `execution_cost.role`, which the
/// reconcile tick's pricer writes and the backend's cost reads compare, and
/// `execution_projection.role`, which the fold writes. Changing a value changes every table's
/// meaning.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobRole {
    /// A task attempt (a child job the engine submitted).
    Task,
    /// A run's coordinator job.
    Coordinator,
}

impl JobRole {
    /// The `attempt_cost.role`, `execution_cost.role` and `execution_projection.role` value.
    pub fn as_str(self) -> &'static str {
        match self {
            JobRole::Task => "task",
            JobRole::Coordinator => "coordinator",
        }
    }

    /// The role a stored value names: a coordinator's, else a task's.
    pub fn of(value: &str) -> JobRole {
        if value == JobRole::Coordinator.as_str() {
            JobRole::Coordinator
        } else {
            JobRole::Task
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

/// The job `job` of run `run` (SQL expressions) has no legacy job-level cost (`attempt_cost`) and
/// is not recorded as one that can never be priced (`unpriceable_job`).
pub fn not_yet_priced(run: &str, job: &str) -> String {
    format!(
        "NOT EXISTS (SELECT 1 FROM attempt_cost c \
              WHERE c.run_id = {run} AND c.batch_job_id = {job}) \
         AND NOT EXISTS (SELECT 1 FROM unpriceable_job u \
              WHERE u.run_id = {run} AND u.batch_job_id = {job})"
    )
}

/// The `unpriceable_execution.reason` of an ended execution with no start recorded. It records
/// why the execution has no cost, and never keeps it from being priced: an execution with no start
/// is not priced anyway ([`execution_to_price`]), and one whose start the fold fills in later is
/// priced then (its record no longer matches it, [`priced_from`], and the pricer drops it).
pub const START_UNKNOWN: &str = "start_unknown";

/// The `execution_cost` or `unpriceable_execution` row aliased `row` was decided from the current
/// inputs of the `execution_projection` row aliased `x`: its host, queue, start and stop. The fold
/// can change them after the pricer has read them (a later fact, or a replay after a reducer fix),
/// and a row decided from other inputs no longer counts, so the execution is owed a line again.
pub fn priced_from(row: &str, x: &str) -> String {
    ["instance_id", "queue", "started_at", "stopped_at"]
        .map(|column| format!("{row}.{column} IS NOT DISTINCT FROM {x}.{column}"))
        .join(" AND ")
}

/// The `execution_projection` row aliased `x` has no cost (`execution_cost`) and is not recorded as
/// one that can never be priced (`unpriceable_execution`, other than [`START_UNKNOWN`]), from its
/// current inputs ([`priced_from`]): a line or record decided from inputs the fold has since
/// changed does not count. A line also records the execution's role, which readers classify it
/// by, so a line whose role the fold has since changed (a replay that rebinds its job as the run's
/// coordinator, or back) does not count either; a record has no role, as no reader reads one by it.
pub fn execution_not_yet_priced(x: &str) -> String {
    format!(
        "NOT EXISTS (SELECT 1 FROM execution_cost xc \
              WHERE xc.run_id = {x}.run_id AND xc.execution_id = {x}.execution_id \
                AND {cost_current} AND xc.role = {x}.role) \
         AND NOT EXISTS (SELECT 1 FROM unpriceable_execution xu \
              WHERE xu.run_id = {x}.run_id AND xu.execution_id = {x}.execution_id \
                AND xu.reason <> '{START_UNKNOWN}' AND {record_current})",
        cost_current = priced_from("xc", x),
        record_current = priced_from("xu", x),
    )
}

/// The execution aliased `x` ran: it ended, and its start is known.
fn ran(x: &str) -> String {
    format!("{x}.ended AND {x}.started_at IS NOT NULL")
}

/// The execution aliased `x`, on the `host_registry` row aliased `h` (its `instance_id`'s), is one
/// the pricer still owes a line: it ran ([`ran`]), on a host of a known type, stopped at or after
/// `window` ([`priceable`]), and has no cost from its current inputs
/// ([`execution_not_yet_priced`]). The pricer's execution queue and the backend's pending count
/// both apply it.
pub fn execution_to_price(x: &str, h: &str, window: &str) -> String {
    format!(
        "{ran} AND {priceable} AND {unpriced}",
        ran = ran(x),
        priceable = priceable(h, &format!("{x}.stopped_at"), window),
        unpriced = execution_not_yet_priced(x),
    )
}

/// The job `job` of run `run` (SQL expressions) is priced per execution: it has an execution that
/// ran ([`ran`]) on a host the registry knows the instance type of, so a run time on a host of its
/// own. Such a job's executions are priced ([`execution_to_price`]) and its legacy job-level line
/// is not; any other job (no execution facts, only a started execution, only ends with no start,
/// or only executions on hosts of unknown type) is priced the legacy way ([`not_yet_priced`]). The
/// pricer and the backend's pending count apply the same test, so a job is never left to neither.
pub fn prices_by_execution(run: &str, job: &str) -> String {
    format!(
        "EXISTS (SELECT 1 FROM execution_projection ex \
              JOIN host_registry eh ON eh.container_instance_id = ex.instance_id \
              WHERE ex.run_id = {run} AND ex.batch_job_id = {job} AND {ran} \
                AND eh.instance_type IS NOT NULL)",
        ran = ran("ex"),
    )
}

/// The run time and queue, for the legacy job-level path, of the coordinator job `job` (an SQL
/// expression: its run's `coordinator_job_id`, the control plane's binding) from its own AWS-Batch
/// facts, as a subquery for a `LATERAL` join with columns `started` (its first start), `finished`
/// (its newest terminal), both epoch micros or NULL, and `queue` (its newest). Facts of either
/// shape count, as for [`batch_job_terminal_kinds`]: the fold reads the job by its binding, not by
/// the shape the translator gave its events. The pricer prices a coordinator from this window, and
/// the backend counts it as pending by the same one, so a coordinator the pricer can never price
/// (its terminal never reached the log) is not pending forever.
pub fn coordinator_window(job: &str) -> String {
    let kinds = |kinds: &[Kind]| {
        kinds.iter().map(|kind| format!("'{}'", kind.as_str())).collect::<Vec<_>>().join(", ")
    };
    let started = kinds(&BATCH_JOB_START_KINDS);
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

#[cfg(test)]
mod tests {
    use super::JobRole;

    #[test]
    fn a_role_reads_back_from_its_stored_value() {
        for role in [JobRole::Task, JobRole::Coordinator] {
            assert_eq!(JobRole::of(role.as_str()), role);
        }
        assert_eq!(JobRole::of("anything else"), JobRole::Task);
    }
}
