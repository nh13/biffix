use super::*;
use crate::engines::SUPPORTED;

#[test]
fn every_buildable_nextflow_release_has_pins_and_no_other_does() {
    for release in crate::engines::NEXTFLOW_RELEASES {
        assert!(nextflow_25(release).is_some(), "{release} is buildable but unpinned");
    }
    // 25.04.0 is retired (crate::engines::RETIRED): its pins are gone with it.
    for unpinned in ["25.04.0", "25.04.1", "25.10.2", "25.10.5"] {
        assert!(nextflow_25(unpinned).is_none(), "{unpinned} is pinned but not listed");
    }
}

fn recipe(engine: &str, version: &str) -> Option<RuntimeRecipe> {
    recipe_for(engine, EngineVersion::parse(version).unwrap())
}

#[test]
fn every_supported_bound_has_a_recipe_and_nothing_outside_does() {
    for range in SUPPORTED {
        let min = EngineVersion::parse(range.min).unwrap();
        let below = EngineVersion::parse(range.below).unwrap();
        assert!(recipe_for(range.engine, min).is_some(), "{} {min}", range.engine);
        // Nextflow pins each release's downloads, so only its pinned releases have a recipe.
        let last = EngineVersion::new(below.major - 1, 99, 99);
        assert_eq!(
            recipe_for(range.engine, last).is_some(),
            range.engine != "nextflow",
            "{} {last}",
            range.engine
        );
        assert!(recipe_for(range.engine, below).is_none(), "{} {below}", range.engine);
    }
    assert!(recipe("snakemake", "8.30.0").is_none());
    assert!(recipe("cromwell", "1.0.0").is_none());
}

#[test]
fn the_key_is_stable_and_names_the_exact_release() {
    let a = recipe("snakemake", "9.27.0").unwrap();
    assert_eq!(runtime_key(&a), runtime_key(&a.clone()));
    assert_eq!(runtime_key(&a).len(), 64);
    let b = recipe("snakemake", "9.27.1").unwrap();
    assert_ne!(runtime_key(&a), runtime_key(&b));
    let nf = recipe("nextflow", "25.10.4").unwrap();
    assert_ne!(runtime_key(&a), runtime_key(&nf));
}

/// A fixed recipe (not the live pin table, which moves), for the pinned keys.
fn fixed_snakemake() -> RuntimeRecipe {
    RuntimeRecipe::Snakemake {
        recipe_version: 1,
        engine_version: "9.1.0".into(),
        pins: SnakemakePins {
            python: PythonBuild {
                release: "r".into(),
                version: "3.12.0".into(),
                sha256_x86_64: "px".into(),
                sha256_aarch64: "pa".into(),
            },
            uv: UvBuild { version: "0.1.0".into(), sha256_x86_64: "ux".into() },
            storage_s3: "0.3.0".into(),
            executor_sha: "e".into(),
            logger_version: "0.1.0".into(),
            exclude_newer: "2026-01-01T00:00:00Z".into(),
        },
    }
}

/// The key is sha256 over the recipe's JSON, pinned to values computed outside this code (a
/// Python sha256 of the documented serialization). A renamed or reordered field changes every
/// tenant's runtime keys, so it must fail here rather than pass silently.
#[test]
fn the_runtime_key_is_pinned() {
    assert_eq!(
        runtime_key(&fixed_snakemake()),
        "11f3fbf389bbcf8506ce8e03909bbfde7c1d121b5592c6515a0f1a4e56c649c6"
    );
    let pinned = RuntimeRecipe::Nextflow {
        recipe_version: 1,
        engine_version: "25.04.0".into(),
        pins: NextflowPins {
            nf_biffix_version: "0.1.0".into(),
            release: NextflowRelease {
                launcher_sha256: "l".into(),
                nf_amazon: PluginPin { version: "2.0.0".into(), sha256: "a".into() },
                nf_cloudcache: PluginPin { version: "0.4.0".into(), sha256: "c".into() },
            },
            workflow_plugins: Vec::new(),
        },
    };
    // A workflow that declares no plugins of its own keys as the release's runtime.
    assert_eq!(
        runtime_key(&pinned),
        "c2c8b6c0ca5e5efdb84bfdc12b5aff087ae68968a1a9c19cf34b8b77a36cdd1a"
    );
    assert_eq!(
        runtime_key(&pinned.clone().with_workflow_plugins(Vec::new()).unwrap()),
        runtime_key(&pinned)
    );
    assert!(!serde_json::to_string(&pinned).unwrap().contains("workflow_plugins"));
}

