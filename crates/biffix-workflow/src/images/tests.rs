use std::collections::BTreeMap;

use sha2::{Digest, Sha256};

use super::*;

const D: &str = "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn parse(s: &str) -> ImageRef {
    ImageRef::parse(s).unwrap_or_else(|e| panic!("{s}: {e}"))
}

#[test]
fn references_parse_like_docker() {
    let r = parse("quay.io/biocontainers/fastqc:0.12.1--hdfd78af_0");
    assert_eq!(
        (r.registry.as_str(), r.repository.as_str(), r.tag.as_deref()),
        ("quay.io", "biocontainers/fastqc", Some("0.12.1--hdfd78af_0"))
    );
    let hub = parse("ubuntu");
    assert_eq!(hub.to_string(), "docker.io/library/ubuntu:latest");
    assert_eq!(parse("biocontainers/samtools:1.21").name(), "docker.io/biocontainers/samtools");
    assert_eq!(parse("registry-1.docker.io/library/ubuntu:24.04").registry, "docker.io");
    let port = parse("localhost:5000/coord:base");
    assert_eq!((port.registry.as_str(), port.tag.as_deref()), ("localhost:5000", Some("base")));
    let pinned = parse(&format!("ghcr.io/org/tool:1.0@{D}"));
    assert_eq!(pinned.digest.as_deref(), Some(D));
    assert_eq!(pinned.to_string(), format!("ghcr.io/org/tool:1.0@{D}"));
    let digest_only = parse(&format!("quay.io/x/y@{D}"));
    assert_eq!(digest_only.tag, None);
    assert!(parse("123456789012.dkr.ecr.us-east-1.amazonaws.com/repo:t").is_private_ecr());
    assert!(!parse("public.ecr.aws/docker/library/ubuntu:24.04").is_private_ecr());
    assert!(parse("123456789012.dkr.ecr.cn-north-1.amazonaws.com.cn/repo:t").is_private_ecr());
    assert!(!parse("123456789012.dkr.ecr.us-east-1.amazonaws.com.evil.io/repo:t").is_private_ecr());
    // A registry host is case-insensitive: it is lowercased, so a mixed-case spelling is the
    // same image (and the same private ECR).
    let mixed = parse("123456789012.DKR.ECR.us-east-1.AmazonAWS.com/repo:t");
    assert!(mixed.is_private_ecr());
    assert_eq!(mixed, parse("123456789012.dkr.ecr.us-east-1.amazonaws.com/repo:t"));
    assert_eq!(
        parse("Quay.IO/biocontainers/fastqc:1").to_string(),
        "quay.io/biocontainers/fastqc:1"
    );
    assert_eq!(parse("Index.Docker.io/ubuntu").name(), "docker.io/library/ubuntu");
    // What Docker's grammar allows: separators inside a component, tags with case and `_`
    // first, 128-character tags, IPv6 and port registries.
    for ok in [
        "quay.io/a.b/c__d/e---f/g_h:V1_x.2-3",
        "x:_tag",
        &format!("x:{}", "a".repeat(128)),
        "[::1]:5000/x:1",
        "my-registry.local:5000/team/tool",
    ] {
        parse(ok);
    }
}

