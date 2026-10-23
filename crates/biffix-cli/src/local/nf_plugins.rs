//! A Nextflow workflow's own plugins (its `nextflow.config` `plugins {}`, e.g. nf-core's
//! nf-schema). The coordinator runs Nextflow offline, so they must be in the version's runtime
//! layer: `biffix inspect` reads them from the pipeline (`nextflow config`) into `biffix.yaml`'s
//! `plugins:`, `register` checks that list against the pipeline the same way, and pins each by the
//! sha256 of its registry zip for the runtime build.

use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use biffix_api::NextflowPluginRequest;
use biffix_workflow::runtime::{
    parse_plugin_specs, plugin_zip_url, recipe_for, RuntimeRecipe, RUNTIME_NEXTFLOW_PLUGINS,
};
use reqwest::StatusCode;
use sha2::{Digest, Sha256};

use crate::error::{CliError, CliResult};

/// The Nextflow runtime `engine_version` names, if biffix has one.
fn nextflow_recipe(engine_version: Option<&str>) -> Option<RuntimeRecipe> {
    let pinned =
        biffix_workflow::engines::check_engine_version("nextflow", engine_version?).ok()?;
    recipe_for("nextflow", pinned.version)
}

// ── Running Nextflow ─────────────────────────────────────────────────────────────────────────────

/// Why `nextflow` gave no answer.
pub(crate) enum NextflowError {
    /// `nextflow` is not on the PATH.
    NotInstalled,
    /// It ran and failed, or its output could not be read.
    Failed(CliError),
    /// It answered, and what the pipeline declares cannot run offline on biffix (an unversioned
    /// plugin, one of biffix's own at another version, an id `plugins:` could not hold).
    Invalid(CliError),
}

impl NextflowError {
    /// As the error `biffix inspect` reports.
    pub(crate) fn into_cli(self) -> CliError {
        match self {
            NextflowError::NotInstalled => CliError::InvalidInput(
                "`nextflow` is not on your PATH; install Nextflow \
                 (https://www.nextflow.io/docs/latest/install.html) to inspect a Nextflow pipeline"
                    .into(),
            ),
            NextflowError::Failed(e) | NextflowError::Invalid(e) => e,
        }
    }
}

/// The Nextflow launcher: the `nextflow` on the PATH.
const NEXTFLOW: &str = "nextflow";

/// Run `<nextflow> <command> <args> .` in `root`; its stdout.
fn try_run_nextflow(
    nextflow: &str,
    root: &Path,
    command: &str,
    args: &[String],
) -> Result<String, NextflowError> {
    let output = Command::new(nextflow)
        .arg(command)
        .args(args)
        .arg(".")
        .current_dir(root)
        .output()
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => NextflowError::NotInstalled,
            _ => NextflowError::Failed(CliError::Unclassified(format!(
                "running `nextflow {command}`: {e}"
            ))),
        })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let tail: Vec<&str> = stderr.lines().rev().take(10).collect();
        return Err(NextflowError::Failed(CliError::InvalidInput(format!(
            "`nextflow {command}` failed ({}):\n{}",
            output.status,
            tail.into_iter().rev().collect::<Vec<_>>().join("\n")
        ))));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Run `nextflow <command> <args> .` in `root`; its stdout.
pub(crate) fn run_nextflow(root: &Path, command: &str, args: &[String]) -> CliResult<String> {
    try_run_nextflow(NEXTFLOW, root, command, args).map_err(NextflowError::into_cli)
}

// ── What the pipeline declares ───────────────────────────────────────────────────────────────────

/// The arguments of the user's `nextflow inspect` that also go to `nextflow config`: only a
/// `-profile` (`-profile x` or `-profile=x`), so profile-scoped config evaluates as it will for the
/// run. `nextflow config` (25.10) takes no `-params-file`, `-c` or `--param`, so config that
/// branches on a param sees its default. The pipeline's plugins at run time are its own config's:
/// biffix adds none through `-c`.
fn profile_args(args: &[String]) -> Vec<String> {
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "-profile" {
            if let Some(p) = it.next() {
                return vec!["-profile".into(), p.clone()];
            }
        } else if let Some(p) = a.strip_prefix("-profile=") {
            return vec!["-profile".into(), p.to_string()];
        }
    }
    Vec::new()
}