fn plugin(id: &str, version: &str) -> WorkflowPlugin {
    WorkflowPlugin { id: id.into(), version: version.into(), sha256: "0".repeat(64) }
}

#[test]
fn workflow_plugins_key_the_runtime_in_any_declared_order() {
    let nf = recipe("nextflow", "25.10.4").unwrap();
    let schema = nf.clone().with_workflow_plugins(vec![plugin("nf-schema", "2.7.2")]).unwrap();
    assert_ne!(runtime_key(&schema), runtime_key(&nf));
    assert_eq!(schema.workflow_plugins(), [plugin("nf-schema", "2.7.2")]);
    let newer = nf.clone().with_workflow_plugins(vec![plugin("nf-schema", "2.7.3")]).unwrap();
    assert_ne!(runtime_key(&schema), runtime_key(&newer));
    let two = |a, b| nf.clone().with_workflow_plugins(vec![a, b]).map(|r| runtime_key(&r)).unwrap();
    assert_eq!(
        two(plugin("nf-schema", "2.7.2"), plugin("nf-prov", "1.4.0")),
        two(plugin("nf-prov", "1.4.0"), plugin("nf-schema", "2.7.2")),
    );
    // The JSON round-trip keeps them.
    let json = serde_json::to_value(&schema).unwrap();
    assert_eq!(serde_json::from_value::<RuntimeRecipe>(json).unwrap(), schema);
}

#[test]
fn workflow_plugins_reach_the_build_env() {
    let sha = "084e5f8af6af38b23b43c4e5c3c5a16aba782fa769d11189eb3a21072c3d67da";
    let r = recipe("nextflow", "25.10.4")
        .unwrap()
        .with_workflow_plugins(vec![
            WorkflowPlugin { id: "nf-schema".into(), version: "2.7.2".into(), sha256: sha.into() },
            plugin("nf-prov", "1.4.0"),
        ])
        .unwrap();
    assert_eq!(
        r.build_env()["BIFFIX_RT_NF_WORKFLOW_PLUGINS"],
        format!("nf-prov@1.4.0@{} nf-schema@2.7.2@{sha}", "0".repeat(64))
    );
    let none = recipe("nextflow", "25.10.4").unwrap();
    assert!(!none.build_env().contains_key("BIFFIX_RT_NF_WORKFLOW_PLUGINS"));
}

