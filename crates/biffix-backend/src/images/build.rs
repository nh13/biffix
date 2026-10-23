//! Images built from a recipe the workflow ships (`images.<name>: {dockerfile: …}` or `{pixi: …}`).
//!
//! A build is tenant-wide, keyed by [`biffix_workflow::images::build_content_key`] (what it is
//! built from), and takes two kinds of CodeBuild build: one per architecture, each on its own
//! architecture's hardware (`codebuild/image/build.sh`, subject `image:build:<key>:<arch>`), then an
//! index joining them (`codebuild/image/import.sh` step `index`, subject `image:build:<key>`), which
//! also keep-tags, probes and archives the image. The index's result is recorded in
//! `image_artifacts` exactly like an import's, so from there on a built image is used, checked and
//! (for Snakemake) derived like an imported one.
//!
//! [`next_step`] is the pure planner; [`ensure`] takes one step, like [`crate::images::ensure`] does
//! for an import.
//!
//! Built images live in the deploy's builds repository ([`crate::DeployConfig::builds_image_repo`]),
//! which only biffix's build and import projects can push to: an architecture's build reuses a
//! `build-*` tag it finds there unchecked (a build is not reproducible, so nothing could check it),
//! which is sound only where no one else can have written the tag. A built image recorded anywhere
//! else (a repository the deploy no longer builds into, such as the workflow repository, where
//! third-party workflow CI can push) is not trusted: it is built again, once, into the builds
//! repository, and its recorded result replaced ([`recorded`]).

use biffix_core::builds::{BuildProjection, BuildState};
use biffix_workflow::images::{build_subject, Arch};

use crate::aws::builds::{ImageBuildJob, ImageIndexJob};
use crate::images::build_follow::{
    builds_seen, follow_step, has_failed, reconcile_stale_build, start_token, BuildError,
    BuildKind, FollowStep, Phase, AUTO_BUILDS,
};
use crate::images::reports_image;
use crate::store::registry::{BuildRecipe, ImageArtifact, VersionImage};
use crate::store::StoreError;
use crate::AppState;

/// Whether `image` (`<repo>@sha256:…`) is in `builds_repo`, the only repository a built image is
/// trusted from (see the module doc). An empty `builds_repo` (a deploy that configures none: tests,
/// local development) trusts any.
pub(crate) fn in_builds_repo(builds_repo: &str, image: &str) -> bool {
    builds_repo.is_empty()
        || image.strip_prefix(builds_repo).and_then(|rest| rest.strip_prefix("@sha256:")).is_some()
}

/// A built image's recorded result: the one to use (in the builds repository), and one recorded
/// from outside it, which is ignored until the image is built again and then replaced (see the
/// module doc).
pub(crate) async fn recorded(
    state: &AppState,
    content_key: &str,
) -> Result<(Option<ImageArtifact>, Option<ImageArtifact>), StoreError> {
    Ok(match state.registry.get_image_artifact(content_key).await? {
        Some(a) if !in_builds_repo(&state.deploy.builds_image_repo, &a.image) => (None, Some(a)),
        artifact => (artifact, None),
    })
}

/// One architecture's build of an image.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ArchBuild {
    pub(crate) arch: Arch,
    pub(crate) build: Option<BuildProjection>,
}

impl ArchBuild {
    /// The image it pushed (`<workflow repo>@sha256:…`), once it succeeded.
    pub(crate) fn image(&self) -> Option<&str> {
        self.build
            .as_ref()
            .filter(|b| b.state == BuildState::Succeeded)
            .and_then(|b| crate::images::build_follow::output(b, "IMAGE"))
    }
}

/// The next step for a built image.
#[derive(Clone, Debug, PartialEq)]
enum Step {
    Nothing,
    /// Start these architectures' builds.
    Start(Vec<Arch>),
    /// Join the architectures' images (in arch order).
    StartIndex(Vec<String>),
    /// Record the result of the succeeded index.
    Record(Box<ImageArtifact>),
}