#[test]
fn bad_references_are_rejected() {
    use ImageRefError::*;
    assert_eq!(ImageRef::parse(" "), Err(Empty));
    let long_name = format!("quay.io/{}", "a".repeat(250));
    let long_tag = format!("x:{}", "a".repeat(129));
    let digest = D.trim_start_matches("sha256:");
    let digest_alone = format!("@{D}");
    type Variant = fn(String) -> ImageRefError;
    let cases: [(&str, Variant); 29] = [
        ("https://quay.io/x", Malformed),
        ("docker://ubuntu:24.04", Malformed),
        ("a b", Malformed),
        ("x@sha256:12", BadDigest),
        ("x@md5:0123", BadDigest),
        (digest, DigestWithoutName),
        (&digest_alone, DigestWithoutName),
        ("x:", BadTag),
        ("foo:-bad", BadTag),
        ("foo:.x", BadTag),
        ("foo:a+b", BadTag),
        (&long_tag, BadTag),
        ("Quay.io/X/Y", BadRepository),
        ("Ubuntu", BadRepository),
        (":tag", BadRepository),
        ("/foo", BadRepository),
        ("foo//bar", BadRepository),
        ("quay.io//x:1", BadRepository),
        ("quay.io/x/:1", BadRepository),
        ("quay.io/", BadRepository),
        ("x/", BadRepository),
        ("-x", BadRepository),
        ("a..b", BadRepository),
        ("a___b", BadRepository),
        ("quay.io/x/../y:1", BadRepository),
        ("./x:1", BadRegistry),
        ("evil.com;$(id)/x:1", BadRegistry),
        ("host:port/x", BadRegistry),
        ("-quay.io/x", BadRegistry),
    ];
    for (bad, error) in cases {
        assert_eq!(ImageRef::parse(bad), Err(error(bad.to_string())), "{bad}");
    }
    assert_eq!(ImageRef::parse(&long_name), Err(NameTooLong(long_name.clone())));
    // The limit is on the normalized name, as Docker applies it: 240 characters as typed are 258
    // as `docker.io/library/…`, and an `index.docker.io/` name 258 as typed is 252 as `docker.io/`.
    let bare = "a".repeat(240);
    assert_eq!(ImageRef::parse(&bare), Err(NameTooLong(bare.clone())));
    let index = format!("index.docker.io/t/{}", "a".repeat(240));
    assert_eq!(parse(&index).name().len(), 252);
    // `sha256:<hex>` reads as Docker reads it (the CLI's worker-repository spelling relies on
    // it passing).
    assert_eq!(parse(D).name(), "docker.io/library/sha256");
}

#[test]
fn keys_depend_only_on_the_digest() {
    assert_eq!(content_key(D), content_key(D));
    assert_ne!(content_key(D), content_key(&D.replace('0', "1")));
    assert_eq!(subject("k"), "image:k");
}

#[test]
fn a_derived_key_names_the_imported_digest_and_the_runtime() {
    let k = derived_key(D, "rt1");
    assert_eq!(k, derived_key(D, "rt1"));
    assert_eq!(k.len(), 64);
    assert_ne!(k, derived_key(D, "rt2"));
    assert_ne!(k, derived_key(&D.replace('0', "1"), "rt1"));
    assert_ne!(k, content_key(D));
    assert_eq!(derived_subject("k"), "image:derived:k");
}

