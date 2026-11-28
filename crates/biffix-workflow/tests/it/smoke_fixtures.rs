//! The single-job acceptance fixtures (`dev/smoke/single-job`): their `biffix.yaml`s parse and
//! validate as registration reads them, and declare what the acceptance runs rely on.

use std::path::{Path, PathBuf};

use biffix_workflow::engines::Engine;
use biffix_workflow::images::{declared_images, BuildKind, ImageSource};
use biffix_workflow::manifest::{BiffixManifest, EngineChoice};

/// The fixtures' root: `dev/smoke/single-job` at the repository root.
fn smoke() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../dev/smoke/single-job")
}

fn manifest(engine: &str) -> BiffixManifest {
    let path = smoke().join(engine).join("biffix.yaml");
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    BiffixManifest::parse_yaml(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

#[test]
fn each_fixture_manifest_parses_with_no_unknown_keys_and_pins_its_engine() {
    for (dir, engine, version) in
        [("snakemake", Engine::Snakemake, "9.27.2"), ("nextflow", Engine::Nextflow, "25.10.4")]
    {
        let m = manifest(dir);
        assert!(m.unknown_keys().is_empty(), "{dir}: {:?}", m.unknown_keys());
        assert_eq!(m.engine, Some(EngineChoice::Engine(engine)), "{dir}");
        assert_eq!(m.engine_version.as_deref(), Some(version), "{dir}");
        // Single-job runs are allowed, at the queue-class's smallest type.
        assert!(m.single_job.is_some(), "{dir}");
        assert_eq!(m.params["input_uri"].format.as_deref(), Some("s3-file"), "{dir}");
    }
}

/// Three distinct images, each a build of the fixture's one Dockerfile told apart only by its
/// `ACCEPTANCE_IMAGE` build arg, so they share every base layer.
#[test]
fn each_fixture_builds_three_images_from_its_one_dockerfile() {
    for dir in ["snakemake", "nextflow"] {
        let m = manifest(dir);
        let images = declared_images(&m.images, None).unwrap_or_else(|e| panic!("{dir}: {e}"));
        let names: Vec<_> = images.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, ["first", "second", "third"], "{dir}");
        for image in &images {
            let ImageSource::Build(build) = &image.source else {
                panic!("{dir}: `{}` is not built", image.name);
            };
            assert_eq!(build.kind, BuildKind::Dockerfile, "{dir}");
            assert_eq!(
                (build.context.as_str(), build.dockerfile.as_deref()),
                ("images", Some("Dockerfile")),
                "{dir}: `{}`",
                image.name
            );
            assert_eq!(build.archs, None, "{dir}: built for both architectures");
            let args: Vec<_> = build.options.build_args.iter().collect();
            assert_eq!(args, [(&"ACCEPTANCE_IMAGE".to_string(), &image.name)], "{dir}");
            assert!(!image.own_runtime, "{dir}: Snakemake's runtime is added to each image");
        }
    }
}
