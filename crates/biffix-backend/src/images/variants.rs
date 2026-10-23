//! Coordinator variants: a workflow version's pinned engine runtime, appended to the deployed base
//! coordinator image, with a Batch job definition to launch it by.
//!
//! A version records its runtime recipe (every pin) and key at registration. A **variant** is that
//! runtime built onto one base image: a CodeBuild build whose subject is
//! `coordinator:<runtime_key>@<base_digest>` (`codebuild/runtime/build.sh`), followed by the job
//! definition the backend registers for the image it pushed. Build state is the build projection
//! (the fold of the build's events); the job definition is recorded in `coordinator_variants`.
//!
//! [`ensure`] moves a variant one step forward — start its build, or register the job definition
//! of a built one — and is idempotent, so registration, submit and the sweep all call it: the
//! sweep is what finishes a variant whose build ended after the request that started it, rebuilds
//! variants for a new base image, and retries failed builds (up to [`AUTO_BUILDS`]). A run launches
//! on the variant for the current base, or else on the newest variant of an older base, so a base
//! bump never blocks runs.

use biffix_core::builds::BuildProjection;
use biffix_workflow::runtime::{coordinator_subject, RuntimeRecipe};

use crate::aws::{
    batch::LaunchVariant, builds::RuntimeBuildJob, registry::CoordinatorBase, AwsError,
};
use crate::images::build_follow::{
    builds_seen, failure_reason, phase_of_build, reconcile_stale_build, start_token, BuildKind,
};
use crate::images::build_follow::{BuildError, FollowStep, Phase, AUTO_BUILDS};
use crate::register::workflow::WorkflowVersion;
use crate::store::registry::CoordinatorVariant;
use crate::AppState;

/// A variant's state: its phase, its latest build (if any) and its job definition (once ready).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct VariantStatus {
    pub(crate) phase: Phase,
    pub(crate) build: Option<BuildProjection>,
    pub(crate) variant: Option<CoordinatorVariant>,
}

/// The next step [`ensure`] takes for a variant.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Step {
    Nothing,
    StartBuild,
    /// Register the job definition of the image the succeeded build pushed.
    RegisterJobDefinition {
        image: String,
    },
}

/// The image a succeeded variant build pushed (`VARIANT_IMAGE`), or `None` if it reported none.
fn variant_image(build: &BuildProjection) -> Option<&String> {
    build.outputs.get("VARIANT_IMAGE").filter(|i| !i.is_empty())
}

/// A variant's phase, from its job-definition row and latest build. A build that succeeded but
/// reported no image failed: no job definition can be registered for it.
fn phase_of(variant: Option<&CoordinatorVariant>, build: Option<&BuildProjection>) -> Phase {
    phase_of_build(variant.is_some(), build, |b| variant_image(b).is_some())
}

/// What to do next for a variant. `retry_failed` lets a failed build be started again while the
/// variant has had fewer than [`AUTO_BUILDS`] builds.
fn next_step(
    variant: Option<&CoordinatorVariant>,
    build: Option<&BuildProjection>,
    retry_failed: bool,
) -> Step {
    match crate::images::build_follow::follow_step(
        variant.is_some(),
        build,
        |b| retry_failed && b.builds < AUTO_BUILDS,
        |b| variant_image(b).cloned(),
    ) {
        FollowStep::Nothing => Step::Nothing,
        FollowStep::StartBuild => Step::StartBuild,
        FollowStep::Record(image) => Step::RegisterJobDefinition { image },
    }
}

/// The version's runtime (key + recipe).
pub(crate) fn runtime_of(version: &WorkflowVersion) -> Result<(&str, RuntimeRecipe), BuildError> {
    let recipe = <RuntimeRecipe as serde::Deserialize>::deserialize(&version.runtime_recipe)
        .map_err(|e| BuildError::Recipe(e.to_string()))?;
    Ok((version.runtime_key.as_str(), recipe))
}

/// The version's runtime, if biffix can still run it: one pinning a retired engine release
/// (`biffix_workflow::engines::RETIRED`) is refused by name rather than built or launched on an old
/// variant.
pub(crate) fn runnable_runtime(
    version: &WorkflowVersion,
) -> Result<(&str, RuntimeRecipe), NotRunnable> {
    let (key, recipe) = runtime_of(version)?;
    if let Some(r) = biffix_workflow::engines::retired(recipe.engine(), recipe.engine_version()) {
        return Err(NotRunnable::Retired(r));
    }
    Ok((key, recipe))
}

