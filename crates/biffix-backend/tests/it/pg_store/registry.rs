//! `PgRegistryStore`: versions, builds, images, reference bundles and the workflow listing.

use super::*;
use biffix_backend::store::runs::RunReads;

#[tokio::test]
#[ignore = "requires --features pg + the Postgres harness"]
async fn a_version_round_trips_with_its_about_page() {
    use biffix_backend::pg::registry::PgRegistryStore;
    use biffix_backend::store::registry::RegistryStore;

    let pool = migrated_pool("version_about").await;
    let registry = PgRegistryStore::new(pool);
    let version = |id: &str, about: Option<&str>| {
        let mut v = registered_version(id);
        v.version.about_markdown = about.map(str::to_string);
        v
    };
    registry.put_version(&version("abc:hash", Some("# Hi\n\nAligns reads."))).await.unwrap();
    registry.put_version(&version("abc:other", None)).await.unwrap();
    let with = registry.get_version("abc:hash").await.unwrap();
    assert_eq!(with.version.about_markdown.as_deref(), Some("# Hi\n\nAligns reads."));
    let without = registry.get_version("abc:other").await.unwrap();
    assert_eq!(without.version.about_markdown, None);
}

#[tokio::test]
#[ignore = "requires --features pg + the Postgres harness"]
async fn builds_of_a_source_record_their_inputs_and_never_share_a_build() {
    use biffix_backend::pg::registry::PgRegistryStore;
    use biffix_backend::store::registry::{RegistryStore, SourceBuild};
    use biffix_backend::store::StoreError;
    use biffix_workflow::engines::Engine;

    let pool = migrated_pool("version_builds").await;
    let registry = PgRegistryStore::new(pool);
    // Every build of one Snakemake source (`abc`/`hash`).
    let version = |id: &str, build: u32, hash: &str| {
        let mut v = registered_version(id);
        v.version.build = build;
        v.version.source_content_hash = "hash".into();
        v.version.engine = Engine::Snakemake;
        v.version.engine_version = "9.3.0".into();
        v.version.entrypoint = "Snakefile".into();
        v.registration_inputs = biffix_api::RegistrationInputs {
            source_mode: "s3".into(),
            cli_version: "0.0.0".into(),
            overrides: biffix_api::RegistrationOverrides {
                engine_version: Some("9.3.0".into()),
                ..Default::default()
            },
            reregistered_from: None,
        };
        v.registration_inputs_hash = hash.to_string();
        v
    };
    // Build 0, then build 1, each with its inputs.
    registry.put_version(&version("abc:hash:0", 0, "h0")).await.unwrap();
    registry.put_version(&version("abc:hash:1", 1, "h1")).await.unwrap();
    // A concurrent registration that also claimed build 1 loses: the unique (source, build) index.
    let err = registry.put_version(&version("abc:hash:1-racer", 1, "h2")).await.unwrap_err();
    assert!(matches!(err, StoreError::Conflict(_)), "{err:?}");
    // A registered version is never overwritten: its id again, even for a build no version has
    // claimed and with other fields, is a conflict, and the original reads back unchanged.
    let mut rewrite = version("abc:hash:1", 2, "h3");
    rewrite.name = "renamed".into();
    rewrite.version.about_markdown = Some("# Rewritten".into());
    let err = registry.put_version(&rewrite).await.unwrap_err();
    assert!(matches!(err, StoreError::Conflict(_)), "{err:?}");

    assert_eq!(
        registry.source_builds("abc", "hash").await.unwrap(),
        vec![
            SourceBuild {
                id: "abc:hash:0".into(),
                build: 0,
                registration_inputs_hash: "h0".into()
            },
            SourceBuild {
                id: "abc:hash:1".into(),
                build: 1,
                registration_inputs_hash: "h1".into()
            },
        ]
    );
    assert!(registry.source_builds("abc", "other").await.unwrap().is_empty());
    let got = registry.get_version("abc:hash:1").await.unwrap();
    assert_eq!(got, version("abc:hash:1", 1, "h1"));
}

