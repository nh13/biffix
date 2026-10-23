//! `biffix workflows reregister`: register versions again from what they recorded.

use biffix_api::{ReregisterPlan, ReregisterRequest, WorkflowVersionSummary};
use biffix_workflow::text::truncate_chars;

use crate::api::ApiClient;
use crate::commands::Session;
use crate::error::{CliError, CliResult};
use crate::render::OutputMode;

use super::wait::wait_for_runtime;

/// Options for `workflows reregister`.
pub struct ReregisterOpts {
    /// A version id or a workflow name; `None` with `all`.
    pub target: Option<String>,
    pub all: bool,
    pub project: Option<String>,
    pub engine_version: Option<String>,
    pub dry_run: bool,
    /// Wait (up to this long) until each re-registered version can run.
    pub wait: Option<std::time::Duration>,
}

/// The versions to re-register: an id as given; a workflow name's newest version; with no target,
/// every workflow's newest version (per project and name). With `--all`, `project` keeps only that
/// project's workflows. With a name, `project` picks the newest version in that project; when the
/// name has none there but is in exactly one other project, that one's newest version (which the
/// request then moves into `project`: see [`reregister`]).
fn reregister_targets(
    versions: &[WorkflowVersionSummary],
    target: Option<&str>,
    project: Option<&str>,
) -> CliResult<Vec<String>> {
    let mut newest: std::collections::BTreeMap<(&str, &str), &WorkflowVersionSummary> =
        std::collections::BTreeMap::new();
    for v in versions {
        if target.is_some_and(|t| v.name != t) {
            continue;
        }
        let slot = newest.entry((&v.project, &v.name)).or_insert(v);
        if v.created_at_micros > slot.created_at_micros {
            *slot = v;
        }
    }
    let id = |v: &&WorkflowVersionSummary| v.id.as_str().to_string();
    let projects = |newest: &std::collections::BTreeMap<(&str, &str), _>| {
        newest.keys().map(|(p, _)| *p).collect::<Vec<_>>().join(", ")
    };
    let Some(name) = target else {
        return Ok(newest
            .iter()
            .filter(|((p, _), _)| project.is_none_or(|want| *p == want))
            .map(|(_, v)| id(v))
            .collect());
    };
    if name.contains(':') {
        return Ok(vec![name.to_string()]);
    }
    if let Some(v) = project.and_then(|p| newest.get(&(p, name))) {
        return Ok(vec![id(v)]);
    }
    match newest.values().collect::<Vec<_>>().as_slice() {
        [] => Err(CliError::NotFound(format!("no workflow named `{name}`"))),
        [only] => Ok(vec![id(only)]),
        _ => Err(CliError::InvalidInput(match project {
            None => format!(
                "workflows named `{name}` are in several projects ({}); choose one with --project",
                projects(&newest)
            ),
            Some(p) => format!(
                "workflows named `{name}` are in several projects ({}), none of them `{p}`; \
                 re-register one by its version id with --project {p}",
                projects(&newest)
            ),
        })),
    }
}

/// What re-registering one version came to.
#[derive(serde::Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
enum Reregistered {
    Registered {
        from: String,
        id: String,
        state: Option<String>,
    },
    /// Nothing new: the same inputs as an existing build.
    UpToDate {
        from: String,
        reason: String,
    },
    Planned {
        plan: ReregisterPlan,
    },
    /// Registered as `id`, but its runtime or images did not become ready (they failed to build,
    /// or the wait ran out): it cannot run yet.
    NotReady {
        from: String,
        id: String,
        reason: String,
    },
    Failed {
        from: String,
        /// The failure's kind, as a JSON error's (`forbidden`, `unavailable`, …).
        kind: &'static str,
        reason: String,
    },
}

impl Reregistered {
    /// Whether this version was not brought up to date (the command then fails).
    fn failed(&self) -> bool {
        matches!(self, Self::NotReady { .. } | Self::Failed { .. })
    }
}

