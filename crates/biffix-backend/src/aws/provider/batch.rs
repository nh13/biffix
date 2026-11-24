//! Batch: the run session, the coordinator's launch, its variant job definitions, the task job
//! definitions, cancel and the job owners the engine ingest route checks.

use async_trait::async_trait;
use aws_sdk_batch::types::{ContainerOverrides, ContainerProperties, JobDefinition, KeyValuePair};

use biffix_core::ids::{CoordinatorJobId, RunId};

use super::errors::{classify_invalid, OrClassify};
use super::AwsCloudProvider;
use crate::aws::batch::{
    BatchJobOwner, BatchJobs, CoordinatorLaunch, LaunchProject, RunSession, VariantTemplate,
};
use crate::aws::coordinator_env::{check_overrides_size, coordinator_env, coordinator_job_tags};
use crate::aws::task_definitions::{self, TaskJobDefinition};
use crate::aws::AwsError;

impl AwsCloudProvider {
    /// The name prefix of variants of `template`'s kind: a coordinator template's, or a single-job
    /// template's (their own, so IAM tells the two apart; [`VariantTemplate`]).
    fn variant_prefix(&self, template: VariantTemplate<'_>) -> Result<&str, AwsError> {
        let (prefix, var) = if template.single_job {
            (
                &self.cfg.single_job_variant_job_definition_prefix,
                "BIFFIX_SINGLE_JOB_VARIANT_JOB_DEFINITION_PREFIX",
            )
        } else {
            (&self.cfg.variant_job_definition_prefix, "BIFFIX_VARIANT_JOB_DEFINITION_PREFIX")
        };
        prefix.as_deref().ok_or_else(|| AwsError::Unsupported(format!("{var} is unset")))
    }

    /// The name prefix of task job definitions.
    fn task_prefix(&self) -> Result<&str, AwsError> {
        self.cfg.task_job_definition_prefix.as_deref().ok_or_else(|| {
            AwsError::Unsupported("BIFFIX_TASK_JOB_DEFINITION_PREFIX is unset".into())
        })
    }

    /// The newest ACTIVE revision of the job definition `name` that `keep` accepts
    /// ([`newest_revision`]), reading every page: registering always adds a revision, so a name
    /// registered again (concurrent launches, a variant rebuilt for many image digests) piles up
    /// revisions past one page.
    async fn newest_active_revision<F>(
        &self,
        name: &str,
        keep: F,
    ) -> Result<Option<String>, AwsError>
    where
        F: Fn(&JobDefinition) -> bool,
    {
        let mut pages = self
            .batch
            .describe_job_definitions()
            .job_definition_name(name)
            .status("ACTIVE")
            .into_paginator()
            .send();
        let mut revisions = Vec::new();
        while let Some(page) = pages.next().await {
            revisions.extend(page.or_classify(AwsError::Batch)?.job_definitions().iter().cloned());
        }
        Ok(newest_revision(&revisions, keep))
    }
}

/// The container a task job definition registers ([`crate::aws::task_definitions`]): the image,
/// the project's run role, its task log group (in `region`), placeholder command and resources
/// (each task's own come in its `SubmitJob`), never privileged, and only the mounts the
/// definition names (the host AWS CLI, read-only, for Nextflow) and its `/dev/shm` size
/// (Snakemake).
fn task_container(definition: &TaskJobDefinition, region: Option<&str>) -> ContainerProperties {
    use aws_sdk_batch::types::{
        Host, LinuxParameters, LogConfiguration, LogDriver, MountPoint, ResourceRequirement,
        ResourceType, Volume,
    };
    let resource = |kind: ResourceType, value: &str| {
        ResourceRequirement::builder().r#type(kind).value(value).build()
    };
    let mut logs = LogConfiguration::builder()
        .log_driver(LogDriver::Awslogs)
        .options("awslogs-group", &definition.log_group);
    if let Some(region) = region {
        logs = logs.options("awslogs-region", region);
    }
    let mut container = ContainerProperties::builder()
        .image(&definition.image)
        .command(task_definitions::PLACEHOLDER_COMMAND)
        .job_role_arn(&definition.job_role_arn)
        .privileged(false)
        .resource_requirements(resource(ResourceType::Vcpu, task_definitions::PLACEHOLDER_VCPUS))
        .resource_requirements(resource(
            ResourceType::Memory,
            task_definitions::PLACEHOLDER_MEMORY_MIB,
        ))
        .log_configuration(logs.build());
    if let Some(size) = definition.shared_memory_mib() {
        container =
            container.linux_parameters(LinuxParameters::builder().shared_memory_size(size).build());
    }
    if definition.mounts_host_aws_cli() {
        let dir = task_definitions::HOST_AWS_CLI_DIR;
        let volume = task_definitions::HOST_AWS_CLI_VOLUME;
        container = container
            .volumes(
                Volume::builder()
                    .name(volume)
                    .host(Host::builder().source_path(dir).build())
                    .build(),
            )
            .mount_points(
                MountPoint::builder()
                    .source_volume(volume)
                    .container_path(dir)
                    .read_only(true)
                    .build(),
            );
    }
    container.build()
}

