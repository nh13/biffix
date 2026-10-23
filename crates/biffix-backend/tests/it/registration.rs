//! Registration-path tests — pure, no AWS.

use biffix_backend::register::version::{
    freeze_version, inputs_hash, RegisterError, RegisterInput,
};
use biffix_backend::register::workflow::WorkflowSource;
use biffix_backend::store::registry::SourceBuild;
use biffix_backend::testing;
use biffix_workflow::digest::ImageDigestError;
use biffix_workflow::engines::Engine;
use biffix_workflow::formschema::{from_snakemake_config_ordered, SchemaOrder, WidgetHint};
use biffix_workflow::manifest::{detect_engine, BiffixManifest, EngineDetectError};
use biffix_workflow::version::content_hash;

// ── engine detection ──────────────────────────────────────────────────────────

#[test]
fn detects_snakemake_from_files() {
    let m = BiffixManifest::default();
    let files = vec!["workflow/Snakefile".to_string(), "README.md".to_string()];
    assert_eq!(detect_engine(&m, &files), Ok(Engine::Snakemake));
}

#[test]
fn detects_nextflow_from_files() {
    let m = BiffixManifest::default();
    let files = vec!["main.nf".to_string()];
    assert_eq!(detect_engine(&m, &files), Ok(Engine::Nextflow));
}

#[test]
fn manifest_overrides_detection() {
    let m = BiffixManifest::parse_yaml("engine: snakemake").unwrap();
    // With no Snakefile present, and even beside a file that says Nextflow, the manifest wins.
    assert_eq!(detect_engine(&m, &[]), Ok(Engine::Snakemake));
    assert_eq!(detect_engine(&m, &["main.nf".to_string()]), Ok(Engine::Snakemake));
    // `auto` defers to the files.
    let auto = BiffixManifest::parse_yaml("engine: auto").unwrap();
    assert_eq!(detect_engine(&auto, &["main.nf".to_string()]), Ok(Engine::Nextflow));
}

#[test]
fn undetected_without_signals() {
    let m = BiffixManifest::default();
    assert_eq!(detect_engine(&m, &["data.csv".to_string()]), Err(EngineDetectError::Undetected));
}

// ── FormSchema adaptation ──────────────────────────────────────────────────────

#[test]
fn adapts_config_with_manifest_hints() {
    let schema = serde_json::json!({
        "properties": {
            "reads":  {"type": "string", "description": "input reads"},
            "outdir": {"type": "string"},
            "aligner": {"type": "string", "enum": ["bwa", "bowtie2"]},
            "api_key": {"type": "string"}
        },
        "required": ["reads", "outdir"]
    });
    let manifest = BiffixManifest::parse_yaml(
        "params:\n  reads: { format: s3-file }\n  outdir: { format: s3-dir, output: true }\n  api_key: { secret: true }\n",
    )
    .unwrap();

    let fs = from_snakemake_config_ordered(&schema, &SchemaOrder::default(), &manifest);
    let by = |n: &str| fs.fields.iter().find(|f| f.name == n).unwrap();

    assert_eq!(by("reads").widget, WidgetHint::S3File);
    assert!(by("reads").is_path_input);
    assert!(by("reads").required);

    assert_eq!(by("outdir").widget, WidgetHint::S3Dir);
    assert!(by("outdir").is_output);
    assert!(!by("outdir").is_path_input, "output paths are not existence-checked");

    assert_eq!(by("aligner").widget, WidgetHint::Enum);
    assert_eq!(
        by("aligner").enum_values.as_deref(),
        Some(["bwa".to_string(), "bowtie2".to_string()].as_slice())
    );

    assert!(by("api_key").is_secret);
}

#[test]
fn adapter_is_lenient_on_malformed_fields() {
    let schema = serde_json::json!({
        "properties": {
            "ok": {"type": "string"},
            "bad": "not-an-object"
        }
    });
    let fs =
        from_snakemake_config_ordered(&schema, &SchemaOrder::default(), &BiffixManifest::default());
    assert!(fs.fields.iter().any(|f| f.name == "ok"));
    assert!(!fs.fields.iter().any(|f| f.name == "bad"));
    assert!(fs.warnings.iter().any(|w| w.contains("bad")));
}

// ── version freeze ──────────────────────────────────────────────────────────────

#[test]
fn content_hash_is_order_independent() {
    let a = content_hash(vec![("a".into(), b"1".to_vec()), ("b".into(), b"2".to_vec())]);
    let b = content_hash(vec![("b".into(), b"2".to_vec()), ("a".into(), b"1".to_vec())]);
    assert_eq!(a, b);
}

fn input(qcs: Vec<String>) -> RegisterInput {
    RegisterInput {
        git_sha: "abc".into(),
        content_hash: "hash".into(),
        image_digest: format!("sha256:{}", "a".repeat(64)),
        image_repository: None,
        image_registry_id: None,
        image_region: None,
        engine: Engine::Snakemake,
        engine_version: "9.3".into(),
        entrypoint: "workflow/Snakefile".into(),
        target: None,
        form_schema: Default::default(),
        queue_classes: qcs,
        about_markdown: None,
        runtime_key: "rt".into(),
        runtime_recipe: serde_json::json!({"engine": "snakemake"}),
    }
}

