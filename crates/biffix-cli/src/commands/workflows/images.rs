//! The images a registration names: the default and named references from `biffix.yaml` and
//! `--image`, and the ones built from a recipe in the workflow (pixi or Dockerfile), whose
//! contexts are tarred, hashed and staged beside the source.

use std::path::Path;

use biffix_api::{ImageRequest, SourceUploadRequest};
use biffix_workflow::manifest::BiffixManifest;

use crate::api::ApiClient;
use crate::error::{CliError, CliResult};
use crate::local::source::{self, TrackedTree};

/// The registration's images from the manifest and `--image`: the default image to send as
/// `image` (`--image` wins over `image:` in `biffix.yaml`), the rest of `images:` that name a
/// reference, by name, and the ones built from a recipe in the workflow. Validated here with the
/// server's rules, so a bad entry fails before anything is staged.
pub(super) struct RequestedImages {
    pub(super) default: Option<String>,
    pub(super) named: std::collections::BTreeMap<String, ImageRequest>,
    pub(super) builds: Vec<RequestedBuild>,
}

/// An `images:` entry built from a recipe in the workflow.
pub(super) struct RequestedBuild {
    name: String,
    spec: biffix_workflow::images::BuildSpec,
    runtime: Option<String>,
}

pub(super) fn request_images(
    manifest: &BiffixManifest,
    cli_image: Option<&str>,
) -> CliResult<RequestedImages> {
    let cli_image = cli_image.filter(|s| !s.is_empty());
    let mut images = manifest.images.clone();
    // `--image` replaces whatever the manifest names as the default.
    if cli_image.is_some() {
        images.remove("default");
    }
    let default = cli_image.or(manifest.image.as_deref()).map(str::to_string);

    let declared = biffix_workflow::images::declared_images(&images, default.as_deref())
        .map_err(|e| CliError::InvalidInput(e.to_string()))?;
    for d in &declared {
        if let biffix_workflow::images::ImageSource::Build(spec) = &d.source {
            let suspect = secret_like_build_args(&spec.options.build_args);
            if !suspect.is_empty() {
                teprintln!(
                    "[biffix] warning: image `{}`: build args {} look like credentials. Build args \
                     are not secret: they are stored on the version, shown in the UI and API, \
                     passed to the build as environment variables and kept in the image's \
                     history. Use a build secret in the image's own CI instead",
                    d.name,
                    suspect.join(", ")
                );
            }
        }
    }
    let builds = declared
        .into_iter()
        .filter_map(|d| match d.source {
            biffix_workflow::images::ImageSource::Build(spec) => Some(RequestedBuild {
                runtime: images.get(&d.name).and_then(|decl| decl.runtime.clone()),
                name: d.name,
                spec,
            }),
            biffix_workflow::images::ImageSource::Ref(_) => None,
        })
        .collect();
    let named = images
        .into_iter()
        .filter_map(|(name, decl)| {
            decl.reference.map(|reference| {
                (
                    name,
                    ImageRequest { reference: Some(reference), build: None, runtime: decl.runtime },
                )
            })
        })
        .collect();
    Ok(RequestedImages { default, named, builds })
}

/// The keys of `build_args` that look like credentials (`TOKEN`, `SECRET`, `PASSWORD` or `_KEY`
/// in them, any case), sorted.
fn secret_like_build_args(build_args: &std::collections::BTreeMap<String, String>) -> Vec<&str> {
    build_args
        .keys()
        .filter(|k| {
            let k = k.to_ascii_uppercase();
            ["TOKEN", "SECRET", "PASSWORD", "PASSWD", "APIKEY", "CREDENTIAL", "_KEY"]
                .iter()
                .any(|s| k.contains(s))
        })
        .map(String::as_str)
        .collect()
}

/// A build's context, tarred and hashed, ready to stage.
pub(super) struct PreparedBuild {
    pub(super) name: String,
    runtime: Option<String>,
    spec: biffix_workflow::images::BuildSpec,
    archs: Vec<biffix_workflow::images::Arch>,
    context: source::ContextTarball,
}

