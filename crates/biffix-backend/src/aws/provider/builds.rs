//! CodeBuild: runtime builds and workflow image imports, builds, indexes and derives, and a
//! build's state polled as the event EventBridge would deliver.

use async_trait::async_trait;

use super::errors::{classify_configured, OrClassify};
use super::AwsCloudProvider;
use crate::aws::builds::{
    BuildService, ImageBuildJob, ImageDeriveJob, ImageImportJob, ImageIndexJob, RuntimeBuildJob,
};
use crate::aws::AwsError;

impl AwsCloudProvider {
    /// Start a build of `project` with `env` overriding its environment; returns the build's ARN.
    /// A repeat with the same `idempotency_token` within 5 minutes returns the first call's build
    /// (see `crate::images::build_follow::start_token`).
    async fn start_codebuild(
        &self,
        project: &str,
        env: &[(&str, &str)],
        idempotency_token: &str,
    ) -> Result<String, AwsError> {
        use aws_sdk_codebuild::types::{EnvironmentVariable, EnvironmentVariableType};
        let env = env
            .iter()
            .map(|(name, value)| {
                EnvironmentVariable::builder()
                    .name(*name)
                    .value(*value)
                    .r#type(EnvironmentVariableType::Plaintext)
                    .build()
                    .map_err(|e| AwsError::CodeBuild(e.to_string()))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let out = self
            .codebuild
            .start_build()
            .project_name(project)
            .idempotency_token(idempotency_token)
            .set_environment_variables_override(Some(env))
            .send()
            .await
            .map_err(|e| {
                classify_configured(
                    AwsError::CodeBuild,
                    &format!("CodeBuild project {project}"),
                    &e,
                )
            })?;
        out.build_value()
            .and_then(|b| b.arn())
            .map(str::to_string)
            .ok_or_else(|| AwsError::CodeBuild("StartBuild returned no build".into()))
    }

    /// The CodeBuild project that imports, derives and indexes workflow images.
    fn image_import_project(&self) -> Result<&str, AwsError> {
        self.cfg
            .image_import_project
            .as_deref()
            .ok_or_else(|| AwsError::Unsupported("BIFFIX_IMAGE_IMPORT_PROJECT is unset".into()))
    }
}

/// A build from `BatchGetBuilds` as the CodeBuild state-change event EventBridge would deliver for
/// its current status (same subject, outputs, reason and log link), or `None` for a build without a
/// biffix subject or status. Its time is when the build ended (or started, while it runs).
fn codebuild_event(
    build: &aws_sdk_codebuild::types::Build,
) -> Option<biffix_core::wire::aws_codebuild::CodeBuildEvent> {
    use biffix_core::wire::aws_codebuild::{CodeBuildEvent, CodeBuildStatus, SUBJECT_VARIABLE};
    let subject = build
        .environment()?
        .environment_variables()
        .iter()
        .find(|v| v.name() == SUBJECT_VARIABLE)?
        .value()
        .to_string();
    let status: CodeBuildStatus =
        serde_json::from_value(serde_json::Value::String(build.build_status()?.as_str().into()))
            .ok()?;
    let ts = build.end_time().or(build.start_time())?.as_secs_f64();
    // CodeBuild reports a phase context's status code and message apart; the event's
    // `phase-context` entries are `STATUS: message` strings, so join them the same way.
    let reason =
        biffix_core::eventbridge::decode::failed_phase_reason(build.phases().iter().map(|phase| {
            let context = phase
                .contexts()
                .iter()
                .map(|c| {
                    format!(
                        "{}: {}",
                        c.status_code().unwrap_or_default(),
                        c.message().unwrap_or_default()
                    )
                })
                .collect();
            (
                phase.phase_status().map(aws_sdk_codebuild::types::StatusType::as_str),
                phase.phase_type().map(aws_sdk_codebuild::types::BuildPhaseType::as_str),
                context,
            )
        }));
    Some(CodeBuildEvent {
        ts: biffix_core::time::Timestamp::from_epoch_seconds(ts),
        build_arn: build.arn()?.to_string(),
        project: build.project_name().unwrap_or_default().to_string(),
        status,
        subject,
        outputs: build
            .exported_environment_variables()
            .iter()
            .filter_map(|v| {
                Some((v.name()?.to_string(), v.value().unwrap_or_default().to_string()))
            })
            .collect(),
        reason,
        logs_url: build.logs().and_then(|l| l.deep_link()).map(str::to_string),
    })
}

#[async_trait]
impl BuildService for AwsCloudProvider {
    async fn start_runtime_build(&self, request: &RuntimeBuildJob) -> Result<String, AwsError> {
        let project =
            self.cfg.runtime_build_project.as_deref().ok_or_else(|| {
                AwsError::Unsupported("BIFFIX_RUNTIME_BUILD_PROJECT is unset".into())
            })?;
        let env: Vec<(&str, &str)> = [
            (biffix_core::wire::aws_codebuild::SUBJECT_VARIABLE, request.subject.as_str()),
            ("BIFFIX_BASE_IMAGE", request.base_image.as_str()),
        ]
        .into_iter()
        .chain(request.env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        .collect();
        self.start_codebuild(project, &env, &request.idempotency_token).await
    }

    async fn start_image_import(&self, request: &ImageImportJob) -> Result<String, AwsError> {
        let project = self.image_import_project()?;
        let env = [
            (biffix_core::wire::aws_codebuild::SUBJECT_VARIABLE, request.subject.as_str()),
            ("BIFFIX_IMAGE_SOURCE", request.source.as_str()),
            ("BIFFIX_CONTENT_KEY", request.content_key.as_str()),
        ];
        self.start_codebuild(project, &env, &request.idempotency_token).await
    }

    async fn start_image_derive(&self, request: &ImageDeriveJob) -> Result<String, AwsError> {
        let project = self.image_import_project()?;
        let env = [
            (biffix_core::wire::aws_codebuild::SUBJECT_VARIABLE, request.subject.as_str()),
            ("BIFFIX_IMAGE_STEP", "derive"),
            ("BIFFIX_IMAGE_BASE", request.base.as_str()),
            ("BIFFIX_RUNTIME_LAYER_AMD64", request.layer_amd64.as_str()),
            ("BIFFIX_RUNTIME_LAYER_ARM64", request.layer_arm64.as_str()),
            ("BIFFIX_DERIVED_KEY", request.derived_key.as_str()),
            ("BIFFIX_SNAKEMAKE_VERSION", request.snakemake_version.as_str()),
        ];
        self.start_codebuild(project, &env, &request.idempotency_token).await
    }

    async fn start_image_build(&self, request: &ImageBuildJob) -> Result<String, AwsError> {
        use base64::Engine as _;
        let project = self.cfg.image_build_projects.get(&request.arch).ok_or_else(|| {
            AwsError::Unsupported(format!(
                "BIFFIX_IMAGE_BUILD_PROJECT_{} is unset",
                request.arch.as_str().to_ascii_uppercase()
            ))
        })?;
        let dockerfile_b64 = request
            .dockerfile_text
            .as_ref()
            .map(|t| base64::engine::general_purpose::STANDARD.encode(t));
        let mut env = vec![
            (biffix_core::wire::aws_codebuild::SUBJECT_VARIABLE, request.subject.as_str()),
            ("BIFFIX_CONTENT_KEY", request.content_key.as_str()),
            ("BIFFIX_CONTEXT_URI", request.context_uri.as_str()),
            ("BIFFIX_CONTEXT_SHA256", request.context_sha256.as_str()),
        ];
        if let Some(dockerfile) = &request.dockerfile {
            env.push(("BIFFIX_DOCKERFILE", dockerfile.as_str()));
        }
        if let Some(b64) = &dockerfile_b64 {
            env.push(("BIFFIX_DOCKERFILE_B64", b64.as_str()));
        }
        self.start_codebuild(project, &env, &request.idempotency_token).await
    }

    async fn start_image_index(&self, request: &ImageIndexJob) -> Result<String, AwsError> {
        let project = self.image_import_project()?;
        let manifests = request.manifests.join(",");
        let env = [
            (biffix_core::wire::aws_codebuild::SUBJECT_VARIABLE, request.subject.as_str()),
            ("BIFFIX_IMAGE_STEP", "index"),
            ("BIFFIX_CONTENT_KEY", request.content_key.as_str()),
            ("BIFFIX_INDEX_MANIFESTS", manifests.as_str()),
        ];
        self.start_codebuild(project, &env, &request.idempotency_token).await
    }

    async fn build_event(
        &self,
        build_arn: &str,
    ) -> Result<Option<biffix_core::wire::aws_codebuild::CodeBuildEvent>, AwsError> {
        let out = self
            .codebuild
            .batch_get_builds()
            .ids(build_arn)
            .send()
            .await
            .or_classify(AwsError::CodeBuild)?;
        Ok(out.builds().first().and_then(codebuild_event))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_polled_build_reads_like_its_state_change_event() {
        use aws_sdk_codebuild::types::{
            Build, BuildPhase, BuildPhaseType, EnvironmentVariable, ExportedEnvironmentVariable,
            LogsLocation, PhaseContext, ProjectEnvironment, StatusType,
        };
        use biffix_core::wire::aws_codebuild::CodeBuildStatus;
        let env_of = |name: &str, value: &str| {
            ProjectEnvironment::builder()
                .r#type("LINUX_CONTAINER".into())
                .image("img")
                .compute_type("BUILD_GENERAL1_MEDIUM".into())
                .environment_variables(
                    EnvironmentVariable::builder().name(name).value(value).build().unwrap(),
                )
                .build()
                .unwrap()
        };
        let env = env_of("BIFFIX_BUILD_SUBJECT", "coordinator:k@sha256:b");
        // CodeBuild reports a phase's context as a status code and a message, apart; the event
        // joins them as `STATUS: message`.
        let phase = |t: BuildPhaseType, s: StatusType, context: Option<(&str, &str)>| {
            let mut p = BuildPhase::builder().phase_type(t).phase_status(s);
            if let Some((code, message)) = context {
                p = p.contexts(PhaseContext::builder().status_code(code).message(message).build());
            }
            p.build()
        };
        let failed = Build::builder()
            .arn("arn:aws:codebuild:us-east-1:123:build/p:1")
            .project_name("biffix-t-runtime-build")
            .build_status(StatusType::Failed)
            .start_time(aws_sdk_codebuild::primitives::DateTime::from_secs(100))
            .end_time(aws_sdk_codebuild::primitives::DateTime::from_secs(160))
            .environment(env.clone())
            .phases(phase(BuildPhaseType::Install, StatusType::Succeeded, None))
            .phases(phase(
                BuildPhaseType::Build,
                StatusType::Failed,
                Some(("COMMAND_EXECUTION_ERROR", "exit 1")),
            ))
            .logs(LogsLocation::builder().deep_link("https://logs").build())
            .build();
        let ev = codebuild_event(&failed).unwrap();
        assert_eq!(ev.status, CodeBuildStatus::Failed);
        assert_eq!(ev.subject, "coordinator:k@sha256:b");
        assert_eq!(ev.reason.as_deref(), Some("BUILD: COMMAND_EXECUTION_ERROR: exit 1"));
        assert_eq!(ev.ts.as_micros(), 160_000_000);
        assert_eq!(ev.logs_url.as_deref(), Some("https://logs"));
        // The state change EventBridge delivers for the same build decodes to the same event.
        let delivered = serde_json::json!({
            "source": "aws.codebuild",
            "detail-type": "CodeBuild Build State Change",
            "time": "1970-01-01T00:02:40Z",
            "detail": {
                "build-status": "FAILED",
                "build-id": "arn:aws:codebuild:us-east-1:123:build/p:1",
                "project-name": "biffix-t-runtime-build",
                "additional-information": {
                    "environment": {"environment-variables": [
                        {"name": "BIFFIX_BUILD_SUBJECT", "value": "coordinator:k@sha256:b"}
                    ]},
                    "logs": {"deep-link": "https://logs"},
                    "phases": [
                        {"phase-type": "INSTALL", "phase-status": "SUCCEEDED"},
                        {"phase-type": "BUILD", "phase-status": "FAILED",
                         "phase-context": ["COMMAND_EXECUTION_ERROR: exit 1"]}
                    ]
                }
            }
        });
        assert_eq!(
            biffix_core::eventbridge::decode::decode_codebuild(&delivered).unwrap().unwrap(),
            biffix_core::translate::from_aws_codebuild(&ev)
        );

        let succeeded = Build::builder()
            .arn("arn:b")
            .build_status(StatusType::Succeeded)
            .start_time(aws_sdk_codebuild::primitives::DateTime::from_secs(100))
            .environment(env)
            .exported_environment_variables(
                ExportedEnvironmentVariable::builder()
                    .name("VARIANT_IMAGE")
                    .value("repo@sha256:v")
                    .build(),
            )
            .build();
        let ev = codebuild_event(&succeeded).unwrap();
        assert_eq!(ev.outputs["VARIANT_IMAGE"], "repo@sha256:v");
        assert_eq!(ev.reason, None);
        // A build with everything but a biffix subject is not a biffix build.
        let foreign = Build::builder()
            .arn("arn:x")
            .build_status(StatusType::Succeeded)
            .start_time(aws_sdk_codebuild::primitives::DateTime::from_secs(100))
            .environment(env_of("OTHER", "x"))
            .build();
        assert!(codebuild_event(&foreign).is_none());
    }
}