/// The variant of `runtime_key` on `base_digest` among `variants` (the runtime's), and its build.
async fn load_among(
    state: &AppState,
    variants: &[CoordinatorVariant],
    runtime_key: &str,
    base_digest: &str,
) -> Result<(Option<CoordinatorVariant>, Option<BuildProjection>), BuildError> {
    let variant = variants.iter().find(|v| v.base_digest == base_digest).cloned();
    let build = state.registry.get_build(&coordinator_subject(runtime_key, base_digest)).await?;
    Ok((variant, build))
}

async fn load(
    state: &AppState,
    runtime_key: &str,
    base_digest: &str,
) -> Result<(Option<CoordinatorVariant>, Option<BuildProjection>), BuildError> {
    let variants = state.registry.coordinator_variants(runtime_key).await?;
    load_among(state, &variants, runtime_key, base_digest).await
}

/// Start a build of the variant (`recipe` onto `base`) and record that it was requested.
async fn start_build(
    state: &AppState,
    runtime_key: &str,
    recipe: &RuntimeRecipe,
    base: &CoordinatorBase,
    builds_seen: u32,
) -> Result<(), BuildError> {
    let subject = coordinator_subject(runtime_key, &base.digest);
    let request = RuntimeBuildJob {
        idempotency_token: start_token(&subject, builds_seen),
        subject: subject.clone(),
        base_image: base.image(),
        env: recipe.build_env().into_iter().map(|(k, v)| (k.to_string(), v)).collect(),
    };
    let build_arn = state.cloud.start_runtime_build(&request).await?;
    tracing::info!(subject, build_arn, "variants: runtime build started");
    crate::images::build_follow::record_requested(
        state,
        &subject,
        &build_arn,
        serde_json::json!({ "recipe": recipe, "base_image": base.image() }),
    )
    .await?;
    Ok(())
}

/// The default project's coordinator template: the one a recorded variant's job definition is
/// cloned from (`coordinator_variants` holds one job definition per variant; another project's
/// is found by name at launch, [`launch_variant`]).
async fn default_template(state: &AppState) -> String {
    state.projects.current().await.default_project().coordinator_job_definition.clone()
}

/// `variant`, cloned again from the deployed coordinator template if it was cloned from an older
/// one (a deploy that changed the template, not the base image: queues, role, resources, env,
/// retries). Its image is unchanged, so no build is needed; the registry then records the new job
/// definition. Every variant is registered again once after such a deploy.
async fn on_current_template(
    state: &AppState,
    variant: CoordinatorVariant,
) -> Result<CoordinatorVariant, BuildError> {
    let template = default_template(state).await;
    if state.cloud.variant_is_current(
        &variant.runtime_key,
        &variant.base_digest,
        &variant.job_definition_arn,
        &template,
    ) {
        return Ok(variant);
    }
    let job_definition_arn = state
        .cloud
        .variant_job_definition(
            &variant.runtime_key,
            &variant.base_digest,
            &variant.image,
            &template,
        )
        .await?;
    tracing::info!(
        runtime_key = variant.runtime_key,
        base = variant.base_digest,
        stale = variant.job_definition_arn,
        job_definition = job_definition_arn,
        "variants: coordinator template changed; variant registered again"
    );
    let variant = CoordinatorVariant { job_definition_arn, ..variant };
    state.registry.put_coordinator_variant(&variant).await?;
    Ok(variant)
}

