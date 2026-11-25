//! The reconcile step: describe stale attempts and coordinators and enqueue the starts,
//! executions and terminals Batch reports, then reap the live children of every ended run.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::time::Duration;

use aws_sdk_batch::error::{DisplayErrorContext, ProvideErrorMetadata};
use biffix_core::clock::{micros, millis, since_epoch};
use biffix_core::event::{RunEvent, BATCH_JOB_START_KINDS};
use biffix_core::eventbridge::describe::{terminal_run_event, DescribedStartedJob};
use biffix_core::eventbridge::execution::BatchJobSnapshot;
use lambda_runtime::Error;

use super::{aws_error, enqueue_events, micros_before, note, State};
use crate::decode::{described_snapshot, described_start, described_terminal};
use crate::drain::warn_of_unidentified_executions;
use crate::reconcile::{
    described_events, forgotten_children, is_pre_terminal_fact, plan_reap, reap_fact_events,
    reap_reason, reap_terminal_event, terminals_to_send, terminate_error_is_benign, DescribedChild,
    DESCRIBE_JOBS_BATCH_SIZE,
};
use crate::store::reconcile::{
    ended_runs_with_live_children, live_children_of_run, record_describe_results,
    run_id_for_coordinator, stale_attempts, stale_coordinators, touch_described, StaleWindow,
};

/// How long an attempt may go without news before the backstop describes it.
const STALE_AFTER: Duration = Duration::from_secs(900);

/// The most stale attempts one tick describes, and the reaper's page size.
const CANDIDATE_LIMIT: i64 = 1000;

/// How far back the ended-run sweep looks for runs with live children. Long enough to retry a
/// failed terminate for hours; short enough that finished history is not rescanned every poll.
const ENDED_RUN_SWEEP_WINDOW: Duration = Duration::from_secs(6 * 3600);

/// How long DescribeJobs may leave a stale candidate out before the backstop stops asking about
/// it. Batch keeps a finished job about a week and never forgets a live one, so an omission that
/// lasts a day of polls is Batch having forgotten the job, not a transient gap.
const GIVE_UP_AFTER_MISSING: Duration = Duration::from_secs(24 * 3600);

/// How far back the stale backstop looks for bound jobs. Batch keeps a finished job about a week,
/// so describing a job bound earlier can recover its missed terminal only if it is still running
/// after more than a week; past this window it is no longer asked about, which keeps every poll
/// proportional to the window rather than to the whole log.
const STALE_JOB_WINDOW: Duration = Duration::from_secs(14 * 24 * 3600);

/// How long DescribeJobs must have left out a child of an ended run before the reaper closes it
/// with a synthetic FAILED: well past any visibility lag of a just-submitted job, and well inside
/// [`ENDED_RUN_SWEEP_WINDOW`] so the sweep is still looking at the run when it elapses.
const REAP_FORGOTTEN_AFTER: Duration = Duration::from_secs(3600);

/// A `reconcile_describe` row for a job Batch still knew is dropped once it is this old: rotation
/// re-describes every live candidate far sooner, so the job's terminal has landed since.
const DESCRIBE_FORGET_AFTER: Duration = Duration::from_secs(30 * 24 * 3600);

/// DescribeJobs for at most [`DESCRIBE_JOBS_BATCH_SIZE`] ids.
async fn describe_jobs(
    state: &State,
    ids: &[String],
) -> Result<Vec<aws_sdk_batch::types::JobDetail>, Error> {
    let result = state
        .batch
        .describe_jobs()
        .set_jobs(Some(ids.to_vec()))
        .send()
        .await
        .map_err(|error| aws_error("DescribeJobs", error))?;
    Ok(result.jobs.unwrap_or_default())
}

/// What a described `job` says of its executions ([`described_snapshot`]), for every path that
/// enqueues them, with a warning when `attempts[]` lists executions without a task ARN
/// ([`warn_of_unidentified_executions`]).
fn execution_snapshot(job: &aws_sdk_batch::types::JobDetail) -> Option<BatchJobSnapshot> {
    let snapshot = described_snapshot(job)?;
    warn_of_unidentified_executions(&snapshot, "reconcile");
    Some(snapshot)
}

