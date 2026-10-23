//! Launching a run once its images are runnable: the tail of submit, shared by the submit handler
//! (a run whose images are all runnable launches at once) and the sweep that launches a run parked
//! while its submit-time images are imported and derived.
//!
//! - [`LaunchPlan`]: the launch request as the handler settled it (params unredacted), everything
//!   [`crate::runs::submit::SubmitRequest`] needs except what is re-resolved at launch (the version
//!   record, its images and default-image digest, the coordinator variant, the image map). It is
//!   also the persisted `pending_launch` format (see its docs).
//! - [`ensure_submit_images`]: move a run's submit-time imports and derives forward.
//! - [`readiness`]: whether a run's submit-time images let it launch now, wait, or never.
//! - [`runnable_default`]: the digest tasks run for the version's default image.
//! - [`launch_run`]: write the image map, run the submit pipeline, and record the anchor and the
//!   coordinator binding (stopping the coordinator if they cannot be recorded).
//! - [`sweep`]: move each parked run's images forward and launch the run once they are runnable
//!   (stopping first any coordinator a launch whose answer was lost left running), or fail it with
//!   why they never will be (or once it has waited [`IMAGE_PREPARE_TIMEOUT`], whatever it waited
//!   on, a step that keeps failing included).

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use biffix_core::event::{
    InputTable, ReferenceUse, Retention, SubmitImage, FAILURE_STAGE_IMAGE_PREPARATION,
    FAILURE_STAGE_LAUNCH,
};
use biffix_core::ids::RunId;

use biffix_core::state::RunState;
use biffix_core::time::Timestamp;

use crate::aws::{batch::LaunchVariant, AwsError};
use crate::images::build_follow::BuildError;
use crate::images::variants::NotRunnable;
use crate::images::{ImageStatus, VersionContext};
use crate::register::workflow::{WorkflowSource, WorkflowVersion};
use crate::runs::submit::{SubmitError, SubmitOutcome, SubmitRequest};
use crate::store::pending::PendingLaunch;
use crate::store::registry::VersionImage;
use crate::store::StoreError;
use crate::AppState;

/// How long a run may wait for its submit-time images (from when it was parked) before it fails:
/// an import and a derive (two hours each at most), queueing, and one retry.
pub(crate) const IMAGE_PREPARE_TIMEOUT: Duration = Duration::from_secs(6 * 60 * 60);

/// Most images one submit may name.
pub(crate) const MAX_SUBMIT_IMAGES: usize = 32;

/// How old a parked launch whose run was never recorded (its `run_preparing` failed to append at
/// submit) must be before the sweep drops it: long enough that it is not a submit still recording.
const ORPHAN_AFTER: Duration = Duration::from_secs(10 * 60);

/// Why no run launches on a deploy without a capability secret (`BIFFIX_CAPABILITY_SECRET_ARN`):
/// its coordinator would be minted a token no ingest accepts (`config::compose`), so it could not
/// report. Submit refuses the run with it (503); the sweep leaves a parked run waiting with it, to
/// fail at [`IMAGE_PREPARE_TIMEOUT`] unless the secret is restored.
pub(crate) const LAUNCH_DISABLED: &str = "launching is disabled: this deploy has no capability \
secret (BIFFIX_CAPABILITY_SECRET_ARN), so a run's coordinator could not report";

/// The [`LaunchPlan`] format this build writes and reads.
pub(crate) const LAUNCH_PLAN_VERSION: u32 = 1;

