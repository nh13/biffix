//! [`FakeCloud`]: the fake [`crate::aws::CloudProvider`], one impl per concern (Batch, CodeBuild,
//! image registries, S3 objects and uploads, CloudWatch Logs, SNS) over one shared state.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use crate::aws::alerts::AlertTopic;
use crate::aws::batch::{BatchJobs, CoordinatorLaunch, RunSession};
use crate::aws::builds::BuildService;
use crate::aws::logs::LogReader;
use crate::aws::objects::ObjectStore;
use crate::aws::registry::ImageRegistry;
use crate::aws::uploads::MultipartUploads;
use crate::aws::AwsError;
use async_trait::async_trait;
use biffix_core::ids::{CoordinatorJobId, RunId};

use super::{FAKE_BASE_DIGEST, FAKE_COORDINATOR_REPO};

/// One log read's paging: `(stream, cursor, direction, limit)`.
pub type LogRead = (String, Option<String>, crate::aws::logs::LogDirection, i32);

/// A fake multipart upload: its object URI and the parts sent so far, by part number.
pub type FakeUpload = (String, BTreeMap<i32, Vec<u8>>);

pub struct FakeCloud {
    pub launched: Mutex<Vec<CoordinatorLaunch>>,
    /// `(job_id, reason)` of every `terminate_job` call.
    pub terminated: Mutex<Vec<(String, String)>>,
    /// Make `terminate_job` fail (an AWS error), to exercise the cancel handler's error path.
    pub fail_terminate: std::sync::atomic::AtomicBool,
    /// The alerts topic's email subscribers; `None` = the deploy has no alerts topic.
    pub alert_subscribers: Mutex<Option<Vec<crate::aws::alerts::AlertSubscriber>>>,
    /// S3 objects by URI, served by `head_object`, `head_object_meta` and `get_object_range`.
    pub objects: Mutex<BTreeMap<String, Vec<u8>>>,
    /// The version id of a versioned object in `objects`, by URI (others are unversioned).
    pub versions: Mutex<BTreeMap<String, String>>,
    /// Every URI `head_object_meta` was asked about, in call order.
    pub heads: Mutex<Vec<String>>,
    /// Every URI `get_object_range` was asked for, in call order.
    pub ranges: Mutex<Vec<String>>,
    /// Make `get_object_range` never answer (after recording the URI), as a stalled S3 read.
    pub stall_ranges: std::sync::atomic::AtomicBool,
    /// Every URI `head_object` and `prefix_exists` were asked about (submit's input existence
    /// check), in call order.
    pub object_checks: Mutex<Vec<String>>,
    /// URIs `head_object` and `prefix_exists` report as missing. Deliberately, `head_object`
    /// treats a URI in neither `absent` nor `objects` as existing (with an empty tag), so a test
    /// need not seed every input it submits; `head_object_meta` still reports it missing.
    pub absent: Mutex<std::collections::BTreeSet<String>>,
    /// URIs that are a folder only: `head_object` finds no object there, `prefix_exists` finds one.
    pub folders: Mutex<std::collections::BTreeSet<String>>,
    /// Multipart uploads in progress, by upload id: the object URI and the parts sent so far.
    pub uploads: Mutex<BTreeMap<String, FakeUpload>>,
    /// The uploads ever created: numbers upload ids, so a finished upload's id is never reused.
    pub upload_seq: AtomicUsize,
    /// Log lines by stream, for streams a test gives; others answer one canned line.
    pub logs: Mutex<BTreeMap<String, Vec<String>>>,
    /// Log lines by stream in the task log group: only these streams are there (others are
    /// `NotFound`, so the reader falls back to the shared group).
    pub project_logs: Mutex<BTreeMap<String, Vec<String>>>,
    /// The `(group, stream)` of every `get_log_events_in` call.
    pub group_reads: Mutex<Vec<(String, String)>>,
    /// The paging `(stream, cursor, direction, limit)` of every `get_log_events` and
    /// `get_log_events_in` call, in call order.
    pub log_reads: Mutex<Vec<LogRead>>,
    /// Make every `get_log_events_in` fail with `AwsError::Forbidden(this)` (the project group is
    /// not readable), before any stream lookup.
    pub fail_group_reads: Mutex<Option<String>>,
    /// Every runtime build started.
    pub runtime_builds: Mutex<Vec<crate::aws::builds::RuntimeBuildJob>>,
    /// `(runtime_key, base_digest, image)` of every variant job definition asked for.
    pub variant_job_definitions: Mutex<Vec<(String, String, String)>>,
    /// The coordinator template of every variant job definition asked for, in the same order.
    pub variant_templates: Mutex<Vec<String>>,
    /// Whether each variant job definition asked for was a single-job template's, in the same
    /// order.
    pub variant_single_job: Mutex<Vec<bool>>,
    /// Every task job definition asked for, in call order.
    pub task_job_definitions: Mutex<Vec<crate::aws::task_definitions::TaskJobDefinition>>,
    /// Make every `task_job_definition` fail with the error this makes (`AwsError` is not
    /// `Clone`), e.g. an unavailable AWS or a denied grant.
    pub fail_task_job_definitions: Mutex<Option<fn() -> AwsError>>,
    /// The project of every run session minted, by run id.
    pub sessions: Mutex<Vec<(String, crate::aws::batch::LaunchProject)>>,
    /// `(bucket, key)` of every presigned source PUT.
    pub source_puts: Mutex<Vec<(String, String)>>,
    /// The deployed coordinator template's revision: variant job definitions are named by it, and
    /// one of another revision is stale (`variant_is_current`).
    pub template_revision: Mutex<u32>,
    /// Every image import started.
    pub image_imports: Mutex<Vec<crate::aws::builds::ImageImportJob>>,
    pub image_derives: Mutex<Vec<crate::aws::builds::ImageDeriveJob>>,
    pub image_builds: Mutex<Vec<crate::aws::builds::ImageBuildJob>>,
    pub image_indexes: Mutex<Vec<crate::aws::builds::ImageIndexJob>>,
    /// Image references `resolve_image_digest` reports as missing.
    pub missing_images: Mutex<std::collections::BTreeSet<String>>,
    /// URIs `put_object` fails to write (an S3 error).
    pub fail_puts: Mutex<std::collections::BTreeSet<String>>,
    /// What `build_event` reports, by build ARN (CodeBuild's view of a build).
    pub build_events: Mutex<BTreeMap<String, biffix_core::wire::aws_codebuild::CodeBuildEvent>>,
    /// The build each idempotency token started: like CodeBuild, a repeated token returns that
    /// build instead of starting another.
    pub build_tokens: Mutex<BTreeMap<String, String>>,
    /// The Batch jobs `batch_job_owners` knows, by job id (others are unknown to Batch).
    pub batch_jobs: Mutex<BTreeMap<String, crate::aws::batch::BatchJobOwner>>,
    /// Make the NEXT `batch_job_owners` call fail with this error (Batch unreachable, or a missing
    /// grant). One-shot, unlike the other `fail_*` switches: that call consumes it, and later
    /// calls succeed (`AwsError` is not `Clone`).
    pub fail_describe_jobs: Mutex<Option<AwsError>>,
    /// Make the next `coordinator_base` lookup fail with this (e.g. an ECR outage). One-shot, as
    /// `fail_describe_jobs`.
    pub fail_coordinator_base: Mutex<Option<AwsError>>,
    /// Make every `launch_coordinator` fail: an unavailable AWS (`true`) or a refusal (`false`).
    pub fail_launch: Mutex<Option<bool>>,
    /// Every coordinator job launched, `(job id, run id)`: what `live_coordinators` lists (less
    /// the ones `terminate_job` stopped).
    pub coordinators: Mutex<Vec<(String, String)>>,
    /// The queue each coordinator job was launched on, by job id: `live_coordinators` lists a
    /// queue's own (as Batch's ListJobs does).
    pub coordinator_queues: Mutex<BTreeMap<String, String>>,
    /// When (epoch micros) each coordinator job was created, by job id (a job a test adds by hand
    /// has none, as if Batch did not report it).
    pub coordinator_created_at: Mutex<BTreeMap<String, i64>>,
    /// Make `launch_coordinator` record each job it creates as created an hour ago (an earlier
    /// launch's, as far as a creation-time check can tell).
    pub backdate_coordinators: std::sync::atomic::AtomicBool,
    /// Every queue `live_coordinators` was asked about, in call order.
    pub live_coordinator_queues: Mutex<Vec<String>>,
    /// Queues Batch deleted: `live_coordinators` refuses to list them (an opaque `Batch` error)
    /// and `job_queue_gone` says they are gone.
    pub missing_queues: Mutex<std::collections::BTreeSet<String>>,
    /// Queues that exist but `live_coordinators` refuses to list with an opaque `Batch` error (as
    /// a denied listing can read: a 400 `ClientException`); `job_queue_gone` says they are not
    /// gone.
    pub unlistable_queues: Mutex<std::collections::BTreeSet<String>>,
    /// Make `job_queue_gone` fail (Batch unavailable).
    pub fail_job_queue_gone: std::sync::atomic::AtomicBool,
    /// Every queue `job_queue_gone` was asked about, in call order.
    pub job_queue_gone_asks: Mutex<Vec<String>>,
    /// Queues `live_coordinators` cannot list now (Batch unavailable for them alone).
    pub unavailable_queues: Mutex<std::collections::BTreeSet<String>>,
    /// Make `live_coordinators` still list a job `terminate_job` was sent, as Batch does until the
    /// job ends.
    pub list_terminated: std::sync::atomic::AtomicBool,
    /// How many of the next `launch_coordinator` calls create their job (`coord-lost-<n>`, not
    /// `coord-<n>`: `n` counts every job created) but lose the reply: an unavailable AWS, as the
    /// caller sees it.
    pub lose_launch_replies: AtomicUsize,
    /// Make `live_coordinators` list nothing (Batch does not list a job it just created yet).
    pub hide_live_coordinators: std::sync::atomic::AtomicBool,
    /// Make `live_coordinators` for these runs (by id) wait this long before it lists anything:
    /// a Batch call that hangs, slept on Tokio's clock (which a paused test runtime advances).
    pub stall_listings_for: Mutex<BTreeMap<String, std::time::Duration>>,
    /// As [`Self::stall_listings_for`], for every run's listing of these queues.
    pub stall_listings_on: Mutex<BTreeMap<String, std::time::Duration>>,
    /// Make `terminate_job` for these jobs (by id) wait this long before it answers: a Batch call
    /// that hangs, slept on Tokio's clock (which a paused test runtime advances).
    pub stall_terminate_of: Mutex<BTreeMap<String, std::time::Duration>>,
    /// Called with the job as each `terminate_job` call begins, before it stops anything: a test's
    /// look at what the code under test has done by then. Called outside this lock.
    #[allow(clippy::type_complexity)]
    pub on_terminate: Mutex<Option<Arc<dyn Fn(&str) + Send + Sync>>>,
    /// Called with the run and the queue as each `live_coordinators` call begins, before it lists
    /// anything: a test's look at what the code under test has done by then. Called outside this
    /// lock, so it may replace or clear itself.
    #[allow(clippy::type_complexity)]
    pub on_listing: Mutex<Option<Arc<dyn Fn(&RunId, &str) + Send + Sync>>>,
    /// Make `live_coordinators` take this long to return its listing, after reading it (a slow
    /// reply: the listing is as of the call, not of its return): the calling thread's clock is
    /// moved on that much ([`biffix_core::clock::testing::advance`]), not slept.
    pub slow_listing_reply: Mutex<std::time::Duration>,
    /// Make `live_coordinators` fail (Batch unavailable).
    pub fail_live_coordinators: std::sync::atomic::AtomicBool,
    /// Make a launch whose reply is lost ([`Self::lose_launch_replies`]) also set
    /// [`Self::fail_live_coordinators`]: Batch unavailable from the lost SubmitJob on, within the
    /// same sweep.
    pub fail_live_coordinators_after_lost_reply: std::sync::atomic::AtomicBool,
    /// Every build `build_event` was asked about, in call order.
    pub build_event_asks: Mutex<Vec<String>>,
    /// Make `build_event` fail (CodeBuild unavailable).
    pub fail_build_event: std::sync::atomic::AtomicBool,
    /// The instance types `describe_instance_types` knows (others are unknown to EC2): by default
    /// [`fake_instance_types`].
    pub instance_types: Mutex<BTreeMap<String, crate::aws::ec2::InstanceTypeInfo>>,
}

