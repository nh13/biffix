//! Derived task images: a Snakemake version's tasks run `python -m snakemake` inside their image,
//! so every image a Snakemake version declares (unless `runtime: own`) is **derived**: the imported
//! image plus the version's engine runtime layer, with a `python` shim that sends exactly that
//! command to the pinned runtime (`codebuild/image/import.sh` derive). Tasks then run the pinned
//! Snakemake whatever the image carries, and the image's own tools keep their own Python.
//!
//! A derive is keyed by the imported digest and the runtime key
//! (`biffix_workflow::images::derived_key`), followed as the build subject `image:derived:<key>`,
//! and its result recorded in `image_artifacts` under that key (it has an import's shape). It needs
//! the runtime's stored layer images, which the runtime build reports (`RUNTIME_IMAGE_*`), so it
//! waits for the runtime as well as for the import. [`ensure`] moves a derive one step forward,
//! like [`crate::images::ensure`] does for an import.

use biffix_core::builds::{BuildProjection, BuildState};
use biffix_workflow::images::{derived_key, derived_subject};
use biffix_workflow::runtime::coordinator_subject;

use crate::aws::builds::ImageDeriveJob;
use crate::images::build_follow::{
    builds_seen, reconcile_stale_build, start_token, BuildError, BuildKind, Phase,
};
use crate::images::{next_step as import_step, Step as ImportStep};
use crate::store::registry::ImageArtifact;
use crate::AppState;

/// A runtime's stored layer images (`codebuild/runtime/build.sh` `RUNTIME_IMAGE_*`):
/// `<coordinator repo>@sha256:…` per architecture.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RuntimeLayer {
    pub(crate) amd64: String,
    pub(crate) arm64: String,
}

impl RuntimeLayer {
    /// The layer images a succeeded runtime build reported, or `None` (one still running, or one
    /// that reported none).
    fn from_build(build: &BuildProjection) -> Option<Self> {
        if build.state != BuildState::Succeeded {
            return None;
        }
        let out = |k: &str| crate::images::build_follow::output(build, k).map(str::to_string);
        Some(Self { amd64: out("RUNTIME_IMAGE_AMD64")?, arm64: out("RUNTIME_IMAGE_ARM64")? })
    }
}

/// The stored layer images of `runtime_key`: from the build of any of its coordinator variants
/// (every one stores or reuses the same layer images).
pub(crate) async fn runtime_layer(
    state: &AppState,
    runtime_key: &str,
) -> Result<Option<RuntimeLayer>, BuildError> {
    for variant in state.registry.coordinator_variants(runtime_key).await? {
        let subject = coordinator_subject(runtime_key, &variant.base_digest);
        if let Some(layer) =
            state.registry.get_build(&subject).await?.as_ref().and_then(RuntimeLayer::from_build)
        {
            return Ok(Some(layer));
        }
    }
    Ok(None)
}

/// What a derive needs from its version: the runtime whose layer it appends.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DeriveRuntime {
    pub(crate) runtime_key: String,
    /// The Snakemake release the runtime installs.
    pub(crate) engine_version: String,
    /// Its stored layer images, once built.
    pub(crate) layer: Option<RuntimeLayer>,
}

/// Where one image's derive is.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct DerivedStatus {
    pub(crate) key: String,
    /// The Snakemake release being added.
    pub(crate) engine_version: String,
    pub(crate) phase: Phase,
    /// No derive can start until the runtime's layer is built.
    pub(crate) waiting_for_runtime: bool,
    pub(crate) build: Option<BuildProjection>,
    pub(crate) artifact: Option<ImageArtifact>,
}

/// The next step [`ensure`] takes for a derive.
#[derive(Clone, Debug, PartialEq)]
enum Step {
    Nothing,
    /// The runtime's layer images do not exist yet.
    WaitRuntime,
    StartBuild,
    Record(Box<ImageArtifact>),
}

/// What to do next for the derive `key`: an import's steps (its build is followed the same way),
/// except that a build cannot start before the runtime's layer exists.
fn next_step(
    key: &str,
    layer: Option<&RuntimeLayer>,
    artifact: Option<&ImageArtifact>,
    build: Option<&BuildProjection>,
    retry_failed: bool,
) -> Step {
    match import_step(key, artifact, build, retry_failed) {
        ImportStep::Nothing => Step::Nothing,
        ImportStep::StartBuild if layer.is_none() => Step::WaitRuntime,
        ImportStep::StartBuild => Step::StartBuild,
        ImportStep::Record(artifact) => Step::Record(artifact),
    }
}