#[test]
fn bad_workflow_plugins_are_refused() {
    let nf = || recipe("nextflow", "25.10.4").unwrap();
    let refused = |plugins: Vec<WorkflowPlugin>| nf().with_workflow_plugins(plugins).unwrap_err().0;
    assert!(
        refused(vec![plugin("nf-amazon", "3.4.4")]).contains("biffix's runtime provides nf-amazon")
    );
    assert!(refused(vec![plugin("nf-biffix", "0.6.0")]).contains("provides nf-biffix"));
    assert!(refused(vec![plugin("nf-cloudcache", "0.5.0")]).contains("provides nf-cloudcache"));
    // An id the layer's `<id>-<version>` lookup of biffix's own would also match.
    for (id, own) in [
        ("nf-biffix-ext", "nf-biffix"),
        ("nf-amazon-2fa", "nf-amazon"),
        ("nf-cloudcache-x", "nf-cloudcache"),
    ] {
        let m = refused(vec![plugin(id, "1.0.0")]);
        assert!(m.contains(&format!("starting with `{own}-`")), "{m}");
    }
    assert!(nf().with_workflow_plugins(vec![plugin("nf-amazonx", "1.0.0")]).is_ok());
    assert!(refused(vec![plugin("nf schema", "1.0.0")]).contains("not a plain plugin id"));
    assert!(refused(vec![plugin("-x", "1.0.0")]).contains("not a plain plugin id"));
    assert!(refused(vec![plugin(&"a".repeat(65), "1.0.0")]).contains("not a plain plugin id"));
    for range in [">=2.0.0", "2.0.0,3", "", "~2", "2.0@1", "1 2"] {
        assert!(refused(vec![plugin("nf-schema", range)]).contains("one exact release"), "{range}");
    }
    let mut upper = plugin("nf-schema", "2.7.2");
    upper.sha256 = "A".repeat(64);
    assert!(refused(vec![upper]).contains("64 lowercase hex"));
    let mut short = plugin("nf-schema", "2.7.2");
    short.sha256 = "a".repeat(63);
    assert!(refused(vec![short]).contains("64 lowercase hex"));
    assert!(refused(vec![plugin("nf-schema", "2.7.2"), plugin("nf-schema", "2.7.3")])
        .contains("more than once"));
    let many = (0..=MAX_WORKFLOW_PLUGINS).map(|i| plugin(&format!("nf-p{i}"), "1.0.0")).collect();
    assert!(refused(many).contains("at most 32"));
    let at_most = (0..MAX_WORKFLOW_PLUGINS).map(|i| plugin(&format!("nf-p{i}"), "1.0.0")).collect();
    assert!(nf().with_workflow_plugins(at_most).is_ok());
    // Snakemake has no Nextflow plugins; none is fine.
    let sm = recipe("snakemake", "9.27.0").unwrap();
    assert!(sm
        .clone()
        .with_workflow_plugins(vec![plugin("nf-schema", "2.7.2")])
        .unwrap_err()
        .0
        .contains("only to Nextflow"));
    assert_eq!(sm.clone().with_workflow_plugins(Vec::new()).unwrap(), sm);
}

#[test]
fn plugin_specs_parse_as_id_at_exact_version() {
    assert_eq!(
        parse_plugin_specs(&["nf-schema@2.7.2", "nf-prov@1.4.0"]).unwrap(),
        [
            ("nf-prov".to_string(), "1.4.0".to_string()),
            ("nf-schema".to_string(), "2.7.2".to_string())
        ]
    );
    assert!(parse_plugin_specs(&["nf-schema"]).unwrap_err().0.contains("has no version"));
    assert!(parse_plugin_specs(&["nf-schema@>=2.0"]).unwrap_err().0.contains("one exact release"));
    assert!(parse_plugin_specs(&["nf-schema@2.7.2", "nf-schema@2.7.2"])
        .unwrap_err()
        .0
        .contains("more than once"));
    assert!(parse_plugin_specs(&["nf-biffix@0.6.0"])
        .unwrap_err()
        .0
        .contains("biffix's runtime provides nf-biffix"));
    assert!(parse_plugin_specs(&["nf-biffix-x@0.6.0"])
        .unwrap_err()
        .0
        .contains("starting with `nf-biffix-`"));
    assert_eq!(
        plugin_zip_url("nf-schema", "2.7.2"),
        "https://registry.nextflow.io/api/v1/plugins/nf-schema/2.7.2/download/nf-schema-2.7.2.zip"
    );
    // The runtime build spells the same URL (its `install_nf_plugin`, with `$id` and `$pv`).
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let script = std::fs::read_to_string(root.join("codebuild/runtime/build.sh")).unwrap();
    assert!(
        script.contains(&format!("fetch \"{}\"", plugin_zip_url("$id", "$pv"))),
        "codebuild/runtime/build.sh does not fetch plugin_zip_url's URL"
    );
}

