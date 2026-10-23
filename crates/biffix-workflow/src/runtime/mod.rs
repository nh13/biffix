//! Engine runtimes: what biffix installs to run one exact engine release, and the key that names it.
//!
//! A workflow version pins its engine (`engine_version: "9.3.0"`); biffix builds that release into a
//! **runtime layer** under `/opt/biffix/` once per tenant (CodeBuild) and appends it to the base
//! coordinator image. The [`RuntimeRecipe`] is every input of that build — the engine release plus
//! the pins below — and its [`runtime_key`] is the sha256 of the recipe, so two versions pinning the
//! same release share one runtime, and any pin change builds a new one.
//!
//! The pin tables are per engine major: every Snakemake 9.x release runs with the same executor,
//! storage and logger plugins, every Nextflow 25.x with the same nf-biffix. biffix-owned code (the
//! executor fork, the logger, nf-biffix) is not downloaded by the build: it ships as wheels in the
//! base coordinator image (`/opt/biffix/wheels/`), and the build checks the base carries exactly the
//! versions the recipe names.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use regex::Regex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::engines::{spelling, supported_range, EngineVersion};

mod pins;
#[cfg(test)]
mod tests;

use pins::{nextflow_25, snakemake_9};

/// Bumped when the runtime build itself changes what it produces from the same pins (a new layout
/// under `/opt/biffix/`, a new wrapper script), so every runtime is rebuilt.
///
/// 2: the Snakemake layer carries the `python` shims derived task images put first on `PATH`, and
/// is stored as its own image (`runtime-<key12>-<arch>`) so task images can be derived from it.
pub const RECIPE_VERSION: u32 = 2;

/// Tag prefix of a stored runtime layer image in the coordinator repository: the runtime build
/// (`codebuild/runtime/build.sh`) tags each layer `runtime-<key12>-<arch>`, and a test ties the
/// script's spelling to this constant. The repository's tags are immutable, so it is pushed once.
pub const RUNTIME_LAYER_TAG_PREFIX: &str = "runtime-";

/// A python-build-standalone CPython, verified against a sha256 per architecture.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PythonBuild {
    /// python-build-standalone release tag (`20260924`).
    pub release: String,
    /// CPython version inside it (`3.12.14`).
    pub version: String,
    /// sha256 of `cpython-<version>+<release>-x86_64-unknown-linux-gnu-install_only.tar.gz`.
    pub sha256_x86_64: String,
    /// sha256 of the same for `aarch64`.
    pub sha256_aarch64: String,
}

/// The resolver the Snakemake build installs with. It runs only on the (x86_64) build host, which
/// installs both architectures' packages without executing them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UvBuild {
    pub version: String,
    /// sha256 of `uv-x86_64-unknown-linux-gnu.tar.gz`.
    pub sha256_x86_64: String,
}

/// Everything a Snakemake runtime installs besides Snakemake itself.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnakemakePins {
    pub python: PythonBuild,
    pub uv: UvBuild,
    /// `snakemake-storage-plugin-s3` release (task I/O goes through S3).
    pub storage_s3: String,
    /// Commit of the AWS Batch executor fork, built into a wheel in the base image. The fork
    /// (branch `biffix-pin/2026-10-03`) is upstream (as of #54; per-rule queues are upstream's
    /// #40, `--aws-batch-spot-attempts` its #62) plus three open upstream PRs: #55 (remote job-state
    /// events, with the job's image), #60 (the strict container image map) and #61
    /// (`--aws-batch-log-group`). Re-pin to upstream once they merge.
    pub executor_sha: String,
    /// `snakemake-logger-plugin-biffix` version, built into a wheel in the base image.
    pub logger_version: String,
    /// Resolution cutoff for every PyPI package except Snakemake (which is pinned exactly and may be
    /// newer): the build resolves a hash-locked set as of this instant, so the same recipe resolves
    /// the same packages next month.
    pub exclude_newer: String,
}

