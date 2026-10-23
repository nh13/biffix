//! Workflow images: every image a version declares is imported into the tenant's workflow
//! repository by digest — copied, keep-tagged, probed and archived by an import build
//! (`codebuild/image/import.sh`) — and a version runs only once its images are imported.
//!
//! An import is tenant-wide, keyed by the source digest (`biffix_workflow::images::content_key`),
//! and followed as the build subject `image:<content_key>`: two versions declaring the same bytes
//! share one import. The build's state is the build projection; its result is recorded in
//! `image_artifacts`. [`ensure`] moves an import one step forward (start the build, or record the
//! result of a finished one), exactly like [`crate::images::variants::ensure`] does for runtimes,
//! and the sweep calls it for every declared image (retries included).
//!
//! A Snakemake version's images (unless `runtime: own`) are then derived with the version's runtime
//! ([`crate::images::derived`]): [`ensure`] chains the derive once the import is done, and an image
//! is ready only once both are.
//!
//! An image built from a recipe the workflow ships ([`crate::images::build`]) takes the import's
//! place: once its builds are done its result is recorded like an import's, and from there on it
//! is checked and derived the same way.
//!
//! The coordinator's own image is here too: a version's coordinator variant ([`variants`]). Every
//! kind of build is followed to its result by [`build_follow`], and a run's image map
//! (`images.json`, [`map`]) points each declared image at the tenant image its tasks run.

pub(crate) mod build;
pub mod build_follow;
pub(crate) mod derived;
pub mod map;
pub mod variants;
pub(crate) mod view;

use biffix_core::builds::BuildProjection;
use biffix_workflow::images::{subject, Capabilities};

use crate::aws::builds::ImageImportJob;
use crate::images::build::ArchBuild;
use crate::images::build_follow::{
    builds_seen, failure_reason, phase_of_build, reconcile_stale_build, start_token, BuildError,
    BuildKind, Phase, AUTO_BUILDS,
};
use crate::images::derived::{DeriveRuntime, DerivedStatus};
use crate::register::workflow::WorkflowVersion;
use crate::store::registry::{ImageArtifact, RegisteredVersion, VersionImage};
use crate::AppState;
use biffix_workflow::engines::Engine;

/// Where one declared image is.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ImageStatus {
    pub(crate) image: VersionImage,
    pub(crate) phase: Phase,
    /// The import's build, or a built image's index build.
    pub(crate) build: Option<BuildProjection>,
    /// A built image's per-architecture builds (empty for an import).
    pub(crate) arch_builds: Vec<ArchBuild>,
    pub(crate) artifact: Option<ImageArtifact>,
    /// The derive that adds the version's Snakemake runtime (a Snakemake image not `runtime: own`),
    /// once the import is done.
    pub(crate) derived: Option<DerivedStatus>,
    /// Why an imported image cannot serve this version (e.g. no bash), which makes it failed.
    pub(crate) problem: Option<String>,
}

impl ImageStatus {
    /// The image tasks run on: the derived image for a derived image, else the imported one.
    pub(crate) fn runnable(&self) -> Option<&ImageArtifact> {
        match &self.derived {
            Some(d) => d.artifact.as_ref(),
            None => self.artifact.as_ref(),
        }
    }

    /// Why the image cannot be run now, as people read it, or `None` if it can.
    pub(crate) fn unready_reason(&self) -> Option<String> {
        let name = &self.image.name;
        if self.phase == Phase::Ready {
            return None;
        }
        if let Some(problem) = &self.problem {
            return Some(problem.clone());
        }
        if self.artifact.is_none() {
            let built = self.image.build.is_some();
            return Some(match (self.phase, built) {
                (Phase::Failed, false) => {
                    format!("image `{name}` failed to import: {}", failure(self.build.as_ref()))
                }
                (Phase::Failed, true) => {
                    match crate::images::build::failed_build(&self.arch_builds, self.build.as_ref())
                    {
                        Some((what, build)) => format!(
                            "image `{name}` failed to build ({what}): {}",
                            failure(Some(build))
                        ),
                        None => format!("image `{name}` failed to build"),
                    }
                }
                (Phase::Unbuilt, false) => format!("image `{name}` is waiting to be imported"),
                (Phase::Unbuilt, true) => format!("image `{name}` is waiting to be built"),
                (_, false) => format!("image `{name}` is being imported"),
                (_, true) => format!("image `{name}` is being built"),
            });
        }
        let derived = self.derived.as_ref()?;
        let v = &derived.engine_version;
        Some(match derived.phase {
            Phase::Failed => format!(
                "image `{name}`: adding the Snakemake {v} runtime failed: {}",
                failure(derived.build.as_ref())
            ),
            _ if derived.waiting_for_runtime => {
                format!("image `{name}` is waiting for the Snakemake {v} runtime to be built")
            }
            _ => format!("image `{name}`: adding the Snakemake {v} runtime"),
        })
    }
}