/// The `plugins` list of `nextflow config -output json`'s document (none when it has no key).
fn plugins_of_config_json(json: &str) -> CliResult<Vec<String>> {
    let doc: serde_json::Value = serde_json::from_str(json).map_err(|e| {
        CliError::Unclassified(format!("`nextflow config` printed no JSON document: {e}"))
    })?;
    match doc.get("plugins") {
        None | Some(serde_json::Value::Null) => Ok(Vec::new()),
        Some(serde_json::Value::Array(items)) => items
            .iter()
            .map(|i| {
                i.as_str().map(str::to_string).ok_or_else(|| {
                    CliError::Unclassified(format!(
                        "`nextflow config`: a plugin is not a string: {i}"
                    ))
                })
            })
            .collect(),
        Some(other) => Err(CliError::Unclassified(format!(
            "`nextflow config`: `plugins` is not a list: {other}"
        ))),
    }
}

/// The plugins the pipeline at `root` declares, as `biffix.yaml`'s `plugins:` would list them
/// ([`pipeline_plugins`]), read with `nextflow config -output json` (given the `-profile` among
/// `args`, [`profile_args`]). `biffix inspect` and `register` both read them this way.
pub(crate) fn detect(
    root: &Path,
    args: &[String],
    engine_version: Option<&str>,
) -> Result<Vec<String>, NextflowError> {
    detect_with(NEXTFLOW, root, args, engine_version)
}

fn detect_with(
    nextflow: &str,
    root: &Path,
    args: &[String],
    engine_version: Option<&str>,
) -> Result<Vec<String>, NextflowError> {
    let config_args =
        [["-output".to_string(), "json".to_string()].as_slice(), &profile_args(args)].concat();
    let json = try_run_nextflow(nextflow, root, "config", &config_args)?;
    let declared = plugins_of_config_json(&json).map_err(NextflowError::Failed)?;
    pipeline_plugins(&declared, engine_version).map_err(NextflowError::Invalid)
}

/// The plugins a pipeline declares (`nextflow config`'s `plugins`) as `biffix.yaml`'s `plugins:`
/// lists them: sorted, `id@version`, without the ones biffix's own runtime provides (nf-amazon,
/// nf-cloudcache, nf-biffix), unversioned or at the versions the runtime of `engine_version`
/// carries. Any other unversioned plugin, one of biffix's at another version, or a plugin
/// `biffix.yaml` could not declare is an error for the author.
fn pipeline_plugins<S: AsRef<str>>(
    declared: &[S],
    engine_version: Option<&str>,
) -> CliResult<Vec<String>> {
    let recipe = nextflow_recipe(engine_version);
    let mut keep = Vec::new();
    for entry in declared {
        let entry = entry.as_ref();
        let (id, version) = match entry.split_once('@') {
            Some((id, version)) => (id, Some(version)),
            None => (entry, None),
        };
        if RUNTIME_NEXTFLOW_PLUGINS.contains(&id) {
            // Unversioned, Nextflow resolves it offline to the one the layer carries.
            let Some(version) = version else { continue };
            match recipe.as_ref().and_then(|r| r.runtime_plugin_version(id)) {
                Some(pinned) if pinned == version => continue,
                Some(pinned) => {
                    return Err(CliError::InvalidInput(format!(
                        "nextflow.config declares {entry}, but biffix's runtime for Nextflow {} \
                         carries {id}@{pinned}; declare that version, or none, or drop it",
                        engine_version.unwrap_or_default()
                    )))
                }
                None => {
                    return Err(CliError::InvalidInput(format!(
                        "nextflow.config declares {entry}, which biffix's runtime provides; set a \
                         supported `engine_version` in biffix.yaml so its version can be checked"
                    )))
                }
            }
        }
        if version.is_none() {
            return Err(CliError::InvalidInput(format!(
                "nextflow.config declares the plugin `{entry}` without a version; pin one in \
                 nextflow.config (`id '{entry}@<version>'`): biffix runs Nextflow offline, so it \
                 cannot fetch the latest"
            )));
        }
        keep.push(entry);
    }
    let parsed = parse_plugin_specs(&keep)
        .map_err(|e| CliError::InvalidInput(format!("nextflow.config: {e}")))?;
    Ok(parsed.into_iter().map(|(id, version)| format!("{id}@{version}")).collect())
}

/// `id@version` as `(id, version)` (`(spec, "")` without an `@`), for comparing lists.
fn split_spec(spec: &str) -> (&str, &str) {
    spec.split_once('@').unwrap_or((spec, ""))
}

/// Whether `listed` (`biffix.yaml`'s `plugins:`) is a different set of `(id, version)` from
/// `detected` (the pipeline's). Order does not count.
pub(crate) fn plugins_changed(detected: &[String], listed: &[String]) -> bool {
    fn set(l: &[String]) -> BTreeSet<(&str, &str)> {
        l.iter().map(|s| split_spec(s)).collect()
    }
    set(detected) != set(listed)
}