/// Everything a Nextflow runtime installs besides the framework.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NextflowPins {
    /// nf-biffix plugin version, shipped as a zip in the base image.
    pub nf_biffix_version: String,
    /// What the build downloads for this release, by sha256.
    pub release: NextflowRelease,
    /// The plugins the workflow itself declares (its `nextflow.config` `plugins {}`, e.g. nf-core's
    /// nf-schema), pinned by sha256 at registration and built into the layer, since the coordinator
    /// runs Nextflow offline. Sorted by id, one per id (see [`RuntimeRecipe::with_workflow_plugins`]).
    /// Empty for a workflow that declares none, which leaves it out of the serialization, and so of
    /// the key: such a recipe keys as the release's own.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub workflow_plugins: Vec<WorkflowPlugin>,
}

/// A Nextflow plugin a workflow declares, at an exact version, with the sha256 of its zip (the
/// plugin registry's download, [`plugin_zip_url`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowPlugin {
    pub id: String,
    pub version: String,
    pub sha256: String,
}

/// The most plugins one workflow may declare.
const MAX_WORKFLOW_PLUGINS: usize = 32;

/// The plugins biffix's own Nextflow runtime provides, at the versions its recipe pins: a workflow
/// cannot declare them (the coordinator enables them itself).
pub const RUNTIME_NEXTFLOW_PLUGINS: [&str; 3] = ["nf-amazon", "nf-cloudcache", "nf-biffix"];

/// Why a workflow's declared plugins are refused; the message is meant for the workflow's author.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct WorkflowPluginError(pub String);

/// Where the CLI downloads a plugin's zip to pin it. The runtime build (codebuild/runtime/build.sh
/// `install_nf_plugin`) spells the same URL itself; a test here keeps the two in step.
pub fn plugin_zip_url(id: &str, version: &str) -> String {
    format!(
        "https://registry.nextflow.io/api/v1/plugins/{id}/{version}/download/{id}-{version}.zip"
    )
}

/// Check one declared plugin's id and version: a plain id that is not one of
/// [`RUNTIME_NEXTFLOW_PLUGINS`] nor starts with one of them and `-` (the coordinator finds each of
/// biffix's plugins in the layer as the one `<id>-<version>` directory, which `nf-biffix-x-1.0.0`
/// would also match), and an exact version (a range such as `>=1.0` has characters the charset
/// excludes). Both go into a URL, a directory name and the build's environment, hence the narrow
/// charsets.
fn validate_plugin_id_version(id: &str, version: &str) -> Result<(), WorkflowPluginError> {
    static ID: OnceLock<Regex> = OnceLock::new();
    static VERSION: OnceLock<Regex> = OnceLock::new();
    let id_re = ID.get_or_init(|| Regex::new(r"^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$").unwrap());
    let version_re =
        VERSION.get_or_init(|| Regex::new(r"^[0-9A-Za-z][0-9A-Za-z.+_-]{0,63}$").unwrap());
    if !id_re.is_match(id) {
        return Err(WorkflowPluginError(format!(
            "Nextflow plugin id `{id}` is not a plain plugin id"
        )));
    }
    if RUNTIME_NEXTFLOW_PLUGINS.contains(&id) {
        return Err(WorkflowPluginError(format!(
            "Nextflow plugin `{id}`: biffix's runtime provides {id}; do not declare it"
        )));
    }
    if let Some(own) = RUNTIME_NEXTFLOW_PLUGINS
        .iter()
        .find(|own| id.strip_prefix(**own).is_some_and(|rest| rest.starts_with('-')))
    {
        return Err(WorkflowPluginError(format!(
            "Nextflow plugin `{id}`: an id starting with `{own}-` would be mistaken for biffix's \
             {own} in the runtime layer, so biffix cannot carry it"
        )));
    }
    if !version_re.is_match(version) {
        return Err(WorkflowPluginError(format!(
            "Nextflow plugin `{id}@{version}`: the version must be one exact release (e.g. 2.7.2)"
        )));
    }
    Ok(())
}