/// Describe the stale attempts and coordinators, enqueue the starts, executions and terminals
/// Batch reports, then reap the live children of every ended run. Returns how many events it
/// enqueued (`emitted`: the stale path's and the reaper's starts, execution events and terminals,
/// and the reaper's synthetic FAILEDs); the poll's log line also counts the starts among them
/// (`started`, of either path) and the children terminated (`reaped`).
pub(super) async fn reconcile(state: &State) -> Result<usize, Error> {
    let now = since_epoch();
    let now_micros = micros(now);
    let poll_millis = millis(now);
    let cutoff_micros = micros_before(now, STALE_AFTER);
    let forget_before = micros_before(now, DESCRIBE_FORGET_AFTER);
    // Backstop BOTH child tasks and the coordinator: a missed coordinator terminal otherwise
    // strands the whole run at "submitted" forever (child-only reconciliation can't advance it).
    let window = StaleWindow {
        cutoff_micros,
        bound_since_micros: micros_before(now, STALE_JOB_WINDOW),
        give_up_before_micros: micros_before(now, GIVE_UP_AFTER_MISSING),
        limit: CANDIDATE_LIMIT,
    };
    let mut candidates = stale_attempts(&state.pool, window).await?;
    let coordinators = stale_coordinators(&state.pool, window).await?;
    // The control plane bound these as coordinators, whatever queue they ran on: a terminal found
    // for one is the coordinator's (and its run's children are reaped), as the fold reads it.
    let bound_coordinators: HashSet<String> =
        coordinators.iter().map(|c| c.batch_job_id.clone()).collect();
    // Union, de-duplicated by batch_job_id (a job is never both a task and a coordinator).
    let known: HashSet<String> = candidates.iter().map(|c| c.batch_job_id.clone()).collect();
    candidates.extend(coordinators.into_iter().filter(|c| !known.contains(&c.batch_job_id)));
    // The keys of the starts and execution events the log already holds, by job: a fact Batch
    // reports is enqueued only when its key is not among them, so a job that runs for hours does
    // not add duplicates to the queue every rotation, while a fact the log lacks (a Batch retry's
    // start, a later execution: each has its own key) still gets backfilled.
    let known_fact_keys: HashMap<String, Vec<String>> = candidates
        .iter_mut()
        .filter(|c| !c.known_fact_keys.is_empty())
        .map(|c| (c.batch_job_id.clone(), std::mem::take(&mut c.known_fact_keys)))
        .collect();
    let ids: Vec<String> = candidates.into_iter().map(|c| c.batch_job_id).collect();

    let mut totals = PollTotals::default();
    // Coordinators found terminal this poll; after enqueuing terminals we reap their live children
    // so a crashed coordinator does not strand orphaned, still-billing Batch jobs.
    let mut terminal_coordinators: Vec<String> = Vec::new();
    // A describe/enqueue failure in one chunk must not abort the poll: record the first error, keep
    // going, run the reaps, then surface it so the Lambda reports the failure (every append and
    // enqueue is idempotent, and the next poll retries what failed).
    let mut first_error: Option<Error> = None;
    for chunk in ids.chunks(DESCRIBE_JOBS_BATCH_SIZE) {
        let jobs = match describe_jobs(state, chunk).await {
            Ok(jobs) => jobs,
            Err(error) => {
                tracing::warn!(error = %error, "reconcile: DescribeJobs chunk failed (will retry)");
                note(&mut first_error, error);
                // Rotate the chunk to the back, so a request Batch always rejects does not pin
                // these candidates at the head of every poll.
                if let Err(error) = touch_described(&state.pool, chunk, now_micros).await {
                    note(&mut first_error, error.into());
                }
                continue;
            }
        };
        let present: BTreeSet<String> =
            jobs.iter().filter_map(|job| job.job_id().map(str::to_owned)).collect();
        let missing: Vec<String> =
            chunk.iter().filter(|id| !present.contains(*id)).cloned().collect();
        let present: Vec<String> = present.into_iter().collect();
        if let Err(error) =
            record_describe_results(&state.pool, &present, &missing, now_micros, forget_before)
                .await
        {
            note(&mut first_error, error.into());
        }

        let (mut facts, mut terminals) = (Vec::new(), Vec::new());
        for job in &jobs {
            let Some(id) = job.job_id() else { continue };
            let bound = bound_coordinators.contains(id);
            let terminal = described_terminal(job, &state.queue_context, bound, poll_millis);
            let start = described_start(job, &state.queue_context, bound);
            let snapshot = execution_snapshot(job);
            if let Some(terminal) =
                terminal.as_ref().filter(|terminal| terminal.facts.is_coordinator)
            {
                terminal_coordinators.push(terminal.batch_job_id.clone());
            }
            let known_keys = known_fact_keys.get(id).map_or(&[][..], Vec::as_slice);
            let events =
                described_events(start.as_ref(), snapshot.as_ref(), terminal.as_ref(), known_keys);
            for event in events {
                if is_pre_terminal_fact(&event) {
                    facts.push(event);
                } else {
                    terminals.push(event);
                }
            }
        }
        let sent = enqueue_facts_then_terminals(state, &facts, terminals, &mut first_error).await;
        totals.emitted += sent.events;
        totals.started += sent.starts;
    }

    // Targeted reap: the live children of every coordinator whose missed terminal was
    // found above — reaped regardless of whether some enqueue failed (the coordinator is confirmed
    // dead). The ended-run sweep below would reach the same run on a later poll once that terminal
    // is folded; reaping here saves that poll's latency, and still covers a coordinator whose
    // terminal is older than the sweep window (a reconciler outage longer than it).
    for coordinator_job_id in &terminal_coordinators {
        let outcome = reap_orphaned_children(state, coordinator_job_id).await;
        if let Some(error) = totals.add_reap(outcome) {
            tracing::warn!(coordinator = coordinator_job_id, error = %error, "reap failed (will retry next poll)");
            note(&mut first_error, error);
        }
    }

    // General sweep: a coordinator that ended normally — cancelled via biffix, or failed with an
    // executor that leaves its children running (Nextflow does on SIGTERM) — has its terminal
    // delivered by EventBridge and never becomes stale, so its live children would bill to
    // completion. Reap every recently-ended run that still has live children; a run is retried on
    // every poll while it is inside the window and still has any.
    let since_micros = micros_before(now, ENDED_RUN_SWEEP_WINDOW);
    match ended_runs_with_live_children(&state.pool, since_micros, CANDIDATE_LIMIT).await {
        Ok(runs) => {
            for (run_id, coordinator_job_id) in &runs {
                let outcome = reap_run_children(state, run_id, coordinator_job_id).await;
                if let Some(error) = totals.add_reap(outcome) {
                    tracing::warn!(run_id, error = %error, "ended-run reap failed (will retry next poll)");
                    note(&mut first_error, error);
                }
            }
        }
        Err(error) => {
            tracing::warn!(error = %error, "ended-run sweep query failed (will retry next poll)");
            note(&mut first_error, error.into());
        }
    }

    let PollTotals { emitted, started, reaped } = totals;
    tracing::info!(
        candidates = ids.len(),
        emitted,
        started,
        reaped,
        "reconciliation poll complete"
    );
    first_error.map_or(Ok(emitted), Err)
}

