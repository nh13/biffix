//! Builders for the RunEvents the control plane itself records (submit, launch, cancel, rename,
//! builds): each sets the control-plane source and the dedup key its kind is keyed on.

use crate::event::{EventData, RunEvent, RunPreparing, RunSubmitted};
use crate::source::EventSource;

/// A control-plane RunEvent for `run_id`: the shared dedup key (one fact per run and kind — the
/// control-plane kinds are singletons, so a re-sent event is a no-op) and envelope.
fn control_plane_event(
    run_id: &crate::ids::RunId,
    ts: crate::time::Timestamp,
    data: EventData,
) -> RunEvent {
    control_plane_event_with(run_id, ts, data, None)
}

/// [`control_plane_event`] with a dedup discriminator `disc` for a repeatable kind (see
/// [`crate::dedup::engine`]): two facts of one kind for one run are one only when their `disc`
/// matches. `None` is the singleton key.
fn control_plane_event_with(
    run_id: &crate::ids::RunId,
    ts: crate::time::Timestamp,
    data: EventData,
    disc: Option<&str>,
) -> RunEvent {
    let dedup_key = crate::dedup::engine(
        EventSource::BiffixControlPlane.as_str(),
        run_id,
        None,
        None,
        data.kind().as_str(),
        disc,
    );
    RunEvent {
        run_id: Some(run_id.clone()),
        ..RunEvent::new(dedup_key, ts, EventSource::BiffixControlPlane, data)
    }
}

/// Build a `run_submitted` RunEvent — the provenance anchor. The control plane builds
/// the [`RunSubmitted`] payload (from [`RunSubmitted::new`] plus whichever optional fields it
/// knows) and passes the source time `ts` (when the run was accepted/created); this is the
/// persisted provenance timestamp, so it must be the real acceptance time, never a placeholder.
pub fn run_submitted(
    run_id: &crate::ids::RunId,
    ts: crate::time::Timestamp,
    anchor: RunSubmitted,
) -> RunEvent {
    control_plane_event(run_id, ts, EventData::RunSubmitted(Box::new(anchor)))
}

/// Build the `coordinator_submitted` binding event.
///
/// This is what lets the AWS coordinator stream be attributed to a run: it carries BOTH the
/// `run_id` and the coordinator's `batch_job_id`, so the fold can stitch `coordinator_*` events
/// that AWS emits with no `run_id`. `batch_job_id` is the join key the AWS stream uses;
/// `coordinator_job_id` records the same value for consumers (see
/// [`RunEvent::coordinator_job_id`]).
pub fn coordinator_submitted(
    run_id: &crate::ids::RunId,
    ts: crate::time::Timestamp,
    coordinator_job_id: &crate::ids::BatchJobId,
) -> RunEvent {
    RunEvent {
        batch_job_id: Some(coordinator_job_id.clone()),
        coordinator_job_id: Some(crate::ids::CoordinatorJobId::new(coordinator_job_id.as_str())),
        ..control_plane_event(run_id, ts, EventData::CoordinatorSubmitted {})
    }
}

/// Build a `cancel_requested` RunEvent: `actor` asked to cancel the run. It latches the run's
/// cancel flag, so the run's eventual non-success terminal (e.g. its terminated coordinator
/// exiting) folds to CANCELLED rather than FAILED. One per run (the dedup key carries no actor): a
/// repeated request is the same fact. `actor_label` is a human label for the actor, when one is
/// known.
pub fn cancel_requested(
    run_id: &crate::ids::RunId,
    ts: crate::time::Timestamp,
    actor: &str,
    actor_label: Option<&str>,
) -> RunEvent {
    control_plane_event(
        run_id,
        ts,
        EventData::CancelRequested {
            actor: actor.to_string(),
            actor_label: actor_label.map(str::to_string),
        },
    )
}

/// Build a `work_deleted` RunEvent: the retention sweep deleted the run's intermediate files. One
/// per run (the dedup key has no attempt), so a re-sent sweep result is a no-op.
pub fn work_deleted(run_id: &crate::ids::RunId, ts: crate::time::Timestamp) -> RunEvent {
    control_plane_event(run_id, ts, EventData::WorkDeleted {})
}