#[test]
fn rejects_unknown_queue_classes() {
    let menu = vec!["cpu-spot".to_string(), "coordinator".to_string()];
    let err =
        freeze_version(&input(vec!["gpu".into(), "tpu".into()]), &menu, &[], "h").unwrap_err();
    assert_eq!(err, RegisterError::UnknownQueueClasses(vec!["gpu".into(), "tpu".into()]));
    // Read for people, not as a Rust list.
    assert_eq!(err.to_string(), "queue-class(es) not in the deploy menu: gpu, tpu");
}

fn prior(id: &str, build: u32, hash: &str) -> SourceBuild {
    SourceBuild { id: id.into(), build, registration_inputs_hash: hash.to_string() }
}

#[test]
fn the_first_registration_of_a_source_is_build_0() {
    let menu = vec!["cpu-spot".to_string()];
    let v = freeze_version(&input(vec!["cpu-spot".into()]), &menu, &[], "h1").unwrap();
    assert_eq!((v.id.as_str(), v.build), ("abc:hash:0", 0));
    assert_eq!(v.engine, Engine::Snakemake);
    assert_eq!(v.image_digest, format!("sha256:{}", "a".repeat(64)));
    assert_eq!(v.runtime_key, "rt");
}

#[test]
fn registering_a_source_again_with_different_inputs_is_the_next_build() {
    let menu = vec!["cpu-spot".to_string()];
    let builds = [prior("abc:hash:0", 0, "h0"), prior("abc:hash:1", 1, "h1")];
    let v = freeze_version(&input(vec!["cpu-spot".into()]), &menu, &builds, "h2").unwrap();
    assert_eq!((v.id.as_str(), v.build), ("abc:hash:2", 2));
}

#[test]
fn registering_a_source_again_with_the_same_inputs_is_refused_as_identical() {
    let menu = vec!["cpu-spot".to_string()];
    let builds = [prior("abc:hash:0", 0, "h1"), prior("abc:hash:1", 1, "h2")];
    let err = freeze_version(&input(vec!["cpu-spot".into()]), &menu, &builds, "h2").unwrap_err();
    assert_eq!(err, RegisterError::IdenticalBuild { id: "abc:hash:1".into(), build: 1 });
    assert!(err.to_string().starts_with("identical to build 1 (abc:hash:1)"));
}

/// The arguments of [`inputs_hash`], so a test can vary one at a time.
#[derive(Clone)]
struct Hashed {
    name: String,
    project: String,
    source: WorkflowSource,
    source_mode: String,
    runtime_key: String,
    images: Vec<String>,
    input: RegisterInput,
}

impl Hashed {
    fn base() -> Self {
        let mut input = input(vec!["cpu-spot".into(), "gpu".into()]);
        input.form_schema = serde_json::from_value(serde_json::json!({
            "fields": [{"name": "reads", "widget": "text", "required": true, "is_output": false,
                        "is_secret": false, "is_path_input": false}],
            "warnings": []
        }))
        .unwrap();
        Self {
            name: "wf".into(),
            project: "p".into(),
            source: WorkflowSource::Git {
                repo: "https://github.com/org/wf".into(),
                git_ref: "main".into(),
            },
            source_mode: "s3".into(),
            runtime_key: "rt".into(),
            images: vec![format!("default=quay.io/x/y:1@sha256:{}", "a".repeat(64))],
            input,
        }
    }

    fn hash(&self) -> String {
        inputs_hash(
            &self.name,
            &self.project,
            &self.source,
            &self.source_mode,
            &self.runtime_key,
            &self.images,
            &self.input,
        )
    }
}