/// A poll's running counts, for its log line and its result.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct PollTotals {
    /// Events SQS accepted: starts, execution events, terminals and the reaper's synthetic
    /// FAILEDs.
    emitted: usize,
    /// The starts among them.
    started: usize,
    /// Children terminated.
    reaped: usize,
}

impl PollTotals {
    /// Add what one reap did, then hand back its error for the caller to log and keep. The counts
    /// are added even when the reap also failed: what SQS accepted and Batch terminated before the
    /// error still happened.
    fn add_reap(&mut self, (reap, error): (Reaped, Option<Error>)) -> Option<Error> {
        self.reaped += reap.terminated;
        self.emitted += reap.enqueued;
        self.started += reap.started;
        error
    }
}

/// What [`enqueue_facts_then_terminals`] got SQS to accept.
#[derive(Clone, Copy, Debug, Default)]
struct Sent {
    /// Events accepted: starts, execution events and terminals.
    events: usize,
    /// The starts among them (a job's start, not an execution's).
    starts: usize,
}

/// Enqueue one describe's events: its pre-terminal `facts` first (starts and execution events,
/// [`is_pre_terminal_fact`]), then the `terminals` [`terminals_to_send`] keeps once SQS has
/// answered for the facts. SendMessageBatch is not atomic, so a terminal sent with its job's start
/// or execution events could be logged while one of them is rejected, and the job would then stop
/// being a candidate with that fact never retried; a terminal whose job had a fact rejected is
/// held back instead, and a later poll retries them all. A failure is noted in `first_error` (the
/// rest still go; every enqueue is idempotent).
async fn enqueue_facts_then_terminals(
    state: &State,
    facts: &[RunEvent],
    terminals: Vec<RunEvent>,
    first_error: &mut Option<Error>,
) -> Sent {
    let sent_facts = enqueue_events(state, facts).await;
    let starts = accepted_starts(facts, &sent_facts.failed);
    let planned = terminals.len();
    let terminals = terminals_to_send(facts, &sent_facts.failed, terminals);
    let held = planned - terminals.len();
    let sent_terminals = enqueue_events(state, &terminals).await;
    for error in [sent_facts.error, sent_terminals.error].into_iter().flatten() {
        tracing::warn!(error = %error, held, "reconcile: enqueue failed (will retry next poll)");
        note(first_error, error);
    }
    Sent { events: sent_facts.sent + sent_terminals.sent, starts }
}