/// The architectures a pixi environment can be built for: the Linux platforms its `pixi.toml`
/// lists (`linux-64` → amd64, `linux-aarch64` → arm64).
fn pixi_archs(pixi_toml: &str, dir: &str) -> CliResult<Vec<biffix_workflow::images::Arch>> {
    use biffix_workflow::images::Arch;
    let doc: toml::Value = toml::from_str(pixi_toml)
        .map_err(|e| CliError::InvalidInput(format!("{dir}/pixi.toml: {e}")))?;
    let platforms = ["workspace", "project"]
        .iter()
        .find_map(|t| doc.get(t).and_then(|t| t.get("platforms")))
        .and_then(|p| p.as_array())
        .ok_or_else(|| CliError::InvalidInput(format!("{dir}/pixi.toml lists no `platforms`")))?;
    let mut archs: Vec<Arch> = platforms
        .iter()
        .filter_map(|p| match p.as_str()? {
            "linux-64" => Some(Arch::Amd64),
            "linux-aarch64" => Some(Arch::Arm64),
            _ => None,
        })
        .collect();
    archs.sort();
    archs.dedup();
    if archs.is_empty() {
        return Err(CliError::InvalidInput(format!(
            "{dir}/pixi.toml has no Linux platform to build an image for; add `linux-64` (and \
             `linux-aarch64` for Graviton), then run `pixi lock`"
        )));
    }
    Ok(archs)
}

/// Tar and hash each build's context (before anything is staged, so a bad one fails first).
pub(super) fn prepare_builds(
    root: &Path,
    tree: &TrackedTree,
    builds: Vec<RequestedBuild>,
) -> CliResult<Vec<PreparedBuild>> {
    use biffix_workflow::images::{Arch, BuildKind};
    builds
        .into_iter()
        .map(|b| {
            let (context, archs) = match b.spec.kind {
                BuildKind::Pixi => {
                    let dir = source::context_label(&b.spec.context);
                    let context = source::context_tarball(
                        tree,
                        &b.spec.context,
                        Some(&biffix_workflow::pixi::PIXI_FILES),
                    )
                    .map_err(|e| match e {
                        CliError::InvalidInput(m) => CliError::InvalidInput(format!(
                            "image `{}`: {m} (a pixi image is built from its committed pixi.toml \
                             and pixi.lock; run `pixi lock` and commit both)",
                            b.name
                        )),
                        e => e,
                    })?;
                    let toml =
                        std::fs::read_to_string(root.join(&b.spec.context).join("pixi.toml"))
                            .map_err(|e| CliError::InvalidInput(format!("{dir}/pixi.toml: {e}")))?;
                    (context, pixi_archs(&toml, dir)?)
                }
                BuildKind::Dockerfile => {
                    let context = source::context_tarball(tree, &b.spec.context, None)?;
                    let dockerfile = b.spec.dockerfile.as_deref().unwrap_or_default();
                    if !context.files.iter().any(|f| f == dockerfile) {
                        return Err(CliError::InvalidInput(format!(
                            "image `{}`: {dockerfile} is not a tracked file in its context `{}` \
                             (only git-tracked files are built)",
                            b.name,
                            source::context_label(&b.spec.context)
                        )));
                    }
                    let archs = b.spec.archs.clone().unwrap_or_else(|| Arch::ALL.to_vec());
                    (context, archs)
                }
            };
            Ok(PreparedBuild { name: b.name, runtime: b.runtime, spec: b.spec, archs, context })
        })
        .collect()
}

/// Stage each build's context beside the source (in `project`'s bucket) and describe the build for
/// the registration.
pub(super) async fn stage_builds(
    client: &ApiClient,
    source_id: &str,
    project: &str,
    builds: Vec<PreparedBuild>,
) -> CliResult<Vec<(String, ImageRequest)>> {
    let mut out = Vec::with_capacity(builds.len());
    for mut b in builds {
        let upload = client
            .source_upload(&SourceUploadRequest {
                version_id: source_id.to_string(),
                context_tree_hash: Some(b.context.tree_hash.clone()),
                project: project.to_string(),
            })
            .await?;
        client.put_presigned(&upload, std::mem::take(&mut b.context.bytes)).await?;
        teprintln!(
            "[biffix] image `{}`: staged its build context ({} for {})",
            b.name,
            source::context_label(&b.spec.context),
            b.archs.iter().map(|a| a.as_str()).collect::<Vec<_>>().join(", ")
        );
        out.push(build_request(b, upload.uri));
    }
    Ok(out)
}

/// Whether any of `images` is a build given a target or build args.
pub(super) fn sends_build_options(
    images: &std::collections::BTreeMap<String, ImageRequest>,
) -> bool {
    images
        .values()
        .filter_map(|i| i.build.as_ref())
        .any(|b| b.target.is_some() || !b.build_args.is_empty())
}