#[test]
fn the_inputs_hash_covers_what_the_version_freezes_and_nothing_else() {
    let base = Hashed::base();
    type Change = (&'static str, fn(&mut Hashed));
    let changes: Vec<Change> = vec![
        ("name", |h| h.name = "other".into()),
        ("project", |h| h.project = "q".into()),
        // Where the coordinator fetches the source from: a moved repository, another ref, or
        // another staged object of the same commit.
        ("source repository", |h| {
            h.source = WorkflowSource::Git {
                repo: "https://github.com/org-renamed/wf".into(),
                git_ref: "main".into(),
            };
        }),
        ("source ref", |h| {
            h.source = WorkflowSource::Git {
                repo: "https://github.com/org/wf".into(),
                git_ref: "v1.0".into(),
            };
        }),
        ("source object", |h| {
            h.source =
                WorkflowSource::S3 { uri: "s3://b/workflow-sources/x.tar.gz".into(), sha256: None };
        }),
        ("source mode", |h| h.source_mode = "git".into()),
        ("runtime key", |h| h.runtime_key = "rt2".into()),
        ("image digest", |h| {
            h.images = vec![format!("default=quay.io/x/y:1@sha256:{}", "b".repeat(64))];
        }),
        ("extra image", |h| h.images.push("samtools=x".into())),
        ("git sha", |h| h.input.git_sha = "def".into()),
        ("content hash", |h| h.input.content_hash = "other".into()),
        ("engine", |h| h.input.engine = Engine::Nextflow),
        ("engine version", |h| h.input.engine_version = "9.4".into()),
        ("entrypoint", |h| h.input.entrypoint = "Snakefile".into()),
        ("target", |h| h.input.target = Some("all".into())),
        ("queue classes", |h| {
            h.input.queue_classes = vec!["gpu".into()];
        }),
        ("form schema", |h| h.input.form_schema = Default::default()),
        ("about", |h| h.input.about_markdown = Some("# hi".into())),
    ];
    for (what, change) in changes {
        let mut changed = base.clone();
        change(&mut changed);
        assert_ne!(base.hash(), changed.hash(), "{what} is not hashed");
    }
    // What the version freezes but the hash leaves out: the order queue classes were given in and
    // their duplicates, and the default image's digest and ECR coordinates, which the pinned
    // `images` already carry.
    let unhashed: Vec<Change> = vec![
        ("queue class order and duplicates", |h| {
            h.input.queue_classes = vec!["gpu".into(), "cpu-spot".into(), "gpu".into()];
        }),
        ("image digest", |h| {
            h.input.image_digest = format!("sha256:{}", "b".repeat(64));
        }),
        ("image repository", |h| {
            h.input.image_repository = Some("workflow".into());
        }),
        ("image registry id", |h| {
            h.input.image_registry_id = Some("123456789012".into());
        }),
        ("image region", |h| {
            h.input.image_region = Some("us-east-1".into());
        }),
    ];
    for (what, change) in unhashed {
        let mut changed = base.clone();
        change(&mut changed);
        assert_eq!(base.hash(), changed.hash(), "{what} is hashed");
    }
}

#[test]
fn both_private_ecr_classifiers_agree_on_the_china_partition() {
    use biffix_workflow::images::ImageRef;
    for (image, private) in [
        ("123456789012.dkr.ecr.us-east-1.amazonaws.com/wf:v1", true),
        ("123456789012.dkr.ecr.cn-north-1.amazonaws.com.cn/wf:v1", true),
        ("quay.io/biocontainers/fastqc:1", false),
        ("public.ecr.aws/docker/library/ubuntu:24.04", false),
    ] {
        let image = ImageRef::parse(image).unwrap();
        assert_eq!(image.is_private_ecr(), private, "{image}");
        assert_eq!(image.ecr_coords().is_ok(), private, "{image}");
    }
}

#[test]
fn rejects_a_floating_image_at_the_snapshot_boundary() {
    let menu = vec!["cpu-spot".to_string()];
    let mut registration = input(vec!["cpu-spot".into()]);
    registration.image_digest =
        "123456789012.dkr.ecr.us-east-1.amazonaws.com/workflow:latest".into();

    let err = freeze_version(&registration, &menu, &[], "h").unwrap_err();
    assert_eq!(err, RegisterError::InvalidImageDigest(ImageDigestError::UnsupportedAlgorithm));
}

#[test]
fn an_empty_digest_freezes_a_version_with_no_default_image() {
    let menu = vec!["cpu-spot".to_string()];
    let mut registration = input(vec!["cpu-spot".into()]);
    registration.image_digest = String::new();
    let v = freeze_version(&registration, &menu, &[], "h").unwrap();
    assert_eq!(v.image_digest, "");
}

// ── the in-memory registry double ─────────────────────────────────────────────
//
// These pin the double's behavior; `testing::registry_orders_and_conflicts` also runs against the
// Postgres registry (pg/registry.rs), so the two are held to one scenario.

fn mem_version(
    id: &str,
    build: u32,
    created_at: i64,
) -> biffix_backend::store::registry::RegisteredVersion {
    use biffix_backend::register::workflow::{WorkflowSource, WorkflowVersion};
    biffix_backend::store::registry::RegisteredVersion {
        name: "wf".into(),
        project: "p".into(),
        source: WorkflowSource::S3 { uri: "s3://b/src.tar.gz".into(), sha256: None },
        version: WorkflowVersion {
            id: id.into(),
            build,
            git_sha: "abc".into(),
            source_content_hash: "hash".into(),
            image_digest: String::new(),
            image_repository: None,
            image_registry_id: None,
            image_region: None,
            engine: Engine::Nextflow,
            engine_version: "25.10.4".into(),
            entrypoint: "main.nf".into(),
            target: None,
            form_schema: serde_json::json!({"fields": [], "warnings": []}),
            queue_classes: vec![],
            about_markdown: None,
            runtime_key: "rt".into(),
            runtime_recipe: serde_json::json!({"engine": "nextflow"}),
        },
        created_by: None,
        created_by_label: None,
        created_at_micros: created_at,
        registration_inputs: Default::default(),
        registration_inputs_hash: format!("inputs-{id}"),
        images: Vec::new(),
    }
}

#[tokio::test]
async fn mem_registry_orders_and_conflicts_like_postgres() {
    testing::registry_orders_and_conflicts(&testing::MemRegistry::new(), mem_version).await;
}