/// The next step [`ensure`] takes for an import: record its artifact once built.
pub(crate) type Step = build_follow::FollowStep<Box<ImageArtifact>>;

/// The image (`IMAGE`) and digest (`IMAGE_DIGEST`) an import-shaped build (an import, a derive,
/// a built image's index) reported, or `None` if it did not report both.
fn reported_image(build: &BuildProjection) -> Option<(String, String)> {
    let out = |k: &str| build_follow::output(build, k).map(str::to_string);
    Some((out("IMAGE")?, out("IMAGE_DIGEST")?))
}

/// Whether a succeeded import-shaped build reported an image [`artifact_from`] can record.
pub(crate) fn reports_image(build: &BuildProjection) -> bool {
    reported_image(build).is_some()
}

/// An import's (or a derive's) phase, from its recorded result and latest build. A build that
/// succeeded but reported no image failed: there is nothing to record.
pub(crate) fn phase_of(artifact: Option<&ImageArtifact>, build: Option<&BuildProjection>) -> Phase {
    phase_of_build(artifact.is_some(), build, reports_image)
}

/// The imported image a succeeded import build reported, or `None` if it reported none.
pub(crate) fn artifact_from(content_key: &str, build: &BuildProjection) -> Option<ImageArtifact> {
    let out = |k: &str| build_follow::output(build, k).map(str::to_string);
    let (image, digest) = reported_image(build)?;
    Some(ImageArtifact {
        content_key: content_key.to_string(),
        source_ref: build
            .request
            .get("source")
            .and_then(|s| s.as_str())
            .unwrap_or_default()
            .to_string(),
        image,
        digest,
        archs: out("ARCHS").map(|a| a.split(',').map(str::to_string).collect()).unwrap_or_default(),
        capabilities: Capabilities::parse(&out("CAPABILITIES").unwrap_or_default()),
        keep_tag: out("KEEP_TAG"),
        archived: out("ARCHIVED").as_deref() == Some("1"),
        archive_bytes: out("ARCHIVE_BYTES").and_then(|b| b.parse().ok()),
        created_at: biffix_core::clock::epoch_micros(),
    })
}

/// What to do next for an import. `retry_failed` lets a failed build start again while the import
/// has had fewer than [`AUTO_BUILDS`] builds.
pub(crate) fn next_step(
    content_key: &str,
    artifact: Option<&ImageArtifact>,
    build: Option<&BuildProjection>,
    retry_failed: bool,
) -> Step {
    build_follow::follow_step(
        artifact.is_some(),
        build,
        |b| retry_failed && b.builds < AUTO_BUILDS,
        |b| artifact_from(content_key, b).map(Box::new),
    )
}