/// `err`, from reading back the version `id` just registered to check `what` it recorded (`build
/// options`, `queue assignment`), annotated with that id: the server stored the version, and an
/// error that did not name it would lose it. Keeps the exit code.
pub(super) fn read_back_failed(id: &str, what: &str, err: CliError) -> CliError {
    err.map_message(|m| format!("registered {id}, but its {what} could not be read back: {m}"))
}

/// Refuse a registration whose deployment dropped the target or build args `sent` (as a backend
/// that predates them ignores fields it does not know): `shown`, the version `id`'s images as the
/// deployment records them, must carry each build's target and build args as sent. The version is
/// registered by then, but its images would build without them.
pub(super) fn check_build_options_kept(
    id: &str,
    sent: &std::collections::BTreeMap<String, ImageRequest>,
    shown: &[biffix_api::WorkflowImage],
) -> CliResult<()> {
    let dropped: Vec<&str> = sent
        .iter()
        .filter_map(|(name, i)| Some((name, i.build.as_ref()?)))
        .filter(|(_, b)| b.target.is_some() || !b.build_args.is_empty())
        .filter(|(name, b)| {
            !shown
                .iter()
                .any(|s| &s.name == *name && s.target == b.target && s.build_args == b.build_args)
        })
        .map(|(name, _)| name.as_str())
        .collect();
    if dropped.is_empty() {
        return Ok(());
    }
    Err(CliError::Unclassified(format!(
        "registered {id}, but the deployment predates build options; deploy it first \
         (`deploy.sh` without `--stack`): it recorded {} without the target or build args sent, \
         so they would build the wrong image. Do not run {id}; register again once it is deployed",
        dropped.iter().map(|n| format!("`{n}`")).collect::<Vec<_>>().join(", ")
    )))
}