/// Re-register `id`. A version registered but not ready is [`Reregistered::NotReady`] when `all`
/// (the rest carry on), else the wait's own error (which names the new version).
async fn reregister_one(
    client: &ApiClient,
    id: &str,
    req: &ReregisterRequest,
    wait: Option<std::time::Duration>,
    all: bool,
) -> CliResult<Reregistered> {
    if req.dry_run {
        let plan = client.reregister_plan(id, req).await?;
        return Ok(Reregistered::Planned { plan });
    }
    match client.reregister_workflow(id, req).await {
        Ok(view) => {
            let new = view.id.as_str().to_string();
            let state = match wait {
                Some(timeout) => {
                    eprintln!("[biffix] {id} → {new}");
                    match wait_for_runtime(client, &new, timeout).await {
                        Ok(_) => Some("ready".to_string()),
                        Err(e) if all && !matches!(e, CliError::AuthRequired(_)) => {
                            return Ok(Reregistered::NotReady {
                                from: id.to_string(),
                                id: new,
                                reason: e.to_string(),
                            })
                        }
                        Err(e) => return Err(e),
                    }
                }
                None => view.state,
            };
            Ok(Reregistered::Registered { from: id.to_string(), id: new, state })
        }
        Err(CliError::InvalidInput(reason))
            if reason.starts_with(biffix_api::IDENTICAL_BUILD_PREFIX) =>
        {
            Ok(Reregistered::UpToDate { from: id.to_string(), reason })
        }
        Err(e) => Err(e),
    }
}

/// Re-register each of `targets` in turn, handing each outcome to `each` as it comes. With `all`,
/// a version that fails is recorded and the rest carry on; but a session that is no longer
/// accepted (`AuthRequired`) fails every version alike, so it stops there. Returns the outcomes so
/// far, and the error that stopped it, if any.
async fn reregister_each(
    client: &ApiClient,
    targets: &[String],
    req: &ReregisterRequest,
    wait: Option<std::time::Duration>,
    all: bool,
    mut each: impl FnMut(&str, &Reregistered),
) -> (Vec<Reregistered>, CliResult<()>) {
    let mut results = Vec::with_capacity(targets.len());
    for id in targets {
        let result = match reregister_one(client, id, req, wait, all).await {
            Ok(result) => result,
            Err(e) if all && !matches!(e, CliError::AuthRequired(_)) => {
                Reregistered::Failed { from: id.clone(), kind: e.kind(), reason: e.to_string() }
            }
            Err(e) => return (results, Err(e)),
        };
        each(id, &result);
        results.push(result);
    }
    (results, Ok(()))
}

/// `workflows reregister`: register versions again from what they recorded. With `--all`, a
/// version that fails is reported and the rest carry on; the command then fails.
pub async fn reregister(session: &mut Session, opts: ReregisterOpts) -> CliResult<()> {
    let client = session.authed_client().await?;
    let versions = client.list_workflows().await?;
    let targets = reregister_targets(
        &versions,
        opts.target.as_deref().filter(|_| !opts.all),
        opts.project.as_deref(),
    )?;
    let names: std::collections::BTreeMap<&str, &str> =
        versions.iter().map(|v| (v.id.as_str(), v.name.as_str())).collect();
    // With a target, `--project` is where the new version goes (its own project as a rule; another
    // moves it there); with `--all` it only chose the workflows.
    let req = ReregisterRequest {
        engine_version: opts.engine_version.clone(),
        dry_run: opts.dry_run,
        project: opts.project.clone().filter(|_| !opts.all),
    };
    let output = session.output;
    let (results, stopped) =
        reregister_each(&client, &targets, &req, opts.wait, opts.all, |id, result| {
            if output == OutputMode::Human {
                let name = names.get(id).copied().unwrap_or("");
                print!("{}", reregistered_line(name, result));
            }
        })
        .await;
    // Whatever was done is reported, also when the session stopped it.
    if output == OutputMode::Json && (opts.all || stopped.is_ok()) {
        crate::render::print_json(&serde_json::json!(results));
    }
    stopped?;
    let failed = results.iter().filter(|r| r.failed()).count();
    if failed > 0 {
        return Err(CliError::Unclassified(format!(
            "{failed} of {} versions could not be re-registered or are not ready to run",
            targets.len()
        )));
    }
    Ok(())
}

