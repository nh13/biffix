//! `biffix workflows register`: freeze an immutable workflow version from a local git checkout.

use std::path::Path;

use biffix_api::{
    RegisterRequest, RegistrationInputs, RegistrationOverrides, SourceUploadRequest,
    WorkflowVersionSummary,
};
use biffix_workflow::engines::Engine;
use biffix_workflow::text::truncate_chars;

use crate::api::ApiClient;
use crate::commands::Session;
use crate::error::{CliError, CliResult};
use crate::local::form::{prepare_form_shipped, read_about};
use crate::local::source::SourceMode;
use crate::local::source::{self, TrackedTree};
use crate::render::OutputMode;

use super::images::{prepare_builds, request_images, stage_builds, RequestedImages};
use super::infer_name;
use super::wait::wait_for_runtime;

/// Options for `workflows register`.
pub struct RegisterOpts {
    pub path: String,
    /// Default worker image; `None` registers a version with no default image (every task declares
    /// its own container).
    pub image: Option<String>,
    pub name: Option<String>,
    pub project: Option<String>,
    pub queue_classes: Vec<String>,
    pub allow_dirty: bool,
    pub engine_version: Option<String>,
    pub entrypoint: Option<String>,
    /// Whether to record a git origin (public) or stage the source tarball to S3 (private-repo).
    pub source_mode: SourceMode,
    /// Wait (up to this long) until the version's engine runtime is built and it can run.
    pub wait: Option<std::time::Duration>,
}

/// Refuse a checkout whose registered identity would not be what its runs execute, before anything
/// is read or staged: `{git_sha}:{content_hash}` must name the source the coordinator runs.
fn check_source_identity(
    root: &Path,
    git: &source::GitInfo,
    mode: SourceMode,
    allow_dirty: bool,
) -> CliResult<()> {
    // A dirty tree is legitimate while iterating with --source-mode s3, but never silent: it
    // requires --allow-dirty and the override is recorded in the output. (Git mode refuses it
    // whatever the flag says, below, so it never suggests the flag.)
    if git.dirty && !allow_dirty && mode == SourceMode::S3 {
        return Err(CliError::InvalidInput(format!(
            "workflow tree at {} has uncommitted changes; commit them or pass --allow-dirty",
            root.display()
        )));
    }
    match mode {
        SourceMode::Git => {
            // The coordinator clones the origin and checks out the commit: uncommitted bytes are
            // hashed into the id but never run.
            if git.dirty {
                return Err(CliError::InvalidInput(format!(
                    "workflow tree at {} has uncommitted changes, and --source-mode git runs the \
                     commit, not the working tree; commit (and push) them, or stage the tree as it \
                     is with --source-mode s3",
                    root.display()
                )));
            }
            // …at the repository's root: the entrypoint, the form and the hash are derived from
            // this directory, but no subdirectory is recorded for the coordinator to run in.
            if !git.prefix.is_empty() {
                return Err(CliError::InvalidInput(format!(
                    "{} is the subdirectory `{}` of its repository, but --source-mode git runs the \
                     repository's root; register from the root (with `entrypoint` relative to \
                     it), or stage this directory with --source-mode s3",
                    root.display(),
                    git.prefix.trim_end_matches('/')
                )));
            }
        }
        SourceMode::S3 => {
            // The id covers a submodule by the commit its gitlink pins, and a staged source is
            // never replaced; so a tarball of anything else would be the version's for good.
            if !git.modified_submodules.is_empty() {
                return Err(CliError::InvalidInput(format!(
                    "submodule(s) {} are checked out at another commit or have edits, so the staged \
                     source would not be the one its id names; commit the submodule bump (`git add` \
                     it) or restore it with `git submodule update`",
                    git.modified_submodules.join(", ")
                )));
            }
        }
    }
    Ok(())
}

