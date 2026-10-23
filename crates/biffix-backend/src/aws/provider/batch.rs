//! Batch: the run session, the coordinator's launch, its variant job definitions, cancel and the
//! job owners the engine ingest route checks.

use async_trait::async_trait;
use aws_sdk_batch::types::{ContainerOverrides, KeyValuePair};

use biffix_core::ids::{CoordinatorJobId, RunId};

use super::errors::{classify_invalid, OrClassify};
use super::AwsCloudProvider;
use crate::aws::batch::{BatchJobOwner, BatchJobs, CoordinatorLaunch, LaunchProject, RunSession};
use crate::aws::coordinator_env::{check_overrides_size, coordinator_env, coordinator_job_tags};
use crate::aws::AwsError;

impl AwsCloudProvider {
    /// The name prefix of variant job definitions.
    fn variant_prefix(&self) -> Result<&str, AwsError> {
        self.cfg.variant_job_definition_prefix.as_deref().ok_or_else(|| {
            AwsError::Unsupported("BIFFIX_VARIANT_JOB_DEFINITION_PREFIX is unset".into())
        })
    }
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
        let env = env
            .into_iter()
            .map(|(name, value)| KeyValuePair::builder().name(name).value(value).build())
            .collect();
        let overrides = ContainerOverrides::builder().set_environment(Some(env)).build();

        let out = self
            .batch
            .submit_job()
            .job_name(crate::aws::batch::coordinator_job_name(&launch.run_id))
            .job_queue(&launch.coordinator_queue)
            .job_definition(&launch.variant.job_definition)
            .container_overrides(overrides)
            .set_tags(Some(coordinator_job_tags(&launch.run_id, &launch.project.id)))
            .send()
            .await
            .or_classify(AwsError::Batch)?;

        out.job_id()
            .map(CoordinatorJobId::new)
            .ok_or_else(|| AwsError::Batch("SubmitJob returned no jobId".into()))
    }

    async fn variant_job_definition(
        &self,
        runtime_key: &str,
        base_digest: &str,
        image: &str,
        template_arn: &str,
    ) -> Result<String, AwsError> {
        use aws_sdk_batch::types::JobDefinitionType;
        let name = variant_job_definition_name(
            self.variant_prefix()?,
            runtime_key,
            base_digest,
            template_arn,
        );
        // Registering always adds a revision, so an existing one for this image is reused. The
        // name carries the template, so a reused revision was cloned from the current one.
        // Every page: a name rebuilt for many image digests piles up revisions past one page.
        let mut pages = self
            .batch
            .describe_job_definitions()
            .job_definition_name(&name)
            .status("ACTIVE")
            .into_paginator()
            .send();
        let mut existing: Option<(Option<i32>, String)> = None;
        while let Some(page) = pages.next().await {
            let page = page.or_classify(AwsError::Batch)?;
            for d in page.job_definitions() {
                let same_image =
                    d.container_properties().and_then(|c| c.image.as_deref()) == Some(image);
                if let (true, Some(arn)) = (same_image, d.job_definition_arn()) {
                    if existing.as_ref().is_none_or(|(rev, _)| d.revision() > *rev) {
                        existing = Some((d.revision(), arn.to_string()));
                    }
                }
            }
        }
        if let Some((_, arn)) = existing {
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

    fn variant_is_current(
        &self,
        runtime_key: &str,
        base_digest: &str,
        job_definition_arn: &str,
        template_arn: &str,
    ) -> bool {
        // Without a prefix no variant can be registered, so there is nothing to compare with.
        let Ok(prefix) = self.variant_prefix() else {
            return true;
        };
        job_definition_name_of(job_definition_arn)
            == Some(&variant_job_definition_name(prefix, runtime_key, base_digest, template_arn))
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
    ) -> Result<Vec<String>, AwsError> {
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
                    .filter_map(|j| j.job_id().map(str::to_string)),
            );
        }
        Ok(live)
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

#[cfg(test)]
mod tests {
    use aws_sdk_batch::types::{ContainerProperties, JobDefinition, KeyValuePair};

    use super::*;

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
}