/// A digest (or key) shortened for the terminal: what follows its last `sha256:`, cut to 12
/// characters.
fn short(s: &str) -> &str {
    truncate_chars(s.rsplit_once("sha256:").map_or(s, |(_, d)| d), 12)
}

/// How one re-registration reads in the terminal (one or more lines, each ending in `\n`).
fn reregistered_line(name: &str, result: &Reregistered) -> String {
    let built = |cached: bool| if cached { "built" } else { "to build" };
    match result {
        Reregistered::Registered { from, id, state } => {
            format!("{name}  {from} → {id}  {}\n", state.as_deref().unwrap_or("registered"))
        }
        Reregistered::UpToDate { from, reason } => {
            format!("{name}  {from}  up to date: {reason}\n")
        }
        Reregistered::NotReady { from, id, reason } => {
            format!("{name}  {from} → {id}  NOT READY: {reason}\n")
        }
        Reregistered::Failed { from, reason, .. } => {
            format!("{name}  {from}  FAILED: {reason}\n")
        }
        Reregistered::Planned { plan } => {
            let mut out = match &plan.identical_to {
                Some(same) => format!("{name}  {}  up to date (identical to {same})\n", plan.from),
                None => format!("{name}  {} → {} (would register)\n", plan.from, plan.id),
            };
            out.push_str(&format!(
                "    engine {}, runtime {} ({})\n",
                plan.engine_version,
                short(&plan.runtime_key),
                built(plan.runtime_cached)
            ));
            for image in &plan.images {
                out.push_str(&format!(
                    "    image {} [{}] {} ({})",
                    image.name,
                    image.kind,
                    image.source_ref,
                    built(image.cached)
                ));
                if let Some(was) = &image.previous_source_ref {
                    out.push_str(&format!(" (was {})", short(was)));
                }
                out.push('\n');
            }
            out
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::workflows::version;

    #[test]
    fn short_never_splits_a_character() {
        assert_eq!(short("quay.io/x@sha256:0123456789abcdef"), "0123456789ab");
        assert_eq!(short("abc"), "abc");
        assert_eq!(short("ééééééééééééé"), "éééééééééééé");
        assert_eq!(truncate_chars("0123456789a€bc", 12), "0123456789a€");
    }

    #[test]
    fn reregister_targets_are_ids_or_each_workflows_newest_version() {
        let versions = [
            version("a:h:0", "p", "align", 1),
            version("a:h:1", "p", "align", 3),
            version("b:h:0", "p", "call", 2),
            version("c:h:0", "q", "align", 5),
        ];
        let targets = |t: Option<&str>, p: Option<&str>| reregister_targets(&versions, t, p);
        assert_eq!(targets(Some("x:y:0"), None).unwrap(), ["x:y:0"]);
        assert_eq!(targets(Some("call"), None).unwrap(), ["b:h:0"]);
        assert_eq!(targets(Some("align"), Some("p")).unwrap(), ["a:h:1"]);
        // A name in one project re-registered into another: its newest (the request moves it).
        assert_eq!(targets(Some("call"), Some("q")).unwrap(), ["b:h:0"]);
        let err = targets(Some("align"), Some("r")).unwrap_err();
        assert!(
            matches!(err, CliError::InvalidInput(ref m)
                if m.contains("several projects (p, q), none of them `r`")),
            "{err:?}"
        );
        let err = targets(Some("align"), None).unwrap_err();
        assert!(
            matches!(err, CliError::InvalidInput(ref m) if m.contains("several projects (p, q)")),
            "{err:?}"
        );
        assert!(matches!(targets(Some("nope"), None), Err(CliError::NotFound(_))));
        assert_eq!(targets(None, None).unwrap(), ["a:h:1", "b:h:0", "c:h:0"]);
        assert_eq!(targets(None, Some("q")).unwrap(), ["c:h:0"]);
    }

    /// `--all` carries on past one version's failure, reports a version registered but not ready
    /// with its new id, and stops at a session the API no longer accepts.
    #[tokio::test]
    async fn reregistering_all_stops_only_at_a_session_wide_failure() {
        use axum::extract::Path;
        use axum::http::StatusCode;
        use std::sync::atomic::{AtomicU32, Ordering};
        use std::sync::Arc;
        let later = Arc::new(AtomicU32::new(0));
        let counted = later.clone();
        let app = axum::Router::new()
            .route(
                "/workflows/{id}/reregister",
                axum::routing::post(move |Path(id): Path<String>| {
                    let counted = counted.clone();
                    async move {
                        match id.as_str() {
                            "a:h:0" => {
                                (StatusCode::FORBIDDEN, "project p is read-only").into_response()
                            }
                            "b:h:0" => axum::Json(version("b:h:1", "p", "wf", 2)).into_response(),
                            "c:h:0" => (StatusCode::UNAUTHORIZED, "token revoked").into_response(),
                            _ => {
                                counted.fetch_add(1, Ordering::SeqCst);
                                axum::Json(version("d:h:1", "p", "wf", 2)).into_response()
                            }
                        }
                    }
                }),
            )
            .route(
                "/workflows/{id}",
                axum::routing::get(|Path(id): Path<String>| async move {
                    axum::Json(serde_json::json!({
                        "id": id, "name": "wf", "project": "p", "image_digest": "",
                        "engine": "snakemake", "engine_version": "9.27.0", "git_sha": "b",
                        "queue_classes": [], "created_at_micros": 0, "source_content_hash": "h",
                        "entrypoint": "Snakefile", "form_schema": {"fields": [], "warnings": []},
                        "source": {"kind": "git", "repo": "r", "git_ref": "main"}, "build": 1,
                        "registration_inputs": {"source_mode": "git", "cli_version": "0.0.0"},
                        "state": "failed", "not_ready": ["image x: FAILED"],
                    }))
                }),
            );
        use axum::response::IntoResponse;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = ApiClient::new(
            reqwest::Client::new(),
            &base,
            crate::secret::Secret::new("tok".into()),
            false,
        );
        let targets: Vec<String> = ["a:h:0", "b:h:0", "c:h:0", "d:h:0"].map(String::from).to_vec();
        let req = ReregisterRequest { engine_version: None, dry_run: false, project: None };
        let wait = Some(std::time::Duration::from_secs(60));
        let (results, stopped) =
            reregister_each(&client, &targets, &req, wait, true, |_, _| {}).await;
        assert!(
            matches!(&stopped, Err(CliError::AuthRequired(m)) if m.contains("token revoked")),
            "{stopped:?}"
        );
        assert_eq!(later.load(Ordering::SeqCst), 0, "nothing after the revoked session");
        assert_eq!(
            serde_json::to_value(&results).unwrap(),
            serde_json::json!([
                {"outcome": "failed", "from": "a:h:0", "kind": "forbidden",
                 "reason": "project p is read-only"},
                {"outcome": "not_ready", "from": "b:h:0", "id": "b:h:1",
                 "reason": "b:h:1: image x: FAILED"},
            ])
        );
        assert!(results.iter().all(Reregistered::failed));
    }

    #[test]
    fn a_dry_run_shows_the_runtime_and_each_image_and_what_moved() {
        let plan = ReregisterPlan {
            from: "a:h:0".into(),
            id: "a:h:1".into(),
            build: 1,
            identical_to: None,
            engine_version: "9.27.0".into(),
            runtime_key: "4dbb7d79f6bc616bb72c".into(),
            runtime_cached: true,
            images: vec![biffix_api::ReregisterImagePlan {
                name: "default".into(),
                kind: "import".into(),
                source_ref: "quay.io/x:1@sha256:222222222222aaaa".into(),
                previous_source_ref: Some("quay.io/x:1@sha256:111111111111aaaa".into()),
                cached: false,
            }],
        };
        let text = reregistered_line("align", &Reregistered::Planned { plan });
        assert_eq!(
            text,
            "align  a:h:0 → a:h:1 (would register)\n    engine 9.27.0, runtime 4dbb7d79f6bc \
             (built)\n    image default [import] quay.io/x:1@sha256:222222222222aaaa (to \
             build) (was 111111111111)\n"
        );
    }
}
