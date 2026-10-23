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
    for b in builds {
        let upload = client
            .source_upload(&SourceUploadRequest {
                version_id: source_id.to_string(),
                context_tree_hash: Some(b.context.tree_hash.clone()),
                project: project.to_string(),
            })
            .await?;
        client.put_presigned(&upload, b.context.bytes).await?;
        eprintln!(
            "[biffix] image `{}`: staged its build context ({} for {})",
            b.name,
            source::context_label(&b.spec.context),
            b.archs.iter().map(|a| a.as_str()).collect::<Vec<_>>().join(", ")
        );
        out.push((
            b.name,
            ImageRequest {
                reference: None,
                build: Some(biffix_api::ImageBuildRequest {
                    kind: b.spec.kind.as_str().to_string(),
                    context: b.spec.context,
                    dockerfile: b.spec.dockerfile,
                    context_uri: upload.uri,
                    context_sha256: b.context.sha256,
                    tree_hash: b.context.tree_hash,
                    archs: b.archs.iter().map(|a| a.as_str().to_string()).collect(),
                }),
                runtime: b.runtime,
            },
        ));
    }
    Ok(out)
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
}
