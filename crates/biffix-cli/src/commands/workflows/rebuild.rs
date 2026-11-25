//! `biffix workflows rebuild`: start a version's failed engine-runtime and image builds again, once
//! their automatic retries are spent (e.g. after a transient upstream outage has passed).

use biffix_api::{VersionRuntime, WorkflowImage, WorkflowVersionDetail};

use super::wait::wait_for_runtime;
use crate::commands::Session;
use crate::error::{CliError, CliResult};
use crate::render::OutputMode;

/// Options for `workflows rebuild`.
pub struct RebuildOpts {
    /// Frozen version id.
    pub id: String,
    /// Rebuild the engine runtime (with no `images`, alone).
    pub runtime: bool,
    /// Rebuild these images (with `runtime` unset, only these).
    pub images: Vec<String>,
    /// Wait (up to this long) until the version can run.
    pub wait: Option<std::time::Duration>,
}

/// What to rebuild: the engine runtime, and which images.
#[derive(Debug, PartialEq)]
struct Targets {
    runtime: bool,
    images: Vec<String>,
}

/// What `--runtime` / `--image` ask for (each image once): neither is the runtime (if the version
/// has one) and every image, since a rebuild leaves anything ready or building as it is. An image
/// the version does not have is refused here, before any build is started.
///
/// A detail with no `state` is one whose builds the server could not read: its runtime and
/// images are then left out, not absent, so nothing can be checked and it is reported as
/// unavailable (named targets too, so a misnamed image never starts a runtime rebuild first).
fn targets(runtime: bool, images: &[String], detail: &WorkflowVersionDetail) -> CliResult<Targets> {
    let mut requested: Vec<String> = Vec::with_capacity(images.len());
    for name in images {
        if !requested.contains(name) {
            requested.push(name.clone());
        }
    }
    if detail.state.is_none() {
        return Err(CliError::Unavailable(format!(
            "{}: the service could not read its runtime and images; try again",
            detail.id
        )));
    }
    let named = runtime || !requested.is_empty();
    let names: Vec<&str> = detail.images.iter().map(|i| i.name.as_str()).collect();
    if !named {
        if detail.runtime.is_none() && names.is_empty() {
            return Err(CliError::InvalidInput(format!(
                "{} has no engine runtime or images to rebuild",
                detail.id
            )));
        }
        return Ok(Targets {
            runtime: detail.runtime.is_some(),
            images: names.iter().map(ToString::to_string).collect(),
        });
    }
    if let Some(unknown) = requested.iter().find(|n| !names.contains(&n.as_str())) {
        let has = if names.is_empty() { "none".to_string() } else { names.join(", ") };
        return Err(CliError::InvalidInput(format!(
            "{} has no image `{unknown}` (its images: {has})",
            detail.id
        )));
    }
    Ok(Targets { runtime, images: requested })
}

/// `biffix workflows rebuild`: start the version's runtime and image builds again (each one the
/// server finds failed; the rest are left as they are), printing each as it is started, then wait
/// until the version can run when `opts.wait` is set. A rebuild that fails does not stop the
/// others; the command then fails naming what was and was not started.
pub async fn rebuild(session: &mut Session, opts: RebuildOpts) -> CliResult<()> {
    let id = super::catalog::version_id(&opts.id)?;
    let client = session.authed_client().await?;
    let detail = client.get_workflow(id).await?;
    let targets = targets(opts.runtime, &opts.images, &detail)?;
    let human = session.output == OutputMode::Human;
    let mut runtime = None;
    let mut images = Vec::with_capacity(targets.images.len());
    let (mut started, mut failed) = (Vec::new(), Vec::new());
    let mut first_error = None;
    if targets.runtime {
        match client.rebuild_runtime(id).await {
            Ok(r) => {
                if human {
                    tprintln!("{}", runtime_line(&r));
                }
                runtime = Some(r);
                started.push("the runtime".to_string());
            }
            Err(e) => {
                failed.push(format!("the runtime ({e})"));
                first_error.get_or_insert(e);
            }
        }
    }
    for name in &targets.images {
        match client.rebuild_image(id, name).await {
            Ok(i) => {
                if human {
                    tprintln!("{}", image_line(&i));
                }
                images.push(i);
                started.push(format!("image {name}"));
            }
            Err(e) => {
                failed.push(format!("image {name} ({e})"));
                first_error.get_or_insert(e);
            }
        }
    }
    if let Some(e) = first_error {
        return Err(e.map_message(|_| not_rebuilt(id, &failed, &started)));
    }
    // The states after the wait, when there was one: the rebuild's answers are as of its start.
    let ready = match opts.wait {
        Some(timeout) => {
            teprintln!("[biffix] waiting until {id} can run…");
            let after = wait_for_runtime(&client, id, timeout).await?;
            runtime = runtime.and(after.runtime);
            images =
                after.images.into_iter().filter(|i| targets.images.contains(&i.name)).collect();
            Some(true)
        }
        None => None,
    };
    match session.output {
        // `ready` is `null` without a wait: the command did not find out.
        OutputMode::Json => crate::render::print_json(&serde_json::json!({
            "id": id, "runtime": runtime, "images": images, "ready": ready,
        })),
        OutputMode::Human if ready.is_some() => tprintln!("{id} is ready to run"),
        OutputMode::Human => {}
    }
    Ok(())
}