/// The instance types a [`FakeCloud`] knows by default, at EC2's sizes: plain x86_64 types, with no
/// accelerator.
pub fn fake_instance_types() -> BTreeMap<String, crate::aws::ec2::InstanceTypeInfo> {
    [
        ("c6id.4xlarge", 16, 32_768),
        ("c6id.8xlarge", 32, 65_536),
        ("c6id.16xlarge", 64, 131_072),
        ("m6id.8xlarge", 32, 131_072),
        ("r6id.8xlarge", 32, 262_144),
    ]
    .into_iter()
    .map(|(kind, vcpus, memory_mib)| {
        let size = crate::aws::ec2::Resources { vcpus, memory_mib };
        (kind.to_string(), crate::aws::ec2::InstanceTypeInfo::x86_64(size))
    })
    .collect()
}

impl Default for FakeCloud {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeCloud {
    /// What `live_coordinators` lists of run `run_id` on `queue` now.
    fn listed_coordinators(
        &self,
        run_id: &RunId,
        queue: &str,
    ) -> Vec<crate::aws::batch::LiveCoordinator> {
        use std::sync::atomic::Ordering::SeqCst;
        if self.hide_live_coordinators.load(SeqCst) {
            return Vec::new();
        }
        let terminated = self.terminated.lock().unwrap();
        // `launch_coordinator`'s order: the queues, the jobs, then their creation times.
        let queues = self.coordinator_queues.lock().unwrap();
        let coordinators = self.coordinators.lock().unwrap();
        let created = self.coordinator_created_at.lock().unwrap();
        coordinators
            .iter()
            .filter(|(job, run)| {
                run == run_id.as_str()
                    && queues.get(job).is_none_or(|q| q == queue)
                    && (self.list_terminated.load(SeqCst)
                        || !terminated.iter().any(|(t, _)| t == job))
            })
            .map(|(job, _)| crate::aws::batch::LiveCoordinator {
                job_id: job.clone(),
                created_at_micros: created.get(job).copied(),
            })
            .collect()
    }