#[test]
fn the_runtime_names_its_own_plugin_versions() {
    let nf = recipe("nextflow", "25.10.4").unwrap();
    assert_eq!(nf.runtime_plugin_version("nf-amazon"), Some("3.4.4"));
    assert_eq!(nf.runtime_plugin_version("nf-cloudcache"), Some("0.5.0"));
    assert_eq!(nf.runtime_plugin_version("nf-biffix"), Some("0.6.2"));
    assert_eq!(nf.runtime_plugin_version("nf-schema"), None);
    let sm = recipe("snakemake", "9.27.0").unwrap();
    assert_eq!(sm.runtime_plugin_version("nf-biffix"), None);
    // Every plugin biffix's runtime provides has a version on a pinned recipe.
    for id in RUNTIME_NEXTFLOW_PLUGINS {
        assert!(nf.runtime_plugin_version(id).is_some(), "{id}");
    }
}

#[test]
fn any_pin_change_changes_the_key() {
    let base = recipe("snakemake", "9.27.0").unwrap();
    let RuntimeRecipe::Snakemake { recipe_version, engine_version, pins } = base.clone() else {
        unreachable!()
    };
    let with = |f: &dyn Fn(&mut SnakemakePins)| {
        let mut pins = pins.clone();
        f(&mut pins);
        RuntimeRecipe::Snakemake { recipe_version, engine_version: engine_version.clone(), pins }
    };
    let mut changed = vec![
        with(&|p| p.python.release = "x".into()),
        with(&|p| p.python.version = "x".into()),
        with(&|p| p.python.sha256_x86_64 = "x".into()),
        with(&|p| p.python.sha256_aarch64 = "x".into()),
        with(&|p| p.uv.version = "x".into()),
        with(&|p| p.uv.sha256_x86_64 = "x".into()),
        with(&|p| p.storage_s3 = "x".into()),
        with(&|p| p.executor_sha = "x".into()),
        with(&|p| p.logger_version = "x".into()),
        with(&|p| p.exclude_newer = "x".into()),
        RuntimeRecipe::Snakemake {
            recipe_version: recipe_version + 1,
            engine_version: engine_version.clone(),
            pins: pins.clone(),
        },
        RuntimeRecipe::Snakemake {
            recipe_version,
            engine_version: "9.27.1".into(),
            pins: pins.clone(),
        },
    ];
    // Swapping the two Python digests is a change too.
    changed.push(with(&|p| {
        std::mem::swap(&mut p.python.sha256_x86_64, &mut p.python.sha256_aarch64);
    }));
    let nf = recipe("nextflow", "25.10.4").unwrap();
    let RuntimeRecipe::Nextflow {
        recipe_version: nf_recipe,
        engine_version: nf_version,
        pins: nf_pins,
    } = nf.clone()
    else {
        unreachable!()
    };
    let nf_changed = [
        RuntimeRecipe::Nextflow {
            recipe_version: nf_recipe,
            engine_version: nf_version.clone(),
            pins: NextflowPins { nf_biffix_version: "x".into(), ..nf_pins.clone() },
        },
        RuntimeRecipe::Nextflow {
            recipe_version: nf_recipe + 1,
            engine_version: nf_version.clone(),
            pins: nf_pins.clone(),
        },
        RuntimeRecipe::Nextflow {
            recipe_version: nf_recipe,
            engine_version: "25.10.5".into(),
            pins: nf_pins,
        },
    ];
    let mut keys = std::collections::BTreeSet::from([runtime_key(&base), runtime_key(&nf)]);
    for other in changed.iter().chain(&nf_changed) {
        assert!(keys.insert(runtime_key(other)), "{other:?}");
    }
}

