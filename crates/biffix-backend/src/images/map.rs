//! A run's image map (`images.json`): every image reference the run's tasks may name, mapped to the
//! tenant image they run. The coordinator hands it to the engine — the Snakemake AWS Batch
//! executor's `container_image_map`, nf-biffix's container resolver — and both are strict: a task
//! whose image is not in the map fails instead of pulling from a public registry.
//!
//! Keys, per image of the version: the reference as declared, its normalized spelling
//! (`ubuntu` = `docker.io/library/ubuntu:latest`), and the pinned source reference; the value is
//! the image tasks run ([`crate::images::ImageStatus::runnable`]: the derived image for a
//! Snakemake image, else the imported one). Every tenant image the run may name as-is — each
//! runnable image, and the worker image the engine gets as its global default — maps to itself,
//! so the default passes the strict lookup without a special case. `$default` names the worker
//! image, for people reading the map.
//!
//! An image built from a recipe has no reference of its own: tasks name it by its `images:` name
//! (`aws_batch_container_image="align"`, `container 'align'`), which is its only key. An imported
//! image may be named by its `images:` name too (`default` for `image:`), and a reference declared
//! with both a tag and a digest by its tag alone ([`ImageRef::map_keys`]). Those keys yield to any
//! image whose reference is spelled so. `biffix inspect` (`biffix_workflow::images::new_images`)
//! treats exactly these keys as declared.

use std::collections::BTreeMap;

use biffix_workflow::images::ImageRef;

use crate::images::ImageStatus;

/// The key naming the run's default (worker) image.
pub const DEFAULT_KEY: &str = "$default";

/// Where a run's map is written: beside its samplesheets, in the project bucket.
pub fn uri_for(project_bucket: &str, run_id: &str) -> String {
    format!(
        "s3://{project_bucket}/{}inputs/images.json",
        crate::runs::output_prefix::run_output_prefix(run_id)
    )
}

/// The map for a run of a version whose images are `images` (all ready) and whose tasks default
/// to `worker_image` (`None`: the engine has no default image).
pub(crate) fn build(
    images: &[ImageStatus],
    worker_image: Option<&str>,
) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    // An imported image's name and a reference's tag-only spelling, added last so they never
    // replace a key some image is spelled as.
    let mut yielding = Vec::new();
    for status in images {
        let Some(runnable) = status.runnable() else {
            continue;
        };
        let target = runnable.image.clone();
        if status.image.build.is_some() {
            map.insert(status.image.name.clone(), target.clone());
        } else {
            for reference in [&status.image.declared_ref, &status.image.source_ref] {
                map.insert(reference.clone(), target.clone());
                if let Ok(parsed) = ImageRef::parse(reference) {
                    let mut keys = parsed.map_keys().into_iter();
                    if let Some(own) = keys.next() {
                        map.insert(own, target.clone());
                    }
                    yielding.extend(keys.map(|k| (k, target.clone())));
                }
            }
            yielding.push((status.image.name.clone(), target.clone()));
        }
        map.insert(target.clone(), target);
    }
    if let Some(worker) = worker_image {
        map.insert(worker.to_string(), worker.to_string());
        map.insert(DEFAULT_KEY.to_string(), worker.to_string());
    }
    for (key, target) in yielding {
        map.entry(key).or_insert(target);
    }
    map
}

/// The map for a run given images at submit (`submit`, all ready) beside the version's: [`build`]
/// of the version's, then every key a submit-time image has, which wins over a version image
/// spelled the same (a moving tag re-pinned for this run). `$default` and the worker image's own
/// key stay the version's: a submit-time image never changes the run's default.
///
/// A submit-time image given with a digest (`ubuntu:24.04@sha256:…`, as a resumed or re-run run
/// re-sends its prior run's image) also takes its spelling without the digest (`ubuntu:24.04`, and
/// its normalized form) — the spelling the prior run's tasks name, which the digest pins for this
/// run. Those keys yield to any submit-time image spelled so, but win over the version's.
pub(crate) fn build_with_overrides(
    version: &[ImageStatus],
    submit: &[ImageStatus],
    worker_image: Option<&str>,
) -> BTreeMap<String, String> {
    let mut map = build(version, worker_image);
    let mut overrides = build(submit, None);
    for status in submit {
        let Some(runnable) = status.runnable() else {
            continue;
        };
        let Some((undigested, _)) = status.image.declared_ref.split_once('@') else {
            continue;
        };
        let Ok(parsed) = ImageRef::parse(undigested) else {
            continue;
        };
        for key in [undigested.to_string(), parsed.to_string()] {
            overrides.entry(key).or_insert_with(|| runnable.image.clone());
        }
    }
    map.extend(overrides);
    // Whatever a submit-time image is spelled as (the worker image itself, say), the run's default
    // stays the version's.
    if let Some(worker) = worker_image {
        map.insert(worker.to_string(), worker.to_string());
        map.insert(DEFAULT_KEY.to_string(), worker.to_string());
    }
    map
}