    pub fn new() -> Self {
        Self {
            launched: Mutex::new(vec![]),
            terminated: Mutex::new(vec![]),
            fail_terminate: std::sync::atomic::AtomicBool::new(false),
            alert_subscribers: Mutex::new(Some(vec![])),
            objects: Mutex::new(BTreeMap::new()),
            versions: Mutex::new(BTreeMap::new()),
            heads: Mutex::new(vec![]),
            ranges: Mutex::new(vec![]),
            stall_ranges: std::sync::atomic::AtomicBool::new(false),
            object_checks: Mutex::new(vec![]),
            absent: Mutex::new(Default::default()),
            folders: Mutex::new(Default::default()),
            uploads: Mutex::new(BTreeMap::new()),
            upload_seq: AtomicUsize::new(0),
            logs: Mutex::new(BTreeMap::new()),
            project_logs: Mutex::new(BTreeMap::new()),
            group_reads: Mutex::new(Vec::new()),
            log_reads: Mutex::new(Vec::new()),
            fail_group_reads: Mutex::new(None),
            runtime_builds: Mutex::new(Vec::new()),
            variant_job_definitions: Mutex::new(Vec::new()),
            variant_templates: Mutex::new(Vec::new()),
            variant_single_job: Mutex::new(Vec::new()),
            task_job_definitions: Mutex::new(Vec::new()),
            fail_task_job_definitions: Mutex::new(None),
            sessions: Mutex::new(Vec::new()),
            source_puts: Mutex::new(Vec::new()),
            template_revision: Mutex::new(1),
            build_events: Mutex::new(BTreeMap::new()),
            image_imports: Mutex::new(Vec::new()),
            image_derives: Mutex::new(Vec::new()),
            image_builds: Mutex::new(Vec::new()),
            image_indexes: Mutex::new(Vec::new()),
            missing_images: Mutex::new(Default::default()),
            fail_puts: Mutex::new(Default::default()),
            build_tokens: Mutex::new(BTreeMap::new()),
            batch_jobs: Mutex::new(BTreeMap::new()),
            fail_describe_jobs: Mutex::new(None),
            fail_coordinator_base: Mutex::new(None),
            fail_launch: Mutex::new(None),
            coordinators: Mutex::new(Vec::new()),
            coordinator_queues: Mutex::new(BTreeMap::new()),
            coordinator_created_at: Mutex::new(BTreeMap::new()),
            backdate_coordinators: std::sync::atomic::AtomicBool::new(false),
            live_coordinator_queues: Mutex::new(Vec::new()),
            missing_queues: Mutex::new(std::collections::BTreeSet::new()),
            unlistable_queues: Mutex::new(std::collections::BTreeSet::new()),
            fail_job_queue_gone: std::sync::atomic::AtomicBool::new(false),
            job_queue_gone_asks: Mutex::new(Vec::new()),
            unavailable_queues: Mutex::new(std::collections::BTreeSet::new()),
            list_terminated: std::sync::atomic::AtomicBool::new(false),
            lose_launch_replies: AtomicUsize::new(0),
            hide_live_coordinators: std::sync::atomic::AtomicBool::new(false),
            stall_listings_for: Mutex::new(BTreeMap::new()),
            stall_listings_on: Mutex::new(BTreeMap::new()),
            stall_terminate_of: Mutex::new(BTreeMap::new()),
            on_terminate: Mutex::new(None),
            on_listing: Mutex::new(None),
            slow_listing_reply: Mutex::new(std::time::Duration::ZERO),
            fail_live_coordinators: std::sync::atomic::AtomicBool::new(false),
            fail_live_coordinators_after_lost_reply: std::sync::atomic::AtomicBool::new(false),
            build_event_asks: Mutex::new(Vec::new()),
            fail_build_event: std::sync::atomic::AtomicBool::new(false),
            instance_types: Mutex::new(fake_instance_types()),
        }
    }