/// A run's launch request as submit settled it: checked, filled, pruned and validated, its inputs
/// existence-checked and its samplesheets written. Parked verbatim (`pending_launch`) while the
/// run waits for its images, so `params` carry secret values: it is never an event.
///
/// A persisted format: a parked plan waits up to [`IMAGE_PREPARE_TIMEOUT`], so it can be read by
/// a newer build (a deploy) or an older one (a rollback) than wrote it. Changes are additive only,
/// and every field added from now on is `#[serde(default)]` so an older row still reads; a change
/// an older build must not misread bumps [`LAUNCH_PLAN_VERSION`], and a sweep leaves a plan newer
/// than it knows parked (for a sweep that knows it, or the timeout) rather than launch it wrongly.
/// `images` stays required: a plan without it is unreadable, and fails its run loudly, rather than
/// launching without the run's images. The coordinator queue is not part of it: a parked run
/// launches on the deploy's queue as it is at launch time.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LaunchPlan {
    /// The plan's format ([`LAUNCH_PLAN_VERSION`] when written).
    pub version: u32,
    /// The workflow version the run launches (re-read at launch).
    pub version_id: String,
    /// Who launched the run (the id `authorize` returned).
    pub actor: String,
    /// A human label for the submitter (email/display name), recorded on the anchor. `None` when
    /// unknown (auth disabled, or no email claim).
    pub submitter_label: Option<String>,
    /// The params as the engine gets them (unredacted).
    pub params: serde_json::Value,
    pub input_uris: Vec<String>,
    /// Per-URI provenance tag captured at the submit-time existence check (S3 `version-id`, else
    /// `ETag`, else empty for a prefix input) — recorded on the `run_submitted` anchor.
    pub input_provenance: BTreeMap<String, String>,
    /// Submit-time target override (engine-agnostic). When `Some` (and non-empty), it takes
    /// precedence over the version's registered target; when `None`, the version's baked target is
    /// used. The resolved target is fed to the coordinator (`BIFFIX_TARGET`) and recorded on the
    /// anchor for provenance.
    pub target: Option<String>,
    /// The output-prefix override (a resume's prior prefix, or the caller's), already validated to
    /// be within the project bucket; `None` ⇒ the isolated per-run default. Either way the pipeline
    /// resolves the effective prefix (`runs::output_prefix::effective_output_prefix`) and records it on the anchor.
    pub output_prefix: Option<String>,
    /// Submit-time default task retry count (`BIFFIX_RETRIES`); `None` ⇒ the coordinator default.
    pub retries: Option<u32>,
    /// Submit-time keep-going (`BIFFIX_KEEP_GOING`); `None` ⇒ the coordinator default.
    pub keep_going: Option<bool>,
    /// A prior run's engine session id to resume (Nextflow only; the handler has checked the prior
    /// run). `None` ⇒ a fresh run: the pipeline mints a new session for a Nextflow version.
    pub engine_session_id: Option<String>,
    /// The run being resumed, recorded on the anchor; set together with `engine_session_id`.
    pub resumed_from: Option<RunId>,
    /// The run's human name, recorded on the anchor.
    pub run_name: Option<String>,
    /// Params dropped because their `show_if` conditions hid them, recorded on the anchor.
    pub dropped_inactive: Vec<String>,
    /// Samplesheets biffix wrote for this run from submitted rows, recorded on the anchor.
    pub input_tables: BTreeMap<String, InputTable>,
    /// The reference bundle chosen at submit and the URIs it filled, recorded on the anchor.
    pub reference_bundle: Option<ReferenceUse>,
    /// How many samples the run was launched with (from a sheet written from rows, else a sheet
    /// file whose sample ids submit could read), on the anchor.
    pub sample_count: Option<u32>,
    /// The run's sample ids and the version's hint for matching tasks to them, on the anchor.
    #[serde(default)]
    pub samples: Option<biffix_core::event::SampleSpec>,
    /// What happens to the run's intermediate files, on the anchor (`None` ⇒ the default).
    pub retention: Option<Retention>,
    /// The images given at submit, as pinned (their keys are in the image map), on the anchor.
    pub images: Vec<SubmitImage>,
    /// The experiment the run was launched into, on the anchor. Additive, with no
    /// [`LAUNCH_PLAN_VERSION`] bump: a build that does not know it drops it from the anchor, which
    /// is safe because the run's `run_preparing` names it too and the fold keeps that one when the
    /// anchor names none (`RunProjection::experiment_id`).
    #[serde(default)]
    pub experiment_id: Option<String>,
}

/// A submit-time image as a version image, for [`crate::images::ensure`]: named by its reference,
/// so its messages read "image `<ref>` is being imported" and the image map adds no other name.
pub(crate) fn as_version_image(image: &SubmitImage) -> VersionImage {
    VersionImage {
        name: image.declared_ref.clone(),
        declared_ref: image.declared_ref.clone(),
        source_ref: image.source_ref.clone(),
        content_key: image.content_key.clone(),
        own_runtime: false,
        build: None,
    }
}

/// Move each submit-time image's import (and, for a Snakemake version, its derive) one step
/// forward, as registration does for a declared image; a failed build is retried while `ensure`
/// still retries it. Concurrently, except that images sharing a content key (two spellings of one
/// digest) share one import: the first of each starts it, the rest read it after. Returns where
/// each is, in `images` order.
pub(crate) async fn ensure_submit_images(
    state: &AppState,
    ctx: &VersionContext,
    images: &[SubmitImage],
) -> Result<Vec<ImageStatus>, BuildError> {
    let mut keys = BTreeSet::new();
    let (first, rest): (Vec<usize>, Vec<usize>) =
        (0..images.len()).partition(|&i| keys.insert(images[i].content_key.as_str()));
    let ensure = |i: usize| async move {
        crate::images::ensure(state, ctx, &as_version_image(&images[i]), true).await
    };
    let mut statuses: Vec<Option<ImageStatus>> = (0..images.len()).map(|_| None).collect();
    let started = futures::future::try_join_all(first.iter().map(|&i| ensure(i))).await?;
    for (i, status) in first.into_iter().zip(started) {
        statuses[i] = Some(status);
    }
    for i in rest {
        statuses[i] = Some(ensure(i).await?);
    }
    Ok(statuses.into_iter().map(|s| s.expect("every image was ensured")).collect())
}

/// Why `status` keeps a run from launching: `None` when it is runnable, else its own reason, or
/// that it is being prepared. The one wording the sweep's reasons and the run page share.
pub(crate) fn not_ready_reason(status: &ImageStatus) -> Option<String> {
    status.runnable().is_none().then(|| {
        status
            .unready_reason()
            .unwrap_or_else(|| format!("image `{}` is being prepared", status.image.name))
    })
}

/// Whether a run's submit-time images let it launch.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum ImagesReadiness {
    /// Every one is runnable: launch now.
    Ready,
    /// Some are still being imported or derived, for these reasons.
    Waiting(Vec<String>),
    /// One will not become runnable (a failure `ensure` will not retry, or an image that cannot
    /// serve the version), for this reason.
    Failed(String),
}

/// Where a run's submit-time images (their statuses after [`crate::images::ensure`]) leave it.
pub(crate) fn readiness(statuses: &[ImageStatus]) -> ImagesReadiness {
    let failed: Vec<String> = statuses
        .iter()
        .filter(|s| s.phase == crate::images::build_follow::Phase::Failed)
        .filter_map(ImageStatus::unready_reason)
        .collect();
    if !failed.is_empty() {
        return ImagesReadiness::Failed(failed.join("; "));
    }
    let waiting: Vec<String> = statuses.iter().filter_map(not_ready_reason).collect();
    if waiting.is_empty() {
        ImagesReadiness::Ready
    } else {
        ImagesReadiness::Waiting(waiting)
    }
}