/// How many of `facts` are a job's start that SQS accepted (`failed` holds the indices of the
/// ones it did not). The poll's `started` count means jobs seen starting, so the execution events
/// sent beside the starts are not counted in it.
fn accepted_starts(facts: &[RunEvent], failed: &BTreeSet<usize>) -> usize {
    facts
        .iter()
        .enumerate()
        .filter(|(index, fact)| {
            !failed.contains(index) && BATCH_JOB_START_KINDS.contains(&fact.kind())
        })
        .count()
}

/// What one reap of a run's live children did, kept when the reap also failed.
#[derive(Clone, Copy, Debug, Default)]
struct Reaped {
    /// Children terminated.
    terminated: usize,
    /// Events enqueued: the children's starts and execution events, recovered terminals and
    /// synthetic FAILEDs.
    enqueued: usize,
    /// The starts among them.
    started: usize,
}

/// Reap the still-live child jobs of a coordinator found terminal this poll. Returns what it did
/// and its first error, as [`reap_run_children`] does.
async fn reap_orphaned_children(
    state: &State,
    coordinator_job_id: &str,
) -> (Reaped, Option<Error>) {
    match run_id_for_coordinator(&state.pool, coordinator_job_id).await {
        Ok(Some(run_id)) => reap_run_children(state, &run_id, coordinator_job_id).await,
        Ok(None) => (Reaped::default(), None),
        Err(error) => (Reaped::default(), Some(error.into())),
    }
}

