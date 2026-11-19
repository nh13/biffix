//! Batch: the run's coordinator job — its session, launch and variant job definitions, its tasks'
//! job definitions, cancel, and what Batch says about the jobs a run's coordinator submits
//! ([`BatchJobs`]).

use std::collections::BTreeMap;

use async_trait::async_trait;
use biffix_core::ids::{BatchJobId, CoordinatorJobId, RunId};
use biffix_workflow::engines::Engine;

use super::AwsError;
use crate::register::workflow::WorkflowSource;

#[derive(Clone, Debug, PartialEq)]
pub struct RunSession {
    /// The run's per-project role ARN (recorded on the Run). The run is not handed credentials for
    /// it: Batch runs the coordinator and its children as this role.
    pub(crate) run_role_arn: String,
    /// The short-lived capability token passed to the coordinator (`BIFFIX_INGEST_TOKEN`).
    pub(crate) capability_token: String,
}

/// The project a run launches in, as its coordinator needs it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LaunchProject {
    /// The project id (`BIFFIX_PROJECT`): every job of the run is tagged with it ([`PROJECT_TAG`]),
    /// and the project's coordinator role may submit, tag and terminate only jobs tagged with it.
    pub id: String,
    /// The project bucket (`BIFFIX_PROJECT_BUCKET`), hosting the run's default output prefix.
    pub bucket: String,
    /// The project's run base role: the run's task job definitions run as it
    /// ([`crate::aws::task_definitions`]), and it is recorded on the run.
    pub run_role_arn: String,
    /// The project's task log group: the run's task job definitions log there.
    pub task_log_group: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CoordinatorLaunch {
    pub run_id: RunId,
    /// The project the run launches in.
    pub project: LaunchProject,
    /// The deploy's coordinator job queue (its ARN, `BIFFIX_COORDINATOR_QUEUE`; on-demand, never
    /// spot), passed to `SubmitJob` as the queue.
    pub coordinator_queue: String,
    /// Resolved default worker-image digest for this WorkflowVersion (the coordinator sets it as
    /// the child jobs' default container). Empty ⇒ the version has no default image of its own: a
    /// Snakemake version's tasks default to the coordinator variant ([`worker_image`]), a Nextflow
    /// version's tasks each declare their own container. The coordinator image itself comes from
    /// the variant's job definition.
    pub image_digest: String,
    /// The ECR repo URI (`<id>.dkr.ecr.<region>.amazonaws.com/<repo>`) the default worker image
    /// lives in, derived from the version's recorded coordinates. `None` (none recorded) falls back
    /// to the tenant workflow repository.
    pub worker_image_repo_uri: Option<String>,
    /// The capability token to inject as env.
    pub capability_token: String,
    /// Where the coordinator fetches the immutable workflow source from.
    pub(crate) source: WorkflowSource,
    /// The exact commit the version pins (checked out after fetch, reproducibility).
    pub(crate) git_sha: String,
    /// The workflow engine.
    pub(crate) engine: Engine,
    /// The engine entrypoint (e.g. `workflow/Snakefile`).
    pub(crate) entrypoint: String,
    /// Optional target rule/output to build.
    pub target: Option<String>,
    /// The run's effective output prefix ([`crate::runs::output_prefix::effective_output_prefix`]): the submit override,
    /// else the per-run default. `Some` sets the coordinator's `BIFFIX_OUTPUT_PREFIX` (the engine's
    /// default storage prefix) — always, whenever the deploy has a project bucket, so what the
    /// coordinator writes to is exactly what the anchor recorded. `None` leaves the entrypoint on
    /// its own fallback (a local composition with no bucket).
    pub output_prefix: Option<String>,
    /// Parameter values the coordinator materializes into the engine config.
    pub params: serde_json::Value,
    /// Worker queue-classes this version declared; the first selects the child-job queue unless
    /// `queues` names a default.
    pub(crate) queue_classes: Vec<String>,
    /// Which queue-class each task runs on (`queues:` in `biffix.yaml`), validated at
    /// registration: its default selects the child-job queue, its selectors the tasks that run
    /// elsewhere.
    pub(crate) queues: biffix_workflow::queues::QueueAssignment,
    /// Default task retry count (`BIFFIX_RETRIES`); `None` leaves the coordinator's default.
    pub retries: Option<u32>,
    /// Keep-going (`BIFFIX_KEEP_GOING`); `None` leaves the coordinator's default.
    pub keep_going: Option<bool>,
    /// The engine session a Nextflow coordinator runs as (`BIFFIX_ENGINE_SESSION_ID`): a fresh id
    /// the engine adopts as its session (`NXF_UUID`), or, with [`Self::resume`], a prior run's
    /// session to `-resume`. It keys the run's resume cache under
    /// `<output_prefix>.nextflow/cache/<id>/`. `None` (Snakemake) lets the engine choose its own.
    pub engine_session_id: Option<String>,
    /// Resume a prior run in the same output prefix (`BIFFIX_RESUME=1`): Nextflow `-resume`s its
    /// session; Snakemake restores the prior run's metadata and reruns only what is missing, stale
    /// or incomplete.
    pub resume: bool,
    /// Snakemake: the between-workflow output cache (`BIFFIX_SNAKEMAKE_CACHE`, an `s3://` prefix)
    /// when the version opted in; `None` ⇒ no cross-run cache.
    pub output_cache: Option<String>,
    /// The coordinator variant the run launches on: the version's engine runtime appended to the
    /// base coordinator image. Batch cannot override a job's image, so each variant has its own
    /// job definition.
    pub variant: LaunchVariant,
    /// Where the run's image map and its map of image to task job definition were written.
    /// `None` (a local composition with no project bucket): task images are not resolved through
    /// a map, and the engine registers its own task job definitions.
    pub maps: Option<RunMapUris>,
    /// A single-job run: the whole host its one job is given (sent as the job's resource
    /// requirements, overriding the template's, and as `BIFFIX_EXECUTION=single_job` with its
    /// size). `None`: the coordinator submits one Batch job per task.
    pub single_job: Option<super::ec2::Resources>,
}