#[tokio::test]
#[ignore = "requires --features pg + the Postgres harness"]
async fn reference_bundles_are_created_edited_and_deleted() {
    use biffix_backend::pg::references::PgReferenceStore;
    use biffix_backend::store::references::ReferenceStore;
    use biffix_backend::store::StoreError;

    let pool = migrated_pool("reference_bundles").await;
    let store = PgReferenceStore::new(pool);
    let bundle = biffix_api::ReferenceBundle {
        id: "grch38".into(),
        kind: "assembly".into(),
        display_name: "Human GRCh38".into(),
        uris: [("fasta".to_string(), "s3://refs/hg38.fa".to_string())].into(),
        notes: Some("GATK".into()),
        igv_genome: None,
        created_at_micros: 1,
        updated_at_micros: 1,
        created_by: Some("u".into()),
    };
    store.create_bundle(&bundle).await.unwrap();
    assert!(matches!(store.create_bundle(&bundle).await, Err(StoreError::Conflict(_))));
    assert_eq!(store.get_bundle("grch38").await.unwrap(), bundle);

    // An edit keeps the creation stamp.
    let edited = biffix_api::ReferenceBundle {
        display_name: "Human GRCh38 (GATK)".into(),
        igv_genome: Some("hg38".into()),
        created_at_micros: 99,
        created_by: None,
        updated_at_micros: 2,
        ..bundle.clone()
    };
    let returned = store.update_bundle(&edited).await.unwrap();
    let got = store.get_bundle("grch38").await.unwrap();
    assert_eq!(returned, got, "the update returns the bundle as stored");
    assert_eq!(got.display_name, "Human GRCh38 (GATK)");
    assert_eq!((got.created_at_micros, got.updated_at_micros), (1, 2));
    assert_eq!(got.created_by.as_deref(), Some("u"));
    assert_eq!(got.igv_genome.as_deref(), Some("hg38"));
    assert_eq!(store.list_bundles().await.unwrap().len(), 1);

    store.delete_bundle("grch38").await.unwrap();
    assert!(matches!(store.delete_bundle("grch38").await, Err(StoreError::NotFound)));

    // Listed by id in byte order (the in-memory store's), whatever the database's collation: a
    // linguistic one would ignore the punctuation.
    for id in ["ab", "a_c", "a-b", "B1"] {
        store
            .create_bundle(&biffix_api::ReferenceBundle { id: id.into(), ..bundle.clone() })
            .await
            .unwrap();
    }
    let ids: Vec<String> = store.list_bundles().await.unwrap().into_iter().map(|b| b.id).collect();
    assert_eq!(ids, ["B1", "a-b", "a_c", "ab"]);
    assert!(matches!(store.update_bundle(&edited).await, Err(StoreError::NotFound)));
}