/// Move the variant of `runtime_key` on `base` one step forward (see [`next_step`]) and return
/// where it is now.
pub(crate) async fn ensure(
    state: &AppState,
    runtime_key: &str,
    recipe: &RuntimeRecipe,
    base: &CoordinatorBase,
    retry_failed: bool,
) -> Result<VariantStatus, BuildError> {
    let (variant, mut build) = load(state, runtime_key, &base.digest).await?;
    let variant = match variant {
        Some(v) => Some(on_current_template(state, v).await?),
        None => None,
    };
    if retry_failed && variant.is_none() {
        build = reconcile_stale_build(
            state,
            build,
            &coordinator_subject(runtime_key, &base.digest),
            BuildKind::Runtime,
        )
        .await?;
    }
    match next_step(variant.as_ref(), build.as_ref(), retry_failed) {
        Step::Nothing => {}
        Step::StartBuild => {
            start_build(state, runtime_key, recipe, base, builds_seen(build.as_ref())).await?;
            let build =
                state.registry.get_build(&coordinator_subject(runtime_key, &base.digest)).await?;
            return Ok(VariantStatus { phase: Phase::Building, build, variant: None });
        }
        Step::RegisterJobDefinition { image } => {
            let job_definition_arn = state
                .cloud
                .variant_job_definition(
                    runtime_key,
                    &base.digest,
                    &image,
                    &default_template(state).await,
                )
                .await?;
            let variant = CoordinatorVariant {
                runtime_key: runtime_key.to_string(),
                base_digest: base.digest.clone(),
                image,
                job_definition_arn,
                created_at: biffix_core::clock::epoch_micros(),
            };
            state.registry.put_coordinator_variant(&variant).await?;
            tracing::info!(
                runtime_key,
                base = base.digest,
                job_definition = variant.job_definition_arn,
                "variants: coordinator variant ready"
            );
            return Ok(VariantStatus { phase: Phase::Ready, build, variant: Some(variant) });
        }
    }
    Ok(VariantStatus { phase: phase_of(variant.as_ref(), build.as_ref()), build, variant })
}

/// Why a version cannot be launched now.
#[derive(Debug, thiserror::Error)]
pub(crate) enum NotRunnable {
    /// The version pins an engine release biffix no longer builds; re-registering on its
    /// replacement is the way forward.
    #[error("{0}")]
    Retired(biffix_workflow::engines::Retired),
    #[error("the version's engine runtime is still being built{}; try again in a few minutes", logs(.0))]
    Building(Option<String>),
    #[error("the version's engine runtime failed to build: {reason}{}", logs(.logs_url))]
    Failed { reason: String, logs_url: Option<String> },
    #[error(transparent)]
    Error(#[from] BuildError),
}

fn logs(url: &Option<String>) -> String {
    url.as_ref().map(|u| format!(" (build log: {u})")).unwrap_or_default()
}

/// The variant to launch `version` on in `project`: the one for the deployed base image (moving it
/// forward if it is not ready), else the newest variant built on an older base. Its job definition
/// is cloned from `project`'s coordinator template (whose job role is the project's coordinator
/// role): the recorded one for the default project, else the project's own, found by name and
/// registered when missing.
pub(crate) async fn launch_variant(
    state: &AppState,
    version: &WorkflowVersion,
    project: &crate::config::projects::ProjectContext,
) -> Result<LaunchVariant, NotRunnable> {
    let (runtime_key, recipe) = runnable_runtime(version)?;
    let base = state.cloud.coordinator_base().await.map_err(BuildError::from)?;
    let status = ensure(state, runtime_key, &recipe, &base, false).await?;
    let variant = match status.variant {
        Some(v) => Some(v),
        // The current base's variant is not ready (a base bump): run on the previous one, on the
        // deployed template.
        None => match state
            .registry
            .coordinator_variants(runtime_key)
            .await
            .map_err(BuildError::from)?
            .into_iter()
            .next()
        {
            Some(v) => Some(on_current_template(state, v).await?),
            None => None,
        },
    };
    match variant {
        Some(v) => Ok(LaunchVariant {
            job_definition: project_job_definition(state, &v, project).await?,
            image: v.image,
            engine_version: recipe.engine_version().to_string(),
            runtime_key: runtime_key.to_string(),
        }),
        None => {
            let build = status.build;
            let logs_url = build.as_ref().and_then(|b| b.logs_url.clone());
            match (status.phase, build) {
                (Phase::Failed, Some(b)) => Err(NotRunnable::Failed {
                    reason: failure_reason(&b).unwrap_or_else(|| "unknown reason".into()),
                    logs_url,
                }),
                _ => Err(NotRunnable::Building(logs_url)),
            }
        }
    }
}

/// The job definition of `variant` for `project`: the recorded one when it is the clone `project`'s
/// own template names (the default project's, usually), else the project's own clone, found by
/// name and registered when missing. Decided by the recorded job definition's name, never by
/// comparing templates with a separately read default: the projects may be read again between the
/// two, and a clone of another project's template runs as that project's coordinator role.
async fn project_job_definition(
    state: &AppState,
    variant: &CoordinatorVariant,
    project: &crate::config::projects::ProjectContext,
) -> Result<String, BuildError> {
    if state.cloud.variant_is_current(
        &variant.runtime_key,
        &variant.base_digest,
        &variant.job_definition_arn,
        &project.coordinator_job_definition,
    ) {
        return Ok(variant.job_definition_arn.clone());
    }
    Ok(state
        .cloud
        .variant_job_definition(
            &variant.runtime_key,
            &variant.base_digest,
            &variant.image,
            &project.coordinator_job_definition,
        )
        .await?)
}

/// A version's runtime as shown to people: the current base's variant, and whether the version can
/// run (on it, or on a variant of an older base). Read-only.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RuntimeReport {
    pub(crate) runtime_key: String,
    pub(crate) recipe: serde_json::Value,
    pub(crate) status: VariantStatus,
    /// The variant a run would launch on now, if any.
    pub(crate) launch: Option<CoordinatorVariant>,
}