/// The error of a rebuild some of whose requests failed: what was not rebuilt (with why), and
/// what was started before or after it.
fn not_rebuilt(id: &str, failed: &[String], started: &[String]) -> String {
    let started = if started.is_empty() {
        String::new()
    } else {
        format!("; started: {}", started.join(", "))
    };
    format!("{id}: could not rebuild {}{started}", failed.join(", "))
}

/// Where a rebuilt runtime is, and, for a failed one, why and its build log.
fn runtime_line(r: &VersionRuntime) -> String {
    format!("runtime: {}{}", r.state, failure(&r.state, r.reason.as_deref(), r.logs_url.as_deref()))
}

/// Where a rebuilt image is (its derive's state too, for a derived image), and, for a failed one,
/// why and its build log. Its `state` covers the derive.
fn image_line(i: &WorkflowImage) -> String {
    let derive = i.derived_state.as_deref().map(|s| format!(" (derive: {s})")).unwrap_or_default();
    let why = failure(&i.state, i.reason.as_deref(), i.logs_url.as_deref());
    format!("image {}: {}{derive}{why}", i.name, i.state)
}

/// For a `failed` build, ` — <reason> (build log: <url>)` (whichever of them there is); else
/// nothing.
fn failure(state: &str, reason: Option<&str>, logs_url: Option<&str>) -> String {
    let mut out = String::new();
    if state != "failed" {
        return out;
    }
    if let Some(reason) = reason {
        out.push_str(&format!(" — {reason}"));
    }
    if let Some(url) = logs_url {
        out.push_str(&format!(" (build log: {url})"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A version detail as `GET /workflows/{id}` sends it: with its readiness (`state`, the runtime
    /// and images) when the server could read it, or with none of them when it could not.
    fn detail(readable: bool, runtime: bool, images: &[&str]) -> WorkflowVersionDetail {
        let mut json = serde_json::json!({
            "id": "a:h:0", "name": "wf", "project": "p", "image_digest": "",
            "engine": "nextflow", "engine_version": "25.10.4", "git_sha": "a",
            "queue_classes": [], "created_at_micros": 0, "source_content_hash": "h",
            "entrypoint": "main.nf", "form_schema": {"fields": [], "warnings": []},
            "source": {"kind": "git", "repo": "r", "git_ref": "main"}, "build": 0,
            "registration_inputs": {"source_mode": "git", "cli_version": "0.0.0"},
        });
        if readable {
            json["state"] = "failed".into();
            json["images"] = images.iter().map(|n| image(n, "failed")).collect();
            if runtime {
                json["runtime"] = serde_json::to_value(runtime_view("failed")).unwrap();
            }
        }
        serde_json::from_value(json).unwrap()
    }

    fn runtime_view(state: &str) -> VersionRuntime {
        serde_json::from_value(serde_json::json!({
            "key": "k", "recipe": {}, "state": state, "ready": false,
        }))
        .unwrap()
    }

    fn image(name: &str, state: &str) -> serde_json::Value {
        serde_json::json!({
            "name": name, "declared_ref": "r", "source_ref": "r", "state": state,
        })
    }

    fn names(n: &[&str]) -> Vec<String> {
        n.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn with_no_flags_the_runtime_and_every_image_are_rebuilt() {
        let d = detail(true, true, &["default", "align"]);
        assert_eq!(
            targets(false, &[], &d).unwrap(),
            Targets { runtime: true, images: names(&["default", "align"]) }
        );
        // A flag narrows it to what it names, each image once.
        assert_eq!(targets(true, &[], &d).unwrap(), Targets { runtime: true, images: vec![] });
        assert_eq!(
            targets(false, &names(&["align", "default", "align"]), &d).unwrap(),
            Targets { runtime: false, images: names(&["align", "default"]) }
        );
        assert_eq!(
            targets(true, &names(&["align"]), &d).unwrap(),
            Targets { runtime: true, images: names(&["align"]) }
        );
    }

    #[test]
    fn an_image_the_version_lacks_or_nothing_to_rebuild_is_refused() {
        let err = targets(true, &names(&["algn"]), &detail(true, true, &["default", "align"]))
            .unwrap_err();
        assert!(
            matches!(&err, CliError::InvalidInput(m)
                if m == "a:h:0 has no image `algn` (its images: default, align)"),
            "{err:?}"
        );
        let err = targets(false, &names(&["x"]), &detail(true, true, &[])).unwrap_err();
        assert!(matches!(&err, CliError::InvalidInput(m) if m.ends_with("(its images: none)")));
        let err = targets(false, &[], &detail(true, false, &[])).unwrap_err();
        assert!(
            matches!(&err, CliError::InvalidInput(m)
                if m == "a:h:0 has no engine runtime or images to rebuild"),
            "{err:?}"
        );
    }

    #[test]
    fn a_version_whose_builds_could_not_be_read_is_unavailable() {
        let unread = detail(false, true, &["default"]);
        // Named targets too: they cannot be checked, and a misnamed image must start nothing.
        for (runtime, images) in [(false, vec![]), (true, names(&["default"])), (true, vec![])] {
            let err = targets(runtime, &images, &unread).unwrap_err();
            assert!(
                matches!(&err, CliError::Unavailable(m)
                    if m == "a:h:0: the service could not read its runtime and images; try again"),
                "{err:?}"
            );
            assert!(err.is_transient());
        }
    }

    #[test]
    fn a_failed_rebuild_names_what_was_and_was_not_started() {
        let failed = names(&["the runtime (version has a retired engine)"]);
        assert_eq!(
            not_rebuilt("a:h:0", &failed, &names(&["image default", "image align"])),
            "a:h:0: could not rebuild the runtime (version has a retired engine); started: image \
             default, image align"
        );
        assert_eq!(
            not_rebuilt("a:h:0", &names(&["image x (boom)", "image y (boom)"]), &[]),
            "a:h:0: could not rebuild image x (boom), image y (boom)"
        );
    }

    #[test]
    fn each_rebuild_says_where_it_is_and_why_it_failed() {
        let mut runtime = runtime_view("failed");
        runtime.reason = Some("download failed".into());
        runtime.logs_url = Some("https://logs.example/b1".into());
        assert_eq!(
            runtime_line(&runtime),
            "runtime: failed — download failed (build log: https://logs.example/b1)"
        );
        // A build that is not failing shows no log.
        let mut ready = runtime_view("ready");
        ready.logs_url = Some("https://logs.example/b0".into());
        assert_eq!(runtime_line(&ready), "runtime: ready");
        // A derived image: its state covers the derive, which is shown too.
        let mut deriving: WorkflowImage =
            serde_json::from_value(image("default", "building")).unwrap();
        deriving.derived_state = Some("building".into());
        deriving.logs_url = Some("https://logs.example/b2".into());
        assert_eq!(image_line(&deriving), "image default: building (derive: building)");
        let mut derive_failed: WorkflowImage =
            serde_json::from_value(image("default", "failed")).unwrap();
        derive_failed.derived_state = Some("failed".into());
        derive_failed.reason = Some("derive: push refused".into());
        assert_eq!(
            image_line(&derive_failed),
            "image default: failed (derive: failed) — derive: push refused"
        );
        let mut failed: WorkflowImage = serde_json::from_value(image("align", "failed")).unwrap();
        failed.reason = Some("no such tag".into());
        assert_eq!(image_line(&failed), "image align: failed — no such tag");
    }
}
