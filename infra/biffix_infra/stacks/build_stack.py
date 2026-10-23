"""CodeBuild projects that build biffix's images.

- `runtime-build` builds coordinator variants: the deployed base coordinator image plus one engine
  runtime layer (a pinned Snakemake or Nextflow release; `codebuild/runtime/build.sh`), one build
  per (runtime, base image). The layer itself is stored once per runtime as layer-only images in
  the coordinator repository (keep-tagged and archived).
- `image-import` imports one workflow image by digest into the tenant's workflow repository,
  keep-tags, probes and archives it (`codebuild/image/import.sh`), one build per distinct digest;
  derives Snakemake task images from imported ones (imported image + a runtime's stored layer),
  one build per (imported digest, runtime); and joins a built image's architectures into one
  image in the builds repository (then keep-tags, probes and archives it like an import). A derive
  is pushed next to its base: an imported image's into the workflow repository, a built one's into
  the builds repository.
- `image-build-amd64` / `image-build-arm64` build a workflow image from the recipe it ships (a
  Dockerfile, or biffix's generated one for a pixi environment; `codebuild/image/build.sh`), one
  build per (recipe, architecture), each on its own architecture's hardware, into the builds
  repository. They are the only privileged projects (BuildKit needs a Docker daemon) and read only
  the build's context tarball and the workflow and builds repositories.

The builds repository is pushed to by the image-build and import projects only (never by the
registration role third-party workflow CI assumes), which is what lets a build reuse a `build-*`
tag it finds there without checking it (`codebuild/image/build.sh`).

The backend starts the builds and follows them through the build projection: the ingest stack
routes these projects' state changes by their exact names (a sibling tenant's share the name
prefix). The projects have
no source: each script is inlined into its buildspec at synth (`InlineScriptProject`, after
`codebuild/lib/common.sh`, which it would otherwise source), so what a build runs is fixed by the
deploy. They are unprivileged except the image builds (images are otherwise handled with crane)
and outside the VPC (they reach only public registries and package hosts, ECR and S3).
"""

from __future__ import annotations

from typing import Any

from aws_cdk import Duration, Stack
from aws_cdk import aws_codebuild as codebuild
from aws_cdk import aws_ecr as ecr
from aws_cdk import aws_iam as iam
from aws_cdk import aws_kms as kms
from aws_cdk import aws_s3 as s3
from constructs import Construct

from biffix_infra.config import IMAGE_BUILD_ARCHS, WORKFLOW_SOURCES_PREFIX, DeployConfig
from biffix_infra.constructs.script_project import InlineScriptProject
from biffix_infra.paths import CODEBUILD_DIR
from biffix_infra.wiring import any_ecr_repository_arn, log_group

RUNTIME_BUILD_SCRIPT = CODEBUILD_DIR / "runtime/build.sh"
IMAGE_IMPORT_SCRIPT = CODEBUILD_DIR / "image/import.sh"
IMAGE_BUILD_SCRIPT = CODEBUILD_DIR / "image/build.sh"
# Where a runtime build keeps what it resolved (the Snakemake package locks), per runtime key.
RUNTIME_ARTIFACT_PREFIX = "runtimes/"
# What a runtime build reports back (codebuild/runtime/build.sh "Outputs"): CodeBuild exports these
# on the build's SUCCEEDED event, which the build projection records as the build's outputs.
RUNTIME_BUILD_OUTPUTS = [
    "VARIANT_IMAGE",
    "VARIANT_DIGEST",
    "ARCHS",
    "RUNTIME_IMAGE_AMD64",
    "RUNTIME_IMAGE_ARM64",
    "RUNTIME_LAYER_AMD64",
    "RUNTIME_LAYER_ARM64",
    "RUNTIME_ARCHIVED",
    "LOCK_SHA256_X86_64",
    "LOCK_SHA256_AARCH64",
    "NF_JAR_SHA256",
    "NF_AMAZON_VERSION",
    "NF_CLOUDCACHE_VERSION",
]
# What an image import reports back (codebuild/image/import.sh "Outputs").
IMAGE_IMPORT_OUTPUTS = [
    "IMAGE",
    "IMAGE_DIGEST",
    "SOURCE_DIGEST",
    "BASE_DIGEST",
    "ARCHS",
    "CAPABILITIES",
    "KEEP_TAG",
    "ARCHIVE_BYTES",
    "ARCHIVED",
]
# What an image build reports back (codebuild/image/build.sh "Outputs").
IMAGE_BUILD_OUTPUTS = ["IMAGE", "IMAGE_DIGEST", "ARCH"]
# The architectures workflow images are built for, and the CodeBuild image native to each.
BUILD_IMAGES = {
    "amd64": codebuild.LinuxBuildImage.AMAZON_LINUX_2023_5,
    "arm64": codebuild.LinuxArmBuildImage.AMAZON_LINUX_2023_STANDARD_3_0,
}
# The config names (and the ingest/sweep rules route) one image-build project per architecture.
if tuple(BUILD_IMAGES) != IMAGE_BUILD_ARCHS:
    raise RuntimeError(f"build images {tuple(BUILD_IMAGES)} != config's {IMAGE_BUILD_ARCHS}")