/// Where a run's image map and its map of image to task job definition were written. A launch
/// writes both for a tasks-mode run, and only the image map for a single-job run, whose tasks are
/// no Batch jobs (`crate::runs::launch`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunMapUris {
    /// The image map (`images.json`, `s3://…`; see `crate::images::map`), handed to the
    /// coordinator as `BIFFIX_IMAGE_MAP_URI`.
    pub image_map: String,
    /// The map of image to task job definition (`job-definitions.json`, `s3://…`; see
    /// [`crate::aws::task_definitions`]), handed to the coordinator as
    /// `BIFFIX_JOB_DEFINITION_MAP_URI`. `None` exactly for a single-job run.
    pub job_definitions: Option<String>,
}

/// The image a run's tasks default to (`BIFFIX_WORKER_IMAGE`): the version's default image,
/// `<repo>@<digest>` in its recorded repository (`repo_uri`, else the deploy's worker repository);
/// for a Snakemake version without one, the coordinator variant (which carries the same pinned
/// Snakemake), unless the run is `single_job`; otherwise none. A single-job run's tasks with no
/// image of their own run in the job itself, which for a version with no default image is that
/// variant: it is not converted to run nested in itself. The run's image map names it too
/// (`crate::images::map`), so both must spell it the same: this is the one place it is spelled.
pub(crate) fn worker_image(
    image_digest: &str,
    repo_uri: Option<&str>,
    fallback_repo: &str,
    engine: biffix_workflow::engines::Engine,
    variant_image: &str,
    single_job: bool,
) -> Option<String> {
    if image_digest.starts_with("sha256:") {
        Some(format!("{}@{image_digest}", repo_uri.unwrap_or(fallback_repo)))
    } else if engine == biffix_workflow::engines::Engine::Snakemake && !single_job {
        Some(variant_image.to_string())
    } else {
        None
    }
}

/// The coordinator variant a run is launched on (see [`CoordinatorLaunch::variant`]).
#[derive(Clone, Debug, PartialEq)]
pub struct LaunchVariant {
    /// The variant's Batch job definition (ARN).
    pub job_definition: String,
    /// `<repo>@sha256:…` of the variant: the coordinator's image, and a Snakemake version's worker
    /// image when it declares none of its own (it carries the same pinned Snakemake).
    pub image: String,
    /// The version's pinned engine release (`BIFFIX_ENGINE_VERSION`).
    pub engine_version: String,
    /// The runtime the variant carries (`BIFFIX_RUNTIME_KEY`).
    pub runtime_key: String,
}

/// The Batch tag naming the biffix run a job belongs to. The backend has the engine's AWS Batch
/// executor put it on every worker job a run's coordinator submits (`SNAKEMAKE_AWS_BATCH_JOB_TAGS`
/// for Snakemake, `process.resourceLabels` for Nextflow — see `docker/coordinator/lib/snakemake.sh`
/// and `lib/nextflow.sh`), and the engine ingest route accepts a `task_submitted` claim on a job
/// only when the tag names the claiming run. The infra IAM conditions name the same key, which an
/// infra test holds to this one.
pub(crate) const RUN_TAG: &str = "biffix:run";