/// Whether `existing`, an ACTIVE revision named for a task job definition, carries `wanted` (the
/// container [`task_container`] builds for it) tagged with `project`. The name already fixes all of
/// that for what the backend registers; comparing every field a definition could use to reach the
/// host (privileged, user, volumes, mount points, devices, tmpfs, `/dev/shm`, ulimits), to put
/// something in every task (environment, secrets, resource requirements), or to change how its
/// jobs run (a timeout, a retry strategy, parameters, a platform other than EC2, a scheduling
/// priority, tag propagation, consumable resources, ECS Exec: none of which the backend
/// registers), keeps a revision registered some other way, or by an older backend, from being
/// used. Fields Batch leaves out when they are empty or default compare equal to empty or default
/// ones.
fn carries(existing: &JobDefinition, wanted: &ContainerProperties, project: &str) -> bool {
    use aws_sdk_batch::types::PlatformCapability;
    let tagged = existing
        .tags()
        .and_then(|tags| tags.get(crate::aws::batch::PROJECT_TAG))
        .map(String::as_str)
        == Some(project);
    let job_defaults = existing.timeout().is_none()
        && existing.retry_strategy().is_none()
        && existing.parameters().is_none_or(std::collections::HashMap::is_empty)
        && existing.platform_capabilities().iter().all(|p| *p == PlatformCapability::Ec2)
        && existing.scheduling_priority().is_none()
        && !existing.propagate_tags().unwrap_or(false)
        && existing
            .consumable_resource_properties()
            .is_none_or(|c| c.consumable_resource_list().is_empty());
    tagged
        && job_defaults
        && existing.container_properties().is_some_and(|c| same_task_container(c, wanted))
}

/// Whether two task containers are the same in everything a task job definition decides: the
/// image, role, privileges, ECS Exec, user, host volumes and mounts, environment and secrets, resource
/// requirements (in order: a reordered answer only costs a new revision), Linux parameters and
/// logging. Absent and empty (or default) compare equal, since Batch may answer either for what
/// was registered.
fn same_task_container(a: &ContainerProperties, b: &ContainerProperties) -> bool {
    use aws_sdk_batch::types::{Device, LinuxParameters, LogDriver, Secret, Tmpfs};
    type Linux<'a> = (&'a [Device], &'a [Tmpfs], Option<i32>, bool, Option<i32>, Option<i32>);
    fn linux(c: &ContainerProperties) -> Linux<'_> {
        let l = c.linux_parameters();
        (
            l.map_or(&[][..], |l| l.devices()),
            l.map_or(&[][..], |l| l.tmpfs()),
            l.and_then(LinuxParameters::shared_memory_size),
            l.and_then(LinuxParameters::init_process_enabled).unwrap_or(false),
            l.and_then(LinuxParameters::max_swap),
            l.and_then(LinuxParameters::swappiness),
        )
    }
    type Logs<'a> = Option<(
        Option<&'a LogDriver>,
        Option<&'a std::collections::HashMap<String, String>>,
        &'a [Secret],
    )>;
    fn logs(c: &ContainerProperties) -> Logs<'_> {
        c.log_configuration().map(|l| (l.log_driver(), l.options(), l.secret_options()))
    }
    fn user(c: &ContainerProperties) -> Option<&str> {
        c.user().filter(|u| !u.is_empty())
    }
    a.image() == b.image()
        && a.job_role_arn() == b.job_role_arn()
        && a.execution_role_arn() == b.execution_role_arn()
        && a.privileged().unwrap_or(false) == b.privileged().unwrap_or(false)
        && a.enable_execute_command().unwrap_or(false)
            == b.enable_execute_command().unwrap_or(false)
        && a.readonly_root_filesystem().unwrap_or(false)
            == b.readonly_root_filesystem().unwrap_or(false)
        && user(a) == user(b)
        && a.volumes() == b.volumes()
        && a.mount_points() == b.mount_points()
        && a.ulimits() == b.ulimits()
        && a.environment() == b.environment()
        && a.secrets() == b.secrets()
        && a.resource_requirements() == b.resource_requirements()
        && linux(a) == linux(b)
        && logs(a) == logs(b)
}

/// The ARN of the newest of `revisions` that `keep` accepts: the highest revision number (one
/// without a number ranks below every numbered one), skipping any without an ARN.
fn newest_revision<'a, I, F>(revisions: I, keep: F) -> Option<String>
where
    I: IntoIterator<Item = &'a JobDefinition>,
    F: Fn(&JobDefinition) -> bool,
{
    revisions
        .into_iter()
        .filter(|d| keep(d))
        .filter_map(|d| d.job_definition_arn().map(|arn| (d.revision(), arn)))
        .max_by_key(|(revision, _)| *revision)
        .map(|(_, arn)| arn.to_string())
}

/// A coordinator variant's job definition name:
/// `<prefix><runtime key12>-<base digest12>-<template8>`, where `template8` is a short hash of the
/// coordinator template's revision ARN. Unique per (runtime, base, template) and within Batch's
/// 128-character limit. A deploy that registers a new template revision therefore names new
/// variants, so each variant is cloned again once after it (the old revisions stay ACTIVE for the
/// runs already launched on them); each project's template names its own variants, so a project's
/// variant is found by name.
fn variant_job_definition_name(
    prefix: &str,
    runtime_key: &str,
    base_digest: &str,
    template_arn: &str,
) -> String {
    let base = base_digest.strip_prefix("sha256:").unwrap_or(base_digest);
    let short = |s: &str| s.chars().take(12).collect::<String>();
    let template = crate::sha256_hex(template_arn);
    format!("{prefix}{}-{}-{}", short(runtime_key), short(base), &template[..8])
}