fn status_of(
    key: String,
    runtime: &DeriveRuntime,
    artifact: Option<ImageArtifact>,
    build: Option<BuildProjection>,
) -> DerivedStatus {
    let phase = crate::images::phase_of(artifact.as_ref(), build.as_ref());
    DerivedStatus {
        key,
        engine_version: runtime.engine_version.clone(),
        waiting_for_runtime: phase == Phase::Unbuilt && runtime.layer.is_none(),
        phase,
        build,
        artifact,
    }
}

async fn load(
    state: &AppState,
    key: &str,
) -> Result<(Option<ImageArtifact>, Option<BuildProjection>), BuildError> {
    let artifact = state.registry.get_image_artifact(key).await?;
    let build = state.registry.get_build(&derived_subject(key)).await?;
    Ok((artifact, build))
}

async fn start_build(
    state: &AppState,
    key: &str,
    imported: &ImageArtifact,
    runtime: &DeriveRuntime,
    layer: &RuntimeLayer,
    builds_seen: u32,
) -> Result<(), BuildError> {
    let subject = derived_subject(key);
    let build_arn = state
        .cloud
        .start_image_derive(&ImageDeriveJob {
            idempotency_token: start_token(&subject, builds_seen),
            subject: subject.clone(),
            base: imported.image.clone(),
            layer_amd64: layer.amd64.clone(),
            layer_arm64: layer.arm64.clone(),
            derived_key: key.to_string(),
            snakemake_version: runtime.engine_version.clone(),
        })
        .await?;
    tracing::info!(subject, build_arn, base = imported.image, "derived: derive started");
    crate::images::build_follow::record_requested(
        state,
        &subject,
        &build_arn,
        serde_json::json!({
            "source": imported.image,
            "derived_key": key,
            "runtime_key": runtime.runtime_key,
        }),
    )
    .await?;
    Ok(())
}

/// Where the derive of `imported` with `runtime` is (read-only).
pub(crate) async fn status(
    state: &AppState,
    imported: &ImageArtifact,
    runtime: &DeriveRuntime,
) -> Result<DerivedStatus, BuildError> {
    let key = derived_key(&imported.digest, &runtime.runtime_key);
    let (artifact, build) = load(state, &key).await?;
    Ok(status_of(key, runtime, artifact, build))
}

/// Move the derive of `imported` with `runtime` one step forward (see [`next_step`]).
pub(crate) async fn ensure(
    state: &AppState,
    imported: &ImageArtifact,
    runtime: &DeriveRuntime,
    retry_failed: bool,
) -> Result<DerivedStatus, BuildError> {
    let key = derived_key(&imported.digest, &runtime.runtime_key);
    let (artifact, mut build) = load(state, &key).await?;
    if retry_failed && artifact.is_none() {
        build =
            reconcile_stale_build(state, build, &derived_subject(&key), BuildKind::Import).await?;
    }
    match next_step(&key, runtime.layer.as_ref(), artifact.as_ref(), build.as_ref(), retry_failed) {
        Step::Nothing | Step::WaitRuntime => Ok(status_of(key, runtime, artifact, build)),
        Step::StartBuild => {
            let layer = runtime.layer.as_ref().expect("a derive starts only with a layer");
            let seen = builds_seen(build.as_ref());
            start_build(state, &key, imported, runtime, layer, seen).await?;
            let build = state.registry.get_build(&derived_subject(&key)).await?;
            Ok(DerivedStatus { phase: Phase::Building, ..status_of(key, runtime, None, build) })
        }
        Step::Record(artifact) => {
            state.registry.put_image_artifact(&artifact).await?;
            tracing::info!(image = artifact.image, "derived: derive recorded");
            Ok(status_of(key, runtime, Some(*artifact), build))
        }
    }
}