pub async fn register(session: &mut Session, opts: RegisterOpts) -> CliResult<()> {
    let root = Path::new(&opts.path);
    let git = source::resolve_git(root)?;
    check_source_identity(root, &git, opts.source_mode, opts.allow_dirty)?;
    if opts.source_mode == SourceMode::Git {
        if let Some(host) = &git.origin_credentials_removed {
            eprintln!(
                "[biffix] warning: the origin URL carries credentials for {host}; recording it \
                 without them (a version's source is visible to every viewer)"
            );
        }
        if source::head_on_a_remote_branch(root) == Some(false) {
            eprintln!(
                "[biffix] warning: {} is on no remote-tracking branch (as of your last fetch); \
                 runs clone the origin and check it out, so push it first",
                git.sha
            );
        }
    }

    // The tracked tree is listed once, for the source tarball and every build context.
    let mut tree = match opts.source_mode {
        SourceMode::S3 => {
            let tree = TrackedTree::list(root)?;
            if !tree.unpopulated_submodules.is_empty() {
                // Nothing of theirs can be staged, and the source key (named by the commit the
                // gitlinks pin) cannot change without a new commit: staging now would freeze the
                // version without them for good.
                return Err(CliError::InvalidInput(format!(
                    "submodule(s) {} are not checked out, so their contents cannot be staged; \
                     run `git submodule update --init --recursive` first",
                    tree.unpopulated_submodules.join(", ")
                )));
            }
            Some(tree)
        }
        SourceMode::Git => None,
    };
    // The source the coordinator gets: the engine is detected from it, the entrypoint must be in
    // it, and the memory check reads it.
    let files = source::shipped_files(root, opts.source_mode, tree.as_ref())?;
    let (manifest, engine, form_schema) = prepare_form_shipped(root, &files)?;
    crate::local::form::warn_form(&form_schema);
    let about_markdown = read_about(root, &manifest, Some(&files))?;

    // Exactly the images `biffix.yaml` declares (`biffix inspect --write` adds the ones the
    // workflow names). A Nextflow version with no default image needs a container on every process;
    // warn so the registrant knows a task without one will fail. (Snakemake tasks without their
    // own image run on the coordinator's image, which carries the version's pinned Snakemake.)
    let RequestedImages { default: default_image, named: mut named_images, builds } =
        request_images(&manifest, opts.image.as_deref())?;
    if tree.is_none() && !builds.is_empty() {
        tree = Some(TrackedTree::list(root)?);
    }
    let entrypoint = crate::local::entrypoint::resolve_entrypoint(
        opts.entrypoint.clone(),
        true,
        &manifest,
        engine,
        &files,
    )?;
    if let Some(note) = &entrypoint.note {
        eprintln!("[biffix] note: {note}");
    }
    let builds = match &tree {
        Some(tree) => prepare_builds(root, tree, builds)?,
        None => Vec::new(),
    };
    if engine == Engine::Nextflow
        && default_image.is_none()
        && !named_images.contains_key("default")
        && !builds.iter().any(|b| b.name == "default")
    {
        eprintln!(
            "[biffix] warning: this version has no default image, so every process must declare \
             its own `container` or its tasks will fail to run"
        );
    }

    let content_hash = source::content_hash(root)?;
    let source_tarball = match (opts.source_mode, &tree) {
        (SourceMode::S3, Some(tree)) => Some(tar_unchanged(root, tree, &content_hash)?),
        _ => None,
    };
    // The source identity: the server appends the build number to make the version id.
    let source_id = biffix_workflow::version::source_id(&git.sha, &content_hash);
    // Recorded so the version can be re-registered from its inputs: what the command line added to
    // the source's own biffix.yaml.
    let registration_inputs = RegistrationInputs {
        source_mode: opts.source_mode.as_str().to_string(),
        cli_version: env!("CARGO_PKG_VERSION").to_string(),
        overrides: RegistrationOverrides {
            engine_version: opts.engine_version.clone(),
            // As recorded: normalized, so a re-registration resolves it the same way.
            entrypoint: opts.entrypoint.as_ref().map(|_| entrypoint.path.clone()),
            image: opts.image.clone().filter(|s| !s.is_empty()),
            queue_classes: opts.queue_classes.clone(),
        },
        reregistered_from: None,
    };

    let name = opts.name.or_else(|| infer_name(root)).ok_or_else(|| {
        CliError::InvalidInput("cannot infer a workflow name; pass --name".into())
    })?;
    let engine_version =
        opts.engine_version.or(manifest.engine_version.clone()).ok_or_else(|| {
            CliError::InvalidInput(
                "engine_version is required (set it in biffix.yaml or pass --engine-version)"
                    .into(),
            )
        })?;
    // Fail before staging anything; the server checks again. Runs use exactly this release: the
    // server builds it into an engine runtime for the version (once per release, tenant-wide).
    // Sent in its canonical spelling (`9.03.0` is `9.3.0`), as the server records it.
    let engine_version =
        biffix_workflow::engines::check_engine_version(engine.as_str(), &engine_version)
            .map_err(|e| CliError::InvalidInput(e.to_string()))?
            .canonical;
    // Advisory: rules that would run with Snakemake's default memory on AWS Batch.
    if engine == Engine::Snakemake {
        crate::local::memory_warning::warn_rules_without_memory(
            root,
            &files,
            &entrypoint.file,
            Some(&engine_version),
        );
    }
    // The workflow's own Nextflow plugins (`plugins:`, already checked by prepare_form): built
    // into its runtime, since the coordinator runs Nextflow offline. A pipeline that declares a
    // plugin biffix.yaml does not list (at that version) would fail every run.
    let workflow_plugins =
        manifest.nextflow_plugins(engine).map_err(|e| CliError::InvalidInput(e.to_string()))?;
    if engine == Engine::Nextflow {
        crate::local::nf_plugins::check_declared(
            root,
            &entrypoint.path,
            &workflow_plugins,
            &engine_version,
        )?;
    }
    let form_schema = serde_json::to_value(&form_schema)
        .map_err(|e| CliError::Unclassified(format!("cannot encode the form: {e}")))?;
    // Checked before anything is staged, as the server would refuse it after.
    let (repo, git_ref) = match (opts.source_mode, &git.origin, &git.git_ref) {
        (SourceMode::Git, Some(repo), Some(git_ref)) => (Some(repo), Some(git_ref)),
        (SourceMode::Git, _, _) => {
            return Err(CliError::InvalidInput(
                "workflow has no git origin/branch to record; push it to a remote first, or \
                 stage the source with --source-mode s3"
                    .into(),
            ))
        }
        (SourceMode::S3, _, _) => (None, None),
    };

    let client = session.authed_client().await?;
    // Pinned (downloaded from the public plugin registry and hashed) before anything is staged.
    let nextflow_plugins = crate::local::nf_plugins::pin(&workflow_plugins).await?;
    let project = match opts.project {
        Some(project) => project,
        None => default_project(&client, &name).await?,
    };

    // Where the coordinator fetches the source at run time: record a git origin (anonymous clone,
    // public repos), or stage an immutable source tarball to the project bucket (private repos —
    // no GitHub credential reaches the coordinator).
    let source_value = match (repo, git_ref, source_tarball) {
        (Some(repo), Some(git_ref), _) => {
            serde_json::json!({ "kind": "git", "repo": repo, "git_ref": git_ref })
        }
        (_, _, Some(tarball)) => {
            stage_source_to_s3(&client, tarball, source_id.as_str(), &project).await?
        }
        _ => unreachable!("an s3 source always has its tarball built"),
    };

    // Built images: their contexts are staged beside the source (whatever the source mode), in
    // the version's project bucket.
    named_images.extend(stage_builds(&client, source_id.as_str(), &project, builds).await?);

    let req = RegisterRequest {
        name,
        project,
        source: source_value,
        git_sha: git.sha.clone(),
        source_content_hash: content_hash,
        image: default_image,
        engine: Some(engine.as_str().to_string()),
        engine_version,
        entrypoint: entrypoint.path,
        target: manifest.target.clone(),
        form_schema,
        queue_classes: if opts.queue_classes.is_empty() {
            None
        } else {
            Some(opts.queue_classes.clone())
        },
        about_markdown,
        registration_inputs,
        images: named_images,
        nextflow_plugins,
    };

    // The same source with the same inputs again is refused, naming the version already registered
    // (an already-registered version is a rejection, exit 6). `reregister`, whose job is to
    // bring versions up to date, reports the same reply as "up to date" instead.
    let view = client.register_workflow(&req).await.map_err(annotate_register)?;
    let id = view.id.as_str().to_string();

    // The server is authoritative for the frozen id, but a divergence from the id the CLI computed
    // locally signals a client/server version-id drift (e.g. mismatched file normalization). Warn
    // on it in every build — a `debug_assert_eq!` would panic a debug build on legitimate server
    // data yet silently ignore the drift in the shipped release binary.
    if !id
        .strip_prefix(source_id.as_str())
        .is_some_and(|build| build.is_empty() || build.starts_with(':'))
    {
        eprintln!(
            "[biffix] warning: the server froze {id}, a version of a different source than the \
             client computed ({source_id}); the CLI's version-id derivation may be out of sync \
             with the server"
        );
    }
    // Said before waiting, so a failed or abandoned wait never loses the id.
    let runtime = match opts.wait {
        Some(timeout) => {
            eprintln!("[biffix] registered {id}; waiting until it can run…");
            wait_for_runtime(&client, &id, timeout).await?
        }
        None => None,
    };
    match session.output {
        OutputMode::Json => crate::render::print_json(&serde_json::json!({
            "id": id, "name": view.name, "image_digest": view.image_digest,
            "dirty": git.dirty, "runtime": runtime,
        })),
        OutputMode::Human => {
            let dirty = if git.dirty { " (registered from a DIRTY tree)" } else { "" };
            // A version may have no default image (every task declares its own).
            let image = if view.image_digest.is_empty() {
                String::new()
            } else {
                format!(" (image {})", view.image_digest)
            };
            println!("registered {} {id}{image}{dirty}", view.name);
            if let Some(r) = &runtime {
                println!("ready to run (engine runtime {})", truncate_chars(&r.key, 12));
            }
        }
    }
    Ok(())
}