/// Tasks run the default image as it is runnable (same workflow repository): its derived image
/// (recording the imported or built one it came from), or a built image, whose digest the version
/// could not record at registration. Sets `version.image_digest` to it and returns what it was
/// derived from (the declared, imported digest, or a built image's), if it was derived.
pub(crate) fn runnable_default(
    images: &[ImageStatus],
    version: &mut WorkflowVersion,
) -> Option<String> {
    let default = images.iter().find(|i| i.image.name == "default")?;
    let runnable = default.runnable()?;
    if runnable.digest == version.image_digest {
        return None;
    }
    let declared = std::mem::replace(&mut version.image_digest, runnable.digest.clone());
    // What it was derived from: the declared (imported) digest, or a built image's.
    default.derived.as_ref().and_then(|_| {
        if declared.is_empty() {
            default.artifact.as_ref().map(|a| a.digest.clone())
        } else {
            Some(declared)
        }
    })
}

/// What [`launch_run`] resolves at launch beside the plan.
pub(crate) struct LaunchInputs<'a> {
    /// The project the run launches in (its version's,
    /// `crate::config::projects::Projects::for_version`).
    pub(crate) project: &'a crate::config::projects::ProjectContext,
    /// Where the coordinator fetches the workflow source from.
    pub(crate) source: &'a WorkflowSource,
    /// The version, its `image_digest` the default image as tasks run it ([`runnable_default`]).
    pub(crate) version: WorkflowVersion,
    pub(crate) image_source_digest: Option<String>,
    pub(crate) variant: LaunchVariant,
    /// The version's images (all runnable).
    pub(crate) version_images: &'a [ImageStatus],
    /// The submit-time images (all runnable), in the plan's order.
    pub(crate) submit_images: &'a [ImageStatus],
}

/// Why [`launch_run`] did not launch a recorded run.
#[derive(Debug, thiserror::Error)]
pub(crate) enum LaunchError {
    /// Writing the run's image map failed.
    #[error("writing the run's image map failed: {0}")]
    ImageMap(#[source] AwsError),
    /// The submit pipeline refused or failed (nothing launched).
    #[error(transparent)]
    Submit(SubmitError),
    /// Coordinator `job_id` launched but the run could not be recorded. No `stop_error`: stopping
    /// it succeeded; else it may still be running, unbound to any run (`stop_error` says why not).
    #[error("failed to persist run: {error}; {}", coordinator_fate(.job_id, .stop_error))]
    Record { error: StoreError, job_id: String, stop_error: Option<String> },
}

/// What became of the coordinator of a run that could not be recorded, as people read it.
fn coordinator_fate(job_id: &str, stop_error: &Option<String>) -> String {
    match stop_error {
        None => format!("its coordinator ({job_id}) was stopped"),
        Some(e) => format!(
            "stopping its coordinator ({job_id}) failed too, so it may still be running: {e}"
        ),
    }
}

impl LaunchError {
    /// Whether trying again later may succeed (AWS or the store was unavailable) without leaving
    /// anything behind. A run whose coordinator may still be running is never retried blind: a
    /// second launch would run it twice. A launch whose SubmitJob answer was lost
    /// ([`SubmitError::LaunchUncertain`]) is retried, by the sweep, which first stops any
    /// coordinator it started ([`stop_unbound_coordinators`]); one that left a coordinator it could
    /// neither record nor stop is never retried.
    pub(crate) fn is_transient(&self) -> bool {
        match self {
            LaunchError::ImageMap(e) | LaunchError::Submit(SubmitError::Aws(e)) => {
                matches!(e, AwsError::Unavailable(_))
            }
            LaunchError::Submit(SubmitError::LaunchUncertain(_)) => true,
            LaunchError::Submit(_) => false,
            LaunchError::Record { error, stop_error, .. } => {
                stop_error.is_none() && matches!(error, StoreError::Unavailable(_))
            }
        }
    }
}

/// Launch `run_id` from `plan`: write its image map (the version's images, with the submit-time
/// ones winning every spelling they have), run the submit pipeline (mint session → anchor → launch
/// coordinator), and record the anchor and the coordinator binding.
pub(crate) async fn launch_run(
    state: &AppState,
    run_id: &RunId,
    plan: &LaunchPlan,
    inputs: LaunchInputs<'_>,
) -> Result<SubmitOutcome, LaunchError> {
    let LaunchInputs {
        project,
        source,
        version,
        image_source_digest,
        variant,
        version_images,
        submit_images,
    } = inputs;
    // The run's image map: every task image resolves through it, strictly (crate::images::map).
    let image_map = if project.bucket.is_empty() {
        None
    } else {
        let worker = crate::aws::batch::worker_image(
            &version.image_digest,
            crate::runs::submit::worker_image_repo_uri(&version).as_deref(),
            &state.deploy.worker_image_repo,
            version.engine,
            &variant.image,
        );
        let map = crate::images::map::build_with_overrides(
            version_images,
            submit_images,
            worker.as_deref(),
        );
        let uri = crate::images::map::uri_for(&project.bucket, run_id.as_str());
        let body = serde_json::to_vec_pretty(&map).expect("a map of strings always serializes");
        state
            .cloud
            .put_object(&uri, body, "application/json")
            .await
            .map_err(LaunchError::ImageMap)?;
        Some((uri, map))
    };
    let sreq = SubmitRequest {
        run_id: run_id.clone(),
        version,
        source: source.clone(),
        plan: plan.clone(),
        coordinator_queue: state.deploy.coordinator_queue.clone(),
        project: project.launch_project(),
        variant,
        image_source_digest,
        image_map,
    };

    let outcome =
        crate::runs::submit::submit(&state.cloud, sreq).await.map_err(LaunchError::Submit)?;

    // Persist the anchor (makes the run listable) AND the coordinator→run binding (batch_job_id →
    // run_id) ATOMICALLY. The binding is what lets the coordinator's AWS lifecycle events
    // (RUNNING/SUCCEEDED/FAILED) be attributed to the run; without it the run stays "submitted"
    // forever and the reconciliation backstop has nothing to key off. Recording them together means
    // a crash can never persist the anchor without the binding, or vice versa.
    //
    // The coordinator is already launched by now. If neither can be recorded, nothing in biffix
    // knows of it (no run to view or cancel, no binding to reap it by), so it is stopped rather
    // than left running unseen; a retry then launches a fresh run.
    if let Err(error) = state
        .events
        .record_all(&[outcome.run_submitted.clone(), outcome.coordinator_submitted.clone()])
        .await
    {
        let job = &outcome.coordinator_job_id;
        let stop_error =
            match state.cloud.terminate_job(job, "biffix: the run could not be recorded").await {
                Ok(()) => None,
                Err(te) => {
                    tracing::warn!(
                        run_id = %run_id,
                        job_id = %job,
                        error = %te,
                        "submit: stopping the coordinator of an unrecorded run failed"
                    );
                    Some(te.to_string())
                }
            };
        return Err(LaunchError::Record { error, job_id: job.clone(), stop_error });
    }
    Ok(outcome)
}

/// What one sweep of the parked launches did.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct LaunchSweepReport {
    /// Parked launches seen.
    pub pending: usize,
    pub launched: usize,
    /// Left parked: images (or the coordinator variant) still being prepared, a plan from a newer
    /// biffix, or held by another sweep or a cancel.
    pub waiting: usize,
    /// Runs failed: an image that will never be runnable, a launch that cannot happen, or the
    /// preparation timed out.
    pub failed: usize,
    /// Parked launches of runs cancelled meanwhile, dropped (a coordinator launched as the cancel
    /// landed stopped).
    pub cancelled: usize,
    /// Parked launches of runs that ended otherwise (failed or succeeded) meanwhile, dropped.
    pub finished: usize,
    /// Parked launches dropped otherwise: the run already launched, or was never recorded.
    pub dropped: usize,
    pub errors: usize,
}