/// The `detected` plugins (`id@version`) that `listed` (`biffix.yaml`'s, checked) lacks at that
/// version. `listed` may hold more: a profile can add plugins.
fn undeclared(detected: &[String], listed: &[(String, String)]) -> Vec<String> {
    let listed: BTreeSet<(&str, &str)> =
        listed.iter().map(|(id, v)| (id.as_str(), v.as_str())).collect();
    detected.iter().filter(|d| !listed.contains(&split_spec(d))).cloned().collect()
}

/// The `id` entries (`id 'x@1'` or `id "x@1"`) of the `plugins { … }` blocks in a
/// `nextflow.config`'s text: a text read, which sees neither `includeConfig` nor profiles.
fn config_text_plugins(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(at) = rest.find("plugins") {
        let before = &rest[..at];
        let after = &rest[at + "plugins".len()..];
        rest = after;
        // The `plugins` scope itself: at the start of a statement, followed by `{`.
        let starts_statement =
            before.chars().next_back().is_none_or(|c| c.is_whitespace() || c == ';' || c == '}');
        let Some(body) = after.trim_start().strip_prefix('{') else {
            continue;
        };
        if !starts_statement {
            continue;
        }
        let body = &body[..body.find('}').unwrap_or(body.len())];
        for stmt in body.split(['\n', ';']) {
            let Some(arg) = stmt.trim().strip_prefix("id") else {
                continue;
            };
            let arg = arg.trim();
            for q in ['\'', '"'] {
                if let Some(spec) = arg.strip_prefix(q).and_then(|a| a.split(q).next()) {
                    out.push(spec.to_string());
                }
            }
        }
    }
    out
}

/// Check `biffix.yaml`'s `plugins:` (`listed`, checked) against what the pipeline at `root`
/// declares, read as `biffix inspect` reads it (no profile): every plugin it declares must be
/// listed at that version, else every run fails offline, so that is an error, as is a declared
/// plugin that cannot run offline at all ([`pipeline_plugins`]). Without `nextflow`
/// on the PATH, `nextflow.config` next to the entrypoint is read as text instead, and what it names
/// beyond biffix's own plugins and `listed` is only a warning (the text read misses
/// `includeConfig`). A `nextflow config` that fails is also only a warning.
pub(crate) fn check_declared(
    root: &Path,
    entrypoint: &str,
    listed: &[(String, String)],
    engine_version: &str,
) -> CliResult<()> {
    check_declared_with(NEXTFLOW, root, entrypoint, listed, engine_version)
}

fn check_declared_with(
    nextflow: &str,
    root: &Path,
    entrypoint: &str,
    listed: &[(String, String)],
    engine_version: &str,
) -> CliResult<()> {
    let lists = || listed.iter().map(|(id, v)| format!("{id}@{v}")).collect::<Vec<_>>().join(", ");
    match detect_with(nextflow, root, &[], Some(engine_version)) {
        Ok(detected) => {
            let missing = undeclared(&detected, listed);
            if missing.is_empty() {
                return Ok(());
            }
            Err(CliError::InvalidInput(format!(
                "the pipeline declares Nextflow plugins biffix.yaml `plugins:` does not list at \
                 that version: {} (biffix.yaml lists [{}]); biffix runs Nextflow offline, so \
                 every run would fail. Run `biffix inspect --write` to set `plugins:`",
                missing.join(", "),
                lists()
            )))
        }
        // What it declares cannot run offline whatever `plugins:` lists.
        Err(NextflowError::Invalid(e)) => Err(e),
        Err(NextflowError::Failed(e)) => {
            eprintln!(
                "[biffix] warning: could not read the pipeline's Nextflow plugins to check \
                 biffix.yaml `plugins:` against them: {e}"
            );
            Ok(())
        }
        Err(NextflowError::NotInstalled) => {
            // The entrypoint's directory, or the entrypoint itself when it names one.
            let entry = root.join(entrypoint);
            let dir = if entry.is_dir() {
                entry
            } else {
                entry.parent().map_or_else(|| root.to_path_buf(), Path::to_path_buf)
            };
            let Ok(text) = std::fs::read_to_string(dir.join("nextflow.config")) else {
                return Ok(());
            };
            let ids: BTreeSet<&str> = listed.iter().map(|(id, _)| id.as_str()).collect();
            let missing: Vec<String> = config_text_plugins(&text)
                .into_iter()
                .filter(|spec| {
                    let (id, version) = split_spec(spec);
                    !RUNTIME_NEXTFLOW_PLUGINS.contains(&id)
                        && if version.is_empty() {
                            !ids.contains(id)
                        } else {
                            !listed.iter().any(|(i, v)| i == id && v == version)
                        }
                })
                .collect();
            if !missing.is_empty() {
                eprintln!(
                    "[biffix] warning: nextflow.config declares Nextflow plugins biffix.yaml \
                     `plugins:` does not list: {} (biffix.yaml lists [{}]); runs would fail \
                     (biffix runs Nextflow offline). `nextflow` is not on your PATH, so only \
                     nextflow.config's text was read; run `biffix inspect --write` where Nextflow \
                     is installed",
                    missing.join(", "),
                    lists()
                );
            }
            Ok(())
        }
    }
}