/// What to do next for a built image. A failed (or image-less) build starts again when `retry`
/// allows it: `force` always does, `retry_failed` while the subject has had fewer than
/// [`AUTO_BUILDS`] builds. A build that pushed outside `builds_repo` (see [`in_builds_repo`]) is
/// superseded: it starts again whatever `retry` says, and its image is never joined or recorded.
/// `artifact` is the trusted recorded result ([`recorded`]).
fn next_step(
    content_key: &str,
    arch_builds: &[ArchBuild],
    index: Option<&BuildProjection>,
    artifact: Option<&ImageArtifact>,
    retry_failed: bool,
    force: bool,
    builds_repo: &str,
) -> Step {
    if artifact.is_some() {
        return Step::Nothing;
    }
    let retry = |b: &BuildProjection| force || (retry_failed && b.builds < AUTO_BUILDS);
    let start: Vec<Arch> = arch_builds
        .iter()
        .filter(|a| match &a.build {
            None => true,
            Some(b) => match b.state {
                BuildState::Failed => retry(b),
                BuildState::Succeeded => match a.image() {
                    Some(image) => !in_builds_repo(builds_repo, image),
                    None => retry(b),
                },
                BuildState::Requested | BuildState::Running => false,
            },
        })
        .map(|a| a.arch)
        .collect();
    if !start.is_empty() {
        return Step::Start(start);
    }
    let Some(manifests) = arch_builds
        .iter()
        .map(|a| a.image().filter(|i| in_builds_repo(builds_repo, i)).map(str::to_string))
        .collect::<Option<Vec<_>>>()
    else {
        return Step::Nothing;
    };
    match follow_step(false, index, retry, |b| crate::images::artifact_from(content_key, b)) {
        FollowStep::Nothing => Step::Nothing,
        FollowStep::StartBuild => Step::StartIndex(manifests),
        FollowStep::Record(artifact) if in_builds_repo(builds_repo, &artifact.image) => {
            Step::Record(Box::new(artifact))
        }
        // An index pushed outside the builds repository is superseded whatever `retry` says.
        FollowStep::Record(_) => Step::StartIndex(manifests),
    }
}

/// A built image's phase: ready once recorded; failed when its latest architecture build or index
/// failed (see [`failed_build`]); unbuilt before any build.
pub(crate) fn phase(
    artifact: Option<&ImageArtifact>,
    arch_builds: &[ArchBuild],
    index: Option<&BuildProjection>,
) -> Phase {
    if artifact.is_some() {
        Phase::Ready
    } else if failed_build(arch_builds, index).is_some() {
        Phase::Failed
    } else if arch_builds.iter().all(|a| a.build.is_none()) && index.is_none() {
        Phase::Unbuilt
    } else {
        Phase::Building
    }
}

/// The failed build of a built image (the first failed architecture, else the index), with what
/// it built, for people. A build that succeeded but reported no image (an architecture's `IMAGE`,
/// or the index's `IMAGE` and `IMAGE_DIGEST`) failed too: nothing can be joined or recorded.
pub(crate) fn failed_build<'a>(
    arch_builds: &'a [ArchBuild],
    index: Option<&'a BuildProjection>,
) -> Option<(String, &'a BuildProjection)> {
    arch_builds
        .iter()
        .find_map(|a| {
            a.build
                .as_ref()
                .filter(|b| has_failed(b, |_| a.image().is_some()))
                .map(|b| (a.arch.to_string(), b))
        })
        .or_else(|| {
            index.filter(|b| has_failed(b, reports_image)).map(|b| ("index".to_string(), b))
        })
}

/// A built image's builds as recorded: each architecture's and the index.
pub(crate) async fn load(
    state: &AppState,
    image: &VersionImage,
    recipe: &BuildRecipe,
) -> Result<(Vec<ArchBuild>, Option<BuildProjection>), BuildError> {
    let mut arch_builds = Vec::new();
    for arch in recipe.archs() {
        let build =
            state.registry.get_build(&build_subject(&image.content_key, Some(arch))).await?;
        arch_builds.push(ArchBuild { arch, build });
    }
    let index = state.registry.get_build(&build_subject(&image.content_key, None)).await?;
    Ok((arch_builds, index))
}

