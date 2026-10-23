//! [`FakeCloud`]: the fake [`crate::aws::CloudProvider`], one impl per concern (Batch, CodeBuild,
//! image registries, S3 objects and uploads, CloudWatch Logs, SNS) over one shared state.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

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
    /// How many of the next `launch_coordinator` calls create their job (`coord-lost-<n>`) but
    /// lose the reply: an unavailable AWS, as the caller sees it.
    pub lose_launch_replies: AtomicUsize,
    /// Make `live_coordinators` list nothing (Batch does not list a job it just created yet).
    pub hide_live_coordinators: std::sync::atomic::AtomicBool,
    /// Make `live_coordinators` fail (Batch unavailable).
    pub fail_live_coordinators: std::sync::atomic::AtomicBool,
    /// Every build `build_event` was asked about, in call order.
    pub build_event_asks: Mutex<Vec<String>>,
    /// Make `build_event` fail (CodeBuild unavailable).
    pub fail_build_event: std::sync::atomic::AtomicBool,
}

impl Default for FakeCloud {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeCloud {
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
            lose_launch_replies: AtomicUsize::new(0),
            hide_live_coordinators: std::sync::atomic::AtomicBool::new(false),
            fail_live_coordinators: std::sync::atomic::AtomicBool::new(false),
            build_event_asks: Mutex::new(Vec::new()),
            fail_build_event: std::sync::atomic::AtomicBool::new(false),
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

/// [`fake_variant_job_definition`] cloned from the coordinator template `template_arn`: a
/// project's own template (any but the default project's, which is empty in these tests) names
/// its variants apart (`…-t<revision>-<template name>:1`).
pub fn fake_project_variant_job_definition(
    runtime_key: &str,
    base_digest: &str,
    template: u32,
    template_arn: &str,
) -> String {
    let default = fake_variant_job_definition(runtime_key, base_digest, template);
    let Some((_, name)) = template_arn.split_once(":job-definition/") else {
        return default;
    };
    let name = name.split(':').next().unwrap_or(name);
    format!("{}-{name}:1", default.trim_end_matches(":1"))
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
        let job = if lost {
            format!("coord-lost-{}", self.coordinators.lock().unwrap().len() + 1)
        } else {
            "coord-1".to_string()
        };
        self.coordinators.lock().unwrap().push((job.clone(), launch.run_id.to_string()));
        if lost {
            return Err(AwsError::Unavailable("connection reset reading SubmitJob's reply".into()));
        }
        Ok(CoordinatorJobId::new(job))
    }

    async fn live_coordinators(
        &self,
        _queue: &str,
        run_id: &RunId,
    ) -> Result<Vec<String>, AwsError> {
        use std::sync::atomic::Ordering::SeqCst;
        if self.fail_live_coordinators.load(SeqCst) {
            return Err(AwsError::Unavailable("batch throttled".into()));
        }
        if self.hide_live_coordinators.load(SeqCst) {
            return Ok(Vec::new());
        }
        let terminated = self.terminated.lock().unwrap();
        Ok(self
            .coordinators
            .lock()
            .unwrap()
            .iter()
            .filter(|(job, run)| {
                run == run_id.as_str() && !terminated.iter().any(|(t, _)| t == job)
            })
            .map(|(job, _)| job.clone())
            .collect())
    }

    async fn variant_job_definition(
        &self,
        runtime_key: &str,
        base_digest: &str,
        image: &str,
        template_arn: &str,
    ) -> Result<String, AwsError> {
        self.variant_job_definitions.lock().unwrap().push((
            runtime_key.to_string(),
            base_digest.to_string(),
            image.to_string(),
        ));
        self.variant_templates.lock().unwrap().push(template_arn.to_string());
        let template = *self.template_revision.lock().unwrap();
        Ok(fake_project_variant_job_definition(runtime_key, base_digest, template, template_arn))
    }

    fn variant_is_current(
        &self,
        runtime_key: &str,
        base_digest: &str,
        job_definition_arn: &str,
        template_arn: &str,
    ) -> bool {
        // Like the provider: current iff it is the one this (runtime, base, template) names.
        let template = *self.template_revision.lock().unwrap();
        job_definition_arn
            == fake_project_variant_job_definition(runtime_key, base_digest, template, template_arn)
    }

    async fn terminate_job(&self, job_id: &str, reason: &str) -> Result<(), AwsError> {
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