/// The Batch tag naming the project a job belongs to (the coordinator env's `BIFFIX_PROJECT`).
/// The engines put it on every child job and the backend on each coordinator job; a project's
/// coordinator role may submit, tag and terminate only jobs tagged with its own project. The infra
/// IAM conditions name the same key, which an infra test holds to this one.
pub(crate) const PROJECT_TAG: &str = "biffix:project";

/// The Batch tag a single-job run's coordinator job is born with ([`SINGLE_JOB_EXECUTION`]); a
/// coordinator of a run one Batch job per task has none. IAM lets the control plane submit a job
/// with it only to a single-job queue on a single-job variant, and one without it only to the
/// coordinator queue on a coordinator variant, so a backend bug cannot put a single-job variant
/// (the run role, `/dev/fuse`, the host's `/scratch`) on the shared coordinator hosts, nor a
/// coordinator variant on the single-job hosts' relaxed seccomp profile. The infra IAM conditions
/// name the same key and value (`infra/biffix_infra/constructs/batch_access.py`).
pub(crate) const EXECUTION_TAG: &str = "biffix:execution";

/// The [`EXECUTION_TAG`] value of a single-job run's coordinator job.
pub(crate) const SINGLE_JOB_EXECUTION: &str = "single_job";

/// The Batch job name of run `run_id`'s coordinator. A run's every launch uses it, so its
/// coordinators can be found by name ([`BatchJobs::live_coordinators`]) when a launch cannot tell
/// whether Batch accepted it.
pub(crate) fn coordinator_job_name(run_id: &RunId) -> String {
    format!("biffix-coordinator-{}", run_id.as_str())
}

/// A coordinator job of a run that has not finished ([`BatchJobs::live_coordinators`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LiveCoordinator {
    /// Its Batch job id.
    pub job_id: String,
    /// When Batch created it (epoch microseconds), if Batch reports it.
    pub created_at_micros: Option<i64>,
}

/// How far Batch's clock may be behind ours: a job a launch begun at `at` (our clock) creates is
/// reported created no earlier than `at` less this.
pub(crate) const BATCH_CLOCK_SKEW: std::time::Duration = std::time::Duration::from_secs(60);

impl LiveCoordinator {
    /// Whether this job may be the one a launch begun at `at` (epoch micros, our clock) created:
    /// Batch created it no earlier than `at` less [`BATCH_CLOCK_SKEW`]. An older job is an earlier
    /// launch's, and one whose creation time Batch does not report is never taken for it.
    pub(crate) fn may_be_from_launch_at(&self, at: i64) -> bool {
        let skew = i64::try_from(BATCH_CLOCK_SKEW.as_micros()).unwrap_or(i64::MAX);
        self.created_at_micros.is_some_and(|created| created >= at.saturating_sub(skew))
    }
}

/// What the engine ingest route needs to know about a Batch job to accept a claim on it
/// ([`BatchJobs::batch_job_owners`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BatchJobOwner {
    /// The run the job's [`RUN_TAG`] names; `None` when the job has no such tag.
    pub(crate) run: Option<RunId>,
    /// When Batch created the job (epoch microseconds), if Batch reports it.
    pub(crate) created_at_micros: Option<i64>,
}

/// A coordinator job definition template a variant is cloned from: a project's coordinator
/// template, or its single-job template. The two kinds' variants are named with different prefixes
/// (`BIFFIX_VARIANT_JOB_DEFINITION_PREFIX`, `BIFFIX_SINGLE_JOB_VARIANT_JOB_DEFINITION_PREFIX`), so
/// IAM can let the control plane submit each only to its own queues: a single-job variant (run
/// role, `/dev/fuse`, the host's `/scratch`) never to the shared coordinator queue, a coordinator
/// variant never to a single-job host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VariantTemplate<'a> {
    /// The template's revisioned ARN.
    pub arn: &'a str,
    /// Whether it is a single-job template.
    pub single_job: bool,
}

/// The run's coordinator job on AWS Batch (see the module docs).
#[async_trait]
pub trait BatchJobs: Send + Sync {
    /// Record the run's role (its `project`'s base role) and mint its capability token.
    async fn mint_run_session(
        &self,
        run_id: &RunId,
        project: &LaunchProject,
    ) -> Result<RunSession, AwsError>;