    /// Record a Batch job whose run tag names `run` (`None`: untagged), created at `created_secs`.
    pub fn add_batch_job(&self, job_id: &str, run: Option<&str>, created_secs: f64) {
        self.batch_jobs.lock().unwrap().insert(
            job_id.to_string(),
            crate::aws::batch::BatchJobOwner {
                run: run.map(RunId::new),
                created_at_micros: Some((created_secs * 1e6) as i64),
            },
        );
    }

    /// Start a build as CodeBuild would with `token`: the build an earlier start with the same
    /// token got, else a new one (`start` records the request and names its ARN).
    fn start_once(&self, token: &str, start: impl FnOnce() -> String) -> String {
        let mut tokens = self.build_tokens.lock().unwrap();
        tokens.entry(token.to_string()).or_insert_with(start).clone()
    }

    /// Store one part of an upload, as a client's `PUT` to a presigned part URL would; returns the
    /// part's ETag.
    pub fn put_part(&self, upload_id: &str, part_number: i32, bytes: Vec<u8>) -> String {
        let etag = Self::part_etag(part_number, bytes.len());
        self.uploads
            .lock()
            .unwrap()
            .get_mut(upload_id)
            .expect("known upload")
            .1
            .insert(part_number, bytes);
        etag
    }

    /// The ETag the fake gives a part.
    pub fn part_etag(part_number: i32, len: usize) -> String {
        format!("part-{part_number}-{len}")
    }

    /// The etag the fake reports for an object of `len` bytes.
    pub fn etag_for(len: usize) -> String {
        format!("etag-{len}")
    }

    /// What a HEAD of `uri` finds in `objects` (no call recorded in `heads`).
    fn object_head(&self, uri: &str) -> Option<crate::aws::objects::ObjectHead> {
        let len = self.objects.lock().unwrap().get(uri)?.len();
        Some(crate::aws::objects::ObjectHead {
            size: Some(len as i64),
            etag: Some(Self::etag_for(len)),
            version_id: self.versions.lock().unwrap().get(uri).cloned(),
            last_modified: Some(1_700_000_000_000_000),
        })
    }
}

/// The name of the variant job definition the fake registers for `runtime_key` on `base_digest`
/// from template revision `template`: like the provider's, it names all three (short forms), so
/// variants of different bases never share one; the template stays last (`…-t<template>:1`).
pub(super) fn fake_variant_job_definition(
    runtime_key: &str,
    base_digest: &str,
    template: u32,
) -> String {
    let base = base_digest.strip_prefix("sha256:").unwrap_or(base_digest);
    let short = |s: &str| s.chars().take(12).collect::<String>();
    format!(
        "arn:aws:batch:us-east-1:123:job-definition/biffix-t-coordinator-rt-{}-{}-t{template}:1",
        short(runtime_key),
        short(base)
    )
}

/// The ARN of the task job definition the fake registers for `definition`: named as the
/// provider names it, under the prefix `biffix-t_task-`.
pub fn fake_task_job_definition(
    definition: &crate::aws::task_definitions::TaskJobDefinition,
) -> String {
    format!("arn:aws:batch:us-east-1:123:job-definition/{}:1", definition.name("biffix-t_task-"))
}

/// [`fake_variant_job_definition`] cloned from the coordinator template `template_arn`: a
/// project's own template (any but the default project's, which is empty in these tests) names
/// its variants apart (`…-t<revision>-<template name>:1`).
pub fn fake_project_variant_job_definition(
    runtime_key: &str,
    base_digest: &str,
    template_revision: u32,
    template: crate::aws::batch::VariantTemplate<'_>,
) -> String {
    let default = fake_variant_job_definition(runtime_key, base_digest, template_revision);
    // A single-job template's variants have names of their own, under the provider's prefix for
    // them (`…-coordinator-sj-`, which IAM tells the two kinds apart by): the same template ARN of
    // the other kind is another one.
    let default = if template.single_job {
        default.replacen("/biffix-t-coordinator-", "/biffix-t-coordinator-sj-", 1)
    } else {
        default
    };
    let Some((_, name)) = template.arn.split_once(":job-definition/") else {
        return default;
    };
    let name = name.split(':').next().unwrap_or(name);
    format!("{}-{name}:1", default.trim_end_matches(":1"))
}

#[async_trait]
impl crate::aws::ec2::InstanceTypes for FakeCloud {
    async fn describe_instance_types(
        &self,
        instance_types: &[String],
    ) -> Result<BTreeMap<String, crate::aws::ec2::InstanceTypeInfo>, AwsError> {
        let known = self.instance_types.lock().unwrap();
        Ok(instance_types
            .iter()
            .filter_map(|t| known.get(t).map(|info| (t.clone(), info.clone())))
            .collect())
    }
}

#[async_trait]
impl BatchJobs for FakeCloud {
    async fn batch_job_owners(
        &self,
        job_ids: &[biffix_core::ids::BatchJobId],
    ) -> Result<BTreeMap<String, crate::aws::batch::BatchJobOwner>, AwsError> {
        if let Some(err) = self.fail_describe_jobs.lock().unwrap().take() {
            return Err(err);
        }
        let jobs = self.batch_jobs.lock().unwrap();
        Ok(job_ids
            .iter()
            .filter_map(|id| {
                jobs.get(id.as_str()).map(|owner| (id.as_str().to_string(), owner.clone()))
            })
            .collect())
    }