/// Build a `run_cancelled` RunEvent — the control plane's explicit cancellation terminal. The
/// reducer folds it to the CANCELLED state (a run may be cancelled from `submitted`/`running`).
pub fn run_cancelled(run_id: &crate::ids::RunId, ts: crate::time::Timestamp) -> RunEvent {
    control_plane_event(run_id, ts, EventData::RunCancelled {})
}

/// Build a `run_preparing` RunEvent: the run was accepted at `ts` and waits for its submit-time
/// images; the `run_submitted` anchor follows when it launches.
pub fn run_preparing(
    run_id: &crate::ids::RunId,
    ts: crate::time::Timestamp,
    preparing: RunPreparing,
) -> RunEvent {
    control_plane_event(run_id, ts, EventData::RunPreparing(Box::new(preparing)))
}

/// Build an `experiment_set` RunEvent: `actor` put the run in `experiment_id` (or, `None`, took it
/// out of its experiment) at `ts`. Repeatable: its dedup key's discriminator is `<ts
/// micros>:<target>` (the target empty for `None`), so every change is its own fact: a later one
/// even when it names an experiment the run was in before (set A, move to B, back to A), and two
/// different changes stamped with the same microsecond both land, ordered by their log `seq`. Only
/// the very same change at the very same `ts` is one fact. The key stays injective: the
/// discriminator is its last part, and `ts` is a decimal number with no `:`.
pub fn experiment_set(
    run_id: &crate::ids::RunId,
    ts: crate::time::Timestamp,
    experiment_id: Option<&str>,
    actor: &str,
) -> RunEvent {
    let disc = format!("{}:{}", ts.as_micros(), experiment_id.unwrap_or(""));
    let data = EventData::ExperimentSet {
        experiment_id: experiment_id.map(str::to_string),
        actor: Some(actor.to_string()),
    };
    control_plane_event_with(run_id, ts, data, Some(&disc))
}

/// Build a `run_renamed` RunEvent: `actor` renamed the run to `run_name` (or, `None`, cleared its
/// name) at `ts`. Repeatable like [`experiment_set`]: its dedup key's discriminator is
/// `<ts micros>:<name>`, so every rename is its own fact (a rename back to an earlier name too),
/// and only the very same rename at the very same `ts` is one fact. The discriminator is the key's
/// last part and `ts` has no `:`, so a `:` in the name keeps the key injective.
pub fn run_renamed(
    run_id: &crate::ids::RunId,
    ts: crate::time::Timestamp,
    run_name: Option<&str>,
    actor: &str,
) -> RunEvent {
    let disc = format!("{}:{}", ts.as_micros(), run_name.unwrap_or(""));
    let data = EventData::RunRenamed {
        run_name: run_name.map(str::to_string),
        actor: Some(actor.to_string()),
    };
    control_plane_event_with(run_id, ts, data, Some(&disc))
}

/// A control-plane `run_failed` stage: the run's submit-time images could not be prepared.
pub const FAILURE_STAGE_IMAGE_PREPARATION: &str = "image_preparation";
/// A control-plane `run_failed` stage: the run could not be launched (anything but its images).
pub const FAILURE_STAGE_LAUNCH: &str = "launch";

/// Build the control plane's `run_failed` RunEvent: the run could not launch, at `stage` (see
/// [`FAILURE_STAGE_IMAGE_PREPARATION`], [`FAILURE_STAGE_LAUNCH`]), for `reason`. Its dedup key
/// carries the control-plane source, so it never collides with the engine's own `run_failed`.
pub fn run_failed(
    run_id: &crate::ids::RunId,
    ts: crate::time::Timestamp,
    stage: &str,
    reason: &str,
) -> RunEvent {
    control_plane_event(
        run_id,
        ts,
        EventData::RunFailed { reason: Some(reason.to_string()), stage: Some(stage.to_string()) },
    )
}

/// Build a `build_requested` RunEvent: the control plane's record that it started build
/// `build_arn` of `subject` (right after `StartBuild` returns), with what it asked for. Not about a
/// run, so keyed on the build rather than a run id.
pub fn build_requested(
    ts: crate::time::Timestamp,
    subject: &str,
    build_arn: &str,
    request: serde_json::Value,
) -> RunEvent {
    RunEvent::new(
        crate::dedup::build_requested(build_arn),
        ts,
        EventSource::BiffixControlPlane,
        EventData::BuildRequested { subject: subject.into(), build_arn: build_arn.into(), request },
    )
}