/// Reap every still-"live" child job of `run_id` (whose coordinator is `coordinator_job_id`).
///
/// Each page of children is described first, and what happens to a child follows Batch's own
/// answer ([`plan_reap`]): a child Batch reports finished gets its real terminal enqueued (a
/// SUCCEEDED the log missed stays a success, and a real FAILED keeps its exit code and reason); a
/// child still active is terminated, and its real FAILED arrives through EventBridge or a later
/// poll's describe; a child Batch has omitted for [`REAP_FORGOTTEN_AFTER`] gets a synthetic FAILED
/// ([`reap_terminal_event`]), since no real terminal can come. Nothing is ever fabricated for a
/// job Batch still knows. A described child's start and its execution events, when Batch reports
/// one the log lacks (its dedup key is not among the child's
/// [`LiveChild::known_fact_keys`](crate::store::reconcile::LiveChild), as the stale path filters),
/// are enqueued too ([`reap_fact_events`]), ahead of the terminals
/// ([`enqueue_facts_then_terminals`]), so a recovered terminal keeps its run time (and the attempt
/// its price) and its executions; a fact the log already holds is not enqueued again on each
/// poll. A synthetic FAILED comes with no execution, since Batch reported none.
///
/// Returns what it terminated and enqueued together with its first error: errors are collected so
/// one child or chunk never stops the rest, and a failed page query ends the paging but keeps what
/// the earlier pages did, since those enqueues and terminations already happened.
async fn reap_run_children(
    state: &State,
    run_id: &str,
    coordinator_job_id: &str,
) -> (Reaped, Option<Error>) {
    let now = since_epoch();
    let now_micros = micros(now);
    let poll_millis = millis(now);
    let forgotten_before = micros_before(now, REAP_FORGOTTEN_AFTER);
    let forget_before = micros_before(now, DESCRIBE_FORGET_AFTER);
    let mut first_error: Option<Error> = None;
    let (mut terminated, mut recovered, mut forgotten) = (0usize, 0usize, 0usize);
    let (mut enqueued, mut started) = (0usize, 0usize);
    // Page through ALL live children by `batch_job_id` keyset, not just the first
    // `CANDIDATE_LIMIT`: what this enqueues is not folded synchronously, so a re-query would return
    // the same still-"live" children; only the cursor makes progress within a poll.
    let reason = reap_reason(coordinator_job_id);
    let mut after: Option<String> = None;
    loop {
        let children = match live_children_of_run(
            &state.pool,
            run_id,
            after.as_deref(),
            CANDIDATE_LIMIT,
        )
        .await
        {
            Ok(children) => children,
            Err(error) => {
                note(&mut first_error, error.into());
                break;
            }
        };
        if children.is_empty() {
            break;
        }
        for chunk in children.chunks(DESCRIBE_JOBS_BATCH_SIZE) {
            let ids: Vec<String> = chunk.iter().map(|child| child.batch_job_id.clone()).collect();
            // Each child of the page, with the keys of the starts and execution events the log
            // holds for it.
            let known_fact_keys: HashMap<String, Vec<String>> = chunk
                .iter()
                .map(|child| (child.batch_job_id.clone(), child.known_fact_keys.clone()))
                .collect();
            let jobs = match describe_jobs(state, &ids).await {
                Ok(jobs) => jobs,
                Err(error) => {
                    // Never terminate or close a child whose state is unknown; the next poll
                    // retries.
                    note(&mut first_error, error);
                    continue;
                }
            };
            let described: Vec<DescribedChild> = jobs
                .iter()
                .filter_map(|job| {
                    let id = job.job_id()?.to_owned();
                    // A run's live children are its task-bound jobs.
                    Some((id, described_terminal(job, &state.queue_context, false, poll_millis)))
                })
                .collect();
            let present: Vec<String> = described.iter().map(|(id, _)| id.clone()).collect();
            // The children's starts, sent ahead of the terminals: a terminal the reaper recovers
            // otherwise leaves its attempt with a host and an end but no start, so it is never
            // priced.
            let described_starts: Vec<DescribedStartedJob> = jobs
                .iter()
                .filter_map(|job| described_start(job, &state.queue_context, false))
                .collect();
            // And their executions, for the same reason a stale job's go ahead of its terminal:
            // once the terminal is logged the child is no longer described.
            let snapshots: Vec<BatchJobSnapshot> =
                jobs.iter().filter_map(execution_snapshot).collect();
            let facts = reap_fact_events(&described_starts, &snapshots, &known_fact_keys);
            let plan = plan_reap(&ids, described);
            let mut terminals: Vec<RunEvent> =
                plan.terminals.iter().map(terminal_run_event).collect();
            recovered += plan.terminals.len();
            for child in &plan.terminate {
                match state.batch.terminate_job().job_id(child).reason(&reason).send().await {
                    Ok(_) => terminated += 1,
                    Err(error) if terminate_error_is_benign(error.code(), error.message()) => {
                        tracing::info!(child, error = %DisplayErrorContext(&error), "reap: child already gone");
                    }
                    Err(error) => {
                        let error = aws_error(&format!("TerminateJob {child}"), error);
                        tracing::warn!(child, error = %error, "reap: terminate_job failed (will retry next poll)");
                        note(&mut first_error, error);
                    }
                }
            }
            match record_describe_results(
                &state.pool,
                &present,
                &plan.missing,
                now_micros,
                forget_before,
            )
            .await
            {
                Ok(missing_since) => {
                    for child in forgotten_children(&plan.missing, &missing_since, forgotten_before)
                    {
                        terminals.push(reap_terminal_event(
                            &child,
                            coordinator_job_id,
                            poll_millis,
                        ));
                        forgotten += 1;
                    }
                }
                Err(error) => note(&mut first_error, error.into()),
            }
            let sent =
                enqueue_facts_then_terminals(state, &facts, terminals, &mut first_error).await;
            enqueued += sent.events;
            started += sent.starts;
        }
        // Advance the keyset cursor past this page; a short page means we reached the end.
        let page_len = children.len() as i64;
        after = children.into_iter().last().map(|child| child.batch_job_id);
        if page_len < CANDIDATE_LIMIT {
            break;
        }
    }
    if terminated + recovered + forgotten + started > 0 {
        tracing::info!(
            run_id,
            coordinator = coordinator_job_id,
            terminated,
            recovered,
            forgotten,
            started,
            "reaped the live children of an ended run"
        );
    }
    (Reaped { terminated, enqueued, started }, first_error)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_reap_keeps_the_counts_of_a_reap_that_also_failed() {
        let mut totals = PollTotals { emitted: 3, started: 1, reaped: 0 };
        let reap = Reaped { terminated: 2, enqueued: 5, started: 4 };

        let error = totals.add_reap((reap, Some(Error::from("SQS rejected an entry"))));

        assert_eq!(totals, PollTotals { emitted: 8, started: 5, reaped: 2 });
        assert_eq!(error.map(|error| error.to_string()).as_deref(), Some("SQS rejected an entry"));
    }

    #[test]
    fn add_reap_adds_a_clean_reap_and_returns_no_error() {
        let mut totals = PollTotals::default();

        let error = totals.add_reap((Reaped { terminated: 1, enqueued: 2, started: 1 }, None));

        assert_eq!(totals, PollTotals { emitted: 2, started: 1, reaped: 1 });
        assert!(error.is_none());
    }

    #[test]
    fn only_an_accepted_job_start_counts_as_started() {
        use biffix_core::event::EventData;
        use biffix_core::source::EventSource;
        use biffix_core::testing::{event, execution_ended, execution_task_arn, RunEventExt};
        let start = |job: &str| {
            let data = EventData::TaskStarted { started_at: None, log_stream: None };
            event(1.0, EventSource::AwsBatch, format!("aws-batch:{job}:RUNNING"), data)
                .with_job(job)
        };
        let facts = [
            start("job-1"),
            execution_ended("job-1", &execution_task_arn(1), Some(3.0), 684.0, None),
            start("job-2"),
            start("job-3"),
        ];

        // An execution event sent beside the starts is not a job starting.
        assert_eq!(accepted_starts(&facts, &BTreeSet::new()), 3);
        // Nor is a start SQS rejected (job-2's); the rejected execution event changes nothing.
        assert_eq!(accepted_starts(&facts, &BTreeSet::from([1, 2])), 2);
        assert_eq!(accepted_starts(&[], &BTreeSet::new()), 0);
    }
}