#[test]
fn nextflow_keeps_its_own_spelling() {
    // However the pin spells the month, the recipe spells it as Nextflow does.
    let r = recipe("nextflow", "25.10.04").unwrap();
    assert_eq!(r.engine_version(), "25.10.4");
    assert_eq!(r.build_env()["BIFFIX_RT_ENGINE_VERSION"], "25.10.4");
    // A retired release has no recipe.
    assert!(recipe("nextflow", "25.04.0").is_none());
}

#[test]
fn the_build_env_carries_the_key_and_every_pin() {
    let r = fixed_snakemake();
    let want: BTreeMap<&str, String> = [
        ("BIFFIX_RT_KEY", runtime_key(&r)),
        ("BIFFIX_RT_ENGINE", "snakemake".into()),
        ("BIFFIX_RT_ENGINE_VERSION", "9.1.0".into()),
        ("BIFFIX_RT_PYTHON_RELEASE", "r".into()),
        ("BIFFIX_RT_PYTHON_VERSION", "3.12.0".into()),
        ("BIFFIX_RT_PYTHON_SHA256_X86_64", "px".into()),
        ("BIFFIX_RT_PYTHON_SHA256_AARCH64", "pa".into()),
        ("BIFFIX_RT_UV_VERSION", "0.1.0".into()),
        ("BIFFIX_RT_UV_SHA256_X86_64", "ux".into()),
        ("BIFFIX_RT_STORAGE_S3", "0.3.0".into()),
        ("BIFFIX_RT_EXECUTOR_SHA", "e".into()),
        ("BIFFIX_RT_LOGGER_VERSION", "0.1.0".into()),
        ("BIFFIX_RT_EXCLUDE_NEWER", "2026-01-01T00:00:00Z".into()),
    ]
    .into();
    assert_eq!(r.build_env(), want);
    let nf = recipe("nextflow", "25.10.4").unwrap();
    let want: BTreeMap<&str, String> = [
        ("BIFFIX_RT_KEY", runtime_key(&nf)),
        ("BIFFIX_RT_ENGINE", "nextflow".into()),
        ("BIFFIX_RT_ENGINE_VERSION", "25.10.4".into()),
        ("BIFFIX_RT_NF_BIFFIX_VERSION", "0.6.2".into()),
        (
            "BIFFIX_RT_NF_SHA256",
            "60db869a7618783e3ee60183a2b0c6c50ffc67221eabec60800b904991786aaa".into(),
        ),
        ("BIFFIX_RT_NF_AMAZON_VERSION", "3.4.4".into()),
        (
            "BIFFIX_RT_NF_AMAZON_SHA256",
            "677f69b82f63a43cb5809eb61d7045846a10af394d29168211c9a3c74fa39ac7".into(),
        ),
        ("BIFFIX_RT_NF_CLOUDCACHE_VERSION", "0.5.0".into()),
        (
            "BIFFIX_RT_NF_CLOUDCACHE_SHA256",
            "39afd45d1dcc2765ce77a0ee429559f2cf1ff52c27347f2a94b2f35a05b46775".into(),
        ),
    ]
    .into();
    assert_eq!(nf.build_env(), want);
}

