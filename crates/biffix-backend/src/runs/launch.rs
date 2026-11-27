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
//! - [`launch_run`]: write the image map, register the run's task job definitions and write their
//!   map, run the submit pipeline, and record the anchor and the coordinator binding (stopping the
//!   coordinator if they cannot be recorded).
//! - [`sweep`]: move each parked run's images forward and launch the run once they are runnable
//!   (stopping first any coordinator a launch whose answer was lost left running), or fail it with
//!   why they never will be (or once it has waited [`IMAGE_PREPARE_TIMEOUT`], whatever it waited
//!   on, a step that keeps failing included). A run that will never launch keeps its request, for
//!   cleanup only, while a coordinator an earlier launch of it may have left running cannot be
//!   stopped ([`release`]).

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

use crate::aws::{
    batch::{LaunchVariant, LiveCoordinator},
    AwsError,
};
use crate::images::build_follow::BuildError;
use crate::images::variants::NotRunnable;
use crate::images::{ImageStatus, VersionContext};
use crate::register::workflow::{WorkflowSource, WorkflowVersion};
use crate::runs::submit::{SubmitError, SubmitOutcome, SubmitRequest};
use crate::store::pending::{
    PendingLaunch, PENDING_CLEANUP_KEY, PENDING_CLEANUP_TRIED_KEY, PENDING_ESCALATED_KEY,
    PENDING_ORPHAN_KEY,
};
use crate::store::registry::VersionImage;
use crate::store::StoreError;
use crate::AppState;

/// How long a run may wait for its submit-time images (from when it was parked) before it fails:
/// an import and a derive (two hours each at most), queueing, and one retry.
pub(crate) const IMAGE_PREPARE_TIMEOUT: Duration = Duration::from_secs(6 * 60 * 60);

/// How long after a run was parked the sweep tries to stop a coordinator an earlier launch of it
/// may have left running unbound, once the run will never launch ([`release`]), before it logs an
/// error naming the run and the queues (once: [`PENDING_ESCALATED_KEY`]). It never gives up: an
/// unbound coordinator can wait RUNNABLE for capacity indefinitely (no queue-state time limit
/// applies), and once it starts it runs the run's tasks under the run role, whether or not its
/// capability token has expired; the request is the last record of it. A week leaves an operator
/// days (a weekend included) to restore a listing or terminate grant, or Batch to recover, before
/// the error.
pub(crate) const CLEANUP_ESCALATE_AFTER: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// How long after a launch whose SubmitJob answer was lost ([`SubmitError::LaunchUncertain`]) Batch
/// may still not list the coordinator it created: until then, a listing that finds none does not
/// prove there is none ([`unsettled_lost_launch`]). Batch's ListJobs is eventually consistent,
/// within seconds as a rule; a quarter of an hour covers a slow one, while a run whose launch was
/// lost waits for its retry (or the release of its request) by no more than three 5-minute sweeps.
pub(crate) const LOST_LAUNCH_LISTED_WITHIN: Duration = Duration::from_secs(15 * 60);

/// The key of a parked plan ([`LaunchPlan::launch_uncertain_at_micros`]) under which the time its
/// latest launch whose outcome is unknown began is recorded: read as JSON, as a plan kept for
/// cleanup only is not a whole [`LaunchPlan`].
const LAUNCH_UNCERTAIN_KEY: &str = "launch_uncertain_at_micros";

/// The key of a parked plan ([`LaunchPlan::launch_answer_lost_at_micros`]) under which the time
/// the answer of its latest launch whose outcome is unknown was found lost is recorded: read as
/// JSON, like [`LAUNCH_UNCERTAIN_KEY`].
const LAUNCH_ANSWER_LOST_KEY: &str = "launch_answer_lost_at_micros";

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

/// The newest [`LaunchPlan`] format this build reads, and the one it writes for a single-job run
/// ([`LaunchPlan::single_job`]): a build that knows only [`TASKS_LAUNCH_PLAN_VERSION`] would drop
/// that field and launch the run one Batch job per task, so it must leave such a plan parked.
pub(crate) const LAUNCH_PLAN_VERSION: u32 = 2;

/// The [`LaunchPlan`] format this build writes for a run that is not single-job: format 1, which it
/// reads the same as every build since format 1, so a rollback still launches it.
pub(crate) const TASKS_LAUNCH_PLAN_VERSION: u32 = 1;

/// The format a plan is written as: [`LAUNCH_PLAN_VERSION`] for a single-job run, else
/// [`TASKS_LAUNCH_PLAN_VERSION`].
pub(crate) fn launch_plan_version(single_job: bool) -> u32 {
    if single_job {
        LAUNCH_PLAN_VERSION
    } else {
        TASKS_LAUNCH_PLAN_VERSION
    }
}

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
/// launches on the deploy's queue as it is at launch time (it records only the queues its launches
/// were submitted to, [`LaunchPlan::attempted_queues`], to look for what they may have started).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LaunchPlan {
    /// The plan's format (as written, [`launch_plan_version`]: [`LAUNCH_PLAN_VERSION`] for a
    /// single-job run, else [`TASKS_LAUNCH_PLAN_VERSION`]).
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
    /// The run executes as one Batch job on one whole host, as submit resolved it
    /// ([`crate::runs::single_job`]). Written only in format [`LAUNCH_PLAN_VERSION`]
    /// ([`launch_plan_version`]): an older build would launch it one Batch job per task.
    /// The same record as the run's anchor's (`RunSubmitted.single_job`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub single_job: Option<biffix_core::event::SingleJobUse>,
    /// Every job queue (ARN) a launch of the parked run was submitted to, in order: recorded
    /// before each sweep's SubmitJob, and by submit when it parks a launch whose answer was lost.
    /// SubmitJob takes no idempotency token, so a coordinator such a launch may have started is
    /// looked for on each of these as well as the deploy's queue now
    /// ([`stop_unbound_coordinators`]): a deploy may move the queue while the run waits. Additive,
    /// with no [`LAUNCH_PLAN_VERSION`] bump: a build that does not know it looks on the current
    /// queue alone, as builds before it did.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attempted_queues: Vec<String>,
    /// When (epoch micros) the latest launch of the parked run that may have created a coordinator
    /// it does not know of began: recorded by submit when it parks a launch whose SubmitJob answer
    /// was lost ([`SubmitError::LaunchUncertain`]), and by the sweep before each SubmitJob
    /// ([`record_launch_attempt`]: a sweep that does not outlive SubmitJob, or whose answer is
    /// lost, leaves it; a definite answer clears it). Batch lists a job it just created only
    /// eventually, so for [`LOST_LAUNCH_LISTED_WITHIN`] after it a listing that finds no
    /// coordinator created since does not prove none was started ([`unsettled_lost_launch`]); an
    /// older one is an earlier launch's. Additive, with no [`LAUNCH_PLAN_VERSION`] bump: a build
    /// that does not know it trusts the listing at once, as builds before it did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launch_uncertain_at_micros: Option<i64>,
    /// When (epoch micros) the answer of that launch was found lost: after Batch accepted the job,
    /// if it did. [`LOST_LAUNCH_LISTED_WITHIN`] counts from here when it is known, so the whole
    /// listing window follows Batch's acceptance, however long the launch took to get there; a
    /// launch whose sweep did not outlive SubmitJob has only
    /// [`Self::launch_uncertain_at_micros`]. Additive, with no [`LAUNCH_PLAN_VERSION`] bump.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launch_answer_lost_at_micros: Option<i64>,
}

impl LaunchPlan {
    /// The job queue (ARN) the run's coordinator is submitted to: the deploy's coordinator queue,
    /// or, for a single-job run, its queue-class's queue as the deploy has it now. `Err` says why
    /// there is none (the queue-class was removed while the run waited).
    pub(crate) fn coordinator_queue(&self, state: &AppState) -> Result<String, String> {
        queue_for(state, self.single_job.as_ref())
    }
}

/// The deploy's job queue (ARN) now for a run's coordinator: the coordinator queue, or, for a
/// single-job run (`single`), its queue-class's. `Err` says why there is none.
fn queue_for(
    state: &AppState,
    single: Option<&biffix_core::event::SingleJobUse>,
) -> Result<String, String> {
    match single {
        None => Ok(state.deploy.coordinator_queue.clone()),
        Some(single) => crate::runs::single_job::queue(single, &state.deploy.single_job_queues)
            .map(str::to_string),
    }
}