#[test]
fn capabilities_parse_the_build_output() {
    let c = Capabilities::parse(
        "bash=1,libc=glibc,snakemake=9.3.0,storage_s3=0.3.6,logger=,executor=3.0.0,extra=x",
    );
    assert!(c.bash);
    assert_eq!(c.libc, "glibc");
    assert_eq!(c.snakemake.as_deref(), Some("9.3.0"));
    assert_eq!(c.storage_s3.as_deref(), Some("0.3.6"));
    assert_eq!((c.logger, c.executor.as_deref()), (None, Some("3.0.0")));
    let bare = Capabilities::parse("bash=0,snakemake=");
    assert_eq!((bare.bash, bare.libc.as_str(), bare.snakemake), (false, "none", None));
    // An empty libc is `none` too, not an empty string.
    assert_eq!(Capabilities::parse("bash=1,libc=").libc, "none");
    // Plugin versions an image does not have are absent.
    let old: Capabilities =
        serde_json::from_str(r#"{"bash":true,"libc":"glibc","snakemake":"9.27.0"}"#).unwrap();
    assert_eq!((old.storage_s3, old.executor), (None, None));
}

fn decl(reference: Option<&str>, dockerfile: Option<&str>) -> ImageDecl {
    ImageDecl {
        reference: reference.map(str::to_string),
        dockerfile: dockerfile.map(str::to_string),
        ..ImageDecl::default()
    }
}

#[test]
fn a_manifest_declares_images_by_name() {
    let yaml = "engine: nextflow\nimage: quay.io/b/base:1\nimages:\n  fastqc:\n    ref: quay.io/b/fastqc:0.12.1\n  sm:\n    ref: ghcr.io/o/sm:9\n    runtime: own\n";
    let manifest: crate::manifest::BiffixManifest = serde_norway::from_str(yaml).unwrap();
    let images = manifest.declared_images().unwrap();
    let names: Vec<&str> = images.iter().map(|i| i.name.as_str()).collect();
    assert_eq!(names, ["default", "fastqc", "sm"]);
    assert!(images[2].own_runtime);
    // An unknown key in an entry is an error, not silently ignored.
    let typo = "images:\n  x:\n    reff: a:1\n";
    assert!(serde_norway::from_str::<crate::manifest::BiffixManifest>(typo).is_err());
}

#[test]
fn auto_names_are_readable_and_valid() {
    assert_eq!(
        auto_name(&parse("quay.io/biocontainers/fastqc:0.12.1--hdfd78af_0")),
        "fastqc-0-12-1-hdfd78af-0"
    );
    assert_eq!(auto_name(&parse("ubuntu")), "ubuntu-latest");
    assert_eq!(auto_name(&parse(&format!("quay.io/x/tool_a@{D}"))), "tool-a-0123456789ab");
}

#[test]
fn only_undeclared_references_are_added_under_free_names() {
    let images = BTreeMap::from([
        ("bwa".to_string(), decl(Some("quay.io/b/bwa:1"), None)),
        ("fastqc-1".to_string(), decl(Some("quay.io/other/x:9"), None)),
    ]);
    let found = [
        // The default image, spelled differently: declared.
        ("docker.io/library/ubuntu:24.04", "Snakefile:1"),
        // A declared named image: declared.
        ("quay.io/b/bwa:1", "Snakefile:2"),
        // New, twice, and a second one whose name is taken.
        ("quay.io/b/fastqc:1", "Snakefile:3"),
        ("quay.io/b/fastqc:1", "Snakefile:4"),
        ("ghcr.io/o/fastqc:1", "Snakefile:5"),
        ("not a ref", "Snakefile:6"),
    ];
    let added = new_images(&images, Some("ubuntu:24.04"), &found);
    let got: Vec<(&str, &str, &str)> = added
        .iter()
        .map(|n| (n.name.as_str(), n.reference.as_str(), n.location.as_str()))
        .collect();
    assert_eq!(
        got,
        [
            ("fastqc-1-2", "quay.io/b/fastqc:1", "Snakefile:3"),
            ("fastqc-1-3", "ghcr.io/o/fastqc:1", "Snakefile:5"),
        ]
    );
}

#[test]
fn declared_images_merge_the_default_and_validate_each_entry() {
    let images = BTreeMap::from([("fastqc".to_string(), decl(Some("quay.io/b/fastqc:1"), None))]);
    let got = declared_images(&images, Some("ubuntu:24.04")).unwrap();
    let names: Vec<&str> = got.iter().map(|i| i.name.as_str()).collect();
    assert_eq!(names, ["default", "fastqc"]);
    assert_eq!(got[0].reference().unwrap().to_string(), "docker.io/library/ubuntu:24.04");

    let with_default = BTreeMap::from([("default".to_string(), decl(Some("x:1"), None))]);
    assert_eq!(declared_images(&with_default, Some("y:1")), Err(ImageDeclError::DuplicateDefault));
    let both = BTreeMap::from([("x".to_string(), decl(Some("a:1"), Some("D")))]);
    assert_eq!(declared_images(&both, None), Err(ImageDeclError::NotExactlyOne("x".into())));
    let bad_name = BTreeMap::from([("Big_Name".to_string(), decl(Some("a:1"), None))]);
    let error = declared_images(&bad_name, None).unwrap_err();
    assert_eq!(error, ImageDeclError::BadName("Big_Name".into()));
    assert_eq!(
        error.to_string(),
        "image name `Big_Name` is invalid: an image name uses only lowercase letters, digits \
             and `-`"
    );
    // A blank `image:` declares nothing.
    assert_eq!(declared_images(&BTreeMap::new(), Some("   ")), Ok(vec![]));
    let bad_ref = BTreeMap::from([("x".to_string(), decl(Some("quay.io//x:1"), None))]);
    assert_eq!(
        declared_images(&bad_ref, None),
        Err(ImageDeclError::BadRef {
            name: "x".into(),
            error: ImageRefError::BadRepository("quay.io//x:1".into()),
        })
    );
    let bad_runtime = BTreeMap::from([(
        "sm".to_string(),
        ImageDecl { runtime: Some("mine".into()), ..decl(Some("a:1"), None) },
    )]);
    assert_eq!(declared_images(&bad_runtime, None), Err(ImageDeclError::BadRuntime("sm".into())));
    let ref_with_archs = BTreeMap::from([(
        "x".to_string(),
        ImageDecl { archs: Some(vec!["amd64".into()]), ..decl(Some("a:1"), None) },
    )]);
    assert_eq!(
        declared_images(&ref_with_archs, None),
        Err(ImageDeclError::ArchsWithoutDockerfile("x".into()))
    );
    // A `ref` is pulled, not built: the message says nothing of pixi.
    assert_eq!(
        ImageDeclError::ArchsWithoutDockerfile("x".into()).to_string(),
        "image `x`: `archs` goes with `dockerfile`"
    );
    let own = BTreeMap::from([(
        "sm".to_string(),
        ImageDecl { runtime: Some("own".into()), ..decl(Some("a:1"), None) },
    )]);
    assert!(declared_images(&own, None).unwrap()[0].own_runtime);
}

fn build_of(entry: ImageDecl) -> Result<BuildSpec, ImageDeclError> {
    let images = BTreeMap::from([("img".to_string(), entry)]);
    declared_images(&images, None).map(|mut d| match d.remove(0).source {
        ImageSource::Build(spec) => spec,
        ImageSource::Ref(r) => panic!("not a build: {r}"),
    })
}

#[test]
fn a_dockerfile_builds_in_its_directory_unless_a_context_is_given() {
    assert_eq!(
        build_of(decl(None, Some("./docker/align/Dockerfile"))),
        Ok(BuildSpec {
            kind: BuildKind::Dockerfile,
            context: "docker/align".into(),
            dockerfile: Some("Dockerfile".into()),
            archs: None,
        })
    );
    assert_eq!(
        build_of(ImageDecl {
            context: Some(".".into()),
            archs: Some(vec!["amd64".into()]),
            ..decl(None, Some("docker/Dockerfile"))
        }),
        Ok(BuildSpec {
            kind: BuildKind::Dockerfile,
            context: "".into(),
            dockerfile: Some("docker/Dockerfile".into()),
            archs: Some(vec![Arch::Amd64]),
        })
    );
    assert_eq!(build_of(decl(None, Some("Dockerfile"))).unwrap().context, "");
}

#[test]
fn a_pixi_environment_builds_from_its_directory() {
    assert_eq!(
        build_of(ImageDecl { pixi: Some("envs/worker/".into()), ..ImageDecl::default() }),
        Ok(BuildSpec {
            kind: BuildKind::Pixi,
            context: "envs/worker".into(),
            dockerfile: None,
            archs: None,
        })
    );
}

#[test]
fn build_entries_are_validated() {
    let err = |entry: ImageDecl| build_of(entry).unwrap_err();
    let img = |path: &str| ImageDeclError::BadPath { name: "img".into(), path: path.into() };
    for path in ["../Dockerfile", "/abs/Dockerfile", "docker\\Dockerfile"] {
        assert_eq!(err(decl(None, Some(path))), img(path));
    }
    assert_eq!(
        img("docker\\Dockerfile").to_string(),
        "image `img`: `docker\\Dockerfile` must be a relative `/`-separated path inside the \
         workflow (no leading `/`, no `\\`, no `..`)"
    );
    assert!(matches!(
        err(ImageDecl { context: Some("other".into()), ..decl(None, Some("docker/Dockerfile")) }),
        ImageDeclError::DockerfileOutsideContext { .. }
    ));
    assert!(matches!(
        err(ImageDecl { archs: Some(vec!["riscv".into()]), ..decl(None, Some("Dockerfile")) }),
        ImageDeclError::BadArch { .. }
    ));
    assert_eq!(
        err(ImageDecl {
            pixi: Some("env".into()),
            archs: Some(vec!["amd64".into()]),
            ..ImageDecl::default()
        }),
        ImageDeclError::ArchsWithPixi("img".into())
    );
    let images = BTreeMap::from([(
        "x".to_string(),
        ImageDecl { context: Some("c".into()), ..decl(Some("a:1"), None) },
    )]);
    assert_eq!(
        declared_images(&images, None),
        Err(ImageDeclError::ContextWithoutDockerfile("x".into()))
    );
    assert_eq!(
        err(ImageDecl {
            pixi: Some("env".into()),
            context: Some("c".into()),
            ..ImageDecl::default()
        }),
        ImageDeclError::ContextWithoutDockerfile("img".into())
    );
    assert_eq!(
        err(ImageDecl { archs: Some(vec![]), ..decl(None, Some("Dockerfile")) }),
        ImageDeclError::NoArchs("img".into())
    );
    assert_eq!(
        ImageDeclError::NoArchs("img".into()).to_string(),
        "image `img`: `archs` is empty; list `amd64` and/or `arm64`, or leave it out"
    );
    // A Dockerfile path that cleans to nothing names no Dockerfile.
    for path in [".", "./", ""] {
        assert_eq!(err(decl(None, Some(path))), ImageDeclError::NoDockerfile("img".into()));
    }
    assert_eq!(
        err(ImageDecl { pixi: Some("../env".into()), ..ImageDecl::default() }),
        img("../env")
    );
}

#[test]
fn a_build_key_changes_with_everything_the_image_is_built_from() {
    let base = build_content_key(BuildKind::Dockerfile, "t", Some("Dockerfile"), &Arch::ALL);
    // Arch order and repeats do not matter.
    assert_eq!(
        base,
        build_content_key(
            BuildKind::Dockerfile,
            "t",
            Some("Dockerfile"),
            &[Arch::Arm64, Arch::Amd64, Arch::Arm64]
        )
    );
    for other in [
        build_content_key(BuildKind::Pixi, "t", Some("Dockerfile"), &Arch::ALL),
        // Another tarball (other files, or the same files with another mode) is another build.
        build_content_key(BuildKind::Dockerfile, "u", Some("Dockerfile"), &Arch::ALL),
        build_content_key(BuildKind::Dockerfile, "t", Some("a/Dockerfile"), &Arch::ALL),
        build_content_key(BuildKind::Dockerfile, "t", Some("Dockerfile"), &[Arch::Amd64]),
    ] {
        assert_ne!(base, other);
    }
    assert_eq!(build_subject("k", Some(Arch::Arm64)), "image:build:k:arm64");
    assert_eq!(build_subject("k", None), "image:build:k");
}

#[test]
fn a_pixi_key_hashes_the_generated_dockerfile_and_a_build_key_names_its_tarball() {
    let sha = |s: &str| hex::encode(Sha256::digest(s));
    // A pixi key covers the generated text (its pinned images), not only the generator version.
    let recipe = sha(&crate::pixi::dockerfile());
    assert_eq!(
        build_content_key(BuildKind::Pixi, "t", None, &Arch::ALL),
        sha(&format!(
            "build2:pixi:{}:{recipe}:t::amd64,arm64",
            crate::pixi::PIXI_GENERATOR_VERSION
        ))
    );
    // The `build2:` scheme: never the key a claimed tree hash `t` was once built under.
    assert_eq!(
        build_content_key(BuildKind::Dockerfile, "t", Some("Dockerfile"), &Arch::ALL),
        sha("build2:dockerfile:1:t:Dockerfile:amd64,arm64")
    );
    assert_ne!(
        build_content_key(BuildKind::Dockerfile, "t", Some("Dockerfile"), &Arch::ALL),
        sha("build:dockerfile:1:t:Dockerfile:amd64,arm64")
    );
}

#[test]
fn a_build_kind_reads_back_its_name() {
    for kind in [BuildKind::Dockerfile, BuildKind::Pixi] {
        assert_eq!(kind.as_str().parse::<BuildKind>(), Ok(kind));
    }
    assert_eq!("Docker".parse::<BuildKind>(), Err(UnknownBuildKind("Docker".into())));
}

/// The build scripts and the restore tool spell the keep-tag prefix this crate names.
#[test]
fn the_keep_tag_prefix_is_the_scripts_prefix() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let read = |rel: &str| std::fs::read_to_string(root.join(rel)).unwrap();
    assert!(
        read("codebuild/lib/common.sh").contains(&format!("\nKEEP_TAG_PREFIX={KEEP_TAG_PREFIX}\n"))
    );
    assert!(read("infra/biffix_infra/image_restore.py")
        .contains(&format!("\nKEEP_TAG_PREFIX = \"{KEEP_TAG_PREFIX}\"\n")));
}