#[tokio::test]
#[ignore = "requires --features pg + the Postgres harness"]
async fn a_version_keeps_its_runtime_and_variants_and_builds_are_read_back() {
    use biffix_backend::pg::registry::PgRegistryStore;
    use biffix_backend::store::registry::{CoordinatorVariant, RegistryStore};
    use biffix_core::builds::BuildState;
    use biffix_workflow::engines::Engine;

    let pool = migrated_pool("version_runtime").await;
    let registry = PgRegistryStore::new(pool.clone());
    let recipe = serde_json::json!({"engine": "snakemake", "engine_version": "9.27.0"});
    // A Snakemake version with its own runtime (its source `rt`).
    let mut version = registered_version("abc:rt:0");
    version.version.source_content_hash = "rt".into();
    version.version.engine = Engine::Snakemake;
    version.version.engine_version = "9.27.0".into();
    version.version.entrypoint = "Snakefile".into();
    version.version.runtime_key = "key".into();
    version.version.runtime_recipe = recipe.clone();
    registry.put_version(&version).await.unwrap();
    let got = registry.get_version("abc:rt:0").await.unwrap();
    assert_eq!(got.version.runtime_key, "key");
    assert_eq!(got.version.runtime_recipe, recipe);

    // Variants: newest first, and recording one twice keeps the first.
    let variant = |base: &str, image: &str, at: i64| CoordinatorVariant {
        runtime_key: "key".into(),
        base_digest: base.into(),
        image: image.into(),
        job_definition_arn: format!("arn:jd:{base}"),
        created_at: at,
    };
    registry.put_coordinator_variant(&variant("sha256:old", "repo@sha256:1", 1)).await.unwrap();
    registry.put_coordinator_variant(&variant("sha256:new", "repo@sha256:2", 2)).await.unwrap();
    registry.put_coordinator_variant(&variant("sha256:new", "repo@sha256:other", 3)).await.unwrap();
    let variants = registry.coordinator_variants("key").await.unwrap();
    assert_eq!(variants.len(), 2);
    assert_eq!(variants[0].base_digest, "sha256:new");
    assert_eq!(variants[0].image, "repo@sha256:2");
    assert_eq!(variants[0].job_definition_arn, "arn:jd:sha256:new");
    // The same image recorded again (cloned from a newer coordinator template) takes the new job
    // definition.
    registry
        .put_coordinator_variant(&CoordinatorVariant {
            job_definition_arn: "arn:jd:new-template".into(),
            ..variant("sha256:new", "repo@sha256:2", 4)
        })
        .await
        .unwrap();
    let variants = registry.coordinator_variants("key").await.unwrap();
    assert_eq!(variants[0].job_definition_arn, "arn:jd:new-template");
    assert_eq!(variants[0].image, "repo@sha256:2");
    assert!(registry.coordinator_variants("other").await.unwrap().is_empty());

    // Builds are the ingest fold's rows.
    assert_eq!(registry.get_build("coordinator:key@sha256:new").await.unwrap(), None);
    exec_all(
        &pool,
        &["INSERT INTO build_projection
           (subject, state, build_arn, outputs, reason, logs_url, request, builds, updated_at)
         VALUES ('coordinator:key@sha256:new', 'succeeded', 'arn:b',
                 '{\"VARIANT_IMAGE\": \"repo@sha256:2\"}', NULL, 'https://logs', NULL, 2, 5)"],
    )
    .await;
    let build = registry.get_build("coordinator:key@sha256:new").await.unwrap().unwrap();
    assert_eq!(build.state, BuildState::Succeeded);
    assert_eq!(build.outputs["VARIANT_IMAGE"], "repo@sha256:2");
    assert_eq!((build.builds, build.request.is_null()), (2, true));
}

#[tokio::test]
#[ignore = "requires --features pg + the Postgres harness"]
async fn a_version_keeps_its_images_and_imports_are_recorded_once() {
    use biffix_backend::pg::registry::PgRegistryStore;
    use biffix_backend::store::registry::{ImageArtifact, RegistryStore, VersionImage};

    let pool = migrated_pool("version_images").await;
    let registry = PgRegistryStore::new(pool);
    let image = |name: &str, key: &str| VersionImage {
        name: name.into(),
        declared_ref: format!("quay.io/b/{name}:1"),
        source_ref: format!("quay.io/b/{name}:1@sha256:{key}"),
        content_key: key.into(),
        own_runtime: name == "sm",
        build: None,
    };
    let built = VersionImage {
        name: "align".into(),
        declared_ref: "align".into(),
        source_ref: "dockerfile:docker/align/Dockerfile".into(),
        content_key: "kb".into(),
        own_runtime: false,
        build: Some(biffix_backend::store::registry::BuildRecipe {
            kind: "dockerfile".into(),
            context: "docker/align".into(),
            context_uri: "s3://b/workflow-sources/v/contexts/t.tar.gz".into(),
            context_sha256: "s".into(),
            tree_hash: "t".into(),
            dockerfile: Some("Dockerfile".into()),
            dockerfile_text: None,
            archs: vec!["amd64".into()],
        }),
    };
    let version = |id: &str, hash: &str, images: Vec<VersionImage>| {
        let mut v = registered_version(id);
        v.version.source_content_hash = hash.into();
        v.version.engine_version = "25.10.4".into();
        v.images = images;
        v
    };
    registry
        .put_version(&version(
            "abc:h1:0",
            "h1",
            vec![image("fastqc", "k1"), image("default", "k0"), image("sm", "k2"), built.clone()],
        ))
        .await
        .unwrap();
    registry.put_version(&version("abc:h2:0", "h2", vec![])).await.unwrap();
    let got = registry.get_version("abc:h1:0").await.unwrap();
    let names: Vec<&str> = got.images.iter().map(|i| i.name.as_str()).collect();
    assert_eq!(names, ["align", "default", "fastqc", "sm"]);
    assert!(got.images[3].own_runtime);
    // A built image keeps its recipe (and kind); an imported one has none.
    assert_eq!(got.images[0], built);
    assert_eq!(got.images[0].kind(), "dockerfile");
    assert_eq!(got.images[1].build, None);
    let listed = registry.list_versions().await.unwrap();
    let with = listed.iter().find(|r| r.version.id == "abc:h1:0").unwrap();
    assert_eq!(with.images.len(), 4);
    assert!(listed.iter().find(|r| r.version.id == "abc:h2:0").unwrap().images.is_empty());

    assert_eq!(registry.get_image_artifact("k1").await.unwrap(), None);
    let artifact = |image: &str| ImageArtifact {
        content_key: "k1".into(),
        source_ref: "quay.io/b/fastqc:1@sha256:k1".into(),
        image: image.into(),
        digest: "sha256:k1".into(),
        archs: vec!["amd64".into(), "arm64".into()],
        capabilities: biffix_workflow::images::Capabilities::parse("bash=1,libc=glibc"),
        keep_tag: Some("biffix-keep-k1".into()),
        archived: true,
        archive_bytes: Some(42),
        created_at: 7,
    };
    registry.put_image_artifact(&artifact("repo@sha256:k1")).await.unwrap();
    registry.put_image_artifact(&artifact("repo@sha256:other")).await.unwrap();
    assert_eq!(registry.get_image_artifact("k1").await.unwrap(), Some(artifact("repo@sha256:k1")));
    // A replacement lands only over the image it names as stale (a built image recorded outside
    // the builds repository): a second replacement naming the old image is a no-op, and nothing is
    // recorded where nothing was.
    let rebuilt = ImageArtifact {
        digest: "sha256:new".into(),
        keep_tag: Some("biffix-keep-new".into()),
        archive_bytes: None,
        created_at: 8,
        ..artifact("builds@sha256:new")
    };
    registry.replace_image_artifact("repo@sha256:other", &rebuilt).await.unwrap();
    assert_eq!(registry.get_image_artifact("k1").await.unwrap(), Some(artifact("repo@sha256:k1")));
    registry.replace_image_artifact("repo@sha256:k1", &rebuilt).await.unwrap();
    assert_eq!(registry.get_image_artifact("k1").await.unwrap(), Some(rebuilt.clone()));
    registry
        .replace_image_artifact("repo@sha256:k1", &artifact("builds@sha256:late"))
        .await
        .unwrap();
    assert_eq!(registry.get_image_artifact("k1").await.unwrap(), Some(rebuilt));
    let elsewhere = ImageArtifact { content_key: "k9".into(), ..artifact("builds@sha256:k9") };
    registry.replace_image_artifact("repo@sha256:k9", &elsewhere).await.unwrap();
    assert_eq!(registry.get_image_artifact("k9").await.unwrap(), None);
}

#[tokio::test]
#[ignore = "requires --features pg + the Postgres harness"]
async fn run_stats_by_workflow_count_runs_and_keep_the_newest_submit() {
    use biffix_backend::store::runs::RunStats;
    let pool = migrated_pool("run_stats_by_workflow").await;
    exec_all(
        &pool,
        &["INSERT INTO run_projection (run_id, state, workflow_version, submitted_at) VALUES \
           ('a', 'running', 'v1', 5), ('b', 'succeeded', 'v1', 9), ('c', 'failed', 'v2', NULL), \
           ('d', 'submitted', NULL, 20), ('e', 'running', 'v1', NULL)"],
    )
    .await;
    let store = PgStore::new(pool);
    assert_eq!(
        store.run_stats_by_workflow().await.unwrap(),
        [
            ("v1".to_string(), RunStats { count: 3, last_submitted_micros: Some(9) }),
            ("v2".to_string(), RunStats { count: 1, last_submitted_micros: None }),
        ]
        .into()
    );
}

#[tokio::test]
#[ignore = "requires --features pg + the Postgres harness"]
async fn a_workflow_is_hidden_from_the_listing_only_once_registered() {
    use biffix_backend::pg::registry::PgRegistryStore;
    use biffix_backend::store::registry::RegistryStore;
    use biffix_backend::store::StoreError;

    let registry = PgRegistryStore::new(migrated_pool("workflow_listing").await);
    assert!(registry.hidden_workflows().await.unwrap().is_empty());
    // No version of it: nothing to curate.
    assert!(matches!(
        registry.put_workflow_listing("p", "wf", true, Some("u1")).await,
        Err(StoreError::NotFound)
    ));
    assert!(registry.hidden_workflows().await.unwrap().is_empty());
    assert_eq!(registry.workflow_listing("p", "wf").await.unwrap(), None);

    let mut other = registered_version("abc:h2");
    other.name = "team/other".into();
    registry.put_version(&registered_version("abc:h1")).await.unwrap();
    registry.put_version(&other).await.unwrap();
    // The same name in another project is not the same workflow.
    assert!(matches!(
        registry.put_workflow_listing("q", "wf", true, None).await,
        Err(StoreError::NotFound)
    ));

    let hidden = registry.put_workflow_listing("p", "wf", true, Some("u1")).await.unwrap();
    assert!(hidden.hidden);
    assert_eq!(hidden.updated_by.as_deref(), Some("u1"));
    assert!(hidden.updated_at_micros > 0);
    // A second put replaces the first (who and when included).
    let shown = registry.put_workflow_listing("p", "wf", false, Some("u2")).await.unwrap();
    assert!(!shown.hidden);
    assert_eq!(shown.updated_by.as_deref(), Some("u2"));
    assert!(shown.updated_at_micros >= hidden.updated_at_micros);
    let slashed = registry.put_workflow_listing("p", "team/other", true, None).await.unwrap();
    assert_eq!(slashed.updated_by, None);

    // Only the hidden one is in the set; each row reads back as stored.
    assert_eq!(
        registry.hidden_workflows().await.unwrap(),
        [("p".to_string(), "team/other".to_string())].into()
    );
    assert_eq!(registry.workflow_listing("p", "wf").await.unwrap(), Some(shown));
    assert_eq!(registry.workflow_listing("p", "team/other").await.unwrap(), Some(slashed));
    assert_eq!(registry.workflow_listing("q", "wf").await.unwrap(), None);
}

/// A version with every optional column set to its own value, so a misordered bind or read in the
/// registry swaps two of them and fails the whole-record comparison.
fn distinct_version(
    id: &str,
    build: u32,
    created_at: i64,
) -> biffix_backend::store::registry::RegisteredVersion {
    use biffix_backend::register::workflow::{WorkflowSource, WorkflowVersion};
    use biffix_backend::store::registry::{BuildRecipe, RegisteredVersion, VersionImage};
    use biffix_workflow::engines::Engine;
    RegisteredVersion {
        name: format!("name-{id}"),
        project: format!("project-{id}"),
        source: WorkflowSource::Git {
            repo: format!("https://git.example/{id}"),
            git_ref: format!("ref-{id}"),
        },
        version: WorkflowVersion {
            id: id.into(),
            build,
            git_sha: "sha-distinct".into(),
            source_content_hash: "hash-distinct".into(),
            image_digest: format!("sha256:digest-{id}"),
            image_repository: Some(format!("repository-{id}")),
            image_registry_id: Some(format!("registry-id-{id}")),
            image_region: Some(format!("region-{id}")),
            engine: Engine::Nextflow,
            engine_version: format!("engine-version-{id}"),
            entrypoint: format!("entrypoint-{id}"),
            target: Some(format!("target-{id}")),
            form_schema: serde_json::json!({"fields": [], "warnings": [id]}),
            queue_classes: vec![format!("queue-a-{id}"), format!("queue-b-{id}")],
            about_markdown: Some(format!("about-{id}")),
            runtime_key: format!("runtime-key-{id}"),
            runtime_recipe: serde_json::json!({"recipe": id}),
        },
        registration_inputs: biffix_api::RegistrationInputs {
            source_mode: "git".into(),
            cli_version: format!("cli-{id}"),
            overrides: biffix_api::RegistrationOverrides {
                engine_version: Some(format!("override-{id}")),
                ..Default::default()
            },
            reregistered_from: Some(format!("from-{id}").into()),
        },
        registration_inputs_hash: format!("inputs-hash-{id}"),
        created_by: Some(format!("user:creator-{id}")),
        created_by_label: Some(format!("label-{id}")),
        created_at_micros: created_at,
        // Sorted by name, as the store returns them; one built, one imported.
        images: vec![
            VersionImage {
                name: "align".into(),
                declared_ref: format!("declared-align-{id}"),
                source_ref: format!("source-align-{id}"),
                content_key: format!("key-align-{id}"),
                own_runtime: true,
                build: Some(BuildRecipe {
                    kind: "dockerfile".into(),
                    context: "docker/align".into(),
                    context_uri: format!("s3://b/contexts/{id}.tar.gz"),
                    context_sha256: format!("context-sha-{id}"),
                    tree_hash: format!("tree-{id}"),
                    dockerfile: Some("Dockerfile".into()),
                    dockerfile_text: None,
                    archs: vec!["amd64".into(), "arm64".into()],
                }),
            },
            VersionImage {
                name: "default".into(),
                declared_ref: format!("declared-default-{id}"),
                source_ref: format!("source-default-{id}"),
                content_key: format!("key-default-{id}"),
                own_runtime: false,
                build: None,
            },
        ],
    }
}

#[tokio::test]
#[ignore = "requires --features pg + the Postgres harness"]
async fn every_version_column_round_trips_to_its_own_field() {
    use biffix_backend::pg::registry::PgRegistryStore;
    use biffix_backend::store::registry::RegistryStore;
    let registry = PgRegistryStore::new(migrated_pool("version_distinct_columns").await);
    let rec = distinct_version("sha-distinct:hash-distinct:7", 7, 42);
    registry.put_version(&rec).await.unwrap();
    assert_eq!(registry.get_version(&rec.version.id).await.unwrap(), rec);
    assert_eq!(registry.list_versions().await.unwrap(), vec![rec]);
}

/// The scenario registration.rs runs against the in-memory registry double.
#[tokio::test]
#[ignore = "requires --features pg + the Postgres harness"]
async fn the_registry_orders_and_conflicts_like_its_in_memory_double() {
    use biffix_backend::pg::registry::PgRegistryStore;
    let registry = PgRegistryStore::new(migrated_pool("registry_parity").await);
    testing::registry_orders_and_conflicts(&registry, distinct_version).await;
}

#[tokio::test]
#[ignore = "requires --features pg + the Postgres harness"]
async fn versions_are_listed_newest_first_and_builds_read_back_whole() {
    use biffix_backend::pg::registry::PgRegistryStore;
    use biffix_backend::store::registry::RegistryStore;
    use biffix_backend::store::StoreError;
    use biffix_core::builds::{BuildProjection, BuildState};
    let pool = migrated_pool("version_order_and_builds").await;
    let registry = PgRegistryStore::new(pool.clone());
    // Registered out of order; two share a registration time (ties by id, bytewise: `vB` before
    // `v_b`, which the harness collation orders the other way).
    assert_collation_disagrees(&pool, "v_b", "vB").await;
    for (id, build, at) in [("v_b", 1, 10), ("v-old", 2, 5), ("v-new", 3, 20), ("vB", 4, 10)] {
        registry.put_version(&distinct_version(id, build, at)).await.unwrap();
    }
    let ids: Vec<String> =
        registry.list_versions().await.unwrap().into_iter().map(|r| r.version.id).collect();
    assert_eq!(ids, ["v-new", "vB", "v_b", "v-old"]);

    exec_all(
        &pool,
        &["INSERT INTO build_projection
           (subject, state, build_arn, outputs, reason, logs_url, request, builds, updated_at)
         VALUES ('image:x', 'failed', 'arn:build', '{\"IMAGE\": \"repo@sha256:1\"}',
                 'it broke', 'https://logs/x', '{\"arch\": \"amd64\"}', 3, 1234567),
                ('image:bad', 'exploded', 'arn:bad', '{}', NULL, NULL, NULL, 1, 1)"],
    )
    .await;
    assert_eq!(
        registry.get_build("image:x").await.unwrap(),
        Some(BuildProjection {
            subject: "image:x".into(),
            state: BuildState::Failed,
            build_arn: "arn:build".into(),
            outputs: [("IMAGE".to_string(), "repo@sha256:1".to_string())].into(),
            reason: Some("it broke".into()),
            logs_url: Some("https://logs/x".into()),
            request: serde_json::json!({"arch": "amd64"}),
            builds: 3,
            updated: biffix_core::time::Timestamp::from_micros(1234567),
        })
    );
    let err = registry.get_build("image:bad").await.unwrap_err();
    assert!(matches!(err, StoreError::Serialization(_)), "{err:?}");
}