/// Parse a manifest `plugins:` entry, `id@version`, and check it ([`validate_plugin_id_version`]).
fn parse_plugin_spec(spec: &str) -> Result<(String, String), WorkflowPluginError> {
    let Some((id, version)) = spec.split_once('@') else {
        return Err(WorkflowPluginError(format!(
            "Nextflow plugin `{spec}` has no version: pin one as `{spec}@<version>` (the \
             coordinator runs Nextflow offline, so it cannot pick the latest)"
        )));
    };
    validate_plugin_id_version(id, version)?;
    Ok((id.to_string(), version.to_string()))
}

/// Parse a manifest's `plugins:` list (each `id@version`): every entry checked, no id twice, at
/// most 32 (`MAX_WORKFLOW_PLUGINS`). Sorted by id.
pub fn parse_plugin_specs<S: AsRef<str>>(
    specs: &[S],
) -> Result<Vec<(String, String)>, WorkflowPluginError> {
    let mut out =
        specs.iter().map(|s| parse_plugin_spec(s.as_ref())).collect::<Result<Vec<_>, _>>()?;
    check_count_and_duplicates(out.iter().map(|(id, _)| id.as_str()))?;
    out.sort();
    Ok(out)
}

fn check_count_and_duplicates<'a, I: Iterator<Item = &'a str>>(
    ids: I,
) -> Result<(), WorkflowPluginError> {
    let mut seen = std::collections::BTreeSet::new();
    for id in ids {
        if !seen.insert(id) {
            return Err(WorkflowPluginError(format!(
                "Nextflow plugin `{id}` is declared more than once"
            )));
        }
    }
    if seen.len() > MAX_WORKFLOW_PLUGINS {
        return Err(WorkflowPluginError(format!(
            "a workflow may declare at most {MAX_WORKFLOW_PLUGINS} Nextflow plugins, not {}",
            seen.len()
        )));
    }
    Ok(())
}

/// Check a workflow's pinned plugins (what registration receives): each id and version
/// ([`validate_plugin_id_version`]), a sha256 of 64 lowercase hex digits, no id twice, at most
/// [`MAX_WORKFLOW_PLUGINS`]. Returns them sorted by id, the order a recipe keeps.
fn validate_workflow_plugins(
    mut plugins: Vec<WorkflowPlugin>,
) -> Result<Vec<WorkflowPlugin>, WorkflowPluginError> {
    for p in &plugins {
        validate_plugin_id_version(&p.id, &p.version)?;
        if !crate::digest::is_sha256_hex(&p.sha256) {
            return Err(WorkflowPluginError(format!(
                "Nextflow plugin `{}@{}`: sha256 must be 64 lowercase hex digits",
                p.id, p.version
            )));
        }
    }
    check_count_and_duplicates(plugins.iter().map(|p| p.id.as_str()))?;
    plugins.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(plugins)
}

/// The downloads of one Nextflow release: its self-contained `-dist` launcher and the nf-amazon
/// and nf-cloudcache releases it defaults to (its `META-INF/plugins-info.txt`, which the build
/// checks), each by the sha256 of the file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NextflowRelease {
    /// sha256 of `nextflow-<release>-dist` (the GitHub release asset).
    pub launcher_sha256: String,
    pub nf_amazon: PluginPin,
    pub nf_cloudcache: PluginPin,
}

/// A Nextflow plugin release and the sha256 of its zip (the plugin registry's download).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginPin {
    pub version: String,
    pub sha256: String,
}