/// Where one parked launch was left.
enum Swept {
    Launched,
    Waiting,
    Failed,
    Cancelled,
    Finished,
    Dropped,
}

/// Which step of a parked run's launch a wait, an error or a failure is at: the `run_failed` stage
/// it fails with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    /// Its submit-time images being imported and derived.
    ImagePreparation,
    /// Everything else: the plan, the version and its engine runtime (however it is missing or
    /// unreadable, including when reading it to derive images), the coordinator variant,
    /// launching.
    Launch,
}

impl Stage {
    fn as_str(self) -> &'static str {
        match self {
            Stage::ImagePreparation => FAILURE_STAGE_IMAGE_PREPARATION,
            Stage::Launch => FAILURE_STAGE_LAUNCH,
        }
    }

    /// What was timing out, as people read it.
    fn doing(self) -> &'static str {
        match self {
            Stage::ImagePreparation => "image preparation",
            Stage::Launch => "launching",
        }
    }
}

/// Why a parked run waits still.
struct Wait {
    stage: Stage,
    reasons: Vec<String>,
}

/// A parked run's step that erred.
enum StepError {
    /// At `Stage`: may pass on a later sweep, so the run waits, until the timeout fails it.
    At(Stage, BuildError),
    /// Recording the run's failure (or dropping its row) failed: the next sweep retries it.
    Recording(BuildError),
}

/// Launch each parked run whose submit-time images are now runnable (oldest first, one at a time:
/// see [`crate::store::pending::PendingLaunchStore::claim_run_launch`]), moving its imports and
/// derives forward first so a build that finished since the last sweep launches now. A run whose
/// image will never be runnable fails with why; one that has waited [`IMAGE_PREPARE_TIMEOUT`] fails
/// with what it still waits for, or the error its step keeps hitting. A step that fails before then
/// is logged and counted, and its launch stays parked for the next sweep.
pub async fn sweep(state: &AppState) -> Result<LaunchSweepReport, BuildError> {
    let rows = state.store.list_pending_launches().await?;
    let mut report = LaunchSweepReport { pending: rows.len(), ..Default::default() };
    if !rows.is_empty() && state.capability.is_none() {
        // A coordinator launched now would get a token no ingest accepts (compose.rs): refuse,
        // loudly; the runs fail at the timeout rather than launch blind.
        tracing::error!(
            pending = rows.len(),
            "launch sweep: no capability secret (BIFFIX_CAPABILITY_SECRET_ARN), so parked runs \
             cannot launch"
        );
    }
    for row in rows {
        match sweep_one(state, &row).await {
            Ok(Swept::Launched) => report.launched += 1,
            Ok(Swept::Waiting) => report.waiting += 1,
            Ok(Swept::Failed) => report.failed += 1,
            Ok(Swept::Cancelled) => report.cancelled += 1,
            Ok(Swept::Finished) => report.finished += 1,
            Ok(Swept::Dropped) => report.dropped += 1,
            Err(e) => {
                report.errors += 1;
                tracing::warn!(run_id = %row.run_id, error = %e, "launch sweep: step failed");
            }
        }
    }
    Ok(report)
}