/// Why `caps` (an imported image's probe) cannot serve as `image` in a version run by `engine` at
/// `engine_version` (whose runtime installs `storage_s3`), or `None` if it can.
///
/// A Snakemake task runs `python -m snakemake` inside its image. An image biffix derives gets the
/// pinned runtime (a glibc build of CPython), so it needs glibc; a `runtime: own` image runs its
/// own Snakemake, which must be exactly the pinned release, with exactly the storage plugin the
/// runtime pins (what a task loads; the logger and executor plugins run only in the coordinator).
fn capability_problem(
    engine: Engine,
    engine_version: &str,
    storage_s3: Option<&str>,
    image: &VersionImage,
    caps: &Capabilities,
) -> Option<String> {
    let name = &image.name;
    if !caps.bash {
        return Some(format!("image `{name}` has no bash, which the engine's task scripts need"));
    }
    if engine != Engine::Snakemake {
        return None;
    }
    if !image.own_runtime {
        return (caps.libc != "glibc").then(|| {
            format!(
                "image `{name}` has no glibc (it is {}), which biffix's Snakemake runtime needs; \
                 an image carrying its own Snakemake {engine_version} can declare `runtime: own`",
                match caps.libc.as_str() {
                    "none" => "static or distroless",
                    other => other,
                }
            )
        });
    }
    let mismatch = |what: &str, have: Option<&String>, want: &str| {
        (have.map(String::as_str) != Some(want)).then(|| {
            format!(
                "image `{name}` (`runtime: own`) carries {what} {}, but this version runs \
                 {what} {want}",
                have.map_or("none".to_string(), std::clone::Clone::clone)
            )
        })
    };
    mismatch("Snakemake", caps.snakemake.as_ref(), engine_version).or_else(|| {
        storage_s3.and_then(|want| {
            mismatch("snakemake-storage-plugin-s3", caps.storage_s3.as_ref(), want)
        })
    })
}

/// What a version's images are checked and derived against. Read it once per version
/// ([`VersionContext::of`]) and pass it to [`ensure`] for each image.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct VersionContext {
    engine: Engine,
    engine_version: String,
    storage_s3: Option<String>,
    /// The runtime a derive adds (a Snakemake version).
    derive: Option<DeriveRuntime>,
}

impl VersionContext {
    pub(crate) async fn of(
        state: &AppState,
        version: &WorkflowVersion,
    ) -> Result<Self, BuildError> {
        let (key, recipe) = crate::images::variants::runtime_of(version)?;
        let derive = match version.engine {
            Engine::Snakemake => Some(DeriveRuntime {
                runtime_key: key.to_string(),
                engine_version: recipe.engine_version().to_string(),
                layer: crate::images::derived::runtime_layer(state, key).await?,
            }),
            Engine::Nextflow => None,
        };
        Ok(Self {
            engine: version.engine,
            engine_version: version.engine_version.clone(),
            storage_s3: recipe.storage_s3().map(str::to_string),
            derive,
        })
    }

    /// The runtime `image` is derived with, if it is derived.
    fn derive_for(&self, image: &VersionImage) -> Option<&DeriveRuntime> {
        self.derive.as_ref().filter(|_| !image.own_runtime)
    }
}

/// An image's recorded result and builds: the import's build, or a built image's index and
/// per-architecture builds.
async fn load(
    state: &AppState,
    image: &VersionImage,
) -> Result<(Option<ImageArtifact>, Option<BuildProjection>, Vec<ArchBuild>), BuildError> {
    let artifact = usable_artifact(state, image).await?;
    match &image.build {
        None => {
            let build = state.registry.get_build(&subject(&image.content_key)).await?;
            Ok((artifact, build, Vec::new()))
        }
        Some(recipe) => {
            let (arch_builds, index) = crate::images::build::load(state, image, recipe).await?;
            Ok((artifact, index, arch_builds))
        }
    }
}

/// The recorded result an image runs from: an import's, or a built image's only from the builds
/// repository ([`crate::images::build::recorded`]).
pub(crate) async fn usable_artifact(
    state: &AppState,
    image: &VersionImage,
) -> Result<Option<ImageArtifact>, crate::store::StoreError> {
    match image.build {
        None => state.registry.get_image_artifact(&image.content_key).await,
        Some(_) => Ok(crate::images::build::recorded(state, &image.content_key).await?.0),
    }
}

/// Where an image's import (or build) is, before any derive.
fn acquire_phase(
    image: &VersionImage,
    artifact: Option<&ImageArtifact>,
    build: Option<&BuildProjection>,
    arch_builds: &[ArchBuild],
) -> Phase {
    match image.build {
        None => phase_of(artifact, build),
        Some(_) => crate::images::build::phase(artifact, arch_builds, build),
    }
}

async fn start_build(
    state: &AppState,
    image: &VersionImage,
    builds_seen: u32,
) -> Result<(), BuildError> {
    let subject = subject(&image.content_key);
    let build_arn = state
        .cloud
        .start_image_import(&ImageImportJob {
            idempotency_token: start_token(&subject, builds_seen),
            subject: subject.clone(),
            source: image.source_ref.clone(),
            content_key: image.content_key.clone(),
        })
        .await?;
    tracing::info!(subject, build_arn, source = image.source_ref, "images: import started");
    crate::images::build_follow::record_requested(
        state,
        &subject,
        &build_arn,
        serde_json::json!({ "source": image.source_ref, "content_key": image.content_key }),
    )
    .await?;
    Ok(())
}