/// Every input of one runtime build. Serialized (and hashed) as-is.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
// A recipe is built once per registration and never stored in bulk, so the Snakemake variant's
// size is immaterial; boxing it would only complicate the internally-tagged serde derivation.
#[allow(clippy::large_enum_variant)]
#[serde(tag = "engine", rename_all = "snake_case")]
pub enum RuntimeRecipe {
    Snakemake { recipe_version: u32, engine_version: String, pins: SnakemakePins },
    Nextflow { recipe_version: u32, engine_version: String, pins: NextflowPins },
}

/// The runtime recipe for `engine` at `version`, or `None` when biffix cannot build one (an
/// unknown engine, a version outside [`crate::engines::SUPPORTED`], or a Nextflow release whose
/// downloads are not pinned).
pub fn recipe_for(engine: &str, version: EngineVersion) -> Option<RuntimeRecipe> {
    if !supported_range(engine)?.contains(version) {
        return None;
    }
    // Spelled as the engine writes it (Nextflow zero-pads the month: `25.04.0`), so the build can
    // pass it straight to the engine, and as registration records it.
    let engine_version = spelling(engine, version);
    match (engine, version.major) {
        ("snakemake", 9) => Some(RuntimeRecipe::Snakemake {
            recipe_version: RECIPE_VERSION,
            engine_version,
            pins: snakemake_9(),
        }),
        ("nextflow", 25) => nextflow_25(&engine_version).map(|pins| RuntimeRecipe::Nextflow {
            recipe_version: RECIPE_VERSION,
            engine_version,
            pins,
        }),
        _ => None,
    }
}

/// The runtime's identity: sha256 (hex) of the serialized recipe.
pub fn runtime_key(recipe: &RuntimeRecipe) -> String {
    let bytes = serde_json::to_vec(recipe).expect("a recipe of strings always serializes");
    hex::encode(Sha256::digest(bytes))
}

/// The build subject of the coordinator variant = `base@base_digest` + the runtime's layer.
pub fn coordinator_subject(runtime_key: &str, base_digest: &str) -> String {
    format!("coordinator:{runtime_key}@{base_digest}")
}

