//! How the API shows an image's readiness (the registration routes and a run's detail).

/// One image's readiness as the API shows it. `build` (and so `reason`/`logs_url`) is the build
/// where the image is stuck: its derive's once the import is done, else a built image's failed
/// architecture build (or index), else its import's. `reason` prefers the image's own problem
/// (e.g. a capability mismatch) over the build's.
pub(crate) fn image_view(status: &crate::images::ImageStatus) -> biffix_api::WorkflowImage {
    let artifact = status.artifact.as_ref();
    let derived = status.derived.as_ref();
    // The derive's build once the import is done: that is where a failure is; for a built image
    // before that, its failed architecture build (or index).
    let build = derived.and_then(|d| d.build.as_ref()).or_else(|| {
        crate::images::build::failed_build(&status.arch_builds, status.build.as_ref())
            .map(|(_, b)| b)
            .or(status.build.as_ref())
    });
    biffix_api::WorkflowImage {
        name: status.image.name.clone(),
        kind: status.image.kind().to_string(),
        declared_ref: status.image.declared_ref.clone(),
        source_ref: status.image.source_ref.clone(),
        state: status.phase.as_str().to_string(),
        image: artifact.map(|a| a.image.clone()),
        archs: artifact.map(|a| a.archs.clone()).unwrap_or_default(),
        capabilities: artifact
            .and_then(|a| serde_json::to_value(&a.capabilities).ok())
            .unwrap_or_default(),
        archived: artifact.is_some_and(|a| a.archived),
        keep_tag: artifact.and_then(|a| a.keep_tag.clone()),
        derived_image: derived.and_then(|d| d.artifact.as_ref().map(|a| a.image.clone())),
        derived_state: derived.map(|d| d.phase.as_str().to_string()),
        reason: status.problem.clone().or_else(|| {
            build.and_then(|b| {
                reason_of(b, status.phase == crate::images::build_follow::Phase::Failed)
            })
        }),
        logs_url: build.and_then(|b| b.logs_url.clone()),
        generated_dockerfile: status.image.build.as_ref().and_then(|b| b.dockerfile_text.clone()),
        arch_builds: status
            .arch_builds
            .iter()
            .map(|a| biffix_api::ImageArchBuild {
                arch: a.arch.to_string(),
                state: a.build.as_ref().map_or("unbuilt", |b| b.state.as_str()).to_string(),
                reason: a.build.as_ref().and_then(|b| b.reason.clone()),
                logs_url: a.build.as_ref().and_then(|b| b.logs_url.clone()),
            })
            .collect(),
    }
}

/// A build's reason as the API shows it: when its subject failed, why (a build that succeeded but
/// reported no image says so, rather than leaving `reason` empty beside `state: failed`).
pub(crate) fn reason_of(
    build: &biffix_core::builds::BuildProjection,
    failed: bool,
) -> Option<String> {
    if failed {
        crate::images::build_follow::failure_reason(build)
    } else {
        build.reason.clone()
    }
}