/// Why an imported image cannot serve `image` of the version, or `None` if it can.
fn problem_of(
    ctx: &VersionContext,
    image: &VersionImage,
    artifact: &ImageArtifact,
) -> Option<String> {
    capability_problem(
        ctx.engine,
        &ctx.engine_version,
        ctx.storage_s3.as_deref(),
        image,
        &artifact.capabilities,
    )
}

/// An image's status from its import and (if it is derived) its derive. It is ready once the
/// import suits the version and the derive is done.
fn status_of(
    ctx: &VersionContext,
    image: &VersionImage,
    artifact: Option<ImageArtifact>,
    build: Option<BuildProjection>,
    arch_builds: Vec<ArchBuild>,
    derived: Option<DerivedStatus>,
) -> ImageStatus {
    let problem = artifact.as_ref().and_then(|a| problem_of(ctx, image, a));
    let phase = match (
        acquire_phase(image, artifact.as_ref(), build.as_ref(), &arch_builds),
        &problem,
        &derived,
    ) {
        (Phase::Ready, Some(_), _) => Phase::Failed,
        (Phase::Ready, None, Some(d)) => match d.phase {
            Phase::Unbuilt => Phase::Building,
            phase => phase,
        },
        (phase, _, _) => phase,
    };
    ImageStatus { image: image.clone(), phase, build, arch_builds, artifact, derived, problem }
}

/// The derive to follow for `image`: one whose import is done and suits the version.
fn derive_of<'a>(
    ctx: &'a VersionContext,
    image: &VersionImage,
    artifact: Option<&'a ImageArtifact>,
) -> Option<(&'a ImageArtifact, &'a DeriveRuntime)> {
    let artifact = artifact.filter(|a| problem_of(ctx, image, a).is_none())?;
    Some((artifact, ctx.derive_for(image)?))
}

/// Move one declared image of the version `ctx` was read for one step forward: its import (see
/// [`next_step`]), then its derive (see `crate::images::derived::next_step`), and return where it
/// is.
pub(crate) async fn ensure(
    state: &AppState,
    ctx: &VersionContext,
    image: &VersionImage,
    retry_failed: bool,
) -> Result<ImageStatus, BuildError> {
    let (artifact, build, arch_builds) = match &image.build {
        None => {
            let (artifact, build) = ensure_import(state, image, retry_failed).await?;
            (artifact, build, Vec::new())
        }
        Some(recipe) => {
            let (artifact, arch_builds, index) =
                crate::images::build::ensure(state, image, recipe, retry_failed, false).await?;
            (artifact, index, arch_builds)
        }
    };
    let derived = match derive_of(ctx, image, artifact.as_ref()) {
        Some((imported, runtime)) => {
            Some(crate::images::derived::ensure(state, imported, runtime, retry_failed).await?)
        }
        None => None,
    };
    Ok(status_of(ctx, image, artifact, build, arch_builds, derived))
}

/// Move an import one step forward (see [`next_step`]); returns its recorded result and build.
async fn ensure_import(
    state: &AppState,
    image: &VersionImage,
    retry_failed: bool,
) -> Result<(Option<ImageArtifact>, Option<BuildProjection>), BuildError> {
    let artifact = state.registry.get_image_artifact(&image.content_key).await?;
    let mut build = state.registry.get_build(&subject(&image.content_key)).await?;
    if retry_failed && artifact.is_none() {
        build =
            reconcile_stale_build(state, build, &subject(&image.content_key), BuildKind::Import)
                .await?;
    }
    match next_step(&image.content_key, artifact.as_ref(), build.as_ref(), retry_failed) {
        Step::Nothing => Ok((artifact, build)),
        Step::StartBuild => {
            start_build(state, image, builds_seen(build.as_ref())).await?;
            let build = state.registry.get_build(&subject(&image.content_key)).await?;
            Ok((None, build))
        }
        Step::Record(artifact) => {
            state.registry.put_image_artifact(&artifact).await?;
            tracing::info!(image = artifact.image, "images: import recorded");
            Ok((Some(*artifact), build))
        }
    }
}