/// The registration's request for build `b`, whose context was staged at `context_uri`.
fn build_request(b: PreparedBuild, context_uri: String) -> (String, ImageRequest) {
    let options = b.spec.options;
    (
        b.name,
        ImageRequest {
            reference: None,
            build: Some(biffix_api::ImageBuildRequest {
                kind: b.spec.kind.as_str().to_string(),
                context: b.spec.context,
                dockerfile: b.spec.dockerfile,
                context_uri,
                context_sha256: b.context.sha256,
                tree_hash: b.context.tree_hash,
                archs: b.archs.iter().map(|a| a.as_str().to_string()).collect(),
                target: options.target,
                build_args: options.build_args,
            }),
            runtime: b.runtime,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn images_come_from_the_manifest_and_image_overrides_the_default() {
        let manifest: BiffixManifest = serde_norway::from_str(
            "image: quay.io/a/base:1\nimages:\n  fastqc:\n    ref: quay.io/b/fastqc:1\n",
        )
        .unwrap();
        let got = request_images(&manifest, None).unwrap();
        assert_eq!(got.default.as_deref(), Some("quay.io/a/base:1"));
        assert_eq!(got.named["fastqc"].reference.as_deref(), Some("quay.io/b/fastqc:1"));
        let got = request_images(&manifest, Some("sha256:abc")).unwrap();
        assert_eq!(got.default.as_deref(), Some("sha256:abc"));
        // `images.default` travels by name; `--image` replaces it.
        let by_name: BiffixManifest =
            serde_norway::from_str("images:\n  default:\n    ref: quay.io/a/base:1\n").unwrap();
        let got = request_images(&by_name, None).unwrap();
        assert_eq!((got.default, got.named.len()), (None, 1));
        let got = request_images(&by_name, Some("x:2")).unwrap();
        assert_eq!((got.default.as_deref(), got.named.len()), (Some("x:2"), 0));
        // A build is set apart to be staged; a bad one fails here.
        let builds: BiffixManifest = serde_norway::from_str(
            "images:\n  a:\n    dockerfile: docker/Dockerfile\n    runtime: own\n",
        )
        .unwrap();
        let got = request_images(&builds, None).unwrap();
        assert!(got.named.is_empty());
        assert_eq!(got.builds[0].name, "a");
        assert_eq!(got.builds[0].spec.context, "docker");
        assert_eq!(got.builds[0].runtime.as_deref(), Some("own"));
        let bad: BiffixManifest =
            serde_norway::from_str("images:\n  a:\n    dockerfile: ../Dockerfile\n").unwrap();
        assert!(request_images(&bad, None).is_err());
    }

    #[test]
    fn a_pixi_image_is_built_for_the_linux_platforms_it_locks() {
        use biffix_workflow::images::Arch;
        let toml = "[workspace]\nplatforms = [\"osx-arm64\", \"linux-aarch64\", \"linux-64\"]\n";
        assert_eq!(pixi_archs(toml, "env").unwrap(), [Arch::Amd64, Arch::Arm64]);
        let legacy = "[project]\nplatforms = [\"linux-64\"]\n";
        assert_eq!(pixi_archs(legacy, "env").unwrap(), [Arch::Amd64]);
        assert!(pixi_archs("[workspace]\nplatforms = [\"osx-arm64\"]\n", "env").is_err());
        assert!(pixi_archs("[workspace]\n", "env").is_err());
    }

    fn repo_with(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| crate::test_git::git_ok(dir.path(), args);
        git(&["init", "-q"]);
        git(&["config", "user.email", "t@t.t"]);
        git(&["config", "user.name", "t"]);
        git(&["config", "commit.gpgsign", "false"]);
        for (path, text) in files {
            let path = dir.path().join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "c"]);
        dir
    }

    fn builds_of(yaml: &str) -> Vec<RequestedBuild> {
        let manifest: BiffixManifest = serde_norway::from_str(yaml).unwrap();
        request_images(&manifest, None).unwrap().builds
    }

    fn prepare_builds_in(
        root: &Path,
        builds: Vec<RequestedBuild>,
    ) -> CliResult<Vec<PreparedBuild>> {
        prepare_builds(root, &TrackedTree::list(root)?, builds)
    }

    #[test]
    fn builds_are_prepared_from_their_tracked_context() {
        use biffix_workflow::images::Arch;
        let repo = repo_with(&[
            ("env/pixi.toml", "[workspace]\nplatforms = [\"linux-64\", \"osx-arm64\"]\n"),
            ("env/pixi.lock", "version: 6\n"),
            ("env/README.md", "not part of the image\n"),
            ("docker/Dockerfile", "FROM x\n"),
        ]);
        let prepared = prepare_builds_in(
            repo.path(),
            builds_of(
                "images:\n  default:\n    pixi: env\n  tool:\n    dockerfile: docker/Dockerfile\n    archs: [arm64]\n",
            ),
        )
        .unwrap();
        let (default, tool) = (&prepared[0], &prepared[1]);
        assert_eq!(default.name, "default");
        assert_eq!(default.context.files, ["pixi.lock", "pixi.toml"]);
        assert_eq!(default.archs, [Arch::Amd64]);
        assert_eq!(tool.context.files, ["Dockerfile"]);
        assert_eq!(tool.archs, [Arch::Arm64]);

        // A pixi environment without its lock, or a Dockerfile that is not tracked, fails here.
        let unlocked = repo_with(&[("env/pixi.toml", "[workspace]\nplatforms = [\"linux-64\"]\n")]);
        let err =
            prepare_builds_in(unlocked.path(), builds_of("images:\n  default:\n    pixi: env\n"))
                .err()
                .unwrap();
        assert!(
            matches!(err, CliError::InvalidInput(ref m)
                if m.contains("env/pixi.lock is not a tracked file") && m.contains("pixi lock")),
            "{err:?}"
        );
        let untracked = repo_with(&[("docker/other", "x\n")]);
        let err = prepare_builds_in(
            untracked.path(),
            builds_of("images:\n  tool:\n    dockerfile: docker/Dockerfile\n"),
        )
        .err()
        .unwrap();
        assert!(
            matches!(err, CliError::InvalidInput(ref m)
                if m.contains("Dockerfile is not a tracked file in its context")),
            "{err:?}"
        );
    }

    #[test]
    fn a_dockerfile_build_s_target_and_build_args_reach_its_request() {
        let repo = repo_with(&[("docker/Dockerfile", "FROM x AS builder\nFROM x\n")]);
        let prepared = prepare_builds_in(
            repo.path(),
            builds_of(
                "images:\n  builder:\n    dockerfile: docker/Dockerfile\n    target: builder\n    \
                 build_args: {V: \"1\"}\n  worker:\n    dockerfile: docker/Dockerfile\n",
            ),
        )
        .unwrap();
        let mut prepared = prepared.into_iter();
        let builder = build_request(prepared.next().unwrap(), "s3://b/c.tar.gz".into());
        assert_eq!(builder.0, "builder");
        let build = builder.1.build.unwrap();
        assert_eq!(build.target.as_deref(), Some("builder"));
        assert_eq!(build.build_args, [("V".to_string(), "1".to_string())].into());
        assert_eq!(build.context_uri, "s3://b/c.tar.gz");
        // Without them, the request is as before.
        let worker = build_request(prepared.next().unwrap(), "s3://b/c.tar.gz".into()).1;
        let json = serde_json::to_value(worker.build.unwrap()).unwrap();
        assert!(json.get("target").is_none() && json.get("build_args").is_none(), "{json}");
    }

    #[test]
    fn a_deployment_that_drops_build_options_is_caught_after_registering() {
        let sent = |target: Option<&str>, args: &[(&str, &str)]| {
            std::collections::BTreeMap::from([(
                "builder".to_string(),
                ImageRequest {
                    reference: None,
                    build: Some(biffix_api::ImageBuildRequest {
                        kind: "dockerfile".into(),
                        context: "docker".into(),
                        dockerfile: Some("Dockerfile".into()),
                        context_uri: "s3://b/c.tar.gz".into(),
                        context_sha256: "0".repeat(64),
                        tree_hash: "t".into(),
                        archs: vec!["amd64".into()],
                        target: target.map(str::to_string),
                        build_args: args
                            .iter()
                            .map(|(k, v)| (k.to_string(), v.to_string()))
                            .collect(),
                    }),
                    runtime: None,
                },
            )])
        };
        // What an older backend shows: the image, without the options it never read.
        let shown = |target: Option<&str>, args: &[(&str, &str)]| {
            serde_json::from_value::<Vec<biffix_api::WorkflowImage>>(serde_json::json!([{
                "name": "builder", "kind": "dockerfile", "declared_ref": "builder",
                "source_ref": "dockerfile:docker/Dockerfile", "state": "building",
                "target": target,
                "build_args": args.iter().map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect::<std::collections::BTreeMap<_, _>>(),
            }]))
            .unwrap()
        };
        let options = sent(Some("builder"), &[("V", "1")]);
        // Nothing to compare when nothing was sent: no options, or no build.
        assert!(!sends_build_options(&sent(None, &[])));
        assert!(sends_build_options(&options));
        assert!(check_build_options_kept("v:1", &options, &shown(Some("builder"), &[("V", "1")]))
            .is_ok());
        for dropped in [shown(None, &[]), shown(Some("builder"), &[]), shown(None, &[("V", "1")])] {
            let err = check_build_options_kept("v:1", &options, &dropped).unwrap_err();
            assert!(
                matches!(&err, CliError::Unclassified(m)
                    if m.contains("the deployment predates build options; deploy it first")
                        && m.contains("v:1") && m.contains("`builder`")),
                "{err:?}"
            );
        }
    }

    #[test]
    fn a_failed_read_back_names_the_version_and_keeps_its_exit_code() {
        for err in [
            CliError::Unavailable("service unavailable".into()),
            CliError::Forbidden("forbidden".into()),
        ] {
            let code = err.exit_code();
            let annotated = read_back_failed("v:1", "queue assignment", err);
            assert_eq!(annotated.exit_code(), code);
            let m = annotated.to_string();
            assert!(
                m.contains("registered v:1, but its queue assignment could not be read back: "),
                "{m}"
            );
        }
    }

    #[test]
    fn build_args_that_look_like_credentials_are_named() {
        let args = |keys: &[&str]| -> std::collections::BTreeMap<String, String> {
            keys.iter().map(|k| (k.to_string(), "v".to_string())).collect()
        };
        assert_eq!(
            secret_like_build_args(&args(&[
                "GITHUB_TOKEN",
                "db_password",
                "Client_Secret",
                "AWS_ACCESS_KEY_ID",
                "api_key",
                "DB_PASSWD",
                "SERVICE_APIKEY",
                "GCP_CREDENTIALS",
                "TOOL_VERSION",
                "KEYRING",
                "MONKEY",
            ])),
            [
                "AWS_ACCESS_KEY_ID",
                "Client_Secret",
                "DB_PASSWD",
                "GCP_CREDENTIALS",
                "GITHUB_TOKEN",
                "SERVICE_APIKEY",
                "api_key",
                "db_password"
            ]
        );
        assert!(secret_like_build_args(&args(&["TOOL_VERSION"])).is_empty());
    }
}