#[test]
fn a_reference_that_names_a_declared_image_is_not_new() {
    let images =
        BTreeMap::from([("align".to_string(), decl(None, Some("docker/align/Dockerfile")))]);
    let found = [("align", "Snakefile:3"), ("quay.io/b/x:1", "Snakefile:4")];
    let added = new_images(&images, None, &found);
    assert_eq!(added.len(), 1);
    assert_eq!(added[0].reference, "quay.io/b/x:1");
}

#[test]
fn a_reference_is_declared_exactly_when_the_image_map_keys_it() {
    let images = BTreeMap::from([
        ("fastqc".to_string(), decl(Some("quay.io/b/fastqc:1"), None)),
        ("bwa".to_string(), decl(Some(&format!("quay.io/b/bwa:2@{D}")), None)),
    ]);
    let other = D.replace('0', "1");
    let bwa_other_digest = format!("quay.io/b/bwa:2@{other}");
    let bwa_digest_only = format!("quay.io/b/bwa@{D}");
    let found = [
        // An imported image's `images:` name, and `default` for `image:`.
        ("fastqc", "a:1"),
        ("default", "a:2"),
        // The tag alone of a reference pinned with a tag and a digest.
        ("quay.io/b/bwa:2", "a:3"),
        // Not keys: another digest of that tag, the digest without the tag, another tag.
        (bwa_other_digest.as_str(), "a:4"),
        (bwa_digest_only.as_str(), "a:5"),
        ("quay.io/b/bwa:3", "a:6"),
    ];
    let added = new_images(&images, Some("ubuntu:24.04"), &found);
    let got: Vec<&str> = added.iter().map(|n| n.reference.as_str()).collect();
    assert_eq!(got, [bwa_other_digest.as_str(), bwa_digest_only.as_str(), "quay.io/b/bwa:3"]);
    // Without `image:`, nothing is named `default`: a task naming it is not declared.
    let added = new_images(&BTreeMap::new(), None, &[("default", "a:1")]);
    assert_eq!(added[0].reference, "default");
    // A new tag-and-digest reference also declares its tag alone.
    let pinned = format!("quay.io/n/new:1@{D}");
    let found = [(pinned.as_str(), "a:1"), ("quay.io/n/new:1", "a:2")];
    let added = new_images(&BTreeMap::new(), None, &found);
    let got: Vec<&str> = added.iter().map(|n| n.reference.as_str()).collect();
    assert_eq!(got, [pinned.as_str()]);
}

