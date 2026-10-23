//! Reducer tests — the event-sourcing invariant and the known-hard interleavings.
//!
//! No committed data files: fixtures are built programmatically. The small logs are scrambled
//! exhaustively (every permutation), which is exactly the property the design asks for
//! (incremental == replay under reordering + duplication); `invariant::property` adds proptest over
//! a larger representative log, with arbitrary arrival order and redelivery.

use crate::common;

use biffix_core::testing::{self, RunEventExt};

use biffix_core::event::{
    EventData, LogSeq, RunEvent, RunSubmitted, TaskVerdict, TerminationCategory,
};
use biffix_core::ids::{Attempt, RunId, TaskKey};
use biffix_core::reducer::Projection;
use biffix_core::source::EventSource;
use biffix_core::state::{AttemptState, RunState, TaskState};
use biffix_core::time::Timestamp;
use common::{fold_in_order, permutations};

mod attempts;
mod coordinator;
mod experiments;
mod invariant;
mod local_rules;
mod notifications;
mod outputs;
mod preparing;
mod reconciliation;
mod verdicts;

/// A test event keyed on `(kind, ts)`, not the translator's key: fixtures that need two facts a
/// real log would collapse to one key say so where they override it.
fn ev(ts: f64, source: EventSource, data: EventData) -> RunEvent {
    testing::event(ts, source, format!("{:?}:{ts}", data.kind()), data)
}

/// The control plane's `run_submitted` anchor for `run` (named apart from the library's
/// `biffix_core::event::run_submitted` constructor, which it does not use).
fn run_anchor(ts: f64, run: &str) -> RunEvent {
    ev(
        ts,
        EventSource::BiffixControlPlane,
        EventData::RunSubmitted(Box::new(RunSubmitted {
            workflow_version_id: "wf1".into(),
            ..testing::anchor()
        })),
    )
    .with_run(run)
}

fn task_submitted(ts: f64, run: &str, task: &str, attempt: u32, job: &str) -> RunEvent {
    ev(ts, EventSource::Snakemake, EventData::TaskSubmitted { image: None })
        .with_run(run)
        .with_task(task)
        .with_attempt(attempt)
        .with_job(job)
}

fn batch(ts: f64, job: &str, data: EventData) -> RunEvent {
    ev(ts, EventSource::AwsBatch, data).with_job(job)
}

/// `e`, reported from the (container-instance) host `host`.
fn on_host(e: RunEvent, host: &str) -> RunEvent {
    e.with_instance(host)
}

fn run_level(ts: f64, run: &str, data: EventData) -> RunEvent {
    ev(ts, EventSource::Snakemake, data).with_run(run)
}

/// A run-scoped fact the control plane publishes (its kinds fold only from it).
fn control(ts: f64, run: &str, data: EventData) -> RunEvent {
    ev(ts, EventSource::BiffixControlPlane, data).with_run(run)
}

fn coord(ts: f64, run: &str, data: EventData) -> RunEvent {
    ev(ts, EventSource::AwsBatch, data).with_run(run)
}

fn run_state(p: &Projection, run: &str) -> RunState {
    p.runs.get(run).unwrap().state()
}
fn task_state(p: &Projection, run: &str, task: &str) -> TaskState {
    p.runs.get(run).unwrap().tasks.get(task).unwrap().state()
}

/// The core `projection == fold` invariant, tested for real: for EVERY arrival ordering of `log`,
/// an **incremental** fold (apply one-at-a-time in that order) yields the identical projection as a
/// single **replay** (which sorts by `(ts, seq)`). Comparing `runs` compares every attempt, every
/// monotonic flag, and the progress counters — so an order-dependent reducer (the old
/// `retry_pending`/cancel/progress bugs) makes some permutation diverge. Replay is the ground
/// truth.
fn assert_incremental_equals_replay(log: &[RunEvent]) {
    let replayed = Projection::replay(log);
    for perm in permutations(log) {
        let mut inc = Projection::new();
        for e in &perm {
            inc.apply(e);
        }
        assert_eq!(
            inc.runs, replayed.runs,
            "incremental fold diverged from replay for an arrival ordering"
        );
    }
}