/// The tarball of `tree`, checked to hold the content `content_hash` (the version id) names: the
/// files are read again for it, so one edited after it was hashed would be staged under an id
/// that does not describe it. Such a registration is refused before anything is staged.
fn tar_unchanged(root: &Path, tree: &TrackedTree, content_hash: &str) -> CliResult<Vec<u8>> {
    let tarball = source::tar_tracked_files(tree)?;
    if source::content_hash(root)? != content_hash {
        return Err(CliError::InvalidInput(
            "the workflow's files changed while it was being registered; nothing was staged, \
             register again"
                .into(),
        ));
    }
    Ok(tarball)
}

/// Stage the workflow's source `tarball` as an immutable object in `project`'s bucket and
/// return its `kind:s3` source descriptor. The backend presigns a PUT for the content-addressed
/// key, the CLI uploads over plain HTTPS (no AWS creds), then register records the returned uri.
async fn stage_source_to_s3(
    client: &ApiClient,
    tarball: Vec<u8>,
    version_id: &str,
    project: &str,
) -> CliResult<serde_json::Value> {
    // Recorded with the source: the coordinator runs the staged object only if it is these bytes.
    let sha256 = source::sha256_hex(&tarball);
    let upload = client
        .source_upload(&SourceUploadRequest {
            version_id: version_id.to_string(),
            context_tree_hash: None,
            project: project.to_string(),
        })
        .await?;
    // A staged source is never replaced (a version runs whatever its key holds), so one already
    // there is kept, and the sha256 recorded must be that object's, not this tarball's.
    let sha256 = if client.put_presigned(&upload, tarball).await? {
        sha256
    } else {
        staged_sha256(client, version_id, &upload.uri, sha256).await?
    };
    Ok(serde_json::json!({ "kind": "s3", "uri": upload.uri, "sha256": sha256 }))
}