#[test]
fn map_keys_add_the_tag_alone_of_a_pinned_tag() {
    assert_eq!(
        parse(&format!("quay.io/b/bwa:2@{D}")).map_keys(),
        [format!("quay.io/b/bwa:2@{D}"), "quay.io/b/bwa:2".into()]
    );
    assert_eq!(parse("ubuntu").map_keys(), ["docker.io/library/ubuntu:latest"]);
    assert_eq!(parse(&format!("quay.io/b/bwa@{D}")).map_keys(), [format!("quay.io/b/bwa@{D}")]);
}

#[test]
fn a_private_ecr_image_has_the_coordinates_its_host_and_repository_name() {
    let coords = parse("123456789012.dkr.ecr.us-east-1.amazonaws.com/team/workflow:v1.2.3-rc_1")
        .ecr_coords()
        .unwrap();
    assert_eq!(
        coords,
        EcrCoords {
            registry_id: "123456789012".into(),
            region: "us-east-1".into(),
            repository: "team/workflow".into(),
        }
    );
    let china =
        parse(&format!("123456789012.dkr.ecr.cn-north-1.amazonaws.com.cn/team/workflow@{D}"));
    assert_eq!(china.ecr_coords().unwrap().region, "cn-north-1");
    assert_eq!(
        parse("123456789012.dkr.ecr.us-gov-west-1.amazonaws.com/wf:v1")
            .ecr_coords()
            .unwrap()
            .region,
        "us-gov-west-1"
    );
}