#[cfg(test)]
mod tests {
    use biffix_workflow::images::Capabilities;

    use super::*;
    use crate::images::build_follow::Phase;
    use crate::images::derived::DerivedStatus;
    use crate::store::registry::{ImageArtifact, VersionImage};

    const REPO: &str = "123.dkr.ecr.us-east-1.amazonaws.com/workers";

    fn artifact(key: &str, digest: &str) -> ImageArtifact {
        ImageArtifact {
            content_key: key.into(),
            source_ref: String::new(),
            image: format!("{REPO}@{digest}"),
            digest: digest.into(),
            archs: vec![],
            capabilities: Capabilities::default(),
            keep_tag: None,
            archived: true,
            archive_bytes: None,
            created_at: 0,
        }
    }

    fn status(name: &str, declared: &str, derived: Option<&str>) -> ImageStatus {
        ImageStatus {
            image: VersionImage {
                name: name.into(),
                declared_ref: declared.into(),
                source_ref: format!("{declared}@sha256:src-{name}"),
                content_key: name.into(),
                own_runtime: false,
                build: None,
            },
            phase: Phase::Ready,
            build: None,
            arch_builds: vec![],
            artifact: Some(artifact(name, &format!("sha256:imp-{name}"))),
            derived: derived.map(|d| DerivedStatus {
                key: "d".into(),
                engine_version: "9.27.0".into(),
                phase: Phase::Ready,
                waiting_for_runtime: false,
                build: None,
                artifact: Some(artifact("d", d)),
            }),
            problem: None,
        }
    }

    #[test]
    fn every_spelling_maps_to_the_image_tasks_run() {
        let images = [
            status("default", "ubuntu:24.04", Some("sha256:der-default")),
            status("fastqc", "quay.io/b/fastqc:1", None),
        ];
        let worker = format!("{REPO}@sha256:der-default");
        let map = build(&images, Some(&worker));
        let derived_default = format!("{REPO}@sha256:der-default");
        let fastqc = format!("{REPO}@sha256:imp-fastqc");
        assert_eq!(map["ubuntu:24.04"], derived_default);
        assert_eq!(map["docker.io/library/ubuntu:24.04"], derived_default);
        assert_eq!(map["ubuntu:24.04@sha256:src-default"], derived_default);
        assert_eq!(map["quay.io/b/fastqc:1"], fastqc);
        // Tenant images pass through as themselves (the engine's global default among them).
        assert_eq!(map[&fastqc], fastqc);
        assert_eq!(map[&worker], worker);
        assert_eq!(map[DEFAULT_KEY], worker);
        // The imported (not derived) copy of a derived image is not something tasks may run.
        assert!(!map.contains_key(&format!("{REPO}@sha256:imp-default")));
    }