/// Where one image is, in `ctx` (read-only).
pub(crate) async fn status(
    state: &AppState,
    ctx: &VersionContext,
    image: &VersionImage,
) -> Result<ImageStatus, BuildError> {
    let (artifact, build, arch_builds) = load(state, image).await?;
    let derived = match derive_of(ctx, image, artifact.as_ref()) {
        Some((imported, runtime)) => {
            Some(crate::images::derived::status(state, imported, runtime).await?)
        }
        None => None,
    };
    Ok(status_of(ctx, image, artifact, build, arch_builds, derived))
}

/// Where each of a version's images is (read-only).
pub(crate) async fn statuses(
    state: &AppState,
    rec: &RegisteredVersion,
) -> Result<Vec<ImageStatus>, BuildError> {
    let ctx = VersionContext::of(state, &rec.version).await?;
    let mut out = Vec::with_capacity(rec.images.len());
    for image in &rec.images {
        out.push(status(state, &ctx, image).await?);
    }
    Ok(out)
}

/// Import (or derive) `name` of the version again, whatever its earlier builds did (a failure, or a
/// success that reported no image), unless one is in progress or it is done. Returns its status,
/// or `None` if the version has no such image.
pub(crate) async fn rebuild(
    state: &AppState,
    rec: &RegisteredVersion,
    name: &str,
) -> Result<Option<ImageStatus>, BuildError> {
    let Some(image) = rec.images.iter().find(|i| i.name == name) else {
        return Ok(None);
    };
    let ctx = VersionContext::of(state, &rec.version).await?;
    let (artifact, build, arch_builds) = load(state, image).await?;
    if acquire_phase(image, artifact.as_ref(), build.as_ref(), &arch_builds) == Phase::Failed {
        match &image.build {
            None => start_build(state, image, builds_seen(build.as_ref())).await?,
            Some(recipe) => {
                crate::images::build::ensure(state, image, recipe, false, true).await?;
            }
        }
    } else if let Some((imported, runtime)) = derive_of(&ctx, image, artifact.as_ref()) {
        crate::images::derived::rebuild(state, imported, runtime).await?;
    }
    Ok(Some(ensure(state, &ctx, image, false).await?))
}

/// Whether a version can run, as `WorkflowVersionSummary.state` names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReadinessState {
    /// It can run.
    Ready,
    /// Its runtime or an image is still being built.
    Pending,
    /// Its runtime or an image failed.
    Failed,
}

impl ReadinessState {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Pending => "pending",
            Self::Failed => "failed",
        }
    }
}

impl std::fmt::Display for ReadinessState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Whether a version can run: its runtime (a coordinator variant) and every image are ready.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Readiness {
    pub(crate) state: ReadinessState,
    /// Why it cannot run now (empty when ready).
    pub(crate) not_ready: Vec<String>,
    pub(crate) runtime: crate::images::variants::RuntimeReport,
    pub(crate) images: Vec<ImageStatus>,
}

/// A build's failure as people read it: its reason and log.
fn failure(build: Option<&BuildProjection>) -> String {
    let reason = build.and_then(failure_reason).unwrap_or_else(|| "unknown reason".into());
    match build.and_then(|b| b.logs_url.as_deref()) {
        Some(url) => format!("{reason} (build log: {url})"),
        None => reason,
    }
}

/// Where a version stands (read-only).
pub(crate) async fn readiness(
    state: &AppState,
    rec: &RegisteredVersion,
) -> Result<Readiness, BuildError> {
    let runtime = crate::images::variants::report(state, &rec.version).await?;
    let images = statuses(state, rec).await?;
    let mut not_ready = Vec::new();
    let mut failed = false;
    // A version pinning a retired engine release never runs again, whatever its old variants.
    let retired = match crate::images::variants::runnable_runtime(&rec.version) {
        Err(crate::images::variants::NotRunnable::Retired(r)) => Some(r),
        _ => None,
    };
    match retired {
        Some(retired) => {
            failed = true;
            not_ready.push(retired.to_string());
        }
        None if runtime.launch.is_none() => {
            let r = &runtime;
            if r.status.phase == Phase::Failed {
                failed = true;
                not_ready.push(format!(
                    "the engine runtime failed to build: {}",
                    failure(r.status.build.as_ref())
                ));
            } else if r.status.phase == Phase::Unbuilt {
                not_ready.push("the engine runtime is waiting to be built".into());
            } else {
                not_ready.push("the engine runtime is being built".into());
            }
        }
        None => {}
    }
    for image in &images {
        if let Some(reason) = image.unready_reason() {
            failed |= image.phase == Phase::Failed;
            not_ready.push(reason);
        }
    }
    let state = match (not_ready.is_empty(), failed) {
        (true, _) => ReadinessState::Ready,
        (false, true) => ReadinessState::Failed,
        (false, false) => ReadinessState::Pending,
    };
    Ok(Readiness { state, not_ready, runtime, images })
}