    async fn mint_run_session(
        &self,
        run_id: &RunId,
        project: &crate::aws::batch::LaunchProject,
    ) -> Result<RunSession, AwsError> {
        self.sessions.lock().unwrap().push((run_id.as_str().to_string(), project.clone()));
        // The project's role, as the provider records; a context without one (the tests' default
        // project) gets a canned one.
        let run_role_arn = if project.run_role_arn.is_empty() {
            "arn:aws:iam::123:role/biffix-run".to_string()
        } else {
            project.run_role_arn.clone()
        };
        Ok(RunSession { run_role_arn, capability_token: "tok-abc".into() })
    }

    async fn launch_coordinator(
        &self,
        launch: &CoordinatorLaunch,
    ) -> Result<CoordinatorJobId, AwsError> {
        match *self.fail_launch.lock().unwrap() {
            Some(true) => return Err(AwsError::Unavailable("batch throttled".into())),
            Some(false) => return Err(AwsError::Forbidden("SubmitJob denied".into())),
            None => {}
        }
        self.launched.lock().unwrap().push(launch.clone());
        let lost = self
            .lose_launch_replies
            .fetch_update(
                std::sync::atomic::Ordering::SeqCst,
                std::sync::atomic::Ordering::SeqCst,
                |n| n.checked_sub(1),
            )
            .is_ok();
        // Each job its own id (`coord-<n>`, or `coord-lost-<n>`), numbered by every job created,
        // so one launch's queue never overwrites another's. Numbered, recorded and given its
        // queue in one critical section (the queues' lock, then the jobs', the order
        // `live_coordinators` takes them in), so two concurrent launches never take the same
        // number, and no listing sees the job before its queue (it would list it on every queue).
        let job = {
            let mut queues = self.coordinator_queues.lock().unwrap();
            let mut coordinators = self.coordinators.lock().unwrap();
            let n = coordinators.len() + 1;
            let job = if lost { format!("coord-lost-{n}") } else { format!("coord-{n}") };
            coordinators.push((job.clone(), launch.run_id.to_string()));
            queues.insert(job.clone(), launch.coordinator_queue.clone());
            let backdate = self.backdate_coordinators.load(std::sync::atomic::Ordering::SeqCst);
            let created =
                biffix_core::clock::epoch_micros() - if backdate { 3_600_000_000 } else { 0 };
            self.coordinator_created_at.lock().unwrap().insert(job.clone(), created);
            job
        };
        if lost {
            use std::sync::atomic::Ordering::SeqCst;
            if self.fail_live_coordinators_after_lost_reply.load(SeqCst) {
                self.fail_live_coordinators.store(true, SeqCst);
            }
            return Err(AwsError::Unavailable("connection reset reading SubmitJob's reply".into()));
        }
        Ok(CoordinatorJobId::new(job))
    }

    async fn live_coordinators(
        &self,
        queue: &str,
        run_id: &RunId,
    ) -> Result<Vec<crate::aws::batch::LiveCoordinator>, AwsError> {
        use std::sync::atomic::Ordering::SeqCst;
        self.live_coordinator_queues.lock().unwrap().push(queue.to_string());
        let on_listing = self.on_listing.lock().unwrap().clone();
        if let Some(on_listing) = on_listing {
            on_listing(run_id, queue);
        }
        let stall = self.stall_listings_for.lock().unwrap().get(run_id.as_str()).copied();
        let stall = stall.or_else(|| self.stall_listings_on.lock().unwrap().get(queue).copied());
        if let Some(stall) = stall {
            tokio::time::sleep(stall).await;
        }
        if self.missing_queues.lock().unwrap().contains(queue) {
            return Err(AwsError::Batch(format!("job queue {queue} does not exist")));
        }
        if self.unlistable_queues.lock().unwrap().contains(queue) {
            return Err(AwsError::Batch(format!("ClientException: listing {queue} refused")));
        }
        if self.fail_live_coordinators.load(SeqCst)
            || self.unavailable_queues.lock().unwrap().contains(queue)
        {
            return Err(AwsError::Unavailable("batch throttled".into()));
        }
        let listed = self.listed_coordinators(run_id, queue);
        let slow = *self.slow_listing_reply.lock().unwrap();
        if !slow.is_zero() {
            biffix_core::clock::testing::advance(slow);
        }
        Ok(listed)
    }

    async fn variant_job_definition(
        &self,
        runtime_key: &str,
        base_digest: &str,
        image: &str,
        template: crate::aws::batch::VariantTemplate<'_>,
    ) -> Result<String, AwsError> {
        let template_arn = template.arn;
        {
            // One request's entries pushed together, under all three locks (always taken in this
            // order), so index `i` of each is the same request even when calls run concurrently.
            let mut definitions = self.variant_job_definitions.lock().unwrap();
            let mut templates = self.variant_templates.lock().unwrap();
            let mut single_job = self.variant_single_job.lock().unwrap();
            definitions.push((runtime_key.to_string(), base_digest.to_string(), image.to_string()));
            templates.push(template_arn.to_string());
            single_job.push(template.single_job);
        }
        let revision = *self.template_revision.lock().unwrap();
        Ok(fake_project_variant_job_definition(runtime_key, base_digest, revision, template))
    }

    async fn task_job_definition(
        &self,
        definition: &crate::aws::task_definitions::TaskJobDefinition,
    ) -> Result<String, AwsError> {
        if let Some(fail) = *self.fail_task_job_definitions.lock().unwrap() {
            return Err(fail());
        }
        self.task_job_definitions.lock().unwrap().push(definition.clone());
        Ok(fake_task_job_definition(definition))
    }

    fn variant_is_current(
        &self,
        runtime_key: &str,
        base_digest: &str,
        job_definition_arn: &str,
        template: crate::aws::batch::VariantTemplate<'_>,
    ) -> bool {
        // Like the provider: current iff it is the one this (runtime, base, template) names.
        let revision = *self.template_revision.lock().unwrap();
        job_definition_arn
            == fake_project_variant_job_definition(runtime_key, base_digest, revision, template)
    }

    async fn job_queue_gone(&self, queue: &str) -> Result<bool, AwsError> {
        self.job_queue_gone_asks.lock().unwrap().push(queue.to_string());
        if self.fail_job_queue_gone.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(AwsError::Unavailable("batch throttled".into()));
        }
        Ok(self.missing_queues.lock().unwrap().contains(queue))
    }