impl RuntimeRecipe {
    pub fn engine(&self) -> &'static str {
        match self {
            Self::Snakemake { .. } => crate::engines::Engine::Snakemake.as_str(),
            Self::Nextflow { .. } => crate::engines::Engine::Nextflow.as_str(),
        }
    }

    pub fn engine_version(&self) -> &str {
        match self {
            Self::Snakemake { engine_version, .. } | Self::Nextflow { engine_version, .. } => {
                engine_version
            }
        }
    }

    /// The [`RECIPE_VERSION`] the recipe was made with.
    pub fn recipe_version(&self) -> u32 {
        match self {
            Self::Snakemake { recipe_version, .. } | Self::Nextflow { recipe_version, .. } => {
                *recipe_version
            }
        }
    }

    /// The recipe with the workflow's own Nextflow plugins built in ([`NextflowPins::
    /// workflow_plugins`]), checked by `validate_workflow_plugins` and kept sorted by id so the
    /// same set always gives the same key. None is a no-op for either engine; a Snakemake workflow
    /// has no Nextflow plugins, so any is refused.
    pub fn with_workflow_plugins(
        self,
        plugins: Vec<WorkflowPlugin>,
    ) -> Result<Self, WorkflowPluginError> {
        if plugins.is_empty() {
            return Ok(self);
        }
        match self {
            Self::Snakemake { .. } => {
                Err(WorkflowPluginError("Nextflow plugins apply only to Nextflow workflows".into()))
            }
            Self::Nextflow { recipe_version, engine_version, mut pins } => {
                pins.workflow_plugins = validate_workflow_plugins(plugins)?;
                Ok(Self::Nextflow { recipe_version, engine_version, pins })
            }
        }
    }

    /// The workflow's own Nextflow plugins the runtime carries (none for Snakemake).
    pub fn workflow_plugins(&self) -> &[WorkflowPlugin] {
        match self {
            Self::Snakemake { .. } => &[],
            Self::Nextflow { pins, .. } => &pins.workflow_plugins,
        }
    }

    /// The version of a plugin biffix's Nextflow runtime itself carries ([`RUNTIME_NEXTFLOW_PLUGINS`]),
    /// or `None` (not a Nextflow recipe, or an unknown id).
    pub fn runtime_plugin_version(&self, id: &str) -> Option<&str> {
        let Self::Nextflow { pins, .. } = self else {
            return None;
        };
        match id {
            "nf-biffix" => Some(&pins.nf_biffix_version),
            "nf-amazon" => Some(&pins.release.nf_amazon.version),
            "nf-cloudcache" => Some(&pins.release.nf_cloudcache.version),
            _ => None,
        }
    }

    /// The `snakemake-storage-plugin-s3` a Snakemake runtime installs.
    pub fn storage_s3(&self) -> Option<&str> {
        match self {
            Self::Snakemake { pins, .. } => Some(&pins.storage_s3),
            Self::Nextflow { .. } => None,
        }
    }

    /// The recipe as the environment variables the runtime buildspec reads
    /// (`codebuild/runtime/build.sh`); this map is that script's whole input contract.
    pub fn build_env(&self) -> BTreeMap<&'static str, String> {
        let mut env = BTreeMap::from([
            ("BIFFIX_RT_KEY", runtime_key(self)),
            ("BIFFIX_RT_ENGINE", self.engine().to_string()),
            ("BIFFIX_RT_ENGINE_VERSION", self.engine_version().to_string()),
        ]);
        match self {
            Self::Snakemake { pins, .. } => env.extend([
                ("BIFFIX_RT_PYTHON_RELEASE", pins.python.release.clone()),
                ("BIFFIX_RT_PYTHON_VERSION", pins.python.version.clone()),
                ("BIFFIX_RT_PYTHON_SHA256_X86_64", pins.python.sha256_x86_64.clone()),
                ("BIFFIX_RT_PYTHON_SHA256_AARCH64", pins.python.sha256_aarch64.clone()),
                ("BIFFIX_RT_UV_VERSION", pins.uv.version.clone()),
                ("BIFFIX_RT_UV_SHA256_X86_64", pins.uv.sha256_x86_64.clone()),
                ("BIFFIX_RT_STORAGE_S3", pins.storage_s3.clone()),
                ("BIFFIX_RT_EXECUTOR_SHA", pins.executor_sha.clone()),
                ("BIFFIX_RT_LOGGER_VERSION", pins.logger_version.clone()),
                ("BIFFIX_RT_EXCLUDE_NEWER", pins.exclude_newer.clone()),
            ]),
            Self::Nextflow { pins, .. } => {
                env.insert("BIFFIX_RT_NF_BIFFIX_VERSION", pins.nf_biffix_version.clone());
                let r = &pins.release;
                env.extend([
                    ("BIFFIX_RT_NF_SHA256", r.launcher_sha256.clone()),
                    ("BIFFIX_RT_NF_AMAZON_VERSION", r.nf_amazon.version.clone()),
                    ("BIFFIX_RT_NF_AMAZON_SHA256", r.nf_amazon.sha256.clone()),
                    ("BIFFIX_RT_NF_CLOUDCACHE_VERSION", r.nf_cloudcache.version.clone()),
                    ("BIFFIX_RT_NF_CLOUDCACHE_SHA256", r.nf_cloudcache.sha256.clone()),
                ]);
                // `id@version@sha256`, space-separated (the charsets admit neither `@` in a
                // version nor a space anywhere).
                if !pins.workflow_plugins.is_empty() {
                    let plugins: Vec<String> = pins
                        .workflow_plugins
                        .iter()
                        .map(|p| format!("{}@{}@{}", p.id, p.version, p.sha256))
                        .collect();
                    env.insert("BIFFIX_RT_NF_WORKFLOW_PLUGINS", plugins.join(" "));
                }
            }
        }
        env
    }
}