/// Derive `imported` again whatever its earlier builds did (a failure, or a success that reported
/// no image), unless one is in progress, it is derived, or the runtime's layer is not built.
pub(crate) async fn rebuild(
    state: &AppState,
    imported: &ImageArtifact,
    runtime: &DeriveRuntime,
) -> Result<DerivedStatus, BuildError> {
    let key = derived_key(&imported.digest, &runtime.runtime_key);
    let (artifact, build) = load(state, &key).await?;
    if let (Phase::Failed, Some(layer)) =
        (crate::images::phase_of(artifact.as_ref(), build.as_ref()), runtime.layer.as_ref())
    {
        let seen = builds_seen(build.as_ref());
        start_build(state, &key, imported, runtime, layer, seen).await?;
    }
    ensure(state, imported, runtime, false).await
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use biffix_core::time::Timestamp;

    use super::*;

    fn build(state: BuildState, outputs: &[(&str, &str)]) -> BuildProjection {
        BuildProjection {
            subject: "s".into(),
            state,
            build_arn: "arn:b".into(),
            outputs: outputs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect::<BTreeMap<_, _>>(),
            reason: None,
            logs_url: None,
            request: serde_json::json!({"source": "repo@sha256:i"}),
            builds: 1,
            updated: Timestamp::from_micros(0),
        }
    }

    const LAYERS: &[(&str, &str)] =
        &[("RUNTIME_IMAGE_AMD64", "coord@sha256:a"), ("RUNTIME_IMAGE_ARM64", "coord@sha256:b")];

    #[test]
    fn a_runtime_build_reports_its_layer_images_once_it_succeeded() {
        let layer = RuntimeLayer::from_build(&build(BuildState::Succeeded, LAYERS)).unwrap();
        assert_eq!(
            (layer.amd64.as_str(), layer.arm64.as_str()),
            ("coord@sha256:a", "coord@sha256:b")
        );
        assert_eq!(RuntimeLayer::from_build(&build(BuildState::Running, LAYERS)), None);
        // A variant built before layers were stored reports none.
        let old = build(BuildState::Succeeded, &[("VARIANT_IMAGE", "coord@sha256:v")]);
        assert_eq!(RuntimeLayer::from_build(&old), None);
    }

    #[test]
    fn a_derive_waits_for_the_runtime_layer_then_follows_its_build() {
        let layer = RuntimeLayer { amd64: "a".into(), arm64: "b".into() };
        assert_eq!(next_step("k", None, None, None, true), Step::WaitRuntime);
        assert_eq!(next_step("k", Some(&layer), None, None, true), Step::StartBuild);
        let running = build(BuildState::Running, &[]);
        assert_eq!(next_step("k", Some(&layer), None, Some(&running), true), Step::Nothing);
        let done = build(
            BuildState::Succeeded,
            &[
                ("IMAGE", "repo@sha256:d"),
                ("IMAGE_DIGEST", "sha256:d"),
                ("CAPABILITIES", "bash=1,libc=glibc,snakemake=9.27.0"),
            ],
        );
        let Step::Record(artifact) = next_step("k", Some(&layer), None, Some(&done), true) else {
            panic!("record");
        };
        assert_eq!(artifact.content_key, "k");
        assert_eq!(artifact.digest, "sha256:d");
        // The imported image it was derived from.
        assert_eq!(artifact.source_ref, "repo@sha256:i");
        let failed = build(BuildState::Failed, &[]);
        assert_eq!(next_step("k", Some(&layer), None, Some(&failed), true), Step::StartBuild);
    }

    #[test]
    fn a_derive_that_succeeded_without_an_image_failed_even_once_its_retries_are_used() {
        let runtime = DeriveRuntime {
            runtime_key: "rt".into(),
            engine_version: "9.27.0".into(),
            layer: Some(RuntimeLayer { amd64: "a".into(), arm64: "b".into() }),
        };
        for builds in [1, crate::images::build_follow::AUTO_BUILDS] {
            let empty = BuildProjection {
                builds,
                ..build(BuildState::Succeeded, &[("IMAGE", "repo@sha256:d")])
            };
            let status = status_of("k".into(), &runtime, None, Some(empty.clone()));
            assert_eq!(status.phase, Phase::Failed, "{builds}");
            assert!(!status.waiting_for_runtime);
            let sweep = next_step("k", runtime.layer.as_ref(), None, Some(&empty), true);
            let want = if builds < crate::images::build_follow::AUTO_BUILDS {
                Step::StartBuild
            } else {
                Step::Nothing
            };
            assert_eq!(sweep, want, "{builds}");
        }
    }
}