    async fn terminate_job(&self, job_id: &str, reason: &str) -> Result<(), AwsError> {
        let on_terminate = self.on_terminate.lock().unwrap().clone();
        if let Some(on_terminate) = on_terminate {
            on_terminate(job_id);
        }
        let stall = self.stall_terminate_of.lock().unwrap().get(job_id).copied();
        if let Some(stall) = stall {
            tokio::time::sleep(stall).await;
        }
        if self.fail_terminate.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(AwsError::Batch("terminate denied".into()));
        }
        self.terminated.lock().unwrap().push((job_id.to_string(), reason.to_string()));
        Ok(())
    }
}

#[async_trait]
impl BuildService for FakeCloud {
    async fn start_runtime_build(
        &self,
        request: &crate::aws::builds::RuntimeBuildJob,
    ) -> Result<String, AwsError> {
        Ok(self.start_once(&request.idempotency_token, || {
            let mut builds = self.runtime_builds.lock().unwrap();
            builds.push(request.clone());
            format!("arn:aws:codebuild:us-east-1:123:build/biffix-t-runtime-build:{}", builds.len())
        }))
    }

    async fn start_image_import(
        &self,
        request: &crate::aws::builds::ImageImportJob,
    ) -> Result<String, AwsError> {
        Ok(self.start_once(&request.idempotency_token, || {
            let mut imports = self.image_imports.lock().unwrap();
            imports.push(request.clone());
            format!("arn:aws:codebuild:us-east-1:123:build/biffix-t-image-import:{}", imports.len())
        }))
    }

    async fn start_image_derive(
        &self,
        request: &crate::aws::builds::ImageDeriveJob,
    ) -> Result<String, AwsError> {
        Ok(self.start_once(&request.idempotency_token, || {
            let mut derives = self.image_derives.lock().unwrap();
            derives.push(request.clone());
            format!(
                "arn:aws:codebuild:us-east-1:123:build/biffix-t-image-import:derive-{}",
                derives.len()
            )
        }))
    }

    async fn start_image_build(
        &self,
        request: &crate::aws::builds::ImageBuildJob,
    ) -> Result<String, AwsError> {
        Ok(self.start_once(&request.idempotency_token, || {
            let mut builds = self.image_builds.lock().unwrap();
            builds.push(request.clone());
            format!(
                "arn:aws:codebuild:us-east-1:123:build/biffix-t-image-build-{}:{}",
                request.arch,
                builds.len()
            )
        }))
    }

    async fn start_image_index(
        &self,
        request: &crate::aws::builds::ImageIndexJob,
    ) -> Result<String, AwsError> {
        Ok(self.start_once(&request.idempotency_token, || {
            let mut indexes = self.image_indexes.lock().unwrap();
            indexes.push(request.clone());
            format!(
                "arn:aws:codebuild:us-east-1:123:build/biffix-t-image-import:index-{}",
                indexes.len()
            )
        }))
    }

    async fn build_event(
        &self,
        build_arn: &str,
    ) -> Result<Option<biffix_core::wire::aws_codebuild::CodeBuildEvent>, AwsError> {
        self.build_event_asks.lock().unwrap().push(build_arn.to_string());
        if self.fail_build_event.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(AwsError::Unavailable("codebuild throttled".into()));
        }
        Ok(self.build_events.lock().unwrap().get(build_arn).cloned())
    }
}

#[async_trait]
impl ImageRegistry for FakeCloud {
    async fn coordinator_base(&self) -> Result<crate::aws::registry::CoordinatorBase, AwsError> {
        if let Some(err) = self.fail_coordinator_base.lock().unwrap().take() {
            return Err(err);
        }
        Ok(crate::aws::registry::CoordinatorBase {
            repo: FAKE_COORDINATOR_REPO.into(),
            digest: FAKE_BASE_DIGEST.into(),
        })
    }

    /// A tag resolves to the sha256 of the reference itself (stable per reference).
    async fn resolve_image_digest(
        &self,
        image: &biffix_workflow::images::ImageRef,
    ) -> Result<String, AwsError> {
        if let Some(digest) = &image.digest {
            return Ok(digest.clone());
        }
        // A missing image (a public tag, or a private ECR one ECR says does not exist) is the
        // caller's error, as the provider reports it.
        if self.missing_images.lock().unwrap().contains(&image.to_string()) {
            return Err(AwsError::Image(format!("{image}: not found in its registry")));
        }
        // Like the provider: a private ECR tag resolves through ECR, here to a fixed fake digest.
        if image.is_private_ecr() {
            return Ok(format!("sha256:{}", "a".repeat(64)));
        }
        Ok(format!("sha256:{}", crate::sha256_hex(image.to_string())))
    }
}

#[async_trait]
impl ObjectStore for FakeCloud {
    async fn head_object(&self, uri: &str) -> Result<Option<String>, AwsError> {
        self.object_checks.lock().unwrap().push(uri.to_string());
        // Missing if a test lists it in `absent` (or it is only a folder).
        if self.absent.lock().unwrap().contains(uri) || self.folders.lock().unwrap().contains(uri) {
            return Ok(None);
        }
        // A seeded object: its tag as the trait default the real provider uses derives it from
        // the same HEAD `head_object_meta` answers (the version id, else the ETag).
        if let Some(head) = self.object_head(uri) {
            return Ok(Some(head.version_id.or(head.etag).unwrap_or_default()));
        }
        // Deliberate simplification: any other URI exists, with no tag (see `absent`).
        Ok(Some(String::new()))
    }

    async fn prefix_exists(&self, uri: &str) -> Result<bool, AwsError> {
        self.object_checks.lock().unwrap().push(uri.to_string());
        Ok(!self.absent.lock().unwrap().contains(uri))
    }