async fn start_arch(
    state: &AppState,
    image: &VersionImage,
    recipe: &BuildRecipe,
    arch: Arch,
    builds_seen: u32,
) -> Result<(), BuildError> {
    let subject = build_subject(&image.content_key, Some(arch));
    let build_arn = state
        .cloud
        .start_image_build(&ImageBuildJob {
            idempotency_token: start_token(&subject, builds_seen),
            subject: subject.clone(),
            arch,
            content_key: image.content_key.clone(),
            context_uri: recipe.context_uri.clone(),
            context_sha256: recipe.context_sha256.clone(),
            dockerfile: recipe.dockerfile.clone(),
            dockerfile_text: recipe.dockerfile_text.clone(),
        })
        .await?;
    tracing::info!(subject, build_arn, "images: build started");
    crate::images::build_follow::record_requested(
        state,
        &subject,
        &build_arn,
        serde_json::json!({
            "kind": recipe.kind,
            "context": recipe.context,
            "arch": arch.as_str(),
            "content_key": image.content_key,
        }),
    )
    .await?;
    Ok(())
}

async fn start_index(
    state: &AppState,
    image: &VersionImage,
    manifests: Vec<String>,
    builds_seen: u32,
) -> Result<(), BuildError> {
    let subject = build_subject(&image.content_key, None);
    let build_arn = state
        .cloud
        .start_image_index(&ImageIndexJob {
            idempotency_token: start_token(&subject, builds_seen),
            subject: subject.clone(),
            content_key: image.content_key.clone(),
            manifests: manifests.clone(),
        })
        .await?;
    tracing::info!(subject, build_arn, "images: index started");
    // `source` is what the recorded artifact names as its origin (`crate::images::artifact_from`).
    crate::images::build_follow::record_requested(
        state,
        &subject,
        &build_arn,
        serde_json::json!({
            "source": image.source_ref,
            "content_key": image.content_key,
            "manifests": manifests,
        }),
    )
    .await?;
    Ok(())
}