/// One parked launch's step: see [`sweep`]. `listed` is the row as the sweep listed it, which only
/// names the run: what decides is the row as it is once the claim is held.
async fn sweep_one(state: &AppState, listed: &PendingLaunch) -> Result<Swept, BuildError> {
    let run_id = &listed.run_id;
    // One launcher per run: another sweep holding it now is launching it (or a cancel is ending
    // it). Held through the launch (see `claim_run_launch`).
    let Some(_claim) = state.store.claim_run_launch(run_id).await? else {
        return Ok(Swept::Waiting);
    };
    // Read again under the claim: since the listing, another sweep may have marked the launch as
    // leaving a coordinator unbound (which this one must not launch again), or dropped it.
    let Some(row) = state.store.get_pending_launch(run_id).await? else {
        return Ok(Swept::Dropped);
    };
    let row = &row;
    let now = biffix_core::clock::epoch_micros();
    let age = Duration::from_micros(u64::try_from(now - row.created_at_micros).unwrap_or(0));
    let run_state = state.store.run_state(run_id).await?;
    // A launch left a coordinator it could neither record nor stop: never launch again.
    if let Some(job) = row.orphan_job_id() {
        return settle_orphan(state, run_id, job, run_state.as_deref()).await;
    }
    match run_state.as_deref().map(str::parse::<RunState>) {
        // Not recorded: a submit still recording it, or one whose record failed (an orphan).
        None if age < ORPHAN_AFTER => return Ok(Swept::Waiting),
        None => {
            drop_row(state, run_id).await?;
            return Ok(Swept::Dropped);
        }
        Some(Ok(RunState::Cancelled)) => {
            drop_row(state, run_id).await?;
            return Ok(Swept::Cancelled);
        }
        Some(Ok(s)) if s.is_terminal() => {
            drop_row(state, run_id).await?;
            return Ok(Swept::Finished);
        }
        // Launched already (its request outlived the launch): never launch it again.
        Some(s) if s != Ok(RunState::PreparingImages) => {
            drop_row(state, run_id).await?;
            return Ok(Swept::Dropped);
        }
        Some(_) => {}
    }
    let newer = row
        .plan
        .get("version")
        .and_then(serde_json::Value::as_u64)
        .filter(|v| *v > u64::from(LAUNCH_PLAN_VERSION));
    let step = match newer {
        Some(v) => {
            tracing::warn!(run_id = %run_id, plan_version = v, "launch sweep: the run's launch request is newer than this biffix; leaving it parked");
            Ok(Err(Wait {
                stage: Stage::Launch,
                reasons: vec![format!(
                    "its launch request is from a newer biffix (format {v}), which this one \
                     cannot launch"
                )],
            }))
        }
        None => match LaunchPlan::deserialize(&row.plan) {
            Ok(plan) => advance(state, run_id, &plan).await,
            Err(e) => {
                let reason = format!("the run's launch request is unreadable: {e}");
                return fail(state, run_id, Stage::Launch, &reason).await;
            }
        },
    };
    let wait = match step {
        Ok(Ok(swept)) => return Ok(swept),
        Ok(Err(wait)) => wait,
        // Past the bound a step that keeps failing fails the run, rather than keep it preparing.
        Err(StepError::At(stage, e)) if age > IMAGE_PREPARE_TIMEOUT => {
            let reason = format!(
                "{} timed out after {} h; last error: {e}",
                stage.doing(),
                IMAGE_PREPARE_TIMEOUT.as_secs() / 3600
            );
            return fail(state, run_id, stage, &reason).await;
        }
        Err(StepError::At(_, e) | StepError::Recording(e)) => return Err(e),
    };
    if age > IMAGE_PREPARE_TIMEOUT {
        let reason = format!(
            "{} timed out after {} h: {}",
            wait.stage.doing(),
            IMAGE_PREPARE_TIMEOUT.as_secs() / 3600,
            wait.reasons.join("; ")
        );
        return fail(state, run_id, wait.stage, &reason).await;
    }
    Ok(Swept::Waiting)
}

/// Move a parked run's submit-time images forward and, once they are runnable, launch it. `Err`
/// from a step that may pass on a later sweep; `Ok(Err(wait))` while it waits; else where it was
/// left (launched, or failed for good).
async fn advance(
    state: &AppState,
    run_id: &RunId,
    plan: &LaunchPlan,
) -> Result<Result<Swept, Wait>, StepError> {
    // The version and its runtime are the launch's, even read here to derive images: one root
    // cause (a missing or unreadable runtime) always fails at one stage.
    let at_launch = |e: BuildError| StepError::At(Stage::Launch, e);
    let at_images = |e: BuildError| StepError::At(Stage::ImagePreparation, e);
    let rec = match state.registry.get_version(&plan.version_id).await {
        Ok(rec) => rec,
        Err(StoreError::NotFound) => {
            let reason = format!("workflow version {} no longer exists", plan.version_id);
            return failed(state, run_id, Stage::Launch, &reason).await;
        }
        Err(e) => return Err(at_launch(e.into())),
    };
    let ctx = VersionContext::of(state, &rec.version).await.map_err(at_launch)?;
    let statuses = ensure_submit_images(state, &ctx, &plan.images).await.map_err(at_images)?;
    match readiness(&statuses) {
        ImagesReadiness::Failed(reason) => {
            failed(state, run_id, Stage::ImagePreparation, &reason).await
        }
        ImagesReadiness::Waiting(reasons) => {
            Ok(Err(Wait { stage: Stage::ImagePreparation, reasons }))
        }
        ImagesReadiness::Ready => launch_ready(state, run_id, plan, &rec, &statuses).await,
    }
}