/// The sha256 of the source already staged at `uri` for the source `source_id`, where this
/// command's tarball (`ours`) could not be written. The key is named by the source's content, so
/// it holds these files; but another biffix version may have archived them as other bytes, and
/// the coordinator runs the object only if it has the recorded sha256. So the sha256 an earlier
/// build of this source recorded for the object is the one to record. With no earlier build (the
/// staging registered nothing, e.g. a refused registration), the object cannot be checked: `ours`,
/// which this same CLI's deterministic archive matches.
async fn staged_sha256(
    client: &ApiClient,
    source_id: &str,
    uri: &str,
    ours: String,
) -> CliResult<String> {
    let builds: Vec<String> = client
        .list_workflows()
        .await?
        .into_iter()
        .map(|v| v.id.as_str().to_string())
        .filter(|id| id.strip_prefix(source_id).is_some_and(|build| build.starts_with(':')))
        .collect();
    for id in builds {
        let detail = client.get_workflow(&id).await?;
        let recorded =
            detail.source.get("sha256").and_then(serde_json::Value::as_str).filter(|_| {
                detail.source.get("uri").and_then(serde_json::Value::as_str) == Some(uri)
            });
        let Some(recorded) = recorded else { continue };
        if recorded == ours {
            eprintln!("[biffix] the source is already staged; using it");
        } else {
            eprintln!(
                "[biffix] the source is already staged (by {id}) as a different archive of the \
                 same files; using it, with its sha256"
            );
        }
        return Ok(recorded.to_string());
    }
    eprintln!(
        "[biffix] warning: the source is already staged, but no registered version records its \
         sha256, so it cannot be checked against this tarball; if another biffix version staged \
         it, runs of this version will refuse it"
    );
    Ok(ours)
}