/// The name of the job definition a Batch job-definition ARN names
/// (`arn:aws:batch:<region>:<account>:job-definition/<name>:<revision>`).
fn job_definition_name_of(arn: &str) -> Option<&str> {
    let (_, rest) = arn.split_once(":job-definition/")?;
    Some(rest.rsplit_once(':').map_or(rest, |(name, _)| name))
}

/// The container overrides of a coordinator's SubmitJob: its environment (`env`), and for a
/// single-job run (`single_job`, the whole host its one job is given) the job's vCPUs and memory,
/// overriding the template's own request (a placeholder), so Batch places the job alone on one
/// host of the queue-class's exact types. A coordinator of a run one Batch job per task keeps the
/// template's.
fn coordinator_overrides(
    env: Vec<(&'static str, String)>,
    single_job: Option<crate::aws::ec2::Resources>,
) -> ContainerOverrides {
    use aws_sdk_batch::types::{ResourceRequirement, ResourceType};
    let env = env
        .into_iter()
        .map(|(name, value)| KeyValuePair::builder().name(name).value(value).build())
        .collect();
    let resources = single_job.map(|size| {
        [(ResourceType::Vcpu, u64::from(size.vcpus)), (ResourceType::Memory, size.memory_mib)]
            .into_iter()
            .map(|(kind, value)| {
                ResourceRequirement::builder().r#type(kind).value(value.to_string()).build()
            })
            .collect::<Vec<_>>()
    });
    ContainerOverrides::builder()
        .set_environment(Some(env))
        .set_resource_requirements(resources)
        .build()
}

/// The coordinator job definition's container, running the variant `image` instead: everything
/// else (command, resources, job role, environment, logging) is the template's.
fn variant_container(
    template: &aws_sdk_batch::types::JobDefinition,
    image: &str,
) -> Result<aws_sdk_batch::types::ContainerProperties, AwsError> {
    let mut container = template.container_properties().cloned().ok_or_else(|| {
        AwsError::Batch("the coordinator job definition has no container properties".into())
    })?;
    container.image = Some(image.to_string());
    Ok(container)
}

/// Send `submit` once, with no SDK retry. SubmitJob takes no idempotency token, so an attempt that
/// timed out may have created a job: a retry that then succeeded would create a second, and the
/// launch would bind only that one, leaving the first running unbound with no record of it. A
/// failure the SDK would have retried (a timeout, throttling, a 5xx) instead reaches the launch as
/// [`AwsError::Unavailable`], whose lost-answer path looks for the job (`crate::runs::submit`).
async fn submit_job_once(
    submit: aws_sdk_batch::operation::submit_job::builders::SubmitJobFluentBuilder,
) -> Result<aws_sdk_batch::operation::submit_job::SubmitJobOutput, AwsError> {
    submit
        .customize()
        .config_override(
            aws_sdk_batch::config::Builder::default()
                .retry_config(aws_sdk_batch::config::retry::RetryConfig::disabled()),
        )
        .send()
        .await
        .or_classify(AwsError::Batch)
}

#[async_trait]
impl BatchJobs for AwsCloudProvider {
    async fn mint_run_session(
        &self,
        run_id: &RunId,
        project: &LaunchProject,
    ) -> Result<RunSession, AwsError> {
        // Scoping is per-project: the run uses its project's base role directly (delivered to
        // the coordinator and its children via Batch, not via forwarded credentials), so there is
        // no STS AssumeRole here. The role ARN is recorded on the Run as its identity/provenance.
        Ok(RunSession {
            run_role_arn: project.run_role_arn.clone(),
            capability_token: (self.token_minter)(run_id),
        })
    }

    async fn launch_coordinator(
        &self,
        launch: &CoordinatorLaunch,
    ) -> Result<CoordinatorJobId, AwsError> {
        let env = coordinator_env(launch, &self.cfg.ingest_url, &self.cfg.worker_image_repo)?;
        check_overrides_size(&env)?;
        let overrides = coordinator_overrides(env, launch.single_job);

        let submit = self
            .batch
            .submit_job()
            .job_name(crate::aws::batch::coordinator_job_name(&launch.run_id))
            .job_queue(&launch.coordinator_queue)
            .job_definition(&launch.variant.job_definition)
            .container_overrides(overrides)
            .set_tags(Some(coordinator_job_tags(
                &launch.run_id,
                &launch.project.id,
                launch.single_job.is_some(),
            )));
        let out = submit_job_once(submit).await?;

        out.job_id()
            .map(CoordinatorJobId::new)
            .ok_or_else(|| AwsError::Batch("SubmitJob returned no jobId".into()))
    }

    async fn variant_job_definition(
        &self,
        runtime_key: &str,
        base_digest: &str,
        image: &str,
        template: VariantTemplate<'_>,
    ) -> Result<String, AwsError> {
        use aws_sdk_batch::types::JobDefinitionType;
        let template_arn = template.arn;
        let name = variant_job_definition_name(
            self.variant_prefix(template)?,
            runtime_key,
            base_digest,
            template_arn,
        );
        // Registering always adds a revision, so an existing one for this image is reused. The
        // name carries the template, so a reused revision was cloned from the current one.
        let same_image =
            |d: &JobDefinition| d.container_properties().and_then(|c| c.image()) == Some(image);
        if let Some(arn) = self.newest_active_revision(&name, same_image).await? {
            return Ok(arn);
        }

        let template = self
            .batch
            .describe_job_definitions()
            .job_definitions(template_arn)
            .send()
            .await
            .or_classify(AwsError::Batch)?;
        let template = template.job_definitions().first().ok_or_else(|| {
            AwsError::Batch(format!("coordinator job definition {template_arn} not found"))
        })?;
        let container = variant_container(template, image)?;
        let out = self
            .batch
            .register_job_definition()
            .job_definition_name(&name)
            .r#type(JobDefinitionType::Container)
            .container_properties(container)
            .set_retry_strategy(template.retry_strategy().cloned())
            .set_timeout(template.timeout().cloned())
            .set_parameters(template.parameters().cloned())
            .set_propagate_tags(template.propagate_tags())
            .set_platform_capabilities(Some(template.platform_capabilities().to_vec()))
            .tags("biffix:runtime", runtime_key)
            .tags("biffix:base", base_digest)
            .tags("biffix:template", template_arn)
            .send()
            .await
            .or_classify(AwsError::Batch)?;
        out.job_definition_arn()
            .map(str::to_string)
            .ok_or_else(|| AwsError::Batch("RegisterJobDefinition returned no ARN".into()))
    }

    async fn task_job_definition(
        &self,
        definition: &TaskJobDefinition,
    ) -> Result<String, AwsError> {
        use aws_sdk_batch::types::{JobDefinitionType, PlatformCapability};
        let name = definition.name(self.task_prefix()?);
        let region = self.batch.config().region().map(|r| r.as_ref().to_string());
        let container = task_container(definition, region.as_deref());
        // Reuse the newest ACTIVE revision that carries it (concurrent launches may have
        // registered several).
        let keep = |d: &JobDefinition| carries(d, &container, &definition.project);
        if let Some(arn) = self.newest_active_revision(&name, keep).await? {
            return Ok(arn);
        }
        let out = self
            .batch
            .register_job_definition()
            .job_definition_name(&name)
            .r#type(JobDefinitionType::Container)
            .container_properties(container)
            .platform_capabilities(PlatformCapability::Ec2)
            // The project only (IAM allows this one tag): a definition serves every run of it.
            .tags(crate::aws::batch::PROJECT_TAG, &definition.project)
            .send()
            .await
            .or_classify(AwsError::Batch)?;
        out.job_definition_arn()
            .map(str::to_string)
            .ok_or_else(|| AwsError::Batch("RegisterJobDefinition returned no ARN".into()))
    }

    fn variant_is_current(
        &self,
        runtime_key: &str,
        base_digest: &str,
        job_definition_arn: &str,
        template: VariantTemplate<'_>,
    ) -> bool {
        // Without a prefix no variant of the template's kind can be registered. A coordinator
        // template's recorded variant is then the one to use (nothing to compare it with); a
        // single-job template's never is (it is a coordinator template's clone), so registering
        // one is attempted and fails naming the unset prefix.
        let Ok(prefix) = self.variant_prefix(template) else {
            return !template.single_job;
        };
        job_definition_name_of(job_definition_arn)
            == Some(&variant_job_definition_name(prefix, runtime_key, base_digest, template.arn))
    }

    async fn batch_job_owners(
        &self,
        job_ids: &[biffix_core::ids::BatchJobId],
    ) -> Result<std::collections::BTreeMap<String, BatchJobOwner>, AwsError> {
        // DescribeJobs takes at most 100 ids per call.
        const DESCRIBE_JOBS_MAX: usize = 100;
        let mut owners = std::collections::BTreeMap::new();
        for chunk in job_ids.chunks(DESCRIBE_JOBS_MAX) {
            let out = self
                .batch
                .describe_jobs()
                .set_jobs(Some(chunk.iter().map(|id| id.as_str().to_string()).collect()))
                .send()
                .await
                // Batch refuses a job id it cannot parse with a `ClientException`: the claimant's.
                .map_err(|e| classify_invalid(AwsError::Batch, &["ClientException"], &e))?;
            for job in out.jobs() {
                let Some(job_id) = job.job_id() else {
                    continue;
                };
                let run = job
                    .tags()
                    .and_then(|tags| tags.get(crate::aws::batch::RUN_TAG))
                    .map(|run| RunId::new(run.as_str()));
                owners.insert(
                    job_id.to_string(),
                    BatchJobOwner {
                        run,
                        created_at_micros: job.created_at().map(|ms| ms.saturating_mul(1000)),
                    },
                );
            }
        }
        Ok(owners)
    }

    async fn live_coordinators(
        &self,
        queue: &str,
        run_id: &RunId,
    ) -> Result<Vec<crate::aws::batch::LiveCoordinator>, AwsError> {
        use aws_sdk_batch::types::{JobStatus, KeyValuesPair};
        // With a filter, ListJobs lists jobs of every status (that Batch still keeps); a job-name
        // filter without a trailing `*` matches the whole name.
        let mut pages = self
            .batch
            .list_jobs()
            .job_queue(queue)
            .filters(
                KeyValuesPair::builder()
                    .name("JOB_NAME")
                    .values(crate::aws::batch::coordinator_job_name(run_id))
                    .build(),
            )
            .into_paginator()
            .send();
        let mut live = Vec::new();
        while let Some(page) = pages.next().await {
            let page = page.or_classify(AwsError::Batch)?;
            live.extend(
                page.job_summary_list()
                    .iter()
                    .filter(|j| {
                        !matches!(j.status(), Some(JobStatus::Succeeded | JobStatus::Failed))
                    })
                    .filter_map(|j| {
                        Some(crate::aws::batch::LiveCoordinator {
                            job_id: j.job_id()?.to_string(),
                            created_at_micros: j.created_at().map(|ms| ms.saturating_mul(1000)),
                        })
                    }),
            );
        }
        Ok(live)
    }

    async fn job_queue_gone(&self, queue: &str) -> Result<bool, AwsError> {
        let out = self
            .batch
            .describe_job_queues()
            .job_queues(queue)
            .send()
            .await
            .or_classify(AwsError::Batch)?;
        Ok(job_queue_gone(out.job_queues(), queue))
    }

    async fn terminate_job(&self, job_id: &str, reason: &str) -> Result<(), AwsError> {
        self.batch
            .terminate_job()
            .job_id(job_id)
            .reason(reason)
            .send()
            .await
            .or_classify(AwsError::Batch)?;
        Ok(())
    }
}

/// Whether DescribeJobQueues's answer for `queue` (an ARN or name), `described`, says it is gone:
/// an empty answer (Batch knows no such queue), or one with at least one queue of that ARN or name
/// and every such queue DELETED. Not DELETING: Batch terminates a deleted queue's jobs only
/// eventually, so a coordinator may still run on it. An answer naming only other queues says
/// nothing of this one, so it is not gone.
fn job_queue_gone(described: &[aws_sdk_batch::types::JobQueueDetail], queue: &str) -> bool {
    use aws_sdk_batch::types::JqStatus;
    if described.is_empty() {
        return true;
    }
    let mut matching = described
        .iter()
        .filter(|q| q.job_queue_arn() == Some(queue) || q.job_queue_name() == Some(queue))
        .peekable();
    matching.peek().is_some() && matching.all(|q| matches!(q.status(), Some(JqStatus::Deleted)))
}

#[cfg(test)]
mod tests {
    use aws_sdk_batch::types::{
        ContainerProperties, JobDefinition, KeyValuePair, PlatformCapability, ResourceRequirement,
        ResourceType,
    };

    use super::*;

    /// A SubmitJob Batch answers with a server error is sent once: the SDK's default retry policy
    /// (which this client keeps for every other call) would send it again, and an attempt that
    /// timed out after Batch created the job would then create a second.
    #[tokio::test]
    async fn a_failed_submit_job_is_never_retried() {
        let sent = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = sent.clone();
        let http_client = aws_smithy_runtime_api::client::http::http_client_fn(move |_, _| {
            aws_smithy_runtime_api::client::http::SharedHttpConnector::new(ServerError {
                sent: counted.clone(),
            })
        });
        let config = aws_config::SdkConfig::builder()
            .http_client(http_client)
            .credentials_provider(aws_sdk_batch::config::SharedCredentialsProvider::new(
                aws_sdk_batch::config::Credentials::new("akid", "secret", None, None, "test"),
            ))
            .region(aws_sdk_batch::config::Region::new("us-east-1"))
            .behavior_version(aws_sdk_batch::config::BehaviorVersion::latest())
            .build();
        let client = aws_sdk_batch::Client::new(&config);
        let submit = || client.submit_job().job_name("j").job_queue("q").job_definition("d");
        // The client as configured retries a server error...
        assert!(submit().send().await.is_err());
        let retried = sent.swap(0, std::sync::atomic::Ordering::SeqCst);
        assert!(retried > 1, "the default policy retries: {retried}");
        // ...but a coordinator's SubmitJob is sent once, its failure unavailable.
        let err = submit_job_once(submit()).await.unwrap_err();
        assert_eq!(sent.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert!(matches!(err, AwsError::Unavailable(_)), "{err:?}");
    }

    /// Answers every request with Batch's server error, counting them.
    #[derive(Debug)]
    struct ServerError {
        sent: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }

    impl aws_smithy_runtime_api::client::http::HttpConnector for ServerError {
        fn call(
            &self,
            _request: aws_smithy_runtime_api::client::orchestrator::HttpRequest,
        ) -> aws_smithy_runtime_api::client::http::HttpConnectorFuture {
            self.sent.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let body = r#"{"__type":"ServerException","message":"m"}"#;
            let mut response = aws_smithy_runtime_api::client::orchestrator::HttpResponse::new(
                500.try_into().unwrap(),
                aws_smithy_types::body::SdkBody::from(body),
            );
            response.headers_mut().insert("x-amzn-errortype", "ServerException");
            aws_smithy_runtime_api::client::http::HttpConnectorFuture::ready(Ok(response))
        }
    }

    const TEMPLATE: &str = "arn:aws:batch:us-east-1:123:job-definition/biffix-t-coordinator:7";
    const RUNTIME: &str = "5f7b17f81c0795ac01066ff9d1300a8ad2f4a69bc120c5b1bd25c4f0792732ad";
    const BASE: &str = "sha256:263fd06ff40c1093130edbdcecf662c42ff76f9534b6f2c915d04248b91bb365";

    #[test]
    fn a_variant_job_definition_is_named_by_runtime_base_and_template() {
        let name = variant_job_definition_name("biffix-t-coordinator-rt-", RUNTIME, BASE, TEMPLATE);
        let (stem, template) = name.rsplit_once('-').unwrap();
        assert_eq!(stem, "biffix-t-coordinator-rt-5f7b17f81c07-263fd06ff40c");
        assert_eq!(template.len(), 8);
        assert!(name.len() <= 128);
        // A new template revision names a new variant; the same one names the same.
        let next = TEMPLATE.replace(":7", ":8");
        let renamed = variant_job_definition_name("biffix-t-coordinator-rt-", RUNTIME, BASE, &next);
        assert_ne!(name, renamed);
        assert!(renamed.starts_with(stem));
        assert_eq!(
            name,
            variant_job_definition_name("biffix-t-coordinator-rt-", RUNTIME, BASE, TEMPLATE)
        );
    }

    #[test]
    fn a_job_definition_arn_names_its_job_definition() {
        let name = variant_job_definition_name("p-", RUNTIME, BASE, TEMPLATE);
        let arn = format!("arn:aws:batch:us-east-1:123:job-definition/{name}:3");
        assert_eq!(job_definition_name_of(&arn), Some(name.as_str()));
        assert_eq!(
            job_definition_name_of("arn:aws:batch:us-east-1:123:job-definition/x"),
            Some("x")
        );
        assert_eq!(job_definition_name_of("arn:aws:iam::123:role/x"), None);
    }

    #[test]
    fn a_single_job_coordinator_asks_for_the_whole_host_and_a_tasks_one_for_nothing() {
        use aws_sdk_batch::types::ResourceType;
        let env = vec![("BIFFIX_RUN_ID", "run-1".to_string())];
        let size = crate::aws::ec2::Resources { vcpus: 32, memory_mib: 61_184 };
        let single = coordinator_overrides(env.clone(), Some(size));
        let asked: Vec<_> = single
            .resource_requirements()
            .iter()
            .map(|r| (r.r#type().cloned(), r.value()))
            .collect();
        assert_eq!(
            asked,
            [(Some(ResourceType::Vcpu), Some("32")), (Some(ResourceType::Memory), Some("61184"))]
        );
        assert_eq!(single.environment().len(), 1);
        assert_eq!(single.environment()[0].value(), Some("run-1"));
        // One Batch job per task: the template's own request stands.
        let tasks = coordinator_overrides(env, None);
        assert!(tasks.resource_requirements.is_none());
        assert_eq!(tasks.environment()[0].name(), Some("BIFFIX_RUN_ID"));
    }

    #[test]
    fn a_variant_runs_the_template_container_with_its_own_image() {
        let region = KeyValuePair::builder().name("BIFFIX_REGION").value("us-east-1").build();
        let template = JobDefinition::builder()
            .job_definition_name("biffix-t-coordinator")
            .job_definition_arn(TEMPLATE)
            .revision(7)
            .r#type("container")
            .container_properties(
                ContainerProperties::builder()
                    .image("123.dkr.ecr.us-east-1.amazonaws.com/coord:base")
                    .command("biffix-coordinator")
                    .job_role_arn("arn:aws:iam::123:role/coordinator")
                    .environment(region.clone())
                    .build(),
            )
            .build();
        let container = variant_container(&template, "repo@sha256:variant").unwrap();
        assert_eq!(container.image.as_deref(), Some("repo@sha256:variant"));
        assert_eq!(container.command, Some(vec!["biffix-coordinator".to_string()]));
        assert_eq!(container.job_role_arn.as_deref(), Some("arn:aws:iam::123:role/coordinator"));
        assert_eq!(container.environment, Some(vec![region]));
    }

    #[test]
    fn a_template_without_a_container_cannot_be_cloned() {
        let template = JobDefinition::builder()
            .job_definition_name("x")
            .job_definition_arn("arn:x")
            .revision(1)
            .r#type("container")
            .build();
        assert!(variant_container(&template, "repo@sha256:v").is_err());
    }

    fn task_definition(engine: biffix_workflow::engines::Engine) -> TaskJobDefinition {
        TaskJobDefinition {
            engine,
            project: "demo".into(),
            image: "123.dkr.ecr.us-east-1.amazonaws.com/workers@sha256:abc".into(),
            job_role_arn: "arn:aws:iam::123:role/run".into(),
            log_group: "/biffix/t/demo/tasks".into(),
        }
    }

    #[test]
    fn a_snakemake_task_container_is_unprivileged_with_a_fixed_shm_and_no_mounts() {
        let definition = task_definition(biffix_workflow::engines::Engine::Snakemake);
        let c = task_container(&definition, Some("us-east-1"));
        assert_eq!(c.image(), Some(definition.image.as_str()));
        assert_eq!(c.job_role_arn(), Some(definition.job_role_arn.as_str()));
        assert_eq!(c.privileged(), Some(false));
        assert_eq!(
            c.linux_parameters()
                .and_then(aws_sdk_batch::types::LinuxParameters::shared_memory_size),
            Some(task_definitions::SNAKEMAKE_SHARED_MEMORY_MIB)
        );
        // Nothing of the host: no volumes, mounts or devices.
        assert!(c.volumes().is_empty() && c.mount_points().is_empty());
        assert!(c.linux_parameters().is_some_and(|l| l.devices().is_empty()));
        let logs = c.log_configuration().unwrap();
        assert_eq!(logs.log_driver().map(aws_sdk_batch::types::LogDriver::as_str), Some("awslogs"));
        let options = logs.options().unwrap();
        assert_eq!(options["awslogs-group"], definition.log_group);
        assert_eq!(options["awslogs-region"], "us-east-1");
        assert_eq!(c.command(), [task_definitions::PLACEHOLDER_COMMAND]);
        let resources: Vec<_> =
            c.resource_requirements().iter().map(|r| (r.r#type().cloned(), r.value())).collect();
        assert_eq!(
            resources,
            [
                (Some(ResourceType::Vcpu), Some(task_definitions::PLACEHOLDER_VCPUS)),
                (Some(ResourceType::Memory), Some(task_definitions::PLACEHOLDER_MEMORY_MIB)),
            ]
        );
    }

    #[test]
    fn a_nextflow_task_container_mounts_only_the_host_aws_cli_read_only() {
        let c = task_container(&task_definition(biffix_workflow::engines::Engine::Nextflow), None);
        assert_eq!(c.privileged(), Some(false));
        assert!(c.linux_parameters().is_none());
        let [volume] = c.volumes() else { panic!("one volume: {:?}", c.volumes()) };
        assert_eq!(volume.name(), Some(task_definitions::HOST_AWS_CLI_VOLUME));
        assert_eq!(
            volume.host().and_then(|h| h.source_path()),
            Some(task_definitions::HOST_AWS_CLI_DIR)
        );
        let [mount] = c.mount_points() else { panic!("one mount: {:?}", c.mount_points()) };
        assert_eq!(mount.source_volume(), Some(task_definitions::HOST_AWS_CLI_VOLUME));
        assert_eq!(mount.container_path(), Some(task_definitions::HOST_AWS_CLI_DIR));
        assert_eq!(mount.read_only(), Some(true));
        // Without a region, the log driver's own default applies.
        let options = c.log_configuration().unwrap().options().unwrap();
        assert!(!options.contains_key("awslogs-region"));
    }

    /// A revision of the task job definition `biffix-t_task-sm-x`, numbered `revision`, carrying
    /// `container`, tagged with `project` (none: untagged).
    fn revision(
        revision: Option<i32>,
        container: ContainerProperties,
        project: Option<&str>,
    ) -> JobDefinition {
        let mut d = JobDefinition::builder()
            .job_definition_name("biffix-t_task-sm-x")
            .job_definition_arn(format!("arn:x:{}", revision.unwrap_or(0)))
            .set_revision(revision)
            .r#type("container")
            .container_properties(container);
        if let Some(project) = project {
            d = d.tags(crate::aws::batch::PROJECT_TAG, project);
        }
        d.build()
    }

    #[test]
    fn only_a_revision_that_carries_the_definition_is_reused() {
        use aws_sdk_batch::types::{Device, Host, LinuxParameters, LogConfiguration, Volume};
        let definition = task_definition(biffix_workflow::engines::Engine::Snakemake);
        let ours = task_container(&definition, Some("us-east-1"));
        let reused = |container: ContainerProperties| {
            carries(&revision(Some(1), container, Some("demo")), &ours, "demo")
        };
        assert!(reused(ours.clone()));
        // What Batch answers for empty settings it was not sent is the same container.
        let mut answered = ours.clone();
        answered.volumes = Some(Vec::new());
        answered.mount_points = Some(Vec::new());
        answered.ulimits = Some(Vec::new());
        answered.privileged = None;
        answered.user = Some(String::new());
        answered.enable_execute_command = Some(false);
        assert!(reused(answered));
        // Another project's, or an untagged one: never.
        assert!(!carries(&revision(Some(1), ours.clone(), Some("other")), &ours, "demo"));
        assert!(!carries(&revision(Some(1), ours.clone(), None), &ours, "demo"));
        // Anything else it decides, changed: never.
        let changes: [fn(&mut ContainerProperties); 14] = [
            |c| {
                c.resource_requirements = Some(vec![
                    ResourceRequirement::builder()
                        .r#type(ResourceType::Vcpu)
                        .value(task_definitions::PLACEHOLDER_VCPUS)
                        .build(),
                    ResourceRequirement::builder()
                        .r#type(ResourceType::Memory)
                        .value(task_definitions::PLACEHOLDER_MEMORY_MIB)
                        .build(),
                    ResourceRequirement::builder().r#type(ResourceType::Gpu).value("1").build(),
                ]);
            },
            |c| c.resource_requirements = None,
            |c| {
                c.environment =
                    Some(vec![KeyValuePair::builder().name("LD_PRELOAD").value("x").build()]);
            },
            |c| {
                c.secrets = Some(vec![aws_sdk_batch::types::Secret::builder()
                    .name("TOKEN")
                    .value_from("arn:aws:secretsmanager:us-east-1:123:secret:x")
                    .build()]);
            },
            |c| c.image = Some("repo@sha256:other".into()),
            |c| c.job_role_arn = Some("arn:aws:iam::123:role/other".into()),
            |c| c.privileged = Some(true),
            |c| c.enable_execute_command = Some(true),
            |c| c.user = Some("root".into()),
            |c| c.log_configuration = None,
            |c| {
                c.log_configuration = Some(
                    LogConfiguration::builder()
                        .log_driver(aws_sdk_batch::types::LogDriver::Awslogs)
                        .options("awslogs-group", "/biffix/t/other/tasks")
                        .options("awslogs-region", "us-east-1")
                        .build(),
                );
            },
            |c| {
                c.volumes = Some(vec![Volume::builder()
                    .name("root")
                    .host(Host::builder().source_path("/").build())
                    .build()]);
            },
            |c| {
                c.linux_parameters = Some(
                    LinuxParameters::builder()
                        .shared_memory_size(task_definitions::SNAKEMAKE_SHARED_MEMORY_MIB)
                        .devices(Device::builder().host_path("/dev/kvm").build())
                        .build(),
                );
            },
            |c| c.linux_parameters = None,
        ];
        for (i, change) in changes.into_iter().enumerate() {
            let mut other = ours.clone();
            change(&mut other);
            assert!(!reused(other), "change {i} was reused");
        }
        // A job-level setting the backend never registers, set: never.
        let job_changes: [fn(&mut JobDefinition); 7] = [
            |d| {
                d.timeout = Some(
                    aws_sdk_batch::types::JobTimeout::builder()
                        .attempt_duration_seconds(60)
                        .build(),
                );
            },
            |d| {
                d.retry_strategy =
                    Some(aws_sdk_batch::types::RetryStrategy::builder().attempts(3).build());
            },
            |d| {
                d.parameters =
                    Some(std::collections::HashMap::from([("x".to_string(), "y".to_string())]));
            },
            |d| d.platform_capabilities = Some(vec![PlatformCapability::Fargate]),
            |d| d.scheduling_priority = Some(100),
            |d| d.propagate_tags = Some(true),
            |d| {
                d.consumable_resource_properties = Some(
                    aws_sdk_batch::types::ConsumableResourceProperties::builder()
                        .consumable_resource_list(
                            aws_sdk_batch::types::ConsumableResourceRequirement::builder()
                                .consumable_resource("licenses")
                                .quantity(1)
                                .build(),
                        )
                        .build(),
                );
            },
        ];
        for (i, change) in job_changes.into_iter().enumerate() {
            let mut other = revision(Some(1), ours.clone(), Some("demo"));
            change(&mut other);
            assert!(!carries(&other, &ours, "demo"), "job-level change {i} was reused");
        }
        // What Batch answers for the EC2 platform and empty parameters is the same definition.
        let mut answered = revision(Some(1), ours.clone(), Some("demo"));
        answered.platform_capabilities = Some(vec![PlatformCapability::Ec2]);
        answered.parameters = Some(std::collections::HashMap::new());
        answered.propagate_tags = Some(false);
        answered.consumable_resource_properties =
            Some(aws_sdk_batch::types::ConsumableResourceProperties::builder().build());
        assert!(carries(&answered, &ours, "demo"));
    }

    #[test]
    fn the_newest_revision_that_qualifies_is_reused() {
        let definition = task_definition(biffix_workflow::engines::Engine::Nextflow);
        let ours = task_container(&definition, None);
        let mut privileged = ours.clone();
        privileged.privileged = Some(true);
        // Mixed order, the newest failing, one without a revision number.
        let revisions = [
            revision(Some(2), ours.clone(), Some("demo")),
            revision(Some(5), privileged.clone(), Some("demo")),
            revision(None, ours.clone(), Some("demo")),
            revision(Some(4), ours.clone(), Some("demo")),
            revision(Some(3), ours.clone(), Some("demo")),
            revision(Some(6), ours.clone(), None),
        ];
        let keep = |d: &JobDefinition| carries(d, &ours, "demo");
        assert_eq!(newest_revision(&revisions, keep).as_deref(), Some("arn:x:4"));
        // Only one without a number qualifies: it is used.
        let unnumbered = [revision(None, ours.clone(), Some("demo"))];
        assert_eq!(newest_revision(&unnumbered, keep).as_deref(), Some("arn:x:0"));
        // None qualifies (or there are none): register.
        assert_eq!(newest_revision(&revisions[1..2], keep), None);
        assert_eq!(newest_revision(&[], keep), None);
        // A revision Batch answered without an ARN is never chosen.
        let mut no_arn = revision(Some(9), ours.clone(), Some("demo"));
        no_arn.job_definition_arn = None;
        let with = [revisions[0].clone(), no_arn];
        assert_eq!(newest_revision(&with, keep).as_deref(), Some("arn:x:2"));
    }

    #[test]
    fn a_job_queue_is_gone_only_when_batch_knows_none_or_reports_it_deleted() {
        use aws_sdk_batch::types::{JobQueueDetail, JqStatus};
        const ARN: &str = "arn:aws:batch:us-east-1:123:job-queue/biffix-t-coordinator";
        let queue = |status: Option<JqStatus>| {
            let detail =
                JobQueueDetail::builder().job_queue_name("biffix-t-coordinator").job_queue_arn(ARN);
            match status {
                Some(status) => detail.status(status).build(),
                None => detail.build(),
            }
        };
        // Batch knows no such queue.
        assert!(job_queue_gone(&[], ARN));
        // Deleted, by ARN or by name.
        assert!(job_queue_gone(&[queue(Some(JqStatus::Deleted))], ARN));
        assert!(job_queue_gone(&[queue(Some(JqStatus::Deleted))], "biffix-t-coordinator"));
        // Any other state, or none reported, is a queue that may still run jobs: one being deleted
        // too, whose jobs Batch terminates only eventually.
        for status in [
            Some(JqStatus::Deleting),
            Some(JqStatus::Valid),
            Some(JqStatus::Invalid),
            Some(JqStatus::Creating),
            Some(JqStatus::Updating),
            Some(JqStatus::from("SOMETHING_NEW")),
            None,
        ] {
            assert!(!job_queue_gone(&[queue(status.clone())], ARN), "{status:?}");
        }
        // An answer naming only other queues, even deleted ones, says nothing of this one.
        let other = JobQueueDetail::builder()
            .job_queue_name("biffix-t-other")
            .job_queue_arn("arn:aws:batch:us-east-1:123:job-queue/biffix-t-other")
            .status(JqStatus::Deleted)
            .build();
        assert!(!job_queue_gone(std::slice::from_ref(&other), ARN));
        assert!(!job_queue_gone(&[other], "biffix-t-coordinator"));
    }
}