/// The first location of a parked `plan` outside its project's `bucket` (its output prefix, or a
/// samplesheet submit wrote), which its project's roles cannot write to (or read): the deploy's
/// projects changed while it waited. `None` when there is none, or no bucket to compare with.
fn outside_project<'a>(plan: &'a LaunchPlan, bucket: &str) -> Option<&'a str> {
    if bucket.is_empty() {
        return None;
    }
    let root = format!("s3://{bucket}/");
    plan.output_prefix
        .iter()
        .chain(plan.input_tables.values().map(|t| &t.uri))
        .map(String::as_str)
        .find(|uri| !uri.starts_with(&root))
}

/// Launch a parked run whose submit-time images are runnable. `Ok(Err(wait))` when it must wait
/// still (its coordinator variant is being built, or launching failed in a way that may pass).
async fn launch_ready(
    state: &AppState,
    run_id: &RunId,
    plan: &LaunchPlan,
    rec: &crate::store::registry::RegisteredVersion,
    submit_images: &[ImageStatus],
) -> Result<Result<Swept, Wait>, StepError> {
    let at_launch = |e: BuildError| StepError::At(Stage::Launch, e);
    let wait = |reason: String| Ok(Err(Wait { stage: Stage::Launch, reasons: vec![reason] }));
    // The version's own images were runnable at submit; they are again now, or the run cannot run.
    let images = crate::images::statuses(state, rec).await.map_err(at_launch)?;
    let unready: Vec<String> = images.iter().filter_map(ImageStatus::unready_reason).collect();
    if !unready.is_empty() {
        let reason = format!("the version's images are no longer runnable: {}", unready.join("; "));
        return failed(state, run_id, Stage::Launch, &reason).await;
    }
    if state.capability.is_none() {
        return wait(LAUNCH_DISABLED.into());
    }
    // The run launches in its version's project, re-resolved now (the deploy may have changed;
    // read again first if this process's projects lack it, so one a deploy just added is seen).
    let projects = state.projects.current_for(Some(&rec.project)).await;
    let project = match projects.for_version(&rec.project) {
        Ok(project) => project,
        Err(e) => {
            let reason = crate::runs::submit::request::unknown_version_project(&e, &rec.version.id);
            return failed(state, run_id, Stage::Launch, &reason).await;
        }
    };
    if let Some(reason) = crate::register::pipeline::staged_outside_project(rec, project) {
        return failed(state, run_id, Stage::Launch, &reason).await;
    }
    if let Some(uri) = outside_project(plan, &project.bucket) {
        let reason = format!(
            "{uri} is outside this run's project's bucket s3://{}/ (the deploy's projects \
             changed while it waited); resubmit it",
            project.bucket
        );
        return failed(state, run_id, Stage::Launch, &reason).await;
    }
    // Its inputs and reference bundle, by submit's own rule, against the projects as they are now.
    let bundle_uris = plan.reference_bundle.iter().flat_map(|b| b.uris.values());
    if let Some(reason) = plan
        .input_uris
        .iter()
        .chain(bundle_uris)
        .find_map(|uri| crate::runs::submit::request::unreadable_in(&projects, project, uri))
    {
        let reason = format!("{reason}; resubmit it with inputs project `{}` can read", project.id);
        return failed(state, run_id, Stage::Launch, &reason).await;
    }
    let mut version = rec.version.clone();
    let image_source_digest = runnable_default(&images, &mut version);
    let variant = match crate::images::variants::launch_variant(state, &rec.version, project).await
    {
        Ok(variant) => variant,
        Err(e @ (NotRunnable::Retired(_) | NotRunnable::Failed { .. })) => {
            return failed(state, run_id, Stage::Launch, &e.to_string()).await
        }
        Err(e @ NotRunnable::Building(_)) => return wait(e.to_string()),
        Err(NotRunnable::Error(e)) => return Err(at_launch(e)),
    };
    // The coordinator runs on the deploy's queue as it is now, not as it was at submit: a deploy
    // may have replaced the queue while the run waited (as the run page labels the host).
    let queue = &state.deploy.coordinator_queue;
    if let Err(reason) = stop_unbound_coordinators(state, run_id, queue).await {
        return wait(reason);
    }
    let launched = launch_run(
        state,
        run_id,
        plan,
        LaunchInputs {
            project,
            source: &rec.source,
            version,
            image_source_digest,
            variant,
            version_images: &images,
            submit_images,
        },
    )
    .await;
    let outcome = match launched {
        Ok(outcome) => outcome,
        Err(e) if e.is_transient() => {
            tracing::warn!(run_id = %run_id, error = %e, "launch sweep: launching failed; retrying next sweep");
            return wait(format!("launching failed: {e}"));
        }
        Err(LaunchError::Record { error, job_id, stop_error: Some(_) }) => {
            // Its coordinator may be running, unbound: mark the row so no sweep launches the run
            // again, then settle it now (the next sweep does, if this cannot).
            tracing::error!(run_id = %run_id, job_id = %job_id, error = %error, "launch sweep: a coordinator launched but neither recorded nor stopped");
            if let Err(e) = state.store.mark_pending_launch_orphan(run_id, &job_id).await {
                tracing::error!(run_id = %run_id, job_id = %job_id, error = %e, "launch sweep: marking the unbound coordinator failed; a later sweep may launch the run again");
            }
            let run_state =
                state.store.run_state(run_id).await.map_err(|e| StepError::Recording(e.into()))?;
            return settle_orphan(state, run_id, &job_id, run_state.as_deref())
                .await
                .map(Ok)
                .map_err(StepError::Recording);
        }
        Err(e) => return failed(state, run_id, Stage::Launch, &e.to_string()).await,
    };
    tracing::info!(run_id = %run_id, job_id = %outcome.coordinator_job_id, "launch sweep: run launched");
    // Launched: the parked request is spent. Failing to drop it is harmless (the next sweep sees
    // the run launched and drops it).
    if let Err(e) = state.store.delete_pending_launch(run_id).await {
        tracing::warn!(run_id = %run_id, error = %e, "launch sweep: dropping a launched run's request failed");
    }
    // A backstop to cancel's claim: a cancel recorded while the coordinator was launching saw no
    // coordinator to stop, so stop it.
    let cancelled = match state.store.run_state(run_id).await {
        Ok(s) => s.as_deref() == Some(RunState::Cancelled.as_str()),
        Err(e) => {
            tracing::warn!(run_id = %run_id, job_id = %outcome.coordinator_job_id, error = %e, "launch sweep: re-reading a launched run's state failed; a cancel that landed meanwhile would leave its coordinator running");
            false
        }
    };
    if cancelled {
        let job = &outcome.coordinator_job_id;
        if let Err(e) =
            state.cloud.terminate_job(job, "biffix: cancelled while preparing images").await
        {
            tracing::warn!(run_id = %run_id, job_id = %job, error = %e, "launch sweep: stopping a cancelled run's coordinator failed");
        }
        return Ok(Ok(Swept::Cancelled));
    }
    Ok(Ok(Swept::Launched))
}