    /// Launch the coordinator Batch job, named [`coordinator_job_name`]; returns its job id.
    /// SubmitJob takes no idempotency token, so an [`AwsError::Unavailable`] here (a timeout, a
    /// dropped connection, a 5xx) does not say the job was not created: see
    /// [`Self::live_coordinators`].
    async fn launch_coordinator(
        &self,
        launch: &CoordinatorLaunch,
    ) -> Result<CoordinatorJobId, AwsError>;

    /// The job ids of run `run_id`'s coordinators on `queue` that have not finished (by their
    /// name, [`coordinator_job_name`]), in no order: how a launch whose SubmitJob answer was lost
    /// finds the job Batch may have created. Batch lists a job created moments ago only eventually,
    /// so an empty answer right after a launch is not proof there is none.
    async fn live_coordinators(
        &self,
        queue: &str,
        run_id: &RunId,
    ) -> Result<Vec<LiveCoordinator>, AwsError>;

    /// Whether job queue `queue` (an ARN or name) is gone: Batch (DescribeJobQueues) knows no such
    /// queue, or reports it DELETED, its jobs terminated. `false` for a queue in any other state,
    /// DELETING too (Batch terminates its jobs only eventually). What lets the launch sweep pass
    /// over an earlier launch's queue it cannot list (`runs::launch`): only a positively gone one,
    /// never one whose listing was denied.
    async fn job_queue_gone(&self, queue: &str) -> Result<bool, AwsError>;

    /// The job definition of the coordinator variant `image` (built from base `base_digest` with
    /// runtime `runtime_key`) for the coordinator job definition `template` (a project's
    /// template): the existing one, else a clone of the template with the image swapped. Returns
    /// its ARN. Its name is a function of all four, so the variant of one project's template is
    /// found again by name without being recorded.
    async fn variant_job_definition(
        &self,
        runtime_key: &str,
        base_digest: &str,
        image: &str,
        template: VariantTemplate<'_>,
    ) -> Result<String, AwsError>;

    /// Whether `job_definition_arn`, recorded for the variant of `runtime_key` on `base_digest`,
    /// was cloned from the coordinator template `template` (the deployed one). A deploy that
    /// changes only the template (queues, role, resources, env, retries) leaves the base image
    /// alone, so the recorded variant is then stale and [`Self::variant_job_definition`] must clone
    /// it again.
    fn variant_is_current(
        &self,
        runtime_key: &str,
        base_digest: &str,
        job_definition_arn: &str,
        template: VariantTemplate<'_>,
    ) -> bool;

    /// The task job definition `definition` describes, as its revisioned ARN
    /// (`arn:…:job-definition/<name>:<revision>`, the form whose tags IAM evaluates on `SubmitJob`):
    /// an ACTIVE revision of [`crate::aws::task_definitions::TaskJobDefinition::name`] that carries
    /// it, else one registered now. Concurrent launches may each register a revision; any of them
    /// serves, since the name fixes what they carry.
    async fn task_job_definition(
        &self,
        definition: &crate::aws::task_definitions::TaskJobDefinition,
    ) -> Result<String, AwsError>;

    /// Terminate a Batch job (a run's coordinator, on cancel) with a human-readable `reason` that
    /// Batch records as the job's status reason. Terminating a job that already finished is a no-op
    /// on the Batch side.
    async fn terminate_job(&self, job_id: &str, reason: &str) -> Result<(), AwsError>;

    /// What AWS Batch says about each job in `job_ids`, by job id: the run its [`RUN_TAG`] names
    /// and when it was created. A job Batch does not know (a bad id, or one past Batch's retention)
    /// is absent from the map. The engine ingest route checks a `task_submitted` claim against it
    /// before appending (`api::events`), failing closed when this fails. Any number of ids; the
    /// provider pages them.
    async fn batch_job_owners(
        &self,
        job_ids: &[BatchJobId],
    ) -> Result<BTreeMap<String, BatchJobOwner>, AwsError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_job_may_be_a_launch_s_only_if_created_since_it_began_give_or_take_the_skew() {
        let at = 10_000_000_000;
        let skew = i64::try_from(BATCH_CLOCK_SKEW.as_micros()).unwrap();
        let job = |created| LiveCoordinator { job_id: "j".into(), created_at_micros: created };
        assert!(job(Some(at)).may_be_from_launch_at(at));
        assert!(job(Some(at + 1)).may_be_from_launch_at(at));
        // Batch's clock a little behind ours.
        assert!(job(Some(at - skew)).may_be_from_launch_at(at));
        // Older: an earlier launch's.
        assert!(!job(Some(at - skew - 1)).may_be_from_launch_at(at));
        // Batch did not say when: never taken for it.
        assert!(!job(None).may_be_from_launch_at(at));
    }
}
