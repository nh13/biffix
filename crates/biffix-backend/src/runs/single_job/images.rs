//! Architecture admission for the runnable artifacts in a single-job run's effective image map.

use std::collections::BTreeSet;

use biffix_workflow::images::Arch;

use crate::images::ImageStatus;

/// Refuse known incompatible runnable images; missing metadata still needs host-side validation.
/// Pending derives contribute no runnable artifact and remain subject to the launch readiness gate.
pub(crate) fn check_images(
    architecture: Arch,
    version_images: &[ImageStatus],
    submit_images: &[ImageStatus],
    worker_image: Option<&str>,
) -> Result<(), String> {
    let map = crate::images::map::build_with_overrides(version_images, submit_images, worker_image);
    let targets: BTreeSet<_> = map.values().map(String::as_str).collect();
    let mut incompatible = BTreeSet::new();
    for artifact in version_images.iter().chain(submit_images).filter_map(ImageStatus::runnable) {
        if !targets.contains(artifact.image.as_str()) {
            continue;
        }
        let recorded: BTreeSet<_> = artifact
            .archs
            .iter()
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
            .collect();
        if !recorded.is_empty() && !recorded.contains(architecture.as_str()) {
            incompatible.insert(artifact.image.as_str());
        }
    }
    if incompatible.is_empty() {
        return Ok(());
    }
    let images =
        incompatible.into_iter().map(|image| format!("`{image}`")).collect::<Vec<_>>().join(", ");
    Err(format!(
        "single-job tasks require {architecture} images; incompatible task images: {images}; \
         choose images supporting {architecture}"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::images::build_follow::Phase;
    use crate::images::derived::DerivedStatus;
    use crate::store::registry::VersionImage;
    use crate::testing::{fake_artifact, fake_derived_artifact};

    fn image(name: &str, architectures: &[&str]) -> ImageStatus {
        let image = VersionImage {
            name: name.into(),
            declared_ref: format!("example.com/tools/{name}:1"),
            source_ref: format!("example.com/tools/{name}@sha256:{}", name.repeat(64)),
            content_key: name.into(),
            own_runtime: false,
            build: None,
        };
        let mut artifact = fake_artifact(&image);
        artifact.archs = architectures.iter().map(ToString::to_string).collect();
        ImageStatus {
            image,
            phase: Phase::Ready,
            build: None,
            arch_builds: vec![],
            artifact: Some(artifact),
            derived: None,
            problem: None,
        }
    }

    #[test]
    fn recorded_architectures_must_include_the_required_platform() {
        for (architecture, recorded, accepted) in [
            (Arch::Amd64, vec!["amd64"], true),
            (Arch::Arm64, vec!["arm64"], true),
            (Arch::Amd64, vec!["arm64"], false),
            (Arch::Arm64, vec!["amd64"], false),
            (Arch::Amd64, vec!["amd64", "arm64"], true),
            (Arch::Amd64, vec![" amd64 ", ""], true),
            (Arch::Amd64, vec![], true),
            (Arch::Amd64, vec!["", "  "], true),
            (Arch::Amd64, vec!["riscv64"], false),
        ] {
            let status = image("a", &recorded);
            let result = check_images(architecture, std::slice::from_ref(&status), &[], None);
            assert_eq!(result.is_ok(), accepted, "{architecture} {recorded:?}: {result:?}");
            if !accepted {
                let reason = result.unwrap_err();
                assert!(reason.contains(architecture.as_str()), "{reason}");
                assert!(reason.contains(&status.artifact.as_ref().unwrap().image), "{reason}");
                assert!(!reason.contains("queue-class"), "unsupported remedy: {reason}");
            }
        }
    }

    #[test]
    fn derived_metadata_decides_compatibility_not_the_imported_base() {
        let mut status = image("a", &["amd64"]);
        let mut derived = fake_derived_artifact(&status.image, "runtime");
        derived.archs = vec!["arm64".into()];
        let derived_image = derived.image.clone();
        status.derived = Some(DerivedStatus {
            key: derived.content_key.clone(),
            engine_version: "9.27.2".into(),
            phase: Phase::Ready,
            waiting_for_runtime: false,
            build: None,
            artifact: Some(derived),
        });
        let reason =
            check_images(Arch::Amd64, std::slice::from_ref(&status), &[], None).unwrap_err();
        assert!(reason.contains(&derived_image), "{reason}");
        assert!(check_images(Arch::Arm64, &[status], &[], None).is_ok());
    }

    #[test]
    fn aliases_are_deduplicated_and_errors_have_stable_order() {
        let a = image("a", &["arm64"]);
        let b = image("b", &["arm64"]);
        let expected = check_images(Arch::Amd64, &[a.clone(), b.clone()], &[], None).unwrap_err();
        let reversed = check_images(Arch::Amd64, &[b.clone(), a.clone()], &[], None).unwrap_err();
        assert_eq!(expected, reversed);
        let duplicated =
            check_images(Arch::Amd64, &[a.clone(), b.clone()], &[b.clone(), a.clone()], None)
                .unwrap_err();
        assert_eq!(expected, duplicated);
        for status in [a, b] {
            assert_eq!(expected.matches(&status.artifact.unwrap().image).count(), 1, "{expected}");
        }
    }

    #[test]
    fn a_submit_time_override_cannot_hide_an_incompatible_default() {
        let default = image("a", &["arm64"]);
        let worker = default.artifact.as_ref().unwrap().image.clone();
        let mut replacement = image("b", &["amd64"]);
        replacement.image.declared_ref = worker.clone();
        let reason =
            check_images(Arch::Amd64, &[default], &[replacement], Some(&worker)).unwrap_err();
        assert!(reason.contains(&worker), "{reason}");
    }

    #[test]
    fn pending_derived_images_do_not_borrow_the_imported_bases_metadata() {
        let mut status = image("a", &["arm64"]);
        status.phase = Phase::Building;
        status.derived = Some(DerivedStatus {
            key: "pending".into(),
            engine_version: "9.27.2".into(),
            phase: Phase::Building,
            waiting_for_runtime: false,
            build: None,
            artifact: None,
        });
        assert!(check_images(Arch::Amd64, std::slice::from_ref(&status), &[], None).is_ok());
        assert!(status.unready_reason().is_some());
    }
}