    async fn presign_source_put(
        &self,
        bucket: &str,
        key: &str,
        write_once: bool,
    ) -> Result<crate::aws::objects::PresignedSourceUpload, AwsError> {
        self.source_puts.lock().unwrap().push((bucket.to_string(), key.to_string()));
        Ok(crate::aws::objects::PresignedSourceUpload {
            url: format!("https://fake-presigned.example/{key}?sig=fake"),
            headers: write_once
                .then(|| ("if-none-match".to_string(), "*".to_string()))
                .into_iter()
                .collect(),
            uri: format!("s3://{bucket}/{key}"),
        })
    }

    async fn presign_get(
        &self,
        uri: &str,
        expires: std::time::Duration,
        content_disposition: Option<&str>,
        content_type: Option<&str>,
    ) -> Result<String, AwsError> {
        Ok(format!(
            // Not a real URL encoding: the fake only needs to echo what the handler asked for.
            "https://fake-presigned.example/{}?expires={}&type={}#{}",
            uri.trim_start_matches("s3://"),
            expires.as_secs(),
            content_type.unwrap_or(""),
            content_disposition.unwrap_or("")
        ))
    }

    /// Lists `objects` like S3's ListObjectsV2: keys in order, `max_keys` per page (the token is the
    /// next key's index). The bucket `denied` answers AccessDenied.
    async fn list_objects(
        &self,
        bucket: &str,
        prefix: &str,
        delimited: bool,
        token: Option<&str>,
        max_keys: i32,
    ) -> Result<crate::aws::objects::ObjectPage, AwsError> {
        if bucket == "denied" {
            return Err(AwsError::Forbidden("AccessDenied".into()));
        }
        let objects = self.objects.lock().unwrap();
        // Entries in key order: a folder (common prefix) once, or an object.
        let mut entries: Vec<(String, Option<usize>)> = Vec::new();
        for (uri, bytes) in objects.iter() {
            let Some((b, key)) = biffix_core::s3_layout::split_s3_uri(uri) else {
                continue;
            };
            let Some(rest) = key.strip_prefix(prefix).filter(|_| b == bucket) else {
                continue;
            };
            match rest.find('/').filter(|_| delimited) {
                Some(i) => {
                    let folder = format!("{prefix}{}", &rest[..=i]);
                    if entries.last().map(|(k, _)| k) != Some(&folder) {
                        entries.push((folder, None));
                    }
                }
                None => entries.push((key.to_string(), Some(bytes.len()))),
            }
        }
        // Like S3: a token it did not issue is refused (an error the handler maps), never a panic.
        let start: usize =
            match token {
                None => 0,
                Some(t) => t.parse().ok().filter(|&i| i <= entries.len()).ok_or_else(|| {
                    AwsError::Invalid(format!("invalid continuation token {t:?}"))
                })?,
            };
        let end = (start + max_keys.max(0) as usize).min(entries.len());
        let mut page = crate::aws::objects::ObjectPage::default();
        for (key, size) in &entries[start..end] {
            match size {
                None => page.prefixes.push(key.clone()),
                Some(n) => page.objects.push(crate::aws::objects::ListedObject {
                    key: key.clone(),
                    size: *n as i64,
                    last_modified: Some(1_700_000_000_000_000),
                    etag: Some(Self::etag_for(*n)),
                }),
            }
        }
        page.next_token = (end < entries.len()).then(|| end.to_string());
        Ok(page)
    }

    async fn put_object(
        &self,
        uri: &str,
        body: Vec<u8>,
        _content_type: &str,
    ) -> Result<String, AwsError> {
        if self.fail_puts.lock().unwrap().contains(uri) {
            return Err(AwsError::S3("put denied".into()));
        }
        let etag = Self::etag_for(body.len());
        self.objects.lock().unwrap().insert(uri.to_string(), body);
        Ok(etag)
    }

    async fn head_object_meta(
        &self,
        uri: &str,
    ) -> Result<Option<crate::aws::objects::ObjectHead>, AwsError> {
        self.heads.lock().unwrap().push(uri.to_string());
        Ok(self.object_head(uri))
    }

    async fn get_object_range(
        &self,
        uri: &str,
        start: u64,
        end: u64,
    ) -> Result<crate::aws::objects::RangeBody, AwsError> {
        self.ranges.lock().unwrap().push(uri.to_string());
        if self.stall_ranges.load(std::sync::atomic::Ordering::SeqCst) {
            std::future::pending::<()>().await;
        }
        let objects = self.objects.lock().unwrap();
        let bytes = objects.get(uri).ok_or(AwsError::NotFound)?;
        // An inverted range is a handler bug: an error it must surface, not a panic in the fake.
        if start > end {
            return Err(AwsError::Invalid(format!("inverted range bytes={start}-{end}")));
        }
        let start = (start as usize).min(bytes.len());
        let end = (end as usize + 1).min(bytes.len());
        Ok(crate::aws::objects::RangeBody {
            bytes: bytes[start..end].to_vec(),
            total_size: Some(bytes.len() as u64),
        })
    }
}

#[async_trait]
impl MultipartUploads for FakeCloud {
    async fn create_multipart_upload(
        &self,
        uri: &str,
        _content_type: &str,
    ) -> Result<String, AwsError> {
        let mut uploads = self.uploads.lock().unwrap();
        let id = format!("upload-{}", self.upload_seq.fetch_add(1, Ordering::SeqCst) + 1);
        uploads.insert(id.clone(), (uri.to_string(), BTreeMap::new()));
        Ok(id)
    }

    async fn presign_upload_part(
        &self,
        uri: &str,
        upload_id: &str,
        part_number: i32,
        _expires: std::time::Duration,
    ) -> Result<String, AwsError> {
        Ok(format!(
            "https://s3.fake/{}?uploadId={upload_id}&partNumber={part_number}",
            uri.trim_start_matches("s3://")
        ))
    }