    #[test]
    fn an_imported_image_is_also_named_by_its_images_name_and_a_pinned_tag_by_its_tag() {
        const D: &str = "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let mut bwa = status("bwa", &format!("quay.io/b/bwa:2@{D}"), None);
        bwa.image.source_ref = bwa.image.declared_ref.clone();
        // An image whose reference is spelled like another image's name: the reference wins.
        let named_like_a_name = status("tool", "fastqc", None);
        let images = [
            status("default", "ubuntu:24.04", Some("sha256:der-default")),
            status("fastqc", "quay.io/b/fastqc:1", None),
            bwa,
            named_like_a_name,
        ];
        let worker = format!("{REPO}@sha256:der-default");
        let map = build(&images, Some(&worker));
        let target = |name: &str| format!("{REPO}@sha256:imp-{name}");
        assert_eq!(map["default"], worker);
        assert_eq!(map["tool"], target("tool"));
        assert_eq!(map["fastqc"], target("tool"));
        assert_eq!(map["docker.io/library/fastqc:latest"], target("tool"));
        assert_eq!(map["quay.io/b/bwa:2"], target("bwa"));
        assert_eq!(map[&format!("quay.io/b/bwa:2@{D}")], target("bwa"));
        assert_eq!(map["bwa"], target("bwa"));
        // Every key `biffix inspect` treats as declared is a key here.
        let decl = |r: &str| biffix_workflow::images::ImageDecl {
            reference: Some(r.into()),
            ..Default::default()
        };
        let declared = BTreeMap::from([
            ("fastqc".to_string(), decl("quay.io/b/fastqc:1")),
            ("bwa".to_string(), decl(&format!("quay.io/b/bwa:2@{D}"))),
        ]);
        let found = [
            ("default", "a"),
            ("fastqc", "a"),
            ("bwa", "a"),
            ("quay.io/b/bwa:2", "a"),
            ("ubuntu:24.04", "a"),
        ];
        assert!(
            biffix_workflow::images::new_images(&declared, Some("ubuntu:24.04"), &found).is_empty()
        );
        for (reference, _) in found {
            let normalized = ImageRef::parse(reference).map(|r| r.to_string());
            assert!(
                map.contains_key(reference) || normalized.is_ok_and(|n| map.contains_key(&n)),
                "{reference}"
            );
        }
    }

    #[test]
    fn a_version_without_images_maps_only_its_worker_image() {
        let variant = "123.dkr.ecr.us-east-1.amazonaws.com/coord@sha256:variant";
        let map = build(&[], Some(variant));
        assert_eq!(map.len(), 2);
        assert_eq!(map[variant], variant);
        assert!(build(&[], None).is_empty());
        assert_eq!(uri_for("b", "run-1"), "s3://b/biffix/runs/run-1/inputs/images.json");
    }

    #[test]
    fn a_built_image_is_named_by_its_images_name_only() {
        let mut built = status("align", "align", None);
        built.image.source_ref = "dockerfile:docker/align/Dockerfile".into();
        built.image.build = Some(crate::store::registry::BuildRecipe {
            kind: "dockerfile".into(),
            context: "docker/align".into(),
            context_uri: "s3://b/workflow-sources/x/contexts/k.tar.gz".into(),
            context_sha256: "0".repeat(64),
            tree_hash: "t".into(),
            dockerfile: Some("Dockerfile".into()),
            dockerfile_text: None,
            archs: vec!["amd64".into()],
        });
        let map = build(&[built], None);
        let target = format!("{REPO}@sha256:imp-align");
        assert_eq!(
            map,
            BTreeMap::from([("align".to_string(), target.clone()), (target.clone(), target)])
        );
    }