/// The project to register in when `--project` is not given (see [`choose_project`]): it needs the
/// deploy's projects, and on a deploy of several the versions already registered.
async fn default_project(client: &ApiClient, name: &str) -> CliResult<String> {
    let projects = client.list_projects().await?;
    let versions = if projects.len() > 1 { client.list_workflows().await? } else { Vec::new() };
    choose_project(&projects, &versions, name)
}

/// The project a registration of workflow `name` without `--project` goes to, given the deploy's
/// `projects` and the `versions` registered (read only on a deploy of several projects):
///
/// - a deploy of one project: that project;
/// - a deploy of several: the project of the workflow's earlier versions, when they are all in one
///   the deploy configures; else `--project` is needed.
fn choose_project(
    projects: &[biffix_api::Project],
    versions: &[WorkflowVersionSummary],
    name: &str,
) -> CliResult<String> {
    let ids = |projects: &[biffix_api::Project]| {
        projects.iter().map(|p| p.id.as_str()).collect::<Vec<_>>().join(", ")
    };
    match projects {
        [one] => return Ok(one.id.clone()),
        [] => {
            return Err(CliError::InvalidInput(
                "this deploy has no projects to register into; add one under `projects:` in the \
                 deploy config"
                    .into(),
            ))
        }
        _ => {}
    }
    let earlier: Vec<&str> = versions
        .iter()
        .filter(|v| v.name == name)
        .map(|v| v.project.as_str())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    match earlier.as_slice() {
        [only] if projects.iter().any(|p| p.id == *only) => Ok(only.to_string()),
        [] => Err(CliError::InvalidInput(format!(
            "this deploy has projects {}; pass --project",
            ids(projects)
        ))),
        _ => Err(CliError::InvalidInput(format!(
            "this deploy has projects {}, and earlier versions of `{name}` are in {}; pass \
             --project",
            ids(projects),
            earlier.join(", ")
        ))),
    }
}