// ── Pinning ──────────────────────────────────────────────────────────────────────────────────────

/// How a plugin zip is downloaded to pin it.
struct Download {
    /// Refused once the body passes this, so a wrong URL cannot run forever.
    max_bytes: u64,
    /// One attempt's timeout.
    timeout: Duration,
    /// Attempts in all (a dropped connection, a 5xx or a body that breaks off is retried).
    attempts: u32,
    /// The wait before the second attempt, doubled each time after.
    backoff: Duration,
}

/// The registry's plugin zips (the largest plugins are a few tens of MiB).
const REGISTRY: Download = Download {
    max_bytes: 200 * 1024 * 1024,
    timeout: Duration::from_secs(300),
    attempts: 4,
    backoff: Duration::from_secs(1),
};

/// One download attempt's failure, and whether another attempt may succeed.
enum Failure {
    Retry(CliError),
    Fatal(CliError),
}

/// The sha256 (hex) of the file at `url` (`what`, e.g. `nf-schema@2.7.2`, names it in errors),
/// hashed as it streams, retried per `d`. A public download: no credential is ever sent.
async fn download_sha256(
    http: &reqwest::Client,
    url: &str,
    what: &str,
    d: &Download,
) -> CliResult<String> {
    let mut wait = d.backoff;
    for attempt in 1..=d.attempts {
        match download_once(http, url, what, d).await {
            Ok(sha) => return Ok(sha),
            Err(Failure::Fatal(e)) => return Err(e),
            Err(Failure::Retry(e)) if attempt == d.attempts => return Err(e),
            Err(Failure::Retry(e)) => {
                eprintln!("[biffix] {e}; retrying");
                tokio::time::sleep(wait).await;
                wait = wait.saturating_mul(2);
            }
        }
    }
    unreachable!("at least one attempt")
}

async fn download_once(
    http: &reqwest::Client,
    url: &str,
    what: &str,
    d: &Download,
) -> Result<String, Failure> {
    let at = |m: String| format!("downloading Nextflow plugin {what} from {url}: {m}");
    let mut resp = http.get(url).timeout(d.timeout).send().await.map_err(|e| {
        Failure::Retry(CliError::Unavailable(at(CliError::unreachable(
            "the Nextflow plugin registry",
            &e,
        )
        .to_string())))
    })?;
    let status = resp.status();
    if status == StatusCode::NOT_FOUND {
        return Err(Failure::Fatal(CliError::InvalidInput(format!(
            "the Nextflow plugin registry has no {what} ({url}); check `plugins:` in biffix.yaml"
        ))));
    }
    if !status.is_success() {
        let m = at(format!("the Nextflow plugin registry answered {status}"));
        return Err(
            if status.is_server_error()
                || status == StatusCode::TOO_MANY_REQUESTS
                || status == StatusCode::REQUEST_TIMEOUT
            {
                Failure::Retry(CliError::Unavailable(m))
            } else {
                Failure::Fatal(CliError::Unclassified(m))
            },
        );
    }
    let too_large = || {
        Failure::Fatal(CliError::InvalidInput(at(format!(
            "larger than {} MiB",
            d.max_bytes / (1024 * 1024)
        ))))
    };
    if resp.content_length().is_some_and(|n| n > d.max_bytes) {
        return Err(too_large());
    }
    let mut hasher = Sha256::new();
    let mut read: u64 = 0;
    loop {
        match resp.chunk().await {
            Ok(Some(chunk)) => {
                read += chunk.len() as u64;
                if read > d.max_bytes {
                    return Err(too_large());
                }
                hasher.update(&chunk);
            }
            Ok(None) => break,
            Err(e) => {
                return Err(Failure::Retry(CliError::Unavailable(at(format!(
                    "the download broke off: {e}"
                )))))
            }
        }
    }
    Ok(hex::encode(hasher.finalize()))
}