fn cancel(ts: f64, run: &str, actor: &str) -> RunEvent {
    ev(
        ts,
        EventSource::BiffixControlPlane,
        EventData::CancelRequested { actor: actor.into(), actor_label: None },
    )
    .with_run(run)
}

fn task_created(ts: f64, run: &str, task: &str) -> RunEvent {
    run_level(ts, run, EventData::TaskCreated { group: None, deps: vec![] }).with_task(task)
}
fn engine_succeeded(ts: f64, run: &str, task: &str) -> RunEvent {
    // Engine-reported success for a local rule: Snakemake source, run_id + task_key, NO
    // batch_job_id.
    run_level(ts, run, EventData::TaskSucceeded { exit_code: 0, log_stream: None }).with_task(task)
}
fn engine_cached(ts: f64, run: &str, task: &str) -> RunEvent {
    // A cache-hit (Nextflow `-resume`): engine-reported, run_id + task_key, NO batch_job_id.
    run_level(ts, run, EventData::TaskCached {}).with_task(task)
}

fn output_ref_event(ts: f64, path: &str, etag: &str, size: u64) -> RunEvent {
    use biffix_core::event::OutputRef;
    let mut e = ev(
        ts,
        EventSource::Snakemake,
        EventData::TaskOutputs {
            role: None,
            reused: false,
            outputs: vec![OutputRef {
                s3_uri: "s3://b/x.txt".into(),
                path: Some(path.into()),
                etag: Some(etag.into()),
                size: Some(size),
            }],
        },
    );
    e.dedup_key = format!("outputs-{path}");
    e.with_run("r1")
}

fn retrying(ts: f64, next: u32) -> RunEvent {
    let mut e = run_level(
        ts,
        "r1",
        EventData::TaskRetrying { prev_attempt: Attempt(next - 1), next_attempt: Attempt(next) },
    )
    .with_task("align");
    e.dedup_key = format!("retrying-{next}");
    e
}

fn run_preparing(ts: f64, run: &str) -> RunEvent {
    control(
        ts,
        run,
        EventData::RunPreparing(Box::new(biffix_core::event::RunPreparing {
            workflow_version_id: "wf-prep".into(),
            params: serde_json::json!({"k": "v"}),
            input_uris: vec!["s3://b/in".into()],
            submitter: Some("user:u".into()),
            submitter_label: None,
            run_name: Some("prep".into()),
            output_prefix: Some("s3://b/out/".into()),
            resumed_from: None,
            images: vec![biffix_core::event::SubmitImage {
                declared_ref: "repo:tag".into(),
                source_ref: "repo@sha256:a".into(),
                content_key: "k".into(),
            }],
            experiment_id: None,
        })),
    )
}

/// The engine's verdict on `task`'s `attempt` (`None` ⇒ every attempt submitted by `ts`).
fn verdict(ts: f64, run: &str, task: &str, attempt: Option<u32>, v: TaskVerdict) -> RunEvent {
    let mut e = ev(
        ts,
        EventSource::Nextflow,
        EventData::EngineTaskVerdict {
            verdict: v,
            category: Some(TerminationCategory::MissingOutput),
            reason: Some("Missing output file(s) `out.txt`".into()),
        },
    );
    e.run_id = Some(RunId::new(run));
    e.task_key = Some(TaskKey::new(task));
    e.attempt = attempt.map(Attempt);
    e
}

fn aws_succeeded(ts: f64, job: &str) -> RunEvent {
    batch(
        ts,
        job,
        EventData::TaskSucceeded { exit_code: 0, log_stream: Some(format!("{job}/final")) },
    )
}

/// `run` renamed to `name` (`None`: its name cleared) at `ts`.
fn renamed(ts: f64, run: &str, name: Option<&str>) -> RunEvent {
    control(
        ts,
        run,
        EventData::RunRenamed { run_name: name.map(Into::into), actor: Some("alice".into()) },
    )
}