/// Stop every live coordinator of parked run `run_id` (on `queue`) before the sweep launches it: a
/// run still preparing has no coordinator of its own, so one found was started by a launch whose
/// SubmitJob answer was lost ([`SubmitError::LaunchUncertain`]: at submit, or a sweep's), unbound
/// to the run. Batch lists a job only eventually, but by the next sweep, minutes after such a
/// launch, it lists it. `Err` is why the run waits instead: it is not launched while one may run.
async fn stop_unbound_coordinators(
    state: &AppState,
    run_id: &RunId,
    queue: &str,
) -> Result<(), String> {
    let jobs = state.cloud.live_coordinators(queue, run_id).await.map_err(|e| {
        format!(
            "looking for a coordinator an earlier launch of the run may have started failed: {e}"
        )
    })?;
    for job in jobs {
        tracing::warn!(run_id = %run_id, job_id = %job, "launch sweep: stopping a coordinator an earlier launch started unbound");
        state
            .cloud
            .terminate_job(&job, "biffix: a launch of the run whose answer was lost started it")
            .await
            .map_err(|e| {
                format!(
                    "stopping coordinator {job}, which an earlier launch of the run started \
                     unbound, failed: {e}"
                )
            })?;
    }
    Ok(())
}

/// Settle a parked run whose launch left coordinator `job` unbound (neither recorded nor
/// stopped), in state `run_state`. If the run launched after all (the record landed although it
/// reported failing), the coordinator is its own: the row is dropped. Otherwise the coordinator is
/// stopped, and a run not yet ended fails naming it; the row is dropped once that is recorded.
async fn settle_orphan(
    state: &AppState,
    run_id: &RunId,
    job: &str,
    run_state: Option<&str>,
) -> Result<Swept, BuildError> {
    let parsed = run_state.map(str::parse::<RunState>);
    if matches!(parsed, Some(Ok(RunState::Submitted | RunState::Running))) {
        drop_row(state, run_id).await?;
        return Ok(Swept::Dropped);
    }
    let stopped = state.cloud.terminate_job(job, "biffix: the run could not be recorded").await;
    if let Err(e) = &stopped {
        tracing::error!(run_id = %run_id, job_id = %job, error = %e, "launch sweep: stopping an unbound coordinator failed");
    }
    match parsed {
        Some(Ok(RunState::Cancelled)) => {
            drop_row(state, run_id).await?;
            Ok(Swept::Cancelled)
        }
        Some(Ok(s)) if s.is_terminal() => {
            drop_row(state, run_id).await?;
            Ok(Swept::Finished)
        }
        _ => {
            let fate = match stopped {
                Ok(()) => "it was stopped".to_string(),
                Err(e) => format!(
                    "stopping it failed ({e}), so it may still be running: stop AWS Batch job \
                     {job}"
                ),
            };
            let reason = format!(
                "the run's coordinator ({job}) launched but the run could not be recorded; {fate}"
            );
            fail(state, run_id, Stage::Launch, &reason).await
        }
    }
}

/// [`fail`] for a step of [`advance`]: a recording failure is retried next sweep, never timed out.
async fn failed(
    state: &AppState,
    run_id: &RunId,
    stage: Stage,
    reason: &str,
) -> Result<Result<Swept, Wait>, StepError> {
    fail(state, run_id, stage, reason).await.map(Ok).map_err(StepError::Recording)
}

/// Fail a parked run at `stage` with `reason` (the attributable `run_failed`), and drop its
/// request.
async fn fail(
    state: &AppState,
    run_id: &RunId,
    stage: Stage,
    reason: &str,
) -> Result<Swept, BuildError> {
    tracing::info!(run_id = %run_id, stage = stage.as_str(), reason, "launch sweep: run failed");
    let ts = Timestamp::from_micros(biffix_core::clock::epoch_micros());
    state
        .events
        .record(&biffix_core::event::run_failed(run_id, ts, stage.as_str(), reason))
        .await?;
    drop_row(state, run_id).await?;
    Ok(Swept::Failed)
}

async fn drop_row(state: &AppState, run_id: &RunId) -> Result<(), BuildError> {
    Ok(state.store.delete_pending_launch(run_id).await?)
}