/// [`queue_for`] a parked run from its stored `plan` as JSON (its `single_job` alone), so a plan
/// that no longer reads, or one kept for cleanup only, still names it. `None` when there is none.
fn current_queue(state: &AppState, plan: &serde_json::Value) -> Option<String> {
    let single = match plan.get("single_job") {
        None | Some(serde_json::Value::Null) => None,
        Some(single) => Some(serde_json::from_value(single.clone()).ok()?),
    };
    queue_for(state, single.as_ref()).ok()
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
    /// Registering one of the run's task job definitions (`what` names its image and definition),
    /// or writing their map (`what` names it), failed.
    #[error("preparing the run's task job definitions failed: {what}: {source}")]
    JobDefinitions {
        what: String,
        #[source]
        source: AwsError,
    },
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
            LaunchError::ImageMap(e)
            | LaunchError::JobDefinitions { source: e, .. }
            | LaunchError::Submit(SubmitError::Aws(e)) => matches!(e, AwsError::Unavailable(_)),
            LaunchError::Submit(SubmitError::LaunchUncertain(_)) => true,
            LaunchError::Submit(_) => false,
            LaunchError::Record { error, stop_error, .. } => {
                stop_error.is_none() && matches!(error, StoreError::Unavailable(_))
            }
        }
    }
}