    async fn list_upload_parts(
        &self,
        uri: &str,
        upload_id: &str,
    ) -> Result<Vec<crate::aws::uploads::UploadedPart>, AwsError> {
        let uploads = self.uploads.lock().unwrap();
        let (u, parts) = uploads.get(upload_id).ok_or(AwsError::NotFound)?;
        if u != uri {
            return Err(AwsError::NotFound);
        }
        Ok(parts
            .iter()
            .map(|(n, b)| crate::aws::uploads::UploadedPart {
                part_number: *n,
                size: b.len() as i64,
                etag: Self::part_etag(*n, b.len()),
            })
            .collect())
    }

    async fn complete_multipart_upload(
        &self,
        uri: &str,
        upload_id: &str,
        parts: &[(i32, String)],
    ) -> Result<crate::aws::uploads::UploadCompletion, AwsError> {
        use crate::aws::uploads::UploadCompletion;
        // Like S3: a rejected part list or a failed precondition leaves the upload open (it can be
        // completed again, or aborted); only a completed upload is gone.
        let mut uploads = self.uploads.lock().unwrap();
        let (u, stored) = uploads.get(upload_id).ok_or(AwsError::NotFound)?;
        if u != uri {
            return Err(AwsError::NotFound);
        }
        let mut body = Vec::new();
        for (n, etag) in parts {
            match stored.get(n) {
                Some(b) if Self::part_etag(*n, b.len()) == *etag => body.extend_from_slice(b),
                _ => return Err(AwsError::Invalid(format!("InvalidPart: part {n}"))),
            }
        }
        // Create-only (`If-None-Match: *`): an object already at the key is never replaced.
        let mut objects = self.objects.lock().unwrap();
        if objects.contains_key(uri) {
            return Ok(UploadCompletion::Exists);
        }
        let etag = format!("{}-{}", Self::etag_for(body.len()), parts.len());
        objects.insert(uri.to_string(), body);
        uploads.remove(upload_id);
        Ok(UploadCompletion::Created(etag))
    }

    async fn abort_multipart_upload(&self, uri: &str, upload_id: &str) -> Result<(), AwsError> {
        let mut uploads = self.uploads.lock().unwrap();
        match uploads.get(upload_id) {
            Some((u, _)) if u == uri => {
                uploads.remove(upload_id);
                Ok(())
            }
            _ => Err(AwsError::NotFound),
        }
    }
}

#[async_trait]
impl LogReader for FakeCloud {
    async fn get_log_events(
        &self,
        log_stream: &str,
        cursor: Option<&str>,
        direction: crate::aws::logs::LogDirection,
        limit: i32,
    ) -> Result<crate::aws::logs::CloudLogPage, AwsError> {
        self.log_reads.lock().unwrap().push((
            log_stream.to_string(),
            cursor.map(String::from),
            direction,
            limit,
        ));
        // A missing stream 404s; anything else returns one canned line naming the stream so a test
        // can assert the server derived the stream from the run/task (never the client).
        if log_stream == "missing" {
            return Err(AwsError::NotFound);
        }
        if let Some(lines) = self.logs.lock().unwrap().get(log_stream) {
            return Ok(crate::aws::logs::CloudLogPage {
                events: lines
                    .iter()
                    .map(|m| crate::aws::logs::CloudLogEvent {
                        timestamp_ms: 1_700_000_000_000,
                        message: m.clone(),
                    })
                    .collect(),
                next_forward_token: None,
                next_backward_token: None,
            });
        }
        Ok(crate::aws::logs::CloudLogPage {
            events: vec![crate::aws::logs::CloudLogEvent {
                timestamp_ms: 1_700_000_000_000,
                message: format!("log line for {log_stream}"),
            }],
            next_forward_token: Some("f/next".into()),
            next_backward_token: Some("b/next".into()),
        })
    }

    async fn get_log_events_in(
        &self,
        log_group: &str,
        log_stream: &str,
        cursor: Option<&str>,
        direction: crate::aws::logs::LogDirection,
        limit: i32,
    ) -> Result<crate::aws::logs::CloudLogPage, AwsError> {
        self.group_reads.lock().unwrap().push((log_group.to_string(), log_stream.to_string()));
        self.log_reads.lock().unwrap().push((
            log_stream.to_string(),
            cursor.map(String::from),
            direction,
            limit,
        ));
        if let Some(msg) = self.fail_group_reads.lock().unwrap().clone() {
            return Err(AwsError::Forbidden(msg));
        }
        let lines =
            self.project_logs.lock().unwrap().get(log_stream).cloned().ok_or(AwsError::NotFound)?;
        Ok(crate::aws::logs::CloudLogPage {
            events: lines
                .into_iter()
                .map(|message| crate::aws::logs::CloudLogEvent {
                    timestamp_ms: 1_700_000_000_000,
                    message,
                })
                .collect(),
            next_forward_token: None,
            next_backward_token: None,
        })
    }
}

#[async_trait]
impl AlertTopic for FakeCloud {
    async fn list_alert_subscribers(
        &self,
    ) -> Result<Vec<crate::aws::alerts::AlertSubscriber>, AwsError> {
        self.alert_subscribers.lock().unwrap().clone().ok_or(AwsError::AlertsNotConfigured)
    }

    async fn add_alert_subscriber(&self, email: &str) -> Result<(), AwsError> {
        let mut subs = self.alert_subscribers.lock().unwrap();
        let subs = subs.as_mut().ok_or(AwsError::AlertsNotConfigured)?;
        // A new subscription starts pending, as with SNS.
        subs.push(crate::aws::alerts::AlertSubscriber {
            email: email.to_string(),
            subscription_arn: None,
        });
        Ok(())
    }

    async fn remove_alert_subscriber(&self, subscription_arn: &str) -> Result<(), AwsError> {
        let mut subs = self.alert_subscribers.lock().unwrap();
        let subs = subs.as_mut().ok_or(AwsError::AlertsNotConfigured)?;
        // The provider's own guard: only a subscription of the alerts topic (the fake's is
        // `arn:aws:sns:us-east-1:1:alerts`) may be removed.
        crate::aws::alerts::is_subscription_of("arn:aws:sns:us-east-1:1:alerts", subscription_arn)?;
        subs.retain(|s| s.subscription_arn.as_deref() != Some(subscription_arn));
        Ok(())
    }
}