    #[test]
    fn a_submit_time_image_wins_every_spelling_it_has_but_not_the_default() {
        const D: &str = "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let version = [
            status("default", "ubuntu:24.04", Some("sha256:der-default")),
            status("bwa", &format!("quay.io/b/bwa:2@{D}"), None),
            status("fastqc", "quay.io/b/fastqc:1", None),
        ];
        let worker = format!("{REPO}@sha256:der-default");
        // Given at submit: the version's default spelling, bwa's tag, and a new image, each as
        // submit records it (the spelling as given, the reference as pinned).
        let given = |spelling: &str, key: &str, digest: &str| {
            let reference = ImageRef::parse(spelling).unwrap();
            let pinned = reference.pinned(&format!("sha256:{}", "f".repeat(64)));
            let mut status = status(key, spelling, None);
            status.image =
                crate::runs::launch::as_version_image(&biffix_core::event::SubmitImage {
                    declared_ref: spelling.into(),
                    source_ref: pinned.to_string(),
                    content_key: key.into(),
                });
            status.artifact = Some(artifact(key, digest));
            status
        };
        let ubuntu = given("ubuntu:24.04", "u", "sha256:new-ubuntu");
        let bwa = given("quay.io/b/bwa:2", "b", "sha256:new-bwa");
        let extra = given("quay.io/b/samtools:1", "s", "sha256:imp-quay.io/b/samtools:1");
        let map = build_with_overrides(&version, &[ubuntu, bwa, extra], Some(&worker));
        let new_ubuntu = format!("{REPO}@sha256:new-ubuntu");
        let new_bwa = format!("{REPO}@sha256:new-bwa");
        // Declared and normalized spellings, and a tag-only key another image yielded.
        assert_eq!(map["ubuntu:24.04"], new_ubuntu);
        assert_eq!(map["docker.io/library/ubuntu:24.04"], new_ubuntu);
        assert_eq!(map["quay.io/b/bwa:2"], new_bwa);
        assert_eq!(map[&format!("quay.io/b/bwa:2@{D}")], format!("{REPO}@sha256:imp-bwa"));
        assert_eq!(map["quay.io/b/samtools:1"], format!("{REPO}@sha256:imp-quay.io/b/samtools:1"));
        assert_eq!(map[&new_bwa], new_bwa);
        // The version's own names and the default are untouched.
        assert_eq!(map["default"], worker);
        assert_eq!(map["fastqc"], format!("{REPO}@sha256:imp-fastqc"));
        assert_eq!(map[DEFAULT_KEY], worker);
        assert_eq!(map[&worker], worker);
        // Nothing given at submit: the version's map.
        assert_eq!(
            build_with_overrides(&version, &[], Some(&worker)),
            build(&version, Some(&worker))
        );
        // A submit-time image spelled as the worker image never replaces the run's default.
        let mut as_worker = status("x", "x", Some("sha256:rederived"));
        as_worker.image = crate::runs::launch::as_version_image(&biffix_core::event::SubmitImage {
            declared_ref: worker.clone(),
            source_ref: worker.clone(),
            content_key: "w".into(),
        });
        let map = build_with_overrides(&version, &[as_worker], Some(&worker));
        assert_eq!(map[&worker], worker);
        assert_eq!(map[DEFAULT_KEY], worker);
    }

    #[test]
    fn a_submit_time_image_given_with_a_digest_also_takes_its_spelling_without_it() {
        const D: &str = "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let version = [
            status("default", "ubuntu:24.04", Some("sha256:der-default")),
            status("fastqc", "quay.io/b/fastqc:1", None),
        ];
        let worker = format!("{REPO}@sha256:der-default");
        let given = |spelling: &str, key: &str, digest: &str| {
            let mut status = status(key, spelling, None);
            status.image =
                crate::runs::launch::as_version_image(&biffix_core::event::SubmitImage {
                    declared_ref: spelling.into(),
                    source_ref: ImageRef::parse(spelling).unwrap().to_string(),
                    content_key: key.into(),
                });
            status.artifact = Some(artifact(key, digest));
            status
        };
        // A resumed run re-sends its prior run's `ubuntu:24.04` and `quay.io/b/fastqc` (no tag),
        // each with the digest it ran.
        let ubuntu = given(&format!("ubuntu:24.04@{D}"), "u", "sha256:prior-ubuntu");
        let fastqc = given(&format!("quay.io/b/fastqc@{D}"), "f", "sha256:prior-fastqc");
        // Given as spelled, the digest-less key yields to it.
        let fresh = given("quay.io/b/fastqc:1", "g", "sha256:fresh-fastqc");
        let map = build_with_overrides(&version, &[ubuntu, fastqc, fresh], Some(&worker));
        let prior_ubuntu = format!("{REPO}@sha256:prior-ubuntu");
        let prior_fastqc = format!("{REPO}@sha256:prior-fastqc");
        assert_eq!(map[&format!("ubuntu:24.04@{D}")], prior_ubuntu);
        assert_eq!(map["ubuntu:24.04"], prior_ubuntu);
        assert_eq!(map["docker.io/library/ubuntu:24.04"], prior_ubuntu);
        assert_eq!(map["quay.io/b/fastqc"], prior_fastqc);
        assert_eq!(map["quay.io/b/fastqc:latest"], prior_fastqc);
        assert_eq!(map["quay.io/b/fastqc:1"], format!("{REPO}@sha256:fresh-fastqc"));
        // The run's default is still the version's.
        assert_eq!(map["default"], worker);
        assert_eq!(map[DEFAULT_KEY], worker);
    }
}