/// Every variable the build env sets is one the runtime build reads (the per-arch Python
/// digest by a name the script puts together from the architecture).
#[test]
fn the_runtime_build_reads_every_build_env_variable() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let script = std::fs::read_to_string(root.join("codebuild/runtime/build.sh")).unwrap();
    // Read by code, not only named in a comment.
    let code: String =
        script.lines().filter(|l| !l.trim_start().starts_with('#')).collect::<Vec<_>>().join("\n");
    let reads = |name: &str| {
        let at = |i: usize| code[i + name.len()..].chars().next();
        code.match_indices(name)
            .any(|(i, _)| !at(i).is_some_and(|c| c.is_ascii_alphanumeric() || c == '_'))
    };
    let per_arch = "BIFFIX_RT_PYTHON_SHA256_";
    assert!(code.contains(&format!("sha={per_arch}${{pyarch^^}}")));
    assert!(code.contains("pyarch=$([ \"$arch\" = amd64 ] && echo x86_64 || echo aarch64)"));
    let with_plugins = recipe("nextflow", "25.10.4")
        .unwrap()
        .with_workflow_plugins(vec![plugin("nf-schema", "2.7.2")])
        .unwrap();
    for r in [fixed_snakemake(), recipe("nextflow", "25.10.4").unwrap(), with_plugins] {
        for name in r.build_env().keys() {
            if let Some(arch) = name.strip_prefix(per_arch) {
                assert!(["X86_64", "AARCH64"].contains(&arch), "{name}");
            } else {
                assert!(reads(name), "codebuild/runtime/build.sh never reads {name}");
            }
        }
    }
}

#[test]
fn the_recipe_round_trips_through_json() {
    for r in [recipe("snakemake", "9.27.0").unwrap(), recipe("nextflow", "25.10.4").unwrap()] {
        let json = serde_json::to_value(&r).unwrap();
        assert_eq!(json["engine"], r.engine());
        assert_eq!(serde_json::from_value::<RuntimeRecipe>(json).unwrap(), r);
    }
}

/// biffix-owned plugins are built into the base image from other files in the repo (the wheel
/// build's pins, the plugins' own build files, the entrypoint's plugin id); a pin table that
/// disagrees with them names a runtime no base image can build.
#[test]
fn the_pin_tables_match_what_the_base_image_ships() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let read = |rel: &str| std::fs::read_to_string(root.join(rel)).unwrap();
    let has_line = |rel: &str, want: String| {
        assert!(read(rel).lines().any(|l| l == want), "{rel} lacks `{want}`");
    };
    let sm = snakemake_9();
    has_line("docker/coordinator/wheels.env", format!("EXECUTOR_SHA={}", sm.executor_sha));
    has_line(
        "plugins/snakemake-logger-plugin-biffix/pyproject.toml",
        format!("version = \"{}\"", sm.logger_version),
    );
    // (The coordinator enables whichever nf-biffix its runtime layer carries, so the entrypoint
    // names no version of its own.)
    let nf = nextflow_25("25.10.4").unwrap();
    has_line("plugins/nf-biffix/build.gradle", format!("version = '{}'", nf.nf_biffix_version));
}

/// The build scripts spell the names this crate gives the layer images, the shim and the
/// derived tag; a rename on one side only would leave derives appending nothing.
#[test]
fn the_build_scripts_use_the_crate_s_names() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let read = |rel: &str| std::fs::read_to_string(root.join(rel)).unwrap();
    let runtime = read("codebuild/runtime/build.sh");
    assert!(runtime.contains(&format!("\"{RUNTIME_LAYER_TAG_PREFIX}${{KEY12}}-$1\"")));
    assert!(runtime.contains(&format!("{}/python", crate::images::SHIM_DIR)));
    let import = read("codebuild/image/import.sh");
    assert!(import.contains(&format!("SHIM_DIR={}\n", crate::images::SHIM_DIR)));
    assert!(import.contains(&format!(
        "\"$REPO:{}${{BIFFIX_DERIVED_KEY:0:12}}\"",
        crate::images::DERIVED_TAG_PREFIX
    )));
    let build_tag =
        format!("\"$REPO:{}${{BIFFIX_CONTENT_KEY:0:12}}", crate::images::BUILD_TAG_PREFIX);
    assert!(import.contains(&format!("{build_tag}\"")));
    assert!(read("codebuild/image/build.sh").contains(&format!("{build_tag}-$ARCH\"")));
}

#[test]
fn the_coordinator_subject_joins_key_and_base() {
    assert_eq!(coordinator_subject("abc", "sha256:def"), "coordinator:abc@sha256:def");
}