/// Turn the generic server rejections into distinct, actionable messages, keeping the
/// server's own reason wherever it gave one.
fn annotate_register(err: CliError) -> CliError {
    match err {
        // A rejection with no reason: say what usually causes one.
        CliError::InvalidInput(m) if m.starts_with("the server rejected the request") => {
            CliError::InvalidInput(format!(
                "{m} — an unknown queue-class or an unresolvable image can cause this"
            ))
        }
        CliError::Forbidden(m) => {
            CliError::Forbidden(format!("{m} (registering a workflow requires the Developer role)"))
        }
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::workflows::version;

    #[test]
    fn register_rejections_keep_the_server_s_reason() {
        let generic = annotate_register(CliError::InvalidInput(
            "the server rejected the request (400 Bad Request)".into(),
        ));
        assert!(
            matches!(generic, CliError::InvalidInput(ref m) if m.contains("unknown queue-class")),
            "{generic:?}"
        );
        let specific =
            annotate_register(CliError::InvalidInput("version conflict: build 3 is newer".into()));
        assert!(
            matches!(specific, CliError::InvalidInput(ref m) if m == "version conflict: build 3 is newer"),
            "{specific:?}"
        );
        let forbidden = annotate_register(CliError::Forbidden("project p is read-only".into()));
        assert!(
            matches!(forbidden, CliError::Forbidden(ref m)
                if m.contains("project p is read-only") && m.contains("Developer role")),
            "{forbidden:?}"
        );
        assert!(matches!(
            annotate_register(CliError::Unavailable("x".into())),
            CliError::Unavailable(_)
        ));
    }

    fn git_info(dirty: bool, prefix: &str, modified: &[&str]) -> source::GitInfo {
        source::GitInfo {
            origin: Some("https://example.com/wf.git".into()),
            origin_credentials_removed: None,
            git_ref: Some("main".into()),
            sha: "a".repeat(40),
            dirty,
            prefix: prefix.into(),
            modified_submodules: modified.iter().map(std::string::ToString::to_string).collect(),
        }
    }

    #[test]
    fn a_source_is_registered_only_as_its_runs_will_see_it() {
        let root = Path::new("wf");
        let check = |info: source::GitInfo, mode, allow_dirty| {
            check_source_identity(root, &info, mode, allow_dirty)
        };
        let refused = |r: CliResult<()>, needle: &str| {
            assert!(
                matches!(r, Err(CliError::InvalidInput(ref m)) if m.contains(needle)),
                "{needle}: {r:?}"
            );
        };
        assert!(check(git_info(false, "", &[]), SourceMode::Git, false).is_ok());
        // A dirty tree in git mode is refused with or without --allow-dirty, so the refusal never
        // suggests the flag.
        for allow_dirty in [false, true] {
            let r = check(git_info(true, "", &[]), SourceMode::Git, allow_dirty);
            assert!(
                matches!(&r, Err(CliError::InvalidInput(m)) if !m.contains("--allow-dirty")),
                "{r:?}"
            );
            refused(r, "runs the commit, not the working tree");
        }
        refused(check(git_info(true, "", &[]), SourceMode::S3, false), "pass --allow-dirty");
        refused(
            check(git_info(false, "flows/align/", &[]), SourceMode::Git, false),
            "the subdirectory `flows/align`",
        );
        // S3 stages the directory as it is: a subdirectory and a dirty tree (allowed) are fine…
        assert!(check(git_info(true, "flows/align/", &[]), SourceMode::S3, true).is_ok());
        // …but not a submodule whose checkout is not the commit the id names.
        refused(check(git_info(true, "", &["lib"]), SourceMode::S3, true), "submodule(s) lib");
    }

    /// A deploy whose source key already holds an object (the write-once PUT answers 412), staged
    /// for build `x:h:0` with the sha256 `recorded`; `None` when no build was registered.
    async fn already_staged(recorded: Option<&'static str>) -> ApiClient {
        use axum::routing::{get, post, put};
        const URI: &str = "s3://bucket/sources/x:h/source.tar.gz";
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let url = format!("{base}/obj");
        let detail = |id: &str, uri: &str, sha256: &str| {
            serde_json::json!({
                "id": id, "name": "wf", "project": "p", "image_digest": "",
                "engine": "snakemake", "engine_version": "9.27.0", "git_sha": "x",
                "queue_classes": [], "created_at_micros": 0, "source_content_hash": "h",
                "entrypoint": "Snakefile", "form_schema": {"fields": [], "warnings": []},
                "source": {"kind": "s3", "uri": uri, "sha256": sha256}, "build": 0,
                "registration_inputs": {"source_mode": "s3", "cli_version": "0.0.0"},
            })
        };
        let listed: Vec<biffix_api::WorkflowVersionSummary> = match recorded {
            // Another source (`x:hh`) whose id shares the prefix, then this source's build.
            Some(_) => vec![version("x:hh:0", "p", "wf", 1), version("x:h:0", "p", "wf", 2)],
            None => vec![version("x:hh:0", "p", "wf", 1)],
        };
        let app = axum::Router::new()
            .route(
                "/workflows/source-upload",
                post(move || async move {
                    axum::Json(serde_json::json!({
                        "url": url, "headers": {"If-None-Match": "*"}, "uri": URI,
                    }))
                }),
            )
            .route("/obj", put(|| async { axum::http::StatusCode::PRECONDITION_FAILED }))
            .route("/workflows", get(move || async move { axum::Json(listed) }))
            .route(
                "/workflows/{id}",
                get(move |axum::extract::Path(id): axum::extract::Path<String>| async move {
                    axum::Json(match id.as_str() {
                        "x:h:0" => detail(&id, URI, recorded.unwrap_or_default()),
                        _ => detail(&id, "s3://bucket/sources/x:hh/source.tar.gz", "f"),
                    })
                }),
            );
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        ApiClient::new(
            reqwest::Client::new(),
            &base,
            crate::secret::Secret::new("tok".into()),
            false,
        )
    }

    #[tokio::test]
    async fn a_source_staged_already_is_recorded_with_its_own_sha256() {
        let dir = tempfile::tempdir().unwrap();
        crate::test_git::git_ok(dir.path(), &["init", "-q"]);
        std::fs::write(dir.path().join("Snakefile"), "rule all:\n").unwrap();
        crate::test_git::git_ok(dir.path(), &["add", "Snakefile"]);
        let tree = TrackedTree::list(dir.path()).unwrap();
        let tarball = source::tar_tracked_files(&tree).unwrap();
        let ours = source::sha256_hex(&tarball);
        let recorded = "a".repeat(64).leak();

        // An earlier build recorded the staged object's sha256: that is the one to record.
        let client = already_staged(Some(recorded)).await;
        let source = stage_source_to_s3(&client, tarball.clone(), "x:h", "p").await.unwrap();
        assert_eq!(source["sha256"], *recorded);
        assert_eq!(source["uri"], "s3://bucket/sources/x:h/source.tar.gz");

        // Nothing records it: this tarball's (the same CLI archives the same files alike).
        let client = already_staged(None).await;
        let source = stage_source_to_s3(&client, tarball, "x:h", "p").await.unwrap();
        assert_eq!(source["sha256"], ours);
    }

    #[tokio::test]
    async fn a_staged_source_records_the_sha256_of_the_bytes_uploaded() {
        use axum::routing::{post, put};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let url = format!("{base}/obj");
        let put_body = std::sync::Arc::new(std::sync::Mutex::new(Vec::<u8>::new()));
        let record = put_body.clone();
        let app = axum::Router::new()
            .route(
                "/workflows/source-upload",
                post(move || async move {
                    axum::Json(serde_json::json!({
                        "url": url, "headers": {"If-None-Match": "*"},
                        "uri": "s3://bucket/sources/x:h/source.tar.gz",
                    }))
                }),
            )
            .route(
                "/obj",
                put(move |body: axum::body::Bytes| async move {
                    *record.lock().unwrap() = body.to_vec();
                    axum::http::StatusCode::OK
                }),
            );
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = ApiClient::new(
            reqwest::Client::new(),
            &base,
            crate::secret::Secret::new("tok".into()),
            false,
        );
        let dir = tempfile::tempdir().unwrap();
        crate::test_git::git_ok(dir.path(), &["init", "-q"]);
        std::fs::write(dir.path().join("Snakefile"), "rule all:\n").unwrap();
        crate::test_git::git_ok(dir.path(), &["add", "Snakefile"]);
        let tree = TrackedTree::list(dir.path()).unwrap();
        let tarball = source::tar_tracked_files(&tree).unwrap();
        let source = stage_source_to_s3(&client, tarball.clone(), "x:h", "p").await.unwrap();
        let uploaded = put_body.lock().unwrap().clone();
        assert_eq!(uploaded, tarball);
        assert_eq!(
            source,
            serde_json::json!({
                "kind": "s3", "uri": "s3://bucket/sources/x:h/source.tar.gz",
                "sha256": source::sha256_hex(&uploaded),
            })
        );
    }

    #[test]
    fn a_file_edited_after_it_was_hashed_is_not_staged() {
        let dir = tempfile::tempdir().unwrap();
        crate::test_git::git_ok(dir.path(), &["init", "-q"]);
        std::fs::write(dir.path().join("Snakefile"), "rule all:\n").unwrap();
        crate::test_git::git_ok(dir.path(), &["add", "Snakefile"]);
        let hash = source::content_hash(dir.path()).unwrap();
        let tree = TrackedTree::list(dir.path()).unwrap();
        assert_eq!(
            tar_unchanged(dir.path(), &tree, &hash).unwrap(),
            source::tar_tracked_files(&tree).unwrap()
        );
        std::fs::write(dir.path().join("Snakefile"), "rule all:\n    input: []\n").unwrap();
        let err = tar_unchanged(dir.path(), &tree, &hash).unwrap_err();
        assert!(
            matches!(err, CliError::InvalidInput(ref m) if m.contains("changed while it was being registered")),
            "{err:?}"
        );
    }

    #[test]
    fn a_registration_without_project_keeps_the_workflow_s_project_or_asks_for_one() {
        let view = |id: &str, default: bool| biffix_api::Project {
            id: id.into(),
            bucket: format!("biffix-{id}"),
            uploads: false,
            viewer_cors: false,
            default,
        };
        let msg = |r: CliResult<String>| match r {
            Err(CliError::InvalidInput(m)) => m,
            other => panic!("{other:?}"),
        };
        let one = [view("demo", true)];
        let two = [view("a", true), view("b", false)];
        let earlier = [
            version("x:h:0", "b", "wf", 1),
            version("y:h:0", "smoke", "smoke", 1),
            version("z:h:0", "a", "split", 1),
            version("z:h:1", "b", "split", 2),
        ];
        // A deploy of one project: that project, whatever exists.
        assert_eq!(choose_project(&one, &earlier, "wf").unwrap(), "demo");
        // Several: the project of its earlier versions.
        assert_eq!(choose_project(&two, &earlier, "wf").unwrap(), "b");
        // A new workflow, earlier versions in a project the deploy lacks, or in several: ask.
        assert_eq!(
            msg(choose_project(&two, &earlier, "new")),
            "this deploy has projects a, b; pass --project"
        );
        assert_eq!(
            msg(choose_project(&two, &earlier, "smoke")),
            "this deploy has projects a, b, and earlier versions of `smoke` are in smoke; pass \
             --project"
        );
        assert!(msg(choose_project(&two, &earlier, "split")).contains("are in a, b"));
        assert_eq!(
            msg(choose_project(&[], &[], "wf")),
            "this deploy has no projects to register into; add one under `projects:` in the \
             deploy config"
        );
    }
}