/// Pin each of `plugins` (`(id, version)`, checked) by the sha256 of its zip, downloaded from the
/// Nextflow plugin registry (public; needs no biffix session).
pub(crate) async fn pin(plugins: &[(String, String)]) -> CliResult<Vec<NextflowPluginRequest>> {
    pin_with(&reqwest::Client::new(), plugin_zip_url, &REGISTRY, plugins).await
}

async fn pin_with(
    http: &reqwest::Client,
    url_of: impl Fn(&str, &str) -> String,
    d: &Download,
    plugins: &[(String, String)],
) -> CliResult<Vec<NextflowPluginRequest>> {
    let mut out = Vec::with_capacity(plugins.len());
    for (id, version) in plugins {
        let sha256 =
            download_sha256(http, &url_of(id, version), &format!("{id}@{version}"), d).await?;
        eprintln!("[biffix] Nextflow plugin {id}@{version} (sha256 {sha256})");
        out.push(NextflowPluginRequest { id: id.clone(), version: version.clone(), sha256 });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;

    fn err(r: CliResult<Vec<String>>) -> String {
        match r.unwrap_err() {
            CliError::InvalidInput(m) => m,
            other => panic!("expected InvalidInput, got {other:?}"),
        }
    }

    fn strings(list: &[&str]) -> Vec<String> {
        list.iter().map(std::string::ToString::to_string).collect()
    }

    #[test]
    fn the_pipeline_s_plugins_are_listed_without_biffix_s_own() {
        assert_eq!(
            pipeline_plugins(
                &["nf-schema@2.7.2", "nf-amazon@3.4.4", "nf-prov@1.4.0", "nf-biffix@0.6.1"],
                Some("25.10.4")
            )
            .unwrap(),
            ["nf-prov@1.4.0", "nf-schema@2.7.2"]
        );
        assert!(pipeline_plugins::<&str>(&[], None).unwrap().is_empty());
        // Without biffix's own, no engine_version is needed.
        assert_eq!(pipeline_plugins(&["nf-schema@2.7.2"], None).unwrap(), ["nf-schema@2.7.2"]);
        // Unversioned, biffix's own resolve offline to the layer's: dropped, engine_version or not.
        assert_eq!(
            pipeline_plugins(&["nf-amazon", "nf-biffix", "nf-cloudcache", "nf-schema@2.7.2"], None)
                .unwrap(),
            ["nf-schema@2.7.2"]
        );
    }

    #[test]
    fn a_plugin_the_runtime_cannot_carry_is_the_author_s_to_fix() {
        let m = err(pipeline_plugins(&["nf-schema"], Some("25.10.4")));
        assert!(m.contains("without a version") && m.contains("offline"), "{m}");
        let m = err(pipeline_plugins(&["nf-amazon@3.0.0"], Some("25.10.4")));
        assert!(m.contains("runtime for Nextflow 25.10.4 carries nf-amazon@3.4.4"), "{m}");
        let m = err(pipeline_plugins(&["nf-amazon@3.4.4"], None));
        assert!(m.contains("engine_version"), "{m}");
        let m = err(pipeline_plugins(&["nf-schema@2.7.2", "nf-schema@2.7.3"], None));
        assert!(m.contains("more than once"), "{m}");
        let m = err(pipeline_plugins(&["nf-schema@>=2.0"], None));
        assert!(m.contains("one exact release"), "{m}");
    }

    #[test]
    fn plugins_change_only_as_a_set() {
        let a = strings(&["nf-a@1", "nf-a-b@1"]);
        let b = strings(&["nf-a-b@1", "nf-a@1"]);
        assert!(!plugins_changed(&a, &b));
        assert!(!plugins_changed(&b, &a));
        assert!(plugins_changed(&a, &strings(&["nf-a@1", "nf-a-b@2"])));
        assert!(plugins_changed(&a, &strings(&["nf-a@1"])));
        assert!(plugins_changed(&[], &strings(&["nf-a@1"])));
        assert!(!plugins_changed(&[], &[]));
    }

    #[test]
    fn biffix_yaml_must_list_every_detected_plugin_at_its_version() {
        let detected = strings(&["nf-prov@1.4.0", "nf-schema@2.8.0"]);
        assert_eq!(
            undeclared(&detected, &listed(&[("nf-schema", "2.7.2")])),
            ["nf-prov@1.4.0", "nf-schema@2.8.0"]
        );
        // A superset is fine: a profile can add plugins.
        assert!(undeclared(
            &detected,
            &listed(&[("nf-prov", "1.4.0"), ("nf-schema", "2.8.0"), ("nf-x", "1.0.0")])
        )
        .is_empty());
        assert!(undeclared(&[], &[]).is_empty());
    }

    #[test]
    fn a_config_s_plugins_block_is_read_from_its_text() {
        assert_eq!(
            config_text_plugins(
                "manifest { name = 'x' }\nplugins {\n  id 'nf-schema@2.7.2'\n  id \"nf-prov\"\n}\n"
            ),
            ["nf-schema@2.7.2", "nf-prov"]
        );
        assert_eq!(
            config_text_plugins("  plugins{ id 'nf-schema@2.7.2'; id 'nf-amazon' }"),
            ["nf-schema@2.7.2", "nf-amazon"]
        );
        assert!(config_text_plugins("params.plugins = 1\n// no plugins here\n").is_empty());
        assert!(config_text_plugins("pluginsDir = 'x'\nmyplugins { id 'x@1' }\n").is_empty());
    }

    fn listed(l: &[(&str, &str)]) -> Vec<(String, String)> {
        l.iter().map(|(i, v)| (i.to_string(), v.to_string())).collect()
    }

    #[test]
    fn without_nextflow_the_config_text_is_checked_and_only_warned_about() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(
            dir.path().join("sub/nextflow.config"),
            "plugins { id 'nf-schema@2.7.2' }\n",
        )
        .unwrap();
        let missing = dir.path().join("no-such-nextflow");
        let check = |entrypoint: &str| {
            check_declared_with(missing.to_str().unwrap(), dir.path(), entrypoint, &[], "25.10.4")
        };
        assert!(check("sub/main.nf").is_ok());
        // A missing config is nothing to check.
        assert!(check("main.nf").is_ok());
    }

    /// A stand-in `nextflow` whose `config` prints `json` (or fails).
    #[cfg(unix)]
    fn fake_nextflow(dir: &Path, json: Option<&str>) -> String {
        use std::os::unix::fs::PermissionsExt;
        let exe = dir.join("nextflow");
        let body = match json {
            Some(json) => {
                format!("#!/bin/sh\n[ \"$1\" = config ] || exit 9\ncat <<'EOF'\n{json}\nEOF\n")
            }
            None => "#!/bin/sh\necho 'ERROR ~ cannot parse config' >&2\nexit 1\n".into(),
        };
        std::fs::write(&exe, body).unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        exe.to_str().unwrap().to_string()
    }

    #[cfg(unix)]
    #[test]
    fn register_refuses_a_plugins_list_that_drifted_from_the_pipeline() {
        let dir = tempfile::tempdir().unwrap();
        let nf = fake_nextflow(
            dir.path(),
            Some(r#"{"plugins": ["nf-schema@2.8.0", "nf-amazon", "nf-biffix@0.6.1"]}"#),
        );
        let check = |l: &[(&str, &str)]| {
            check_declared_with(&nf, dir.path(), "main.nf", &listed(l), "25.10.4")
        };
        // Matches (biffix's own are not listed); a superset is fine too.
        assert!(check(&[("nf-schema", "2.8.0")]).is_ok());
        assert!(check(&[("nf-schema", "2.8.0"), ("nf-prov", "1.4.0")]).is_ok());
        for stale in [&[("nf-schema", "2.7.2")][..], &[]] {
            match check(stale).unwrap_err() {
                CliError::InvalidInput(m) => assert!(
                    m.contains("does not list at that version: nf-schema@2.8.0")
                        && m.contains("biffix inspect --write"),
                    "{m}"
                ),
                other => panic!("expected InvalidInput, got {other:?}"),
            }
        }
        // A declared plugin that cannot run offline (unversioned) is refused, whatever is listed.
        let bad = tempfile::tempdir().unwrap();
        let nf = fake_nextflow(bad.path(), Some(r#"{"plugins": ["nf-prov"]}"#));
        match check_declared_with(&nf, dir.path(), "main.nf", &[], "25.10.4").unwrap_err() {
            CliError::InvalidInput(m) => assert!(m.contains("without a version"), "{m}"),
            other => panic!("expected InvalidInput, got {other:?}"),
        }
        // A `nextflow config` that fails only warns.
        let failing = tempfile::tempdir().unwrap();
        let nf = fake_nextflow(failing.path(), None);
        assert!(check_declared_with(&nf, dir.path(), "main.nf", &[], "25.10.4").is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn detection_reads_the_pipeline_s_plugins_through_nextflow_config() {
        let dir = tempfile::tempdir().unwrap();
        let nf =
            fake_nextflow(dir.path(), Some(r#"{"plugins": ["nf-schema@2.7.2", "nf-prov@1.4.0"]}"#));
        let got = detect_with(&nf, dir.path(), &[], Some("25.10.4")).ok().unwrap();
        assert_eq!(got, ["nf-prov@1.4.0", "nf-schema@2.7.2"]);
        let missing = dir.path().join("none");
        assert!(matches!(
            detect_with(missing.to_str().unwrap(), dir.path(), &[], None),
            Err(NextflowError::NotInstalled)
        ));
    }

    #[test]
    fn the_profile_alone_reaches_nextflow_config() {
        let args = |a: &[&str]| strings(a);
        assert_eq!(
            profile_args(&args(&["-params-file", "p.json", "-profile", "test,docker"])),
            args(&["-profile", "test,docker"])
        );
        assert_eq!(profile_args(&args(&["-profile=test"])), args(&["-profile", "test"]));
        assert!(profile_args(&args(&["-params-file", "p.json"])).is_empty());
    }

    #[test]
    fn the_config_document_s_plugins_are_read() {
        assert_eq!(
            plugins_of_config_json(r#"{"plugins": ["nf-schema@2.7.2"], "process": {}}"#).unwrap(),
            ["nf-schema@2.7.2"]
        );
        assert!(plugins_of_config_json(r#"{"process": {}}"#).unwrap().is_empty());
        let unclassified = |r: CliResult<Vec<String>>| match r.unwrap_err() {
            CliError::Unclassified(m) => m,
            other => panic!("expected Unclassified, got {other:?}"),
        };
        assert!(
            unclassified(plugins_of_config_json(r#"{"plugins": [1]}"#)).contains("not a string")
        );
        assert!(unclassified(plugins_of_config_json(r#"{"plugins": "x"}"#)).contains("not a list"));
        assert!(unclassified(plugins_of_config_json("not json")).contains("no JSON document"));
    }

    // ── downloads ──

    /// A stub HTTP server: the n-th connection gets `responses[n]` (the last one repeats), raw,
    /// and is closed. Returns its base URL and every request's head as received.
    async fn serve_raw(responses: Vec<Vec<u8>>) -> (String, Arc<Mutex<Vec<String>>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let requests = seen.clone();
        tokio::spawn(async move {
            for n in 0.. {
                let Ok((mut sock, _)) = listener.accept().await else {
                    return;
                };
                let mut head = Vec::new();
                let mut buf = [0u8; 1024];
                while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                    match sock.read(&mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(k) => head.extend_from_slice(&buf[..k]),
                    }
                }
                requests.lock().unwrap().push(String::from_utf8_lossy(&head).into_owned());
                let resp = &responses[n.min(responses.len() - 1)];
                let _ = sock.write_all(resp).await;
                let _ = sock.shutdown().await;
            }
        });
        (base, seen)
    }

    fn ok_with_length(body: &[u8]) -> Vec<u8> {
        let mut r = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .into_bytes();
        r.extend_from_slice(body);
        r
    }

    /// No Content-Length: the body runs to the end of the connection.
    fn ok_unsized(body: &[u8]) -> Vec<u8> {
        let mut r = b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n".to_vec();
        r.extend_from_slice(body);
        r
    }

    fn status(code: &str) -> Vec<u8> {
        format!("HTTP/1.1 {code}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").into_bytes()
    }

    const FAST: Download = Download {
        max_bytes: 1024,
        timeout: Duration::from_secs(10),
        attempts: 3,
        backoff: Duration::from_millis(1),
    };

    async fn pin_from(
        base: &str,
        plugins: &[(&str, &str)],
    ) -> CliResult<Vec<NextflowPluginRequest>> {
        let plugins: Vec<(String, String)> =
            plugins.iter().map(|(i, v)| (i.to_string(), v.to_string())).collect();
        let base = base.to_string();
        pin_with(
            &reqwest::Client::new(),
            move |id, v| format!("{base}/{id}-{v}.zip"),
            &FAST,
            &plugins,
        )
        .await
    }

    #[tokio::test]
    async fn a_pin_is_the_sha256_of_the_zip_and_sends_no_credential() {
        let body = vec![7u8; 1000];
        let (base, seen) = serve_raw(vec![ok_with_length(&body)]).await;
        let got = pin_from(&base, &[("nf-schema", "2.7.2")]).await.unwrap();
        assert_eq!(
            got,
            [NextflowPluginRequest {
                id: "nf-schema".into(),
                version: "2.7.2".into(),
                sha256: crate::local::source::sha256_hex(&body),
            }]
        );
        let seen = seen.lock().unwrap();
        assert!(seen[0].starts_with("GET /nf-schema-2.7.2.zip "), "{}", seen[0]);
        assert!(!seen[0].to_ascii_lowercase().contains("authorization"), "{}", seen[0]);
    }

    #[tokio::test]
    async fn the_cap_holds_on_a_streamed_body_without_a_length() {
        let (base, _) = serve_raw(vec![ok_unsized(&[1u8; 1025])]).await;
        let e = pin_from(&base, &[("nf-schema", "2.7.2")]).await.unwrap_err();
        assert!(
            matches!(e, CliError::InvalidInput(ref m) if m.contains("larger than")
                && m.contains("nf-schema@2.7.2")),
            "{e:?}"
        );
        // At the cap is fine.
        let (base, _) = serve_raw(vec![ok_unsized(&[1u8; 1024])]).await;
        assert!(pin_from(&base, &[("nf-schema", "2.7.2")]).await.is_ok());
        // A declared length over the cap is refused before the body.
        let (base, _) = serve_raw(vec![ok_with_length(&[1u8; 1025])]).await;
        let e = pin_from(&base, &[("nf-schema", "2.7.2")]).await.unwrap_err();
        assert!(matches!(e, CliError::InvalidInput(ref m) if m.contains("larger than")), "{e:?}");
    }

    #[tokio::test]
    async fn a_missing_plugin_names_it_and_the_url() {
        let (base, seen) = serve_raw(vec![status("404 Not Found")]).await;
        let e = pin_from(&base, &[("nf-nope", "1.0.0")]).await.unwrap_err();
        assert!(
            matches!(e, CliError::InvalidInput(ref m)
                if m.contains("registry has no nf-nope@1.0.0")
                    && m.contains(&format!("{base}/nf-nope-1.0.0.zip"))),
            "{e:?}"
        );
        // Not retried.
        assert_eq!(seen.lock().unwrap().len(), 1);
        // Another refusal is the registry's, never biffix's (no `biffix login` for a 401).
        let (base, _) = serve_raw(vec![status("401 Unauthorized")]).await;
        let e = pin_from(&base, &[("nf-x", "1.0.0")]).await.unwrap_err();
        assert!(
            matches!(e, CliError::Unclassified(ref m)
                if m.contains("Nextflow plugin registry answered 401")
                    && m.contains("nf-x@1.0.0")
                    && !m.contains("biffix login")),
            "{e:?}"
        );
    }

    #[tokio::test]
    async fn a_download_that_breaks_off_or_a_5xx_is_retried() {
        let body = vec![9u8; 500];
        // Promises 500 bytes, sends 100, closes; then a 503; then the file.
        let mut cut =
            b"HTTP/1.1 200 OK\r\nContent-Length: 500\r\nConnection: close\r\n\r\n".to_vec();
        cut.extend_from_slice(&body[..100]);
        let (base, seen) =
            serve_raw(vec![cut, status("503 Service Unavailable"), ok_with_length(&body)]).await;
        let got = pin_from(&base, &[("nf-schema", "2.7.2")]).await.unwrap();
        assert_eq!(got[0].sha256, crate::local::source::sha256_hex(&body));
        assert_eq!(seen.lock().unwrap().len(), 3);
        // Out of attempts: the registry's error, naming the plugin.
        let (base, seen) = serve_raw(vec![status("503 Service Unavailable")]).await;
        let e = pin_from(&base, &[("nf-schema", "2.7.2")]).await.unwrap_err();
        assert!(
            matches!(e, CliError::Unavailable(ref m)
                if m.contains("nf-schema@2.7.2") && m.contains("registry answered 503")
                    && !m.contains("biffix API")),
            "{e:?}"
        );
        assert_eq!(seen.lock().unwrap().len(), 3);
    }

    #[tokio::test]
    async fn an_unreachable_registry_is_named_as_such() {
        // A port nothing listens on.
        let port = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        let e =
            pin_from(&format!("http://127.0.0.1:{port}"), &[("nf-x", "1.0.0")]).await.unwrap_err();
        assert!(
            matches!(e, CliError::Unavailable(ref m)
                if m.contains("cannot reach the Nextflow plugin registry")
                    && m.contains("nf-x@1.0.0")),
            "{e:?}"
        );
    }
}