/// A launch that did not happen: writing the image map is AWS's fault (upstream), a refused submit
/// is [`SubmitError`]'s, and a run that could not be recorded its store failure's kind, saying what
/// became of its coordinator.
impl From<LaunchError> for crate::error::ServiceError {
    fn from(err: LaunchError) -> Self {
        let message = err.to_string();
        match err {
            LaunchError::ImageMap(_) => crate::error::ServiceError::Upstream(message),
            LaunchError::Submit(e) => e.into(),
            LaunchError::Record { error, .. } => {
                crate::error::ServiceError::from(error).with_message(message)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use biffix_workflow::images::Capabilities;

    use super::*;
    use crate::images::build_follow::Phase;
    use crate::store::registry::ImageArtifact;

    fn status(name: &str, phase: Phase, artifact: bool, problem: Option<&str>) -> ImageStatus {
        ImageStatus {
            image: as_version_image(&SubmitImage {
                declared_ref: name.into(),
                source_ref: format!("{name}@sha256:s"),
                content_key: "k".into(),
            }),
            phase,
            build: None,
            arch_builds: vec![],
            artifact: artifact.then(|| ImageArtifact {
                content_key: "k".into(),
                source_ref: String::new(),
                image: "repo@sha256:i".into(),
                digest: "sha256:i".into(),
                archs: vec![],
                capabilities: Capabilities::default(),
                keep_tag: None,
                archived: true,
                archive_bytes: None,
                created_at: 0,
            }),
            derived: None,
            problem: problem.map(str::to_string),
        }
    }

    #[test]
    fn readiness_fails_on_any_failed_image_else_waits_on_any_unrunnable_one() {
        assert_eq!(readiness(&[]), ImagesReadiness::Ready);
        let ready = status("a", Phase::Ready, true, None);
        let importing = status("b", Phase::Building, false, None);
        let musl = status("c", Phase::Failed, true, Some("image `c` has no glibc"));
        assert_eq!(readiness(std::slice::from_ref(&ready)), ImagesReadiness::Ready);
        assert_eq!(
            readiness(&[ready.clone(), importing.clone()]),
            ImagesReadiness::Waiting(vec!["image `b` is being imported".into()])
        );
        assert_eq!(
            readiness(&[ready, importing, musl]),
            ImagesReadiness::Failed("image `c` has no glibc".into())
        );
    }

    #[test]
    fn an_image_is_not_ready_for_its_own_reason_else_because_it_is_being_prepared() {
        assert_eq!(not_ready_reason(&status("a", Phase::Ready, true, None)), None);
        assert_eq!(
            not_ready_reason(&status("b", Phase::Building, false, None)),
            Some("image `b` is being imported".into())
        );
        // Recorded but not runnable, with nothing more to say.
        assert_eq!(
            not_ready_reason(&status("c", Phase::Ready, false, None)),
            Some("image `c` is being prepared".into())
        );
    }

    #[test]
    fn only_a_launch_that_left_nothing_running_is_retried() {
        let unavailable = || StoreError::Unavailable("down".into());
        let record = |error: StoreError, terminated: bool| LaunchError::Record {
            error,
            job_id: "coord-1".into(),
            stop_error: (!terminated).then(|| "denied".to_string()),
        };
        assert!(record(unavailable(), true).is_transient());
        assert!(!record(unavailable(), false).is_transient());
        assert!(!record(StoreError::NotFound, true).is_transient());
        assert!(!record(StoreError::NotFound, false).is_transient());
        assert!(LaunchError::ImageMap(AwsError::Unavailable("s3".into())).is_transient());
        assert!(!LaunchError::ImageMap(AwsError::Invalid("s3".into())).is_transient());
        assert!(LaunchError::Submit(SubmitError::Aws(AwsError::Unavailable("batch".into())))
            .is_transient());
        assert!(!LaunchError::Submit(SubmitError::Aws(AwsError::Invalid("batch".into())))
            .is_transient());
        assert!(!LaunchError::Submit(SubmitError::MissingInputs(vec![])).is_transient());
        // Its message says what became of the coordinator.
        assert_eq!(
            record(unavailable(), false).to_string(),
            "failed to persist run: store unavailable: down; stopping its coordinator (coord-1) \
             failed too, so it may still be running: denied"
        );
    }

    #[test]
    fn a_launch_plan_round_trips_through_json() {
        let plan = LaunchPlan {
            version: LAUNCH_PLAN_VERSION,
            version_id: "abc:hash:0".into(),
            actor: "u".into(),
            submitter_label: Some("u@example.com".into()),
            params: serde_json::json!({"reads": "s3://b/r.fq", "token": "secret"}),
            input_uris: vec!["s3://b/r.fq".into()],
            input_provenance: BTreeMap::from([("s3://b/r.fq".into(), "etag".into())]),
            target: Some("all".into()),
            output_prefix: Some("s3://b/out/".into()),
            retries: Some(2),
            keep_going: Some(true),
            engine_session_id: None,
            resumed_from: Some(RunId::new("run-prior")),
            run_name: Some("name".into()),
            dropped_inactive: vec!["hidden".into()],
            input_tables: BTreeMap::new(),
            reference_bundle: None,
            sample_count: Some(3),
            samples: None,
            retention: Some(Retention::Keep),
            images: vec![SubmitImage {
                declared_ref: "quay.io/b/x:1".into(),
                source_ref: "quay.io/b/x:1@sha256:d".into(),
                content_key: "k".into(),
            }],
            experiment_id: Some("exp-1".into()),
        };
        let json = serde_json::to_value(&plan).unwrap();
        assert_eq!(serde_json::from_value::<LaunchPlan>(json.clone()).unwrap(), plan);
        // The images are required: a plan without them does not read.
        for required in ["images", "version"] {
            let mut missing = json.clone();
            missing.as_object_mut().unwrap().remove(required);
            assert!(serde_json::from_value::<LaunchPlan>(missing).is_err(), "{required}");
        }
    }
}