class BuildStack(Stack):
    def __init__(
        self,
        scope: Construct,
        cid: str,
        *,
        config: DeployConfig,
        coordinator_repo: ecr.IRepository,
        artifact_bucket: s3.IBucket,
        workflow_repo: ecr.IRepository,
        builds_repo: ecr.IRepository,
        image_archive_bucket: s3.IBucket,
        **kwargs: Any,
    ) -> None:
        super().__init__(scope, cid, **kwargs)
        self.builds_repo = builds_repo
        archive_uri = image_archive_bucket.s3_url_for_object("")

        self.runtime_project = InlineScriptProject(
            self,
            "RuntimeBuild",
            project_name=config.runtime_build_project_name,
            description="biffix coordinator variants: base coordinator image + engine runtime",
            script=RUNTIME_BUILD_SCRIPT,
            outputs=RUNTIME_BUILD_OUTPUTS,
            # The Nextflow smoke test runs the release's launcher, which needs a JRE.
            runtimes={"java": "corretto21"},
            build_image=codebuild.LinuxBuildImage.AMAZON_LINUX_2023_5,
            compute_type=codebuild.ComputeType.MEDIUM,
            environment_variables={
                "BIFFIX_COORDINATOR_REPO": coordinator_repo.repository_uri,
                "BIFFIX_RUNTIME_ARTIFACT_URI": artifact_bucket.s3_url_for_object(
                    RUNTIME_ARTIFACT_PREFIX
                ),
                "BIFFIX_ARCHIVE_URI": archive_uri,
            },
            timeout=Duration.minutes(30),
            log_group=log_group(self, "RuntimeBuildLogs"),
        )
        # Reads the base image and pushes the layer images and the variant (crane authenticates
        # with the ECR token).
        coordinator_repo.grant_pull_push(self.runtime_project)
        artifact_bucket.grant_put(self.runtime_project, f"{RUNTIME_ARTIFACT_PREFIX}*")
        # Archives the layer images: read (to skip what it holds) and write, never delete.
        image_archive_bucket.grant_read(self.runtime_project)
        image_archive_bucket.grant_put(self.runtime_project)

        self.import_project = InlineScriptProject(
            self,
            "ImageImport",
            project_name=config.image_import_project_name,
            description="biffix workflow images: import, derive or index; keep-tag, probe, archive",
            script=IMAGE_IMPORT_SCRIPT,
            outputs=IMAGE_IMPORT_OUTPUTS,
            build_image=codebuild.LinuxBuildImage.AMAZON_LINUX_2023_5,
            compute_type=codebuild.ComputeType.SMALL,
            environment_variables={
                "BIFFIX_IMAGE_REPO": workflow_repo.repository_uri,
                "BIFFIX_BUILDS_REPO": builds_repo.repository_uri,
                "BIFFIX_ARCHIVE_URI": archive_uri,
            },
            # Large images (tens of GB) take a while to copy and archive.
            timeout=Duration.minutes(120),
            log_group=log_group(self, "ImageImportLogs"),
        )
        workflow_repo.grant_pull_push(self.import_project)
        # An index joins a build's architectures, and a built image's derive is pushed beside it.
        builds_repo.grant_pull_push(self.import_project)
        # A derive appends a runtime's stored layer (a layer image in the coordinator repository).
        coordinator_repo.grant_pull(self.import_project)
        # A declared image may be in any private ECR repository the tenant can read, including
        # another account's whose repository policy grants it: read-only.
        self.import_project.add_to_role_policy(
            iam.PolicyStatement(
                actions=["ecr:BatchGetImage", "ecr:GetDownloadUrlForLayer"],
                resources=[any_ecr_repository_arn(self)],
            )
        )
        # The archive: read (to skip blobs it holds) and write, never delete.
        image_archive_bucket.grant_read(self.import_project)
        image_archive_bucket.grant_put(self.import_project)

        build_logs = log_group(self, "ImageBuildLogs")
        self.build_projects: dict[str, codebuild.Project] = {}
        for arch, build_image in BUILD_IMAGES.items():
            project = InlineScriptProject(
                self,
                f"ImageBuild{arch.capitalize()}",
                project_name=config.image_build_project_name(arch),
                description=f"biffix workflow images: build a recipe for {arch}",
                script=IMAGE_BUILD_SCRIPT,
                outputs=IMAGE_BUILD_OUTPUTS,
                build_image=build_image,
                compute_type=codebuild.ComputeType.MEDIUM,
                # BuildKit runs in the build's own Docker daemon.
                privileged=True,
                environment_variables={
                    "BIFFIX_BUILD_ARCH": arch,
                    "BIFFIX_BUILDS_REPO": builds_repo.repository_uri,
                },
                timeout=Duration.minutes(60),
                log_group=build_logs,
            )
            # Pushes its architecture's image into the builds repository. Its base images come from
            # public registries, or from the workflow repository (an image the workflow's CI
            # pushed), which it only reads.
            builds_repo.grant_pull_push(project)
            workflow_repo.grant_pull(project)
            self.build_projects[arch] = project

    def grant_context_read(self, bucket: s3.IBucket, key: kms.IKey | None) -> None:
        """Let the image builds read the build contexts the CLI stages under `workflow-sources/`
        in `bucket` (and decrypt them with its customer key, when it has one)."""
        for project in self.build_projects.values():
            bucket.grant_read(project, f"{WORKFLOW_SOURCES_PREFIX}*")
            if key is not None:
                key.grant_decrypt(project)

    def grant_start_builds(self, grantee: iam.IGrantable) -> None:
        """Start runtime builds, image imports and image builds, and read their state (the backend,
        whose sweep is also the stale-build backstop)."""
        grantee.grant_principal.add_to_principal_policy(
            iam.PolicyStatement(
                actions=["codebuild:StartBuild", "codebuild:BatchGetBuilds"],
                resources=[
                    self.runtime_project.project_arn,
                    self.import_project.project_arn,
                    *(p.project_arn for p in self.build_projects.values()),
                ],
            )
        )