pub(crate) async fn report(
    state: &AppState,
    version: &WorkflowVersion,
) -> Result<RuntimeReport, BuildError> {
    let (runtime_key, _recipe) = runtime_of(version)?;
    let variants = state.registry.coordinator_variants(runtime_key).await?;
    let status = match state.cloud.coordinator_base().await {
        Ok(base) => {
            let (variant, build) = load_among(state, &variants, runtime_key, &base.digest).await?;
            VariantStatus { phase: phase_of(variant.as_ref(), build.as_ref()), build, variant }
        }
        // No base to compare with (a deploy without variant builds): report what exists.
        Err(AwsError::Unsupported(_)) => VariantStatus {
            phase: phase_of(variants.first(), None),
            build: None,
            variant: variants.first().cloned(),
        },
        // The base cannot be read now (an ECR outage): not "waiting to be built", but unknown.
        Err(e) => return Err(e.into()),
    };
    let launch = status.variant.clone().or_else(|| variants.first().cloned());
    Ok(RuntimeReport {
        runtime_key: runtime_key.to_string(),
        recipe: version.runtime_recipe.clone(),
        status,
        launch,
    })
}

/// Start another build of the version's variant for the deployed base, whatever its earlier builds
/// did (a failure, or a success that reported no image), unless one is in progress or the variant
/// is ready.
pub(crate) async fn rebuild(
    state: &AppState,
    version: &WorkflowVersion,
) -> Result<VariantStatus, NotRunnable> {
    let (runtime_key, recipe) = runnable_runtime(version)?;
    let base = state.cloud.coordinator_base().await.map_err(BuildError::from)?;
    let (variant, build) = load(state, runtime_key, &base.digest).await?;
    if phase_of(variant.as_ref(), build.as_ref()) == Phase::Failed {
        let seen = builds_seen(build.as_ref());
        start_build(state, runtime_key, &recipe, &base, seen).await?;
    }
    Ok(ensure(state, runtime_key, &recipe, &base, false).await?)
}

/// What one sweep of the runtime variants did.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct VariantSweepReport {
    pub runtimes: usize,
    pub ready: usize,
    pub building: usize,
    pub failed: usize,
    pub errors: usize,
}