/// Launch `run_id` from `plan`: write its image map (the version's images, with the submit-time
/// ones winning every spelling they have), register a task job definition for each image in it
/// and write their map ([`write_job_definitions`]), run the submit pipeline (mint session → anchor
/// → launch coordinator), and record the anchor and the coordinator binding.
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
    let single_job = plan.single_job.as_ref();
    let image_map = if project.bucket.is_empty() {
        None
    } else {
        let worker = crate::aws::batch::worker_image(
            &version.image_digest,
            crate::runs::submit::worker_image_repo_uri(&version).as_deref(),
            &state.deploy.worker_image_repo,
            version.engine,
            &variant.image,
            single_job.is_some(),
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
        // Every task runs on a job definition biffix registered (never one the workflow's code
        // chose), so the run's definitions exist before its coordinator does. A single-job run's
        // tasks run inside its one job, on no job definition of their own.
        let job_definitions = match single_job {
            None => Some(
                write_job_definitions(state, run_id, &project.launch_project(), &version, &map)
                    .await?,
            ),
            Some(_) => None,
        };
        Some((crate::aws::batch::RunMapUris { image_map: uri, job_definitions }, map))
    };
    let coordinator_queue = plan
        .coordinator_queue(state)
        .map_err(|e| LaunchError::Submit(SubmitError::Aws(AwsError::Invalid(e))))?;
    let sreq = SubmitRequest {
        run_id: run_id.clone(),
        version,
        source: source.clone(),
        plan: plan.clone(),
        coordinator_queue,
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

/// How many task job definitions one launch looks up (or registers) at once: enough that a
/// version with many images launches promptly, few enough to stay well inside Batch's API rate.
const TASK_DEFINITION_CONCURRENCY: usize = 8;

/// Register (or find) the task job definition of each image in the run's `image_map`
/// (`crate::aws::task_definitions`) and write the map of image to definition beside it; returns
/// where.
async fn write_job_definitions(
    state: &AppState,
    run_id: &RunId,
    project: &crate::aws::batch::LaunchProject,
    version: &WorkflowVersion,
    image_map: &BTreeMap<String, String>,
) -> Result<String, LaunchError> {
    use futures::{StreamExt, TryStreamExt};
    let definitions =
        crate::aws::task_definitions::for_image_map(image_map, version.engine, project);
    let registered: BTreeMap<String, String> = futures::stream::iter(definitions)
        .map(|definition| async move {
            match state.cloud.task_job_definition(&definition).await {
                Ok(arn) => Ok((definition.image, arn)),
                Err(source) => Err(LaunchError::JobDefinitions {
                    what: format!("the definition of image `{}`", definition.image),
                    source,
                }),
            }
        })
        .buffer_unordered(TASK_DEFINITION_CONCURRENCY)
        .try_collect()
        .await?;
    let uri = crate::aws::task_definitions::uri_for(&project.bucket, run_id.as_str());
    let body = serde_json::to_vec_pretty(&registered).expect("a map of strings always serializes");
    state.cloud.put_object(&uri, body, "application/json").await.map_err(|source| {
        LaunchError::JobDefinitions { what: format!("writing their map {uri}"), source }
    })?;
    Ok(uri)
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
    /// Parked launches of runs that will never launch (ended, or never recorded), kept for cleanup
    /// only: a coordinator an earlier launch of the run may have left running unbound could not be
    /// stopped yet ([`release`]).
    pub cleaning_up: usize,
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
    CleaningUp,
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
/// is logged and counted, and its launch stays parked for the next sweep. A run that will never
/// launch (failed, cancelled, ended otherwise, never recorded) has its request dropped once no
/// coordinator an earlier launch of it may have started is running ([`release`]). Requests kept
/// for cleanup only go after the runs that may launch, all but one ([`sweep_order`]).
pub async fn sweep(state: &AppState) -> Result<LaunchSweepReport, BuildError> {
    let rows = sweep_order(state.store.list_pending_launches().await?);
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
    for (i, row) in rows.into_iter().enumerate() {
        // A kept request stepped first (`sweep_order`) gets a bounded turn, so however slow its
        // cleanup's Batch calls are, the runs that may launch still get the rest of the sweep.
        // Its try is recorded before its cleanup ([`record_cleanup_try`]), so the next sweep
        // steps it after the others; the cleanup is retried then (each of its steps is).
        let step = if i == 0 && row.cleanup_only() {
            match tokio::time::timeout(KEPT_FIRST_TURN, sweep_one(state, &row)).await {
                Ok(step) => step,
                Err(_) => {
                    tracing::warn!(run_id = %row.run_id, limit_secs = KEPT_FIRST_TURN.as_secs(), "launch sweep: a kept request's cleanup took its whole turn; stepping the runs that may launch, and trying it again on a later sweep");
                    report.cleaning_up += 1;
                    continue;
                }
            }
        } else {
            sweep_one(state, &row).await
        };
        match step {
            Ok(Swept::Launched) => report.launched += 1,
            Ok(Swept::Waiting) => report.waiting += 1,
            Ok(Swept::Failed) => report.failed += 1,
            Ok(Swept::Cancelled) => report.cancelled += 1,
            Ok(Swept::Finished) => report.finished += 1,
            Ok(Swept::Dropped) => report.dropped += 1,
            Ok(Swept::CleaningUp) => report.cleaning_up += 1,
            Err(e) => {
                report.errors += 1;
                tracing::warn!(run_id = %row.run_id, error = %e, "launch sweep: step failed");
            }
        }
    }
    Ok(report)
}

/// `rows` (oldest first) in the order the sweep steps them: one request kept for cleanup only
/// ([`PendingLaunch::cleanup_only`]), then every run that may still launch, oldest first, then the
/// other kept requests. A kept request's cleanup is retried every sweep and may take its Batch
/// calls' time to fail again, so a sweep that runs out of time does so on those, not before a run
/// that is ready to launch; but one goes first, for at most [`KEPT_FIRST_TURN`] ([`sweep`]), so
/// runs enough to fill a sweep's time cannot keep every coordinator a kept request names running,
/// nor that one's cleanup keep every run from launching. Kept requests go least recently tried first
/// ([`PendingLaunch::cleanup_tried_at_micros`], which [`record_cleanup_try`] writes before each
/// try; never tried first, then oldest first): a request a sweep did not reach goes before the one
/// it ran out of time on, so each gets its turn, the first included.
fn sweep_order(rows: Vec<PendingLaunch>) -> Vec<PendingLaunch> {
    let (mut cleanup, live): (Vec<_>, Vec<_>) =
        rows.into_iter().partition(PendingLaunch::cleanup_only);
    // Stable: equal times keep the listing's oldest-first order.
    cleanup.sort_by_key(|row| row.cleanup_tried_at_micros().unwrap_or(i64::MIN));
    let mut cleanup = cleanup.into_iter();
    cleanup.next().into_iter().chain(live).chain(cleanup).collect()
}

/// The longest the kept request [`sweep_order`] steps first may take ([`sweep`]): a fifth of the
/// sweep Lambda's 5 minutes, so the runs that may launch always get the rest.
const KEPT_FIRST_TURN: Duration = Duration::from_secs(60);

/// Record now, in parked request `row` (just read under the run's launch claim) when it is kept for
/// cleanup only, as when a sweep last tried its cleanup ([`PENDING_CLEANUP_TRIED_KEY`]), before the
/// sweep tries it: should the sweep run out of time on it, the next steps it after every kept
/// request it did not reach ([`sweep_order`]). Returns the row as recorded. A failed write is
/// logged, and the row returned as it was: the cleanup is tried all the same.
async fn record_cleanup_try(state: &AppState, mut row: PendingLaunch) -> PendingLaunch {
    if !row.cleanup_only() {
        return row;
    }
    let before = row.plan.clone();
    let Some(plan) = row.plan.as_object_mut() else {
        return row;
    };
    plan.insert(PENDING_CLEANUP_TRIED_KEY.into(), biffix_core::clock::epoch_micros().into());
    if let Err(e) = state.store.put_pending_launch(&row).await {
        tracing::warn!(run_id = %row.run_id, error = %e, "launch sweep: recording when a kept request's cleanup was tried failed; the next sweep may try it before others again");
        row.plan = before;
    }
    row
}

/// How long ago parked request `row` was parked.
fn parked_for(row: &PendingLaunch) -> Duration {
    let now = biffix_core::clock::epoch_micros();
    Duration::from_micros(u64::try_from(now.saturating_sub(row.created_at_micros)).unwrap_or(0))
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
    let row = &record_cleanup_try(state, row).await;
    let age = parked_for(row);
    let run_state = state.store.run_state(run_id).await?;
    // A launch left a coordinator it could neither record nor stop: never launch again.
    if let Some(job) = row.orphan_job_id() {
        return settle_orphan(state, row, job, age, run_state.as_deref()).await;
    }
    // A run that will never launch keeps its request, for cleanup only, while a coordinator an
    // earlier launch of it may have left running cannot be stopped ([`release`]).
    let released = |swept: Swept| async move {
        Ok(if release(state, row, age).await? { swept } else { Swept::CleaningUp })
    };
    match run_state.as_deref().map(str::parse::<RunState>) {
        // Not recorded: a submit still recording it, or one whose record failed (an orphan).
        None if age < ORPHAN_AFTER && !row.cleanup_only() => return Ok(Swept::Waiting),
        None => return released(Swept::Dropped).await,
        Some(Ok(RunState::Cancelled)) => return released(Swept::Cancelled).await,
        Some(Ok(s)) if s.is_terminal() => return released(Swept::Finished).await,
        // Launched already (its request outlived the launch): never launch it again. Its request
        // is dropped at once: every coordinator an earlier launch started was stopped before the
        // launch that bound it, and the coordinator Batch lists for it now is its own.
        Some(s) if s != Ok(RunState::PreparingImages) => {
            drop_row(state, run_id).await?;
            return Ok(Swept::Dropped);
        }
        // Kept for cleanup only: its failure is recorded, though not folded yet. Never launched,
        // nor failed again.
        Some(_) if row.cleanup_only() => return released(Swept::Finished).await,
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
                return fail_parked(state, row, age, Stage::Launch, &reason).await;
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
            return fail_timed_out(state, run_id, age, stage, &reason).await;
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
        return fail_timed_out(state, run_id, age, wait.stage, &reason).await;
    }
    Ok(Swept::Waiting)
}

/// [`fail_parked`] for parked run `run_id` that timed out in this sweep's step (`age` after it was
/// parked), from its row read again (still under the sweep's claim): the step may have recorded
/// a queue it launched on since [`sweep_one`] read it ([`record_launch_attempt`]), which a
/// cleanup kept from the older row would drop. A failed read fails the step, before any cleanup;
/// a row since dropped only has the run's failure recorded.
async fn fail_timed_out(
    state: &AppState,
    run_id: &RunId,
    age: Duration,
    stage: Stage,
    reason: &str,
) -> Result<Swept, BuildError> {
    match state.store.get_pending_launch(run_id).await? {
        Some(row) => fail_parked(state, &row, age, stage, reason).await,
        None => {
            record_failure(state, run_id, stage, reason).await?;
            Ok(Swept::Failed)
        }
    }
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
            return refused(state, run_id, plan, Stage::Launch, &reason).await;
        }
        Err(e) => return Err(at_launch(e.into())),
    };
    let ctx = VersionContext::of(state, &rec.version).await.map_err(at_launch)?;
    let statuses = ensure_submit_images(state, &ctx, &plan.images).await.map_err(at_images)?;
    match readiness(&statuses) {
        ImagesReadiness::Failed(reason) => {
            refused(state, run_id, plan, Stage::ImagePreparation, &reason).await
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
        return refused(state, run_id, plan, Stage::Launch, &reason).await;
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
            return refused(state, run_id, plan, Stage::Launch, &reason).await;
        }
    };
    if let Some(reason) = crate::register::pipeline::staged_outside_project(rec, project) {
        return refused(state, run_id, plan, Stage::Launch, &reason).await;
    }
    if let Some(uri) = outside_project(plan, &project.bucket) {
        let reason = format!(
            "{uri} is outside this run's project's bucket s3://{}/ (the deploy's projects \
             changed while it waited); resubmit it",
            project.bucket
        );
        return refused(state, run_id, plan, Stage::Launch, &reason).await;
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
        return refused(state, run_id, plan, Stage::Launch, &reason).await;
    }
    let mut version = rec.version.clone();
    let image_source_digest = runnable_default(&images, &mut version);
    // The coordinator runs on the deploy's queue as it is now, not as it was at submit: a deploy
    // may have replaced the queue while the run waited (as the run page labels the host). A
    // single-job run's is its queue-class's. Checked before the variant, which may register a job
    // definition.
    let queue = match plan.coordinator_queue(state) {
        Ok(queue) => queue,
        Err(reason) => return refused(state, run_id, plan, Stage::Launch, &reason).await,
    };
    // A deploy may also have given the queue-class hosts the run's whole-host job no longer fills.
    if let Some(single) = &plan.single_job {
        match crate::runs::single_job::recheck(state, single).await {
            Ok(Ok(architecture)) => {
                let worker = crate::aws::batch::worker_image(
                    &version.image_digest,
                    crate::runs::submit::worker_image_repo_uri(&version).as_deref(),
                    &state.deploy.worker_image_repo,
                    version.engine,
                    "",
                    true,
                );
                if let Err(reason) = crate::runs::single_job::check_images(
                    architecture,
                    &images,
                    submit_images,
                    worker.as_deref(),
                ) {
                    return refused(state, run_id, plan, Stage::Launch, &reason).await;
                }
            }
            Ok(Err(reason)) => return refused(state, run_id, plan, Stage::Launch, &reason).await,
            Err(e) => return wait(format!("describing the single-job hosts failed: {e}")),
        }
    }
    let template = match project.variant_template(plan.single_job.is_some()) {
        Ok(template) => template,
        Err(reason) => return refused(state, run_id, plan, Stage::Launch, &reason).await,
    };
    let variant = match crate::images::variants::launch_variant(state, &rec.version, template).await
    {
        Ok(variant) => variant,
        Err(e @ (NotRunnable::Retired(_) | NotRunnable::Outdated | NotRunnable::Failed { .. })) => {
            return refused(state, run_id, plan, Stage::Launch, &e.to_string()).await
        }
        Err(e @ NotRunnable::Building(_)) => return wait(e.to_string()),
        Err(NotRunnable::Error(e)) => return Err(at_launch(e)),
    };
    match stop_earlier_coordinators(state, run_id, plan, Some(&queue)).await {
        Err(reason) => return wait(reason),
        // Batch lists a stopped job until it ends, so a launch now whose answer was lost could
        // find only that job and take it for its own: the run launches once none is listed.
        Ok(stopped) if !stopped.is_empty() => {
            // One a launch whose answer was lost may have created is now stopped: that launch is
            // settled, and the next sweep need not wait for Batch to list it.
            let lost_launch = plan.launch_uncertain_at_micros;
            if lost_launch.is_some_and(|at| stopped.iter().any(|j| j.may_be_from_launch_at(at))) {
                clear_launch_attempt(state, run_id).await;
            }
            return wait(format!(
                "stopped {} coordinator(s) an earlier launch of the run started unbound; it \
                 launches once Batch no longer lists them",
                stopped.len()
            ));
        }
        Ok(_) => {}
    }
    // Before SubmitJob, so a launch whose answer is lost (or that this process does not outlive)
    // is looked for on this queue even if a deploy moves the run's queue meanwhile, and an empty
    // listing right after it is not taken as proof there is none.
    if let Err(e) = record_launch_attempt(state, run_id, &queue).await {
        return wait(format!("recording the launch it is about to make failed: {e}"));
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
            // A lost answer keeps the launch's record (`record_launch_attempt`), and adds when it
            // was found lost; any other failure created no coordinator.
            if matches!(e, LaunchError::Submit(SubmitError::LaunchUncertain(_))) {
                record_answer_lost(state, run_id).await;
            } else {
                clear_launch_attempt(state, run_id).await;
            }
            return wait(format!("launching failed: {e}"));
        }
        Err(LaunchError::Record { error, job_id, stop_error: Some(_) }) => {
            // Its coordinator may be running, unbound: mark the row so no sweep launches the run
            // again, then settle it now (the next sweep does, if this cannot).
            tracing::error!(run_id = %run_id, job_id = %job_id, error = %error, "launch sweep: a coordinator launched but neither recorded nor stopped");
            if let Err(e) = state.store.mark_pending_launch_orphan(run_id, &job_id).await {
                tracing::error!(run_id = %run_id, job_id = %job_id, error = %e, "launch sweep: marking the unbound coordinator failed; a later sweep may launch the run again");
            }
            let recording = |e: StoreError| StepError::Recording(e.into());
            let run_state = state.store.run_state(run_id).await.map_err(recording)?;
            // The row as it is now, marked (here, should marking have failed) so that a request
            // kept for cleanup still names the coordinator.
            let mut row =
                state.store.get_pending_launch(run_id).await.map_err(recording)?.unwrap_or_else(
                    || PendingLaunch {
                        run_id: run_id.clone(),
                        created_at_micros: biffix_core::clock::epoch_micros(),
                        plan: serde_json::json!({ "attempted_queues": [queue] }),
                    },
                );
            if let Some(plan) = row.plan.as_object_mut() {
                plan.insert(PENDING_ORPHAN_KEY.into(), job_id.clone().into());
            }
            return settle_orphan(state, &row, &job_id, parked_for(&row), run_state.as_deref())
                .await
                .map(Ok)
                .map_err(StepError::Recording);
        }
        Err(e) => {
            clear_launch_attempt(state, run_id).await;
            return failed(state, run_id, Stage::Launch, &e.to_string()).await;
        }
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

/// Fail parked run `run_id` at `stage` for good, before its launch, with `reason`; but first stop
/// every coordinator an earlier launch of it may have started ([`stop_earlier_coordinators`]):
/// failing drops the parked request, and with it the last sweep that would look for one, which
/// would otherwise run on unbound. While that cannot be done the run waits instead, still parked
/// (and past the bound times out, saying why).
async fn refused(
    state: &AppState,
    run_id: &RunId,
    plan: &LaunchPlan,
    stage: Stage,
    reason: &str,
) -> Result<Result<Swept, Wait>, StepError> {
    let queue = plan.coordinator_queue(state).ok();
    if let Err(why) = stop_earlier_coordinators(state, run_id, plan, queue.as_deref()).await {
        let reasons = vec![format!("{why}; the run then fails: {reason}")];
        return Ok(Err(Wait { stage, reasons }));
    }
    failed(state, run_id, stage, reason).await
}

/// The queues an earlier launch of a parked run was submitted to ([`LaunchPlan::attempted_queues`]),
/// read from its stored `plan` as JSON, so an unreadable plan, or a newer biffix's, still names them.
fn attempted_queues(plan: &serde_json::Value) -> Vec<String> {
    plan.get("attempted_queues")
        .and_then(serde_json::Value::as_array)
        .map(|queues| queues.iter().filter_map(|q| q.as_str().map(str::to_string)).collect())
        .unwrap_or_default()
}

/// Drop the parked request `row` of a run that will never launch (it failed, was cancelled, ended
/// otherwise, or was never recorded), `age` after it was parked, once no coordinator a launch of it
/// may have left running unbound is still running: the one it knows by id (its unbound
/// coordinator, [`PendingLaunch::orphan_job_id`]) stopped, and [`stop_unbound_coordinators`] on
/// every queue its launches were submitted to and on the deploy's queue for the run now. The
/// request is the last record of those, so while that cleanup fails it is kept, for cleanup only
/// ([`keep_for_cleanup`]): no sweep launches it, or fails it again, and each one tries the cleanup
/// again until it succeeds (an error is logged once, [`CLEANUP_ESCALATE_AFTER`] after the run was
/// parked). Returns whether the request was dropped.
async fn release(state: &AppState, row: &PendingLaunch, age: Duration) -> Result<bool, BuildError> {
    release_stopped(state, row, age, OrphanStop::Pending).await
}

/// The stop of the unbound coordinator a parked request names ([`PendingLaunch::orphan_job_id`]),
/// for [`release_stopped`].
enum OrphanStop {
    /// Not tried yet: [`release_stopped`] stops it (when the request names one), alongside its
    /// queues' cleanup.
    Pending,
    /// Tried already, by [`settle_orphan`] (to name its outcome in the run's failure): its result.
    Tried(Result<(), String>),
}

/// Why stopping coordinator `job`, which a launch of the run left unbound, failed (`e`).
fn orphan_stop_error(job: &str, e: &dyn std::fmt::Display) -> String {
    format!("stopping coordinator {job}, which a launch of the run left unbound, failed: {e}")
}

/// [`release`], the stop of the unbound coordinator `row` names (when it names one) `orphan`:
/// pending, made here, or already tried.
async fn release_stopped(
    state: &AppState,
    row: &PendingLaunch,
    age: Duration,
    orphan: OrphanStop,
) -> Result<bool, BuildError> {
    let run_id = &row.run_id;
    let current = current_queue(state, &row.plan);
    // Always the deploy's queue for the run too (when it still has one): a plan parked by a
    // biffix from before `attempted_queues` was recorded names no queue, though its launch's
    // answer may have been lost, leaving a coordinator there (the queue of every launch then).
    // Recorded before any Batch call made here ([`record_current_queue`]), so a step cut short
    // there (the sweep's time, or the first turn's, running out) leaves it recorded for the next
    // sweep though a deploy moves the run's queue meanwhile; until it is, the request is kept as
    // it is. (A stop [`settle_orphan`] tried already went first: it is by id, and the queue of the
    // launch that left that coordinator was recorded before that launch.)
    let row = &match record_current_queue(state, row, current.as_deref()).await {
        Ok(row) => row,
        Err(e) => {
            tracing::warn!(run_id = %run_id, error = %e, "launch sweep: recording the run's queue before its cleanup failed; the request is kept as it was");
            return Ok(false);
        }
    };
    let queues = attempted_queues(&row.plan);
    // Before any listing: each is as of when it was asked for, which this is no later than.
    let listed_from = biffix_core::clock::epoch_micros();
    let mut found = Vec::new();
    // The queues whose cleanup failed: kept with the request, so a later sweep looks there even
    // if a deploy moves the run's queue meanwhile.
    let mut failed_on = Vec::new();
    // The unbound coordinator is stopped by id, here alongside the listings or already (Batch
    // lists it until it ends): left to that stop. A request already kept for cleanup was warned
    // about once: quiet from then on.
    let skip = Found { skip: row.orphan_job_id(), quiet: row.cleanup_only() };
    // The unbound coordinator stopped by id, when not tried yet.
    let stop_orphan_by_id = async {
        match (orphan, row.orphan_job_id()) {
            (OrphanStop::Tried(stopped), _) => stopped.err(),
            (OrphanStop::Pending, Some(job)) => stop_orphan(state, run_id, job, row.cleanup_only())
                .await
                .err()
                .map(|e| orphan_stop_error(job, &e)),
            (OrphanStop::Pending, None) => None,
        }
    };
    // Every queue at once, and the unbound coordinator's stop with them, whatever another's
    // answer, so one whose Batch calls hang delays none of the others, nor uses up a bounded turn
    // ([`KEPT_FIRST_TURN`]) before they are looked on.
    let batch = futures::future::join(
        stop_orphan_by_id,
        futures::future::join_all(queues.iter().map(|queue| {
            let is_current = current.as_deref() == Some(queue.as_str());
            stop_unbound_coordinators(state, run_id, queue, is_current, skip)
        })),
    );
    // Bounded, the first cleanup of a run too (its request not kept yet, so stepped among the runs
    // that may launch, oldest first): one whose Batch calls hang is kept for cleanup only, its
    // queues recorded, and later sweeps step it after those runs, rather than this one hanging
    // ahead of them every sweep.
    let Ok((orphan_error, looked)) = tokio::time::timeout(CLEANUP_BATCH_CALLS, batch).await else {
        let why = format!(
            "its cleanup's Batch calls took over {} s; a later sweep tries them again",
            CLEANUP_BATCH_CALLS.as_secs()
        );
        keep_unreleased(state, row, age, &queues, &why, &[]).await;
        return Ok(false);
    };
    let mut errors: Vec<String> = orphan_error.into_iter().collect();
    for (queue, looked) in queues.iter().zip(looked) {
        match looked {
            Ok(jobs) => found.extend(jobs),
            Err(why) => {
                errors.push(why);
                failed_on.push(queue.clone());
            }
        }
    }
    let uncertain_at = row.plan.get(LAUNCH_UNCERTAIN_KEY).and_then(serde_json::Value::as_i64);
    let lost_at = row.plan.get(LAUNCH_ANSWER_LOST_KEY).and_then(serde_json::Value::as_i64);
    // The unbound coordinator, known by id, is the latest launch's own: its answer arrived.
    let settled = row.orphan_job_id().is_some()
        || uncertain_at.is_some_and(|at| found.iter().any(|job| job.may_be_from_launch_at(at)));
    errors.extend(unsettled_lost_launch(uncertain_at, lost_at, settled, listed_from));
    if let Err(why) = joined(errors) {
        keep_unreleased(state, row, age, &queues, &why, &failed_on).await;
        return Ok(false);
    }
    drop_row(state, run_id).await?;
    Ok(true)
}

/// The longest [`release_stopped`]'s Batch calls (the unbound coordinator's stop, and every
/// queue's cleanup) may take: three quarters of a kept request's first turn ([`KEPT_FIRST_TURN`]),
/// so the request is kept, and its try recorded, before that turn ends. A launch's look for an
/// earlier launch's coordinators ([`stop_earlier_coordinators`]) and [`settle_orphan`]'s stop of
/// an unbound coordinator are bounded by it too. A run whose launch is held up so waits, still
/// stepped among the runs that may launch, oldest first: it costs each sweep at most this long,
/// but enough such runs (about six) would still fill a sweep before a newer ready run's turn.
const CLEANUP_BATCH_CALLS: Duration = Duration::from_secs(45);

/// Keep parked request `row` (`age` after it was parked), whose cleanup on `queues` failed (`why`;
/// on `failed_on` among others), for cleanup only ([`keep_for_cleanup`]), escalating it once
/// [`CLEANUP_ESCALATE_AFTER`] after it was parked.
async fn keep_unreleased(
    state: &AppState,
    row: &PendingLaunch,
    age: Duration,
    queues: &[String],
    why: &str,
    failed_on: &[String],
) {
    let run_id = &row.run_id;
    let escalate = age > CLEANUP_ESCALATE_AFTER && !row.cleanup_escalated();
    if escalate {
        tracing::error!(
            run_id = %run_id,
            queues = ?queues,
            unbound_job_id = row.orphan_job_id(),
            error = why,
            "launch sweep: still cannot stop a coordinator a launch of the run may have left \
             running, {} days after it was parked; its request is kept and every sweep keeps \
             trying (logged once); look for it on these queues (and stop the unbound job, if \
             one is named)",
            CLEANUP_ESCALATE_AFTER.as_secs() / 86_400
        );
    }
    keep_for_cleanup(state, row, why, escalate, failed_on).await;
}

/// Keep parked request `row`, which [`release`] could not clean up (`why`), for cleanup only. The
/// first time, its plan is scrubbed to what the cleanup reads ([`cleanup_plan`]: not the run's
/// params, which carry secret values) and the keep is logged; later sweeps keep it quietly (every
/// 5-minute sweep would otherwise log it), until the cleanup succeeds. `escalate` marks it
/// [`PENDING_ESCALATED_KEY`] (its error just logged), so that error is not logged again. Each of
/// `failed_on`, the queues whose cleanup failed, is added to its [`LaunchPlan::attempted_queues`]
/// (written then even when the request was kept before), so the next sweep looks there though a
/// deploy moves the run's queue. A failed write is logged, and the request kept as it was.
async fn keep_for_cleanup(
    state: &AppState,
    row: &PendingLaunch,
    why: &str,
    escalate: bool,
    failed_on: &[String],
) {
    let run_id = &row.run_id;
    let mut tried = attempted_queues(&row.plan);
    let known = tried.len();
    for queue in failed_on {
        if !tried.contains(queue) {
            tried.push(queue.clone());
        }
    }
    if row.cleanup_only() && !escalate && tried.len() == known {
        tracing::debug!(run_id = %run_id, error = why, "launch sweep: still keeping a run's request until a coordinator a launch of it may have started is stopped");
        return;
    }
    if !row.cleanup_only() {
        tracing::warn!(run_id = %run_id, error = why, "launch sweep: keeping a run's request, for cleanup only, until a coordinator a launch of it may have started is stopped (each sweep tries again; logged once)");
    }
    let mut plan = cleanup_plan(&row.plan);
    if let Some(plan) = plan.as_object_mut() {
        if !tried.is_empty() {
            plan.insert("attempted_queues".into(), serde_json::json!(tried));
        }
        if escalate {
            plan.insert(PENDING_ESCALATED_KEY.into(), true.into());
        }
    }
    let scrubbed =
        PendingLaunch { run_id: run_id.clone(), created_at_micros: row.created_at_micros, plan };
    // A few tries: the write may carry the only record of a queue a coordinator was found on.
    let mut attempt = 1;
    while let Err(e) = state.store.put_pending_launch(&scrubbed).await {
        if attempt == KEEP_WRITE_ATTEMPTS {
            tracing::warn!(run_id = %run_id, error = %e, attempts = attempt, "launch sweep: writing a request kept for cleanup failed; it is kept as it was, params included if it was not scrubbed yet, and without a queue whose cleanup just failed");
            return;
        }
        tokio::time::sleep(KEEP_WRITE_BACKOFF * attempt).await;
        attempt += 1;
    }
}

/// How many times [`keep_for_cleanup`] tries its write, [`KEEP_WRITE_BACKOFF`] times the attempt
/// apart: enough to ride out a blip of the store within one sweep, which a later sweep's retry
/// may come too late for (a deploy moving the run's queue between the two).
const KEEP_WRITE_ATTEMPTS: u32 = 3;
const KEEP_WRITE_BACKOFF: Duration = Duration::from_millis(100);

/// A parked `plan` kept for cleanup only: what [`release`] reads of it (the queues its launches
/// were submitted to, its single-job use, which names the deploy's queue for it now, the
/// coordinator a launch left unbound, whether its failure was escalated, and when a sweep last
/// tried its cleanup), marked [`PENDING_CLEANUP_KEY`].
fn cleanup_plan(plan: &serde_json::Value) -> serde_json::Value {
    let mut kept: serde_json::Map<String, serde_json::Value> = [
        "attempted_queues",
        "single_job",
        LAUNCH_UNCERTAIN_KEY,
        LAUNCH_ANSWER_LOST_KEY,
        PENDING_ORPHAN_KEY,
        PENDING_ESCALATED_KEY,
        PENDING_CLEANUP_TRIED_KEY,
    ]
    .into_iter()
    .filter_map(|key| Some((key.to_string(), plan.get(key)?.clone())))
    .collect();
    kept.insert(PENDING_CLEANUP_KEY.into(), true.into());
    serde_json::Value::Object(kept)
}

/// `Ok` when `errors` is empty, else them joined.
fn joined(errors: Vec<String>) -> Result<(), String> {
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

/// [`stop_unbound_coordinators`] on every queue an earlier launch of parked run `run_id` was
/// submitted to ([`LaunchPlan::attempted_queues`]) and on `queue`, the deploy's queue for it now
/// (`None` when it has none: its queue-class was removed): on all of them at once, whatever
/// another's answer, `queue` recorded with the request first. Finding none created since a launch
/// whose answer was lost began, right after it, proves nothing yet ([`unsettled_lost_launch`]), so
/// the run waits then too, as it does when they take over [`CLEANUP_BATCH_CALLS`]. `Ok` is the
/// coordinators it found (and stopped).
async fn stop_earlier_coordinators(
    state: &AppState,
    run_id: &RunId,
    plan: &LaunchPlan,
    queue: Option<&str>,
) -> Result<Vec<LiveCoordinator>, String> {
    // `queue` recorded with the request before any Batch call, so a later sweep looks there even
    // if a deploy moves the run's queue meanwhile (a plan from before `attempted_queues` names
    // none), however this step ends. Until it is, the run waits, and nothing is looked for.
    if let Some(queue) = queue.filter(|q| !plan.attempted_queues.iter().any(|t| t == q)) {
        if let Err(e) = record_attempted_queue(state, run_id, queue).await {
            return Err(format!(
                "recording the queue a coordinator is looked for on ({queue}) failed: {e}"
            ));
        }
    }
    let mut errors = Vec::new();
    let mut found = Vec::new();
    // Before any listing: each is as of when it was asked for, which this is no later than.
    let listed_from = biffix_core::clock::epoch_micros();
    // Every queue at once (the earlier launches' and the deploy's for the run now), whatever
    // another's answer, so one whose Batch calls hang delays none of the others.
    let queues: Vec<(&str, bool)> = plan
        .attempted_queues
        .iter()
        .map(String::as_str)
        .filter(|q| Some(*q) != queue)
        .map(|q| (q, false))
        .chain(queue.map(|q| (q, true)))
        .collect();
    let looking = futures::future::join_all(queues.iter().map(|&(q, current)| {
        stop_unbound_coordinators(state, run_id, q, current, Found::default())
    }));
    // Bounded, as a release's are: a run whose Batch calls hang waits (its queues are recorded,
    // so the next sweep looks on them again), and the runs after it still get the sweep.
    let Ok(looked) = tokio::time::timeout(CLEANUP_BATCH_CALLS, looking).await else {
        return Err(format!(
            "looking for coordinators an earlier launch of the run may have started took over {} \
             s; a later sweep looks again",
            CLEANUP_BATCH_CALLS.as_secs()
        ));
    };
    for looked in looked {
        match looked {
            Ok(jobs) => found.extend(jobs),
            Err(why) => errors.push(why),
        }
    }
    let uncertain_at = plan.launch_uncertain_at_micros;
    let settled =
        uncertain_at.is_some_and(|at| found.iter().any(|job| job.may_be_from_launch_at(at)));
    let lost_at = plan.launch_answer_lost_at_micros;
    errors.extend(unsettled_lost_launch(uncertain_at, lost_at, settled, listed_from));
    joined(errors).map(|()| found)
}

/// Why a cleanup does not yet prove no coordinator is left: the latest launch whose outcome is
/// unknown (begun at `uncertain_at`, epoch micros; its answer found lost at `lost_at`, when known,
/// from which the window counts) was less than [`LOST_LAUNCH_LISTED_WITHIN`] old at `listed_from`
/// (epoch micros, read before the cleanup's first listing: a listing asked for before the window
/// closed says nothing, whenever its answer came) and it is not
/// `settled` (no coordinator it may have created was found: one Batch created since
/// it began, now stopped; an earlier launch's older one, still listed while it ends, says nothing
/// of it), so Batch may not list the coordinator it created yet. `None` once settled, once the
/// launch is older, or when no launch's outcome is unknown.
fn unsettled_lost_launch(
    uncertain_at: Option<i64>,
    lost_at: Option<i64>,
    settled: bool,
    listed_from: i64,
) -> Option<String> {
    let at = uncertain_at.filter(|_| !settled)?;
    let at = lost_at.map_or(at, |lost| lost.max(at));
    // Saturating: a stored time far out of range (a damaged row) reads as long ago, not wrap.
    let ago = Duration::from_micros(u64::try_from(listed_from.saturating_sub(at)).unwrap_or(0));
    (ago < LOST_LAUNCH_LISTED_WITHIN).then(|| {
        format!(
            "a launch whose answer was lost {} s ago may have started a coordinator Batch does not \
             list yet; it is looked for again until {} min after that launch",
            ago.as_secs(),
            LOST_LAUNCH_LISTED_WITHIN.as_secs() / 60
        )
    })
}

/// Stop every live coordinator of parked run `run_id` (on `queue`) before the sweep launches it, or
/// fails it for good ([`refused`]): a run still preparing has no coordinator of its own, so one
/// found was started by a launch whose SubmitJob answer was lost ([`SubmitError::LaunchUncertain`]:
/// at submit, or a sweep's), unbound to the run. Batch lists a job only eventually, but by the next
/// sweep, minutes after such a launch, it lists it (one that does not, within
/// [`LOST_LAUNCH_LISTED_WITHIN`] of the launch, is [`unsettled_lost_launch`]'s). `Err` is why the
/// run waits instead: it is neither launched nor failed while one may run. Each one found is
/// tried, whatever another's answer.
///
/// `current` is whether `queue` is the deploy's queue for the run now. Another one (an earlier
/// launch's, [`LaunchPlan::attempted_queues`]) that Batch refuses to list is passed over only when
/// Batch says it is gone ([`crate::aws::batch::BatchJobs::job_queue_gone`]: a deploy removed it,
/// and Batch has finished deleting it, which terminates its jobs); a listing error says nothing of
/// that itself (a denied listing can read as an opaque `ClientException`). `found` says which job
/// to leave alone and how loudly to log. `Ok` is the coordinators it found and stopped
/// (`found.skip` not among them).
async fn stop_unbound_coordinators(
    state: &AppState,
    run_id: &RunId,
    queue: &str,
    current: bool,
    found: Found<'_>,
) -> Result<Vec<LiveCoordinator>, String> {
    use futures::StreamExt;
    let looking = |e: &dyn std::fmt::Display| {
        format!(
            "looking for a coordinator an earlier launch of the run may have started (on {queue}) \
             failed: {e}"
        )
    };
    let jobs = match state.cloud.live_coordinators(queue, run_id).await {
        Ok(jobs) => jobs,
        Err(e) if current => return Err(looking(&e)),
        Err(e) => match state.cloud.job_queue_gone(queue).await {
            Ok(true) => {
                tracing::warn!(run_id = %run_id, queue, error = %e, "launch sweep: an earlier launch's queue cannot be listed and Batch says it is gone; passing over it");
                return Ok(Vec::new());
            }
            Ok(false) => return Err(looking(&e)),
            Err(gone) => {
                return Err(looking(&format!(
                    "{e}; and asking Batch whether the queue is gone failed: {gone}"
                )))
            }
        },
    };
    let jobs: Vec<LiveCoordinator> =
        jobs.into_iter().filter(|job| Some(job.job_id.as_str()) != found.skip).collect();
    // Side by side (a few at a time, a slot freed as any stop answers, in whatever order), whatever
    // another's answer, so a stop that hangs keeps no other job on the queue from being stopped
    // (every caller bounds them all: [`CLEANUP_BATCH_CALLS`]).
    let errors: Vec<String> = futures::stream::iter(
        jobs.iter().map(|job| stop_unbound_coordinator(state, run_id, &job.job_id, found.quiet)),
    )
    .buffer_unordered(STOPS_AT_ONCE)
    .filter_map(std::future::ready)
    .collect()
    .await;
    joined(errors).map(|()| jobs)
}

/// Stop coordinator `job`, which [`stop_unbound_coordinators`] found on a queue for parked run
/// `run_id` (logged at debug when `quiet`). `Some` is why it failed.
async fn stop_unbound_coordinator(
    state: &AppState,
    run_id: &RunId,
    job: &str,
    quiet: bool,
) -> Option<String> {
    if quiet {
        tracing::debug!(run_id = %run_id, job_id = %job, "launch sweep: stopping a coordinator an earlier launch started unbound");
    } else {
        tracing::warn!(run_id = %run_id, job_id = %job, "launch sweep: stopping a coordinator an earlier launch started unbound");
    }
    let stopped = state
        .cloud
        .terminate_job(job, "biffix: a launch of the run whose answer was lost started it")
        .await;
    stopped.err().map(|e| {
        format!(
            "stopping coordinator {job}, which an earlier launch of the run started unbound, \
             failed: {e}"
        )
    })
}

/// How many of the coordinators [`stop_unbound_coordinators`] finds on one queue it stops at once.
const STOPS_AT_ONCE: usize = 8;

/// Record, before the sweep's SubmitJob, the launch of parked run `run_id` it is about to make on
/// `queue`, in one write, in place in the stored JSON (so a key this build does not know, such as
/// a newer build's, survives): `queue` added to [`LaunchPlan::attempted_queues`], and now as
/// [`LaunchPlan::launch_uncertain_at_micros`], which stays if this sweep does not outlive SubmitJob
/// or its answer is lost ([`clear_launch_attempt`] removes it on a definite one). The caller holds
/// the run's launch claim. A run no longer parked has nothing to record.
async fn record_launch_attempt(
    state: &AppState,
    run_id: &RunId,
    queue: &str,
) -> Result<(), StoreError> {
    let Some(mut row) = state.store.get_pending_launch(run_id).await? else {
        return Ok(());
    };
    let Some(plan) = row.plan.as_object_mut() else {
        return Ok(());
    };
    add_attempted_queue(plan, queue);
    plan.insert(LAUNCH_UNCERTAIN_KEY.into(), biffix_core::clock::epoch_micros().into());
    // An earlier launch's: this one's answer has not been found lost.
    plan.remove(LAUNCH_ANSWER_LOST_KEY);
    state.store.put_pending_launch(&row).await
}

/// Parked request `row` with `current`, the deploy's queue for the run now (`None` when it has
/// none), in its [`LaunchPlan::attempted_queues`]: added to the stored request
/// ([`record_attempted_queue`], tried [`KEEP_WRITE_ATTEMPTS`] times) when it is not there yet,
/// before [`release_stopped`] makes any Batch call there. The caller holds the run's launch claim.
async fn record_current_queue(
    state: &AppState,
    row: &PendingLaunch,
    current: Option<&str>,
) -> Result<PendingLaunch, StoreError> {
    let mut row = row.clone();
    let Some(current) = current else {
        return Ok(row);
    };
    if attempted_queues(&row.plan).iter().any(|q| q == current) {
        return Ok(row);
    }
    // A few tries, as `keep_for_cleanup`'s write: a blip of the store would otherwise hold up
    // the whole cleanup until the next sweep.
    let mut attempt = 1;
    while let Err(e) = record_attempted_queue(state, &row.run_id, current).await {
        if attempt == KEEP_WRITE_ATTEMPTS {
            return Err(e);
        }
        tokio::time::sleep(KEEP_WRITE_BACKOFF * attempt).await;
        attempt += 1;
    }
    add_attempted_queue(plan_object(&mut row.plan), current);
    Ok(row)
}

/// Add `queue` to parked run `run_id`'s stored [`LaunchPlan::attempted_queues`] alone, in place in
/// the stored JSON (no launch time: no launch is made; a plan that is not an object is replaced,
/// [`plan_object`]). The caller holds the run's launch claim. A run no longer parked has nothing to
/// record.
async fn record_attempted_queue(
    state: &AppState,
    run_id: &RunId,
    queue: &str,
) -> Result<(), StoreError> {
    let Some(mut row) = state.store.get_pending_launch(run_id).await? else {
        return Ok(());
    };
    add_attempted_queue(plan_object(&mut row.plan), queue);
    state.store.put_pending_launch(&row).await
}

/// A stored `plan` as a JSON object, to record a queue in: one that is not an object (no store
/// enforces its shape) names nothing readable, and is replaced by an empty one, rather than leave
/// the queue unrecorded and its cleanup with nowhere to look.
fn plan_object(plan: &mut serde_json::Value) -> &mut serde_json::Map<String, serde_json::Value> {
    if !plan.is_object() {
        *plan = serde_json::Value::Object(serde_json::Map::new());
    }
    match plan {
        serde_json::Value::Object(plan) => plan,
        _ => unreachable!("replaced by an object above"),
    }
}

/// Add `queue` to a stored plan's `attempted_queues` (JSON), unless it is there.
fn add_attempted_queue(plan: &mut serde_json::Map<String, serde_json::Value>, queue: &str) {
    let tried = plan.entry("attempted_queues").or_insert_with(|| serde_json::json!([]));
    match tried.as_array_mut() {
        Some(list) if list.iter().any(|q| q.as_str() == Some(queue)) => {}
        Some(list) => list.push(queue.into()),
        None => *tried = serde_json::json!([queue]),
    }
}

/// Record now as when the answer of parked run `run_id`'s launch, which [`record_launch_attempt`]
/// recorded, was found lost ([`LaunchPlan::launch_answer_lost_at_micros`]). A failed write is
/// logged: the listing window then counts from the launch's start, as for a launch the sweep did
/// not outlive.
async fn record_answer_lost(state: &AppState, run_id: &RunId) {
    let recorded = async {
        let Some(mut row) = state.store.get_pending_launch(run_id).await? else {
            return Ok(());
        };
        let Some(plan) = row.plan.as_object_mut() else {
            return Ok(());
        };
        plan.insert(LAUNCH_ANSWER_LOST_KEY.into(), biffix_core::clock::epoch_micros().into());
        state.store.put_pending_launch(&row).await
    };
    if let Err(e) = recorded.await {
        let e: StoreError = e;
        tracing::warn!(run_id = %run_id, error = %e, "launch sweep: recording when a launch's answer was lost failed; Batch's listing window counts from the launch's start");
    }
}

/// Remove the time of parked run `run_id`'s latest launch whose outcome was unknown
/// ([`LaunchPlan::launch_uncertain_at_micros`]) from its stored request, once that launch is
/// settled: answered without a coordinator, or the coordinator it may have created found and
/// stopped. No listing need wait for one then. Its queue stays in
/// [`LaunchPlan::attempted_queues`]. A failed write is logged and leaves the time, which only
/// makes a later listing wait for nothing, for at most [`LOST_LAUNCH_LISTED_WITHIN`].
async fn clear_launch_attempt(state: &AppState, run_id: &RunId) {
    let cleared = async {
        let Some(mut row) = state.store.get_pending_launch(run_id).await? else {
            return Ok(());
        };
        let Some(plan) = row.plan.as_object_mut() else {
            return Ok(());
        };
        let lost = plan.remove(LAUNCH_ANSWER_LOST_KEY);
        if plan.remove(LAUNCH_UNCERTAIN_KEY).is_none() && lost.is_none() {
            return Ok(());
        }
        state.store.put_pending_launch(&row).await
    };
    if let Err(e) = cleared.await {
        let e: StoreError = e;
        tracing::warn!(run_id = %run_id, error = %e, "launch sweep: clearing the time of a launch that started no coordinator failed; a later listing may wait for it");
    }
}

/// What [`stop_unbound_coordinators`] does with the jobs it finds: `skip` is one stopped by id this
/// sweep, already or alongside (Batch lists a job TerminateJob was sent until it ends), and `quiet` logs each
/// stop at debug, for a request kept for cleanup only, whose first keep was warned about (its
/// escalation, a week on, is logged as an error).
#[derive(Clone, Copy, Default)]
struct Found<'a> {
    skip: Option<&'a str>,
    quiet: bool,
}

/// Stop coordinator `job`, which a launch of parked run `run_id` left unbound. A failure is logged
/// as an error, or at debug when `quiet` (a request kept for cleanup only: each sweep tries again,
/// and its escalation, a week on, is the error).
async fn stop_orphan(
    state: &AppState,
    run_id: &RunId,
    job: &str,
    quiet: bool,
) -> Result<(), AwsError> {
    let stopped = state.cloud.terminate_job(job, "biffix: the run could not be recorded").await;
    if let Err(e) = &stopped {
        if quiet {
            tracing::debug!(run_id = %run_id, job_id = %job, error = %e, "launch sweep: stopping an unbound coordinator failed");
        } else {
            tracing::error!(run_id = %run_id, job_id = %job, error = %e, "launch sweep: stopping an unbound coordinator failed");
        }
    }
    stopped
}

/// Settle parked run `row` (`age` after it was parked), whose launch left coordinator `job` unbound
/// (neither recorded nor stopped), in state `run_state`. If the run launched after all (the record
/// landed although it reported failing), the coordinator is its own: the row is dropped. If the run
/// ended (or its request is kept for cleanup only, its failure recorded), the row is [`release`]d:
/// the coordinator is stopped, and the row kept while it cannot be. Otherwise the coordinator is
/// stopped, the run fails naming it, and the row is released likewise.
async fn settle_orphan(
    state: &AppState,
    row: &PendingLaunch,
    job: &str,
    age: Duration,
    run_state: Option<&str>,
) -> Result<Swept, BuildError> {
    let run_id = &row.run_id;
    let ended = match run_state.map(str::parse::<RunState>) {
        Some(Ok(RunState::Submitted | RunState::Running)) => {
            drop_row(state, run_id).await?;
            return Ok(Swept::Dropped);
        }
        Some(Ok(RunState::Cancelled)) => Some(Swept::Cancelled),
        Some(Ok(s)) if s.is_terminal() => Some(Swept::Finished),
        // Failed already (its end not folded yet, or the run never recorded): never failed again.
        None if row.cleanup_only() => Some(Swept::Dropped),
        _ if row.cleanup_only() => Some(Swept::Finished),
        _ => None,
    };
    if let Some(swept) = ended {
        return Ok(if release(state, row, age).await? { swept } else { Swept::CleaningUp });
    }
    // Bounded, as a release's stop of it is: one that hangs reads as failed, so the run fails
    // saying the coordinator may still be running, and its request is kept for cleanup only
    // (later sweeps try the stop again after the runs that may launch).
    let stopped =
        match tokio::time::timeout(CLEANUP_BATCH_CALLS, stop_orphan(state, run_id, job, false))
            .await
        {
            Ok(stopped) => stopped.map_err(|e| e.to_string()),
            Err(_) => Err(format!("it took over {} s", CLEANUP_BATCH_CALLS.as_secs())),
        };
    let fate = match &stopped {
        Ok(()) => "it was stopped".to_string(),
        Err(e) => {
            format!(
                "stopping it failed ({e}), so it may still be running: stop AWS Batch job {job}"
            )
        }
    };
    let reason =
        format!("the run's coordinator ({job}) launched but the run could not be recorded; {fate}");
    record_failure(state, run_id, Stage::Launch, &reason).await?;
    let orphan = stopped.map_err(|e| orphan_stop_error(job, &e));
    release_stopped(state, row, age, OrphanStop::Tried(orphan)).await?;
    Ok(Swept::Failed)
}

/// Fail parked run `row` (`age` after it was parked) at `stage` with `reason`, before its launch,
/// then [`release`] its request: kept, for cleanup only, while a coordinator an earlier launch of it
/// may have left running cannot be stopped. The run fails on time either way.
async fn fail_parked(
    state: &AppState,
    row: &PendingLaunch,
    age: Duration,
    stage: Stage,
    reason: &str,
) -> Result<Swept, BuildError> {
    record_failure(state, &row.run_id, stage, reason).await?;
    release(state, row, age).await?;
    Ok(Swept::Failed)
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
/// request: for a run no coordinator an earlier launch of it started may still be running unbound
/// (see [`fail_parked`] for one that may).
async fn fail(
    state: &AppState,
    run_id: &RunId,
    stage: Stage,
    reason: &str,
) -> Result<Swept, BuildError> {
    record_failure(state, run_id, stage, reason).await?;
    drop_row(state, run_id).await?;
    Ok(Swept::Failed)
}

/// Record parked run `run_id`'s failure at `stage` with `reason` (the attributable `run_failed`).
async fn record_failure(
    state: &AppState,
    run_id: &RunId,
    stage: Stage,
    reason: &str,
) -> Result<(), BuildError> {
    tracing::info!(run_id = %run_id, stage = stage.as_str(), reason, "launch sweep: run failed");
    let ts = Timestamp::from_micros(biffix_core::clock::epoch_micros());
    state
        .events
        .record(&biffix_core::event::run_failed(run_id, ts, stage.as_str(), reason))
        .await?;
    Ok(())
}

async fn drop_row(state: &AppState, run_id: &RunId) -> Result<(), BuildError> {
    Ok(state.store.delete_pending_launch(run_id).await?)
}

/// A launch that did not happen: writing the image map is AWS's fault (upstream); preparing the
/// task job definitions is too, unless AWS is only unavailable (retry) or the deploy does not let
/// the backend register them (a denied or unset grant: a misconfigured deploy, ours); a refused
/// submit is [`SubmitError`]'s, and a run that could not be recorded its store failure's kind,
/// saying what became of its coordinator.
impl From<LaunchError> for crate::error::ServiceError {
    fn from(err: LaunchError) -> Self {
        use crate::error::ServiceError;
        let message = err.to_string();
        match err {
            LaunchError::ImageMap(_) => ServiceError::Upstream(message),
            LaunchError::JobDefinitions { source, .. } => match source {
                AwsError::Unavailable(_) => ServiceError::Unavailable(message),
                AwsError::Forbidden(_) | AwsError::Misconfigured(_) | AwsError::Unsupported(_) => {
                    ServiceError::Internal(message)
                }
                _ => ServiceError::Upstream(message),
            },
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

    #[test]
    fn the_sweep_steps_requests_kept_for_cleanup_after_every_run_that_may_launch() {
        let row = |id: &str, at: i64, cleanup: bool| PendingLaunch {
            run_id: RunId::new(id),
            created_at_micros: at,
            plan: if cleanup {
                serde_json::json!({ PENDING_CLEANUP_KEY: true })
            } else {
                serde_json::json!({})
            },
        };
        // Oldest first, as listed: two kept for cleanup older than the runs that may launch.
        let rows = vec![
            row("kept-1", 1, true),
            row("ready-1", 2, false),
            row("kept-2", 3, true),
            row("ready-2", 4, false),
        ];
        let order: Vec<String> =
            sweep_order(rows).into_iter().map(|r| r.run_id.to_string()).collect();
        // One kept request goes first, its turn kept whatever the runs that may launch take;
        // the others after them.
        assert_eq!(order, ["kept-1", "ready-1", "ready-2", "kept-2"]);
    }

    #[test]
    fn with_no_kept_request_the_runs_that_may_launch_go_oldest_first() {
        let rows = vec![
            PendingLaunch {
                run_id: RunId::new("a"),
                created_at_micros: 1,
                plan: serde_json::json!({}),
            },
            PendingLaunch {
                run_id: RunId::new("b"),
                created_at_micros: 2,
                plan: serde_json::json!({}),
            },
        ];
        let order: Vec<String> =
            sweep_order(rows).into_iter().map(|r| r.run_id.to_string()).collect();
        assert_eq!(order, ["a", "b"]);
    }

    #[test]
    fn kept_requests_are_stepped_least_recently_tried_first() {
        let kept = |id: &str, at: i64, tried: Option<i64>| {
            let mut plan = serde_json::json!({ PENDING_CLEANUP_KEY: true });
            if let Some(tried) = tried {
                plan[PENDING_CLEANUP_TRIED_KEY] = tried.into();
            }
            PendingLaunch { run_id: RunId::new(id), created_at_micros: at, plan }
        };
        // Listed oldest first. A sweep that ran out of time on `slow` (tried last) never reached
        // `starved`; `new` was never tried; `even-*` tie, so the older goes first.
        let rows = vec![
            kept("slow", 1, Some(50)),
            kept("starved", 2, Some(10)),
            kept("even-old", 3, Some(20)),
            kept("even-new", 4, Some(20)),
            kept("new", 5, None),
        ];
        let order: Vec<String> =
            sweep_order(rows).into_iter().map(|r| r.run_id.to_string()).collect();
        assert_eq!(order, ["new", "starved", "even-old", "even-new", "slow"]);
    }

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
    fn a_task_job_definition_failure_says_which_and_whose_fault_it_is() {
        use crate::error::ServiceError;
        let failed = |source| {
            ServiceError::from(LaunchError::JobDefinitions {
                what: "the definition of image `repo@sha256:a`".into(),
                source,
            })
        };
        let denied = failed(AwsError::Forbidden("RegisterJobDefinition".into()));
        assert!(matches!(denied, ServiceError::Internal(_)), "{denied:?}");
        assert!(denied.to_string().contains("the definition of image `repo@sha256:a`"), "{denied}");
        let unset =
            failed(AwsError::Unsupported("BIFFIX_TASK_JOB_DEFINITION_PREFIX is unset".into()));
        assert!(matches!(unset, ServiceError::Internal(_)), "{unset:?}");
        let throttled = failed(AwsError::Unavailable("throttled".into()));
        assert!(matches!(throttled, ServiceError::Unavailable(_)), "{throttled:?}");
        let other = failed(AwsError::Batch("ClientException".into()));
        assert!(matches!(other, ServiceError::Upstream(_)), "{other:?}");
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
        let definitions = |source| LaunchError::JobDefinitions {
            what: "the definition of image `i`".into(),
            source,
        };
        assert!(definitions(AwsError::Unavailable("batch".into())).is_transient());
        assert!(!definitions(AwsError::Forbidden("batch".into())).is_transient());
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
            attempted_queues: Vec::new(),
            launch_uncertain_at_micros: None,
            launch_answer_lost_at_micros: None,
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
            single_job: Some(biffix_core::event::SingleJobUse {
                queue_class: "single".into(),
                instance_type: "c6id.8xlarge".into(),
                vcpus: 32,
                memory_mib: 61_184,
            }),
        };
        let json = serde_json::to_value(&plan).unwrap();
        assert_eq!(serde_json::from_value::<LaunchPlan>(json.clone()).unwrap(), plan);
        // The images are required: a plan without them does not read.
        for required in ["images", "version"] {
            let mut missing = json.clone();
            missing.as_object_mut().unwrap().remove(required);
            assert!(serde_json::from_value::<LaunchPlan>(missing).is_err(), "{required}");
        }
        // A plan from before single-job runs reads as one Batch job per task.
        let mut tasks = json.clone();
        tasks.as_object_mut().unwrap().remove("single_job");
        assert_eq!(serde_json::from_value::<LaunchPlan>(tasks).unwrap().single_job, None);
        // A plan parked by a build whose `single_job` was the backend's own `SingleJobLaunch`
        // (written as exactly these four fields) reads as the same `SingleJobUse`.
        let mut old = json.clone();
        old["single_job"] = serde_json::from_str(
            r#"{"queue_class":"single","instance_type":"c6id.8xlarge","vcpus":32,"memory_mib":61184}"#,
        )
        .unwrap();
        assert_eq!(serde_json::from_value::<LaunchPlan>(old.clone()).unwrap(), plan);
        assert_eq!(serde_json::to_value(&plan).unwrap()["single_job"], old["single_job"]);
    }

    #[test]
    fn only_a_single_job_plan_is_written_in_the_newer_format() {
        assert_eq!(launch_plan_version(false), TASKS_LAUNCH_PLAN_VERSION);
        assert_eq!(launch_plan_version(true), LAUNCH_PLAN_VERSION);
        const { assert!(LAUNCH_PLAN_VERSION > TASKS_LAUNCH_PLAN_VERSION) };
    }
}