#[test]
fn ecr_coordinates_follow_ecr_s_rules() {
    for (reference, error) in [
        ("quay.io/biocontainers/fastqc:1", EcrRefError::NotPrivateEcr),
        ("ubuntu:latest", EcrRefError::NotPrivateEcr),
        ("123456789012.dkr.ecr.us-east-1.amazonaws.com.evil.io/wf:v1", EcrRefError::NotPrivateEcr),
        (
            "123.dkr.ecr.us-east-1.amazonaws.com/workflow:v1",
            EcrRefError::InvalidRegistryId("123".into()),
        ),
        (
            "123456789012.dkr.ecr.foo-bar.amazonaws.com/workflow:v1",
            EcrRefError::InvalidRegion("foo-bar".into()),
        ),
        (
            "123456789012.dkr.ecr.us-east-1.amazonaws.com.cn/workflow:v1",
            EcrRefError::InvalidRegion("us-east-1".into()),
        ),
        (
            "123456789012.dkr.ecr.cn-north-1.amazonaws.com/workflow:v1",
            EcrRefError::InvalidRegion("cn-north-1".into()),
        ),
        // Docker takes `--` and `__` between alphanumerics and a one-letter name; ECR does not.
        (
            "123456789012.dkr.ecr.us-east-1.amazonaws.com/workflow--bad:v1",
            EcrRefError::InvalidRepository("workflow--bad".into()),
        ),
        (
            "123456789012.dkr.ecr.us-east-1.amazonaws.com/a__b:v1",
            EcrRefError::InvalidRepository("a__b".into()),
        ),
        (
            "123456789012.dkr.ecr.us-east-1.amazonaws.com/w:v1",
            EcrRefError::InvalidRepository("w".into()),
        ),
    ] {
        assert_eq!(parse(reference).ecr_coords(), Err(error), "{reference}");
    }
    // The message names the part it rejected.
    let message = parse("123.dkr.ecr.us-east-1.amazonaws.com/workflow:v1")
        .ecr_coords()
        .unwrap_err()
        .to_string();
    assert!(message.contains("`123`"), "{message}");
}

#[test]
fn ecr_coords_of_a_repository_uri() {
    let coords =
        ecr_coords_from_repo_uri("123456789012.dkr.ecr.us-east-1.amazonaws.com/team/workflow")
            .unwrap();
    assert_eq!(coords.registry_id, "123456789012");
    assert_eq!(coords.region, "us-east-1");
    assert_eq!(coords.repository, "team/workflow");
    assert!(ecr_coords_from_repo_uri("ubuntu").is_none());
    // A repository URI names no tag.
    assert!(
        ecr_coords_from_repo_uri("123456789012.dkr.ecr.us-east-1.amazonaws.com/wf:v1").is_none()
    );
}