/// Move every registered runtime's variant for the deployed base forward: start the builds a new
/// base needs, register the job definitions of finished builds, and retry failed builds.
pub async fn sweep(state: &AppState) -> Result<VariantSweepReport, BuildError> {
    let base = match state.cloud.coordinator_base().await {
        Ok(base) => base,
        Err(AwsError::Unsupported(why)) => {
            tracing::info!(why, "variants: no base coordinator image; nothing to sweep");
            return Ok(VariantSweepReport::default());
        }
        Err(e) => return Err(e.into()),
    };
    let mut runtimes = std::collections::BTreeMap::new();
    // Versions whose runtime cannot be read: their runtime is never built, which the report says.
    let mut unreadable = 0;
    for rec in state.registry.list_versions().await? {
        // A retired release's runtime is never built again (its versions are refused at launch).
        match runnable_runtime(&rec.version) {
            Ok((key, recipe)) => {
                runtimes.entry(key.to_string()).or_insert(recipe);
            }
            Err(NotRunnable::Retired(_)) => {}
            Err(e) => {
                tracing::warn!(version = rec.version.id, error = %e, "variants: skipped");
                unreadable += 1;
            }
        }
    }
    let mut report = VariantSweepReport {
        runtimes: runtimes.len(),
        errors: unreadable,
        ..VariantSweepReport::default()
    };
    for (key, recipe) in &runtimes {
        match ensure(state, key, recipe, &base, true).await {
            Ok(status) => match status.phase {
                Phase::Ready => report.ready += 1,
                Phase::Failed => report.failed += 1,
                Phase::Building | Phase::Unbuilt => report.building += 1,
            },
            Err(e) => {
                report.errors += 1;
                tracing::warn!(runtime_key = key, error = %e, "variants: sweep step failed");
            }
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use biffix_core::builds::BuildState;
    use biffix_core::time::Timestamp;
    use std::collections::BTreeMap;

    fn build(state: BuildState, builds: u32, image: Option<&str>) -> BuildProjection {
        BuildProjection {
            subject: "coordinator:k@sha256:b".into(),
            state,
            build_arn: format!("arn:build/{builds}"),
            outputs: image
                .map(|i| BTreeMap::from([("VARIANT_IMAGE".to_string(), i.to_string())]))
                .unwrap_or_default(),
            reason: None,
            logs_url: None,
            request: serde_json::Value::Null,
            builds,
            updated: Timestamp::from_micros(0),
        }
    }

    fn variant() -> CoordinatorVariant {
        CoordinatorVariant {
            runtime_key: "k".into(),
            base_digest: "sha256:b".into(),
            image: "repo@sha256:v".into(),
            job_definition_arn: "arn:jd".into(),
            created_at: 0,
        }
    }

    #[test]
    fn a_registered_variant_is_ready_and_left_alone() {
        let v = variant();
        let b = build(BuildState::Succeeded, 1, Some("repo@sha256:v"));
        assert_eq!(phase_of(Some(&v), Some(&b)), Phase::Ready);
        assert_eq!(next_step(Some(&v), Some(&b), true), Step::Nothing);
    }

    #[test]
    fn an_unbuilt_variant_is_built() {
        assert_eq!(phase_of(None, None), Phase::Unbuilt);
        assert_eq!(next_step(None, None, false), Step::StartBuild);
    }

    #[test]
    fn a_build_in_progress_is_waited_for() {
        for state in [BuildState::Requested, BuildState::Running] {
            let b = build(state, 1, None);
            assert_eq!(phase_of(None, Some(&b)), Phase::Building);
            assert_eq!(next_step(None, Some(&b), true), Step::Nothing);
        }
    }

    #[test]
    fn a_succeeded_build_gets_its_job_definition() {
        let b = build(BuildState::Succeeded, 1, Some("repo@sha256:v"));
        assert_eq!(phase_of(None, Some(&b)), Phase::Building);
        assert_eq!(
            next_step(None, Some(&b), false),
            Step::RegisterJobDefinition { image: "repo@sha256:v".into() }
        );
    }

    #[test]
    fn a_failed_build_is_retried_only_by_the_sweep_and_only_up_to_the_limit() {
        let b = build(BuildState::Failed, 1, None);
        assert_eq!(phase_of(None, Some(&b)), Phase::Failed);
        assert_eq!(next_step(None, Some(&b), false), Step::Nothing);
        assert_eq!(next_step(None, Some(&b), true), Step::StartBuild);
        let exhausted = build(BuildState::Failed, AUTO_BUILDS, None);
        assert_eq!(next_step(None, Some(&exhausted), true), Step::Nothing);
    }

    #[test]
    fn a_succeeded_build_without_an_image_is_rebuilt() {
        let b = build(BuildState::Succeeded, 1, None);
        assert_eq!(next_step(None, Some(&b), true), Step::StartBuild);
        assert_eq!(next_step(None, Some(&b), false), Step::Nothing);
        let empty = build(BuildState::Succeeded, 1, Some(""));
        assert_eq!(next_step(None, Some(&empty), true), Step::StartBuild);
    }

    #[test]
    fn a_succeeded_build_without_an_image_failed_even_once_its_retries_are_used() {
        for builds in [1, AUTO_BUILDS] {
            let b = build(BuildState::Succeeded, builds, None);
            assert_eq!(phase_of(None, Some(&b)), Phase::Failed, "{builds}");
            assert_eq!(failure_reason(&b).as_deref(), Some(crate::images::build_follow::NO_OUTPUT));
        }
        // The sweep gives up on it; only a rebuild (which starts on `Failed`) starts another.
        let exhausted = build(BuildState::Succeeded, AUTO_BUILDS, None);
        assert_eq!(next_step(None, Some(&exhausted), true), Step::Nothing);
    }
}