/// Move a built image one step forward (see [`next_step`]); returns its recorded result and builds.
/// `force` restarts failed builds whatever their count (a rebuild).
pub(crate) async fn ensure(
    state: &AppState,
    image: &VersionImage,
    recipe: &BuildRecipe,
    retry_failed: bool,
    force: bool,
) -> Result<(Option<ImageArtifact>, Vec<ArchBuild>, Option<BuildProjection>), BuildError> {
    let (artifact, superseded) = recorded(state, &image.content_key).await?;
    let (mut arch_builds, mut index) = load(state, image, recipe).await?;
    if (retry_failed || force) && artifact.is_none() {
        for a in &mut arch_builds {
            let subject = build_subject(&image.content_key, Some(a.arch));
            a.build = reconcile_stale_build(state, a.build.take(), &subject, BuildKind::ArchBuild)
                .await?;
        }
        let subject = build_subject(&image.content_key, None);
        // The index runs in the import project.
        index = reconcile_stale_build(state, index, &subject, BuildKind::Import).await?;
    }
    match next_step(
        &image.content_key,
        &arch_builds,
        index.as_ref(),
        artifact.as_ref(),
        retry_failed,
        force,
        &state.deploy.builds_image_repo,
    ) {
        Step::Nothing => Ok((artifact, arch_builds, index)),
        Step::Start(archs) => {
            for arch in archs {
                let seen =
                    arch_builds.iter().find(|a| a.arch == arch).and_then(|a| a.build.as_ref());
                start_arch(state, image, recipe, arch, builds_seen(seen)).await?;
            }
            let (arch_builds, index) = load(state, image, recipe).await?;
            Ok((None, arch_builds, index))
        }
        Step::StartIndex(manifests) => {
            start_index(state, image, manifests, builds_seen(index.as_ref())).await?;
            let (arch_builds, index) = load(state, image, recipe).await?;
            Ok((None, arch_builds, index))
        }
        Step::Record(artifact) => {
            match &superseded {
                // Built again into the builds repository: replace what was recorded outside it.
                Some(old) => {
                    state.registry.replace_image_artifact(&old.image, &artifact).await?;
                    tracing::info!(
                        image = artifact.image,
                        superseded = old.image,
                        "images: build recorded, replacing one outside the builds repository"
                    );
                }
                None => {
                    state.registry.put_image_artifact(&artifact).await?;
                    tracing::info!(image = artifact.image, "images: build recorded");
                }
            }
            Ok((Some(*artifact), arch_builds, index))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn build(state: BuildState, builds: u32, outputs: &[(&str, &str)]) -> BuildProjection {
        BuildProjection {
            subject: "s".into(),
            build_arn: "arn".into(),
            state,
            builds,
            request: serde_json::json!({ "source": "pixi:env" }),
            outputs: outputs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect::<BTreeMap<_, _>>(),
            reason: None,
            logs_url: None,
            updated: biffix_core::time::Timestamp::from_micros(0),
        }
    }

    fn arch(arch: Arch, build: Option<BuildProjection>) -> ArchBuild {
        ArchBuild { arch, build }
    }

    fn pushed(image: &str) -> Option<BuildProjection> {
        Some(build(BuildState::Succeeded, 1, &[("IMAGE", image)]))
    }

    #[test]
    fn architectures_build_first_then_the_index_then_the_result_is_recorded() {
        let step = |archs: &[ArchBuild], index: Option<&BuildProjection>| {
            next_step("k", archs, index, None, true, false, "")
        };
        let both = [arch(Arch::Amd64, None), arch(Arch::Arm64, None)];
        assert_eq!(step(&both, None), Step::Start(vec![Arch::Amd64, Arch::Arm64]));
        let one_running = [
            arch(Arch::Amd64, pushed("r@sha256:a")),
            arch(Arch::Arm64, Some(build(BuildState::Running, 1, &[]))),
        ];
        assert_eq!(step(&one_running, None), Step::Nothing);
        let built =
            [arch(Arch::Amd64, pushed("r@sha256:a")), arch(Arch::Arm64, pushed("r@sha256:b"))];
        assert_eq!(
            step(&built, None),
            Step::StartIndex(vec!["r@sha256:a".into(), "r@sha256:b".into()])
        );
        let running = build(BuildState::Running, 1, &[]);
        assert_eq!(step(&built, Some(&running)), Step::Nothing);
        let indexed = build(
            BuildState::Succeeded,
            1,
            &[("IMAGE", "r@sha256:i"), ("IMAGE_DIGEST", "sha256:i")],
        );
        let Step::Record(artifact) = step(&built, Some(&indexed)) else {
            panic!("expected a record");
        };
        assert_eq!(
            (artifact.image.as_str(), artifact.source_ref.as_str()),
            ("r@sha256:i", "pixi:env")
        );
        assert_eq!(
            next_step("k", &built, Some(&indexed), Some(&artifact), true, false, ""),
            Step::Nothing
        );
    }

    #[test]
    fn a_failed_build_is_retried_a_few_times_then_waits_for_a_rebuild() {
        let failed = |n| {
            [
                arch(Arch::Amd64, pushed("r@sha256:a")),
                arch(Arch::Arm64, Some(build(BuildState::Failed, n, &[]))),
            ]
        };
        assert_eq!(
            next_step("k", &failed(1), None, None, true, false, ""),
            Step::Start(vec![Arch::Arm64])
        );
        assert_eq!(
            next_step("k", &failed(AUTO_BUILDS), None, None, true, false, ""),
            Step::Nothing
        );
        assert_eq!(
            next_step("k", &failed(AUTO_BUILDS), None, None, false, true, ""),
            Step::Start(vec![Arch::Arm64])
        );
        assert_eq!(phase(None, &failed(1), None), Phase::Failed);
        let (what, _) = failed_build(&failed(1), None).unwrap();
        assert_eq!(what, "arm64");
    }

    #[test]
    fn phases_follow_the_builds() {
        let none = [arch(Arch::Amd64, None)];
        assert_eq!(phase(None, &none, None), Phase::Unbuilt);
        let building = [arch(Arch::Amd64, Some(build(BuildState::Running, 1, &[])))];
        assert_eq!(phase(None, &building, None), Phase::Building);
        let failed_index = build(BuildState::Failed, 1, &[]);
        let built = [arch(Arch::Amd64, pushed("r@sha256:a"))];
        assert_eq!(phase(None, &built, Some(&failed_index)), Phase::Failed);
        assert_eq!(failed_build(&built, Some(&failed_index)).unwrap().0, "index");
        let unrecorded = build(
            BuildState::Succeeded,
            1,
            &[("IMAGE", "r@sha256:i"), ("IMAGE_DIGEST", "sha256:i")],
        );
        assert_eq!(phase(None, &built, Some(&unrecorded)), Phase::Building);
    }

    #[test]
    fn a_build_that_succeeded_without_an_image_failed_and_a_rebuild_restarts_it() {
        use crate::images::build_follow::{failure_reason, NO_OUTPUT};
        for builds in [1, AUTO_BUILDS] {
            // An architecture build that pushed nothing.
            let archs = [
                arch(Arch::Amd64, pushed("r@sha256:a")),
                arch(Arch::Arm64, Some(build(BuildState::Succeeded, builds, &[]))),
            ];
            assert_eq!(phase(None, &archs, None), Phase::Failed, "{builds}");
            let (what, b) = failed_build(&archs, None).unwrap();
            assert_eq!((what.as_str(), failure_reason(b).as_deref()), ("arm64", Some(NO_OUTPUT)));
            assert_eq!(
                next_step("k", &archs, None, None, false, true, ""),
                Step::Start(vec![Arch::Arm64])
            );

            // An index that reported no image.
            let built = [arch(Arch::Amd64, pushed("r@sha256:a"))];
            let index = build(BuildState::Succeeded, builds, &[("IMAGE", "r@sha256:i")]);
            assert_eq!(phase(None, &built, Some(&index)), Phase::Failed, "{builds}");
            assert_eq!(failed_build(&built, Some(&index)).unwrap().0, "index");
            assert_eq!(
                next_step("k", &built, Some(&index), None, false, true, ""),
                Step::StartIndex(vec!["r@sha256:a".into()])
            );
        }
        // Once its retries are used, only a rebuild starts another.
        let exhausted = [arch(Arch::Amd64, Some(build(BuildState::Succeeded, AUTO_BUILDS, &[])))];
        assert_eq!(next_step("k", &exhausted, None, None, true, false, ""), Step::Nothing);
    }

    #[test]
    fn only_the_builds_repository_is_trusted() {
        const BUILDS: &str = "reg/builds";
        assert!(in_builds_repo(BUILDS, "reg/builds@sha256:a"));
        for outside in [
            "reg/wf@sha256:a",
            "reg/builds-other@sha256:a",
            "reg/builds:build-0123@sha256:a",
            "other/reg/builds@sha256:a",
            "reg/builds",
        ] {
            assert!(!in_builds_repo(BUILDS, outside), "{outside}");
        }
        // A deploy that configures none trusts any.
        assert!(in_builds_repo("", "reg/wf@sha256:a"));
    }

    #[test]
    fn a_build_outside_the_builds_repository_is_built_again_once_into_it() {
        const BUILDS: &str = "reg/builds";
        let step = |archs: &[ArchBuild], index: Option<&BuildProjection>, retry: bool| {
            next_step("k", archs, index, None, retry, false, BUILDS)
        };
        let indexed = |image: &str| {
            build(BuildState::Succeeded, 1, &[("IMAGE", image), ("IMAGE_DIGEST", "sha256:i")])
        };
        // Built into the workflow repository: both architectures build again
        // (even with no retries allowed: nothing failed), and the old index is never recorded.
        let old = [
            arch(Arch::Amd64, pushed("reg/wf@sha256:a")),
            arch(Arch::Arm64, pushed("reg/wf@sha256:b")),
        ];
        let old_index = indexed("reg/wf@sha256:i");
        for retry in [true, false] {
            assert_eq!(
                step(&old, Some(&old_index), retry),
                Step::Start(vec![Arch::Amd64, Arch::Arm64])
            );
        }
        // It shows as building, not failed, until it is.
        assert_eq!(phase(None, &old, Some(&old_index)), Phase::Building);
        assert!(failed_build(&old, Some(&old_index)).is_none());
        // One architecture rebuilt, the other still old: only the old one starts.
        let half = [
            arch(Arch::Amd64, pushed("reg/builds@sha256:a")),
            arch(Arch::Arm64, pushed("reg/wf@sha256:b")),
        ];
        assert_eq!(step(&half, Some(&old_index), false), Step::Start(vec![Arch::Arm64]));
        // Both rebuilt: the old index is joined again, from the builds repository's images.
        let rebuilt = [
            arch(Arch::Amd64, pushed("reg/builds@sha256:a")),
            arch(Arch::Arm64, pushed("reg/builds@sha256:b")),
        ];
        assert_eq!(
            step(&rebuilt, Some(&old_index), false),
            Step::StartIndex(vec!["reg/builds@sha256:a".into(), "reg/builds@sha256:b".into()])
        );
        // The new index is recorded.
        let Step::Record(artifact) = step(&rebuilt, Some(&indexed("reg/builds@sha256:j")), false)
        else {
            panic!("expected a record");
        };
        assert_eq!(artifact.image, "reg/builds@sha256:j");
    }
}