/// What one sweep of the images did.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct ImageSweepReport {
    pub imports: usize,
    /// Derives followed (a Snakemake image per runtime).
    pub derives: usize,
    pub ready: usize,
    pub(crate) building: usize,
    pub(crate) failed: usize,
    pub errors: usize,
}

/// Move every declared image's import forward: record finished builds, retry failed ones, and ask
/// CodeBuild about builds whose end was never heard.
pub async fn sweep(state: &AppState) -> Result<ImageSweepReport, BuildError> {
    let mut seen = std::collections::BTreeSet::new();
    let mut report = ImageSweepReport::default();
    for rec in state.registry.list_versions().await? {
        // Read once per version, when it first has an image to move.
        let mut cached_ctx: Option<VersionContext> = None;
        for image in &rec.images {
            // One step per import, and per derive of it (the same image under another runtime).
            let derive_runtime = (rec.version.engine == Engine::Snakemake && !image.own_runtime)
                .then_some(rec.version.runtime_key.as_str());
            let step = (image.content_key.clone(), derive_runtime.map(str::to_string));
            if seen.contains(&step) {
                continue;
            }
            report.imports += 1;
            let ctx = match cached_ctx.take() {
                Some(ctx) => ctx,
                None => match VersionContext::of(state, &rec.version).await {
                    Ok(ctx) => ctx,
                    // Not marked seen: another version naming the image can still move it.
                    Err(e) => {
                        report.errors += 1;
                        tracing::warn!(image = image.source_ref, error = %e, "images: sweep step failed");
                        continue;
                    }
                },
            };
            seen.insert(step);
            let result = ensure(state, &ctx, image, true).await;
            cached_ctx = Some(ctx);
            match result {
                // A capability problem belongs to the version, not the image: count the builds.
                Ok(status) => {
                    report.derives += usize::from(status.derived.is_some());
                    match (status.phase, status.runnable().is_some()) {
                        (_, true) => report.ready += 1,
                        (Phase::Failed, false) => report.failed += 1,
                        _ => report.building += 1,
                    }
                }
                Err(e) => {
                    report.errors += 1;
                    tracing::warn!(image = image.source_ref, error = %e, "images: sweep step failed");
                }
            }
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use biffix_core::builds::BuildState;

    use biffix_core::time::Timestamp;

    use super::*;

    fn build(state: BuildState, builds: u32, outputs: &[(&str, &str)]) -> BuildProjection {
        BuildProjection {
            subject: "image:k".into(),
            state,
            build_arn: format!("arn:b:{builds}"),
            outputs: outputs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect::<BTreeMap<_, _>>(),
            reason: None,
            logs_url: None,
            request: serde_json::json!({"source": "quay.io/b/fastqc:1@sha256:s"}),
            builds,
            updated: Timestamp::from_micros(0),
        }
    }

    const OUT: &[(&str, &str)] = &[
        ("IMAGE", "repo@sha256:s"),
        ("IMAGE_DIGEST", "sha256:s"),
        ("ARCHS", "amd64,arm64"),
        ("CAPABILITIES", "bash=1,libc=glibc,snakemake="),
        ("KEEP_TAG", "biffix-keep-s"),
        ("ARCHIVE_BYTES", "123"),
        ("ARCHIVED", "1"),
    ];

    #[test]
    fn a_succeeded_import_is_recorded_from_its_outputs() {
        let b = build(BuildState::Succeeded, 1, OUT);
        let Step::Record(a) = next_step("k", None, Some(&b), false) else {
            panic!("record");
        };
        assert_eq!(a.image, "repo@sha256:s");
        assert_eq!(a.source_ref, "quay.io/b/fastqc:1@sha256:s");
        assert_eq!(a.archs, ["amd64", "arm64"]);
        assert!(a.capabilities.bash && a.archived);
        assert_eq!(a.archive_bytes, Some(123));
        assert_eq!(next_step("k", Some(&*a), Some(&b), true), Step::Nothing);
    }

    #[test]
    fn imports_start_wait_and_retry_like_variant_builds() {
        assert_eq!(next_step("k", None, None, false), Step::StartBuild);
        let running = build(BuildState::Running, 1, &[]);
        assert_eq!(next_step("k", None, Some(&running), true), Step::Nothing);
        let failed = build(BuildState::Failed, 1, &[]);
        assert_eq!(next_step("k", None, Some(&failed), false), Step::Nothing);
        assert_eq!(next_step("k", None, Some(&failed), true), Step::StartBuild);
        let exhausted = build(BuildState::Failed, AUTO_BUILDS, &[]);
        assert_eq!(next_step("k", None, Some(&exhausted), true), Step::Nothing);
        let empty = build(BuildState::Succeeded, 1, &[]);
        assert_eq!(next_step("k", None, Some(&empty), true), Step::StartBuild);
    }

    fn image(name: &str, own_runtime: bool) -> VersionImage {
        VersionImage {
            name: name.into(),
            declared_ref: "x:1".into(),
            source_ref: "x:1@sha256:s".into(),
            content_key: "k".into(),
            own_runtime,
            build: None,
        }
    }

    #[test]
    fn an_image_must_suit_the_engine() {
        let caps = |s: &str| Capabilities::parse(s);
        let nf = |c: &str| {
            capability_problem(Engine::Nextflow, "25.10.4", None, &image("x", false), &caps(c))
        };
        assert_eq!(nf("bash=1,libc=musl"), None);
        assert!(nf("bash=0,libc=glibc").unwrap().contains("no bash"));

        // A derived Snakemake image gets the runtime: any glibc image with bash will do.
        let sm = |c: &str, own: bool| {
            capability_problem(
                Engine::Snakemake,
                "9.27.0",
                Some("0.3.6"),
                &image("samtools", own),
                &caps(c),
            )
        };
        assert_eq!(sm("bash=1,libc=glibc", false), None);
        assert_eq!(sm("bash=1,libc=glibc,snakemake=8.30.0", false), None);
        let musl = sm("bash=1,libc=musl", false).unwrap();
        assert!(musl.contains("no glibc (it is musl)") && musl.contains("runtime: own"), "{musl}");
        assert!(sm("bash=1,libc=none", false).unwrap().contains("static or distroless"));

        // `runtime: own` runs the image's Snakemake: the release and storage plugin must match.
        let own = "bash=1,libc=musl,snakemake=9.27.0,storage_s3=0.3.6,logger=,executor=";
        assert_eq!(sm(own, true), None);
        let old = sm("bash=1,snakemake=8.30.0,storage_s3=0.3.6", true).unwrap();
        assert!(
            old.contains("carries Snakemake 8.30.0, but this version runs Snakemake 9.27.0"),
            "{old}"
        );
        let plugin = sm("bash=1,snakemake=9.27.0,storage_s3=0.3.5", true).unwrap();
        assert!(plugin.contains("snakemake-storage-plugin-s3 0.3.5"), "{plugin}");
        assert!(sm("bash=1,snakemake=9.27.0", true).unwrap().contains("plugin-s3 none"));
    }

    fn imported(caps: &str) -> ImageArtifact {
        ImageArtifact {
            content_key: "k".into(),
            source_ref: "x:1@sha256:s".into(),
            image: "repo@sha256:s".into(),
            digest: "sha256:s".into(),
            archs: vec!["amd64".into()],
            capabilities: Capabilities::parse(caps),
            keep_tag: None,
            archived: true,
            archive_bytes: None,
            created_at: 0,
        }
    }

    fn snakemake_ctx(layer: bool) -> VersionContext {
        VersionContext {
            engine: Engine::Snakemake,
            engine_version: "9.27.0".into(),
            storage_s3: Some("0.3.6".into()),
            derive: Some(DeriveRuntime {
                runtime_key: "rt".into(),
                engine_version: "9.27.0".into(),
                layer: layer.then(|| crate::images::derived::RuntimeLayer {
                    amd64: "coord@sha256:a".into(),
                    arm64: "coord@sha256:b".into(),
                }),
            }),
        }
    }

    fn derived(phase: Phase, artifact: bool, waiting_for_runtime: bool) -> DerivedStatus {
        DerivedStatus {
            key: "d".into(),
            engine_version: "9.27.0".into(),
            phase,
            waiting_for_runtime,
            build: None,
            artifact: artifact.then(|| ImageArtifact {
                image: "repo@sha256:d".into(),
                digest: "sha256:d".into(),
                ..imported("bash=1,libc=glibc,snakemake=9.27.0")
            }),
        }
    }

    #[test]
    fn a_derived_image_is_ready_once_derived_and_runs_the_derived_digest() {
        let ctx = snakemake_ctx(true);
        let img = image("default", false);
        let glibc = || Some(imported("bash=1,libc=glibc"));
        let st = |d| status_of(&ctx, &img, glibc(), None, vec![], Some(d));

        let waiting = st(derived(Phase::Unbuilt, false, true));
        assert_eq!(waiting.phase, Phase::Building);
        assert!(waiting
            .unready_reason()
            .unwrap()
            .contains("waiting for the Snakemake 9.27.0 runtime"));
        let building = st(derived(Phase::Building, false, false));
        assert_eq!(
            building.unready_reason().unwrap(),
            "image `default`: adding the Snakemake 9.27.0 runtime"
        );
        assert_eq!(building.runnable(), None);
        let failed = st(derived(Phase::Failed, false, false));
        assert_eq!(failed.phase, Phase::Failed);
        assert!(failed.unready_reason().unwrap().contains("runtime failed"));
        let ready = st(derived(Phase::Ready, true, false));
        assert_eq!((ready.phase, ready.unready_reason()), (Phase::Ready, None));
        assert_eq!(ready.runnable().unwrap().digest, "sha256:d");

        // Not derived: `runtime: own`, and every Nextflow image, run as imported.
        let own = image("default", true);
        assert!(derive_of(&ctx, &own, glibc().as_ref()).is_none());
        let caps = "bash=1,libc=glibc,snakemake=9.27.0,storage_s3=0.3.6";
        let own_status = status_of(&ctx, &own, Some(imported(caps)), None, vec![], None);
        assert_eq!(own_status.phase, Phase::Ready);
        assert_eq!(own_status.runnable().unwrap().digest, "sha256:s");
        // A musl image is refused before any derive starts.
        assert!(derive_of(&ctx, &img, Some(&imported("bash=1,libc=musl"))).is_none());
    }

    #[test]
    fn a_runtime_without_its_layer_yet_only_waits() {
        let img = image("default", false);
        let ctx = snakemake_ctx(false);
        assert!(derive_of(&ctx, &img, Some(&imported("bash=1,libc=glibc"))).is_some());
    }

    #[test]
    fn readiness_states_keep_their_wire_names() {
        let names = [ReadinessState::Ready, ReadinessState::Pending, ReadinessState::Failed]
            .map(|s| s.to_string());
        assert_eq!(names, ["ready", "pending", "failed"]);
    }

    #[test]
    fn an_import_that_succeeded_without_an_image_failed_even_once_its_retries_are_used() {
        for builds in [1, AUTO_BUILDS] {
            let empty = build(BuildState::Succeeded, builds, &[("IMAGE", "r@sha256:i")]);
            assert_eq!(phase_of(None, Some(&empty)), Phase::Failed, "{builds}");
            assert_eq!(
                failure_reason(&empty).as_deref(),
                Some(crate::images::build_follow::NO_OUTPUT)
            );
            let want = if builds < AUTO_BUILDS { Step::StartBuild } else { Step::Nothing };
            assert_eq!(next_step("k", None, Some(&empty), true), want, "{builds}");
        }
        let reported = build(
            BuildState::Succeeded,
            1,
            &[("IMAGE", "r@sha256:i"), ("IMAGE_DIGEST", "sha256:i")],
        );
        assert_eq!(phase_of(None, Some(&reported)), Phase::Building);
    }
}
