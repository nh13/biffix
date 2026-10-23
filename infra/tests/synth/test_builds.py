"""The image build and import projects, the builds repository and the image archive."""

import base64
import dataclasses
import fnmatch
import gzip
import json
from pathlib import Path

import aws_cdk as cdk
import pytest
from aws_cdk.assertions import Template

from biffix_infra.app_graph import Stacks
from biffix_infra.config import ImageBuildTrust, parse_image_archive
from biffix_infra.constructs.script_project import COMMON_SCRIPT, bundled_script
from biffix_infra.stacks.build_stack import IMAGE_BUILD_SCRIPT, IMAGE_IMPORT_SCRIPT
from biffix_infra.stacks.data_stack import DataStack
from tests.helpers import REPO_ROOT
from tests.synth.synth_helpers import (
    CONFIG,
    Json,
    _actions,
    _actions_for_role,
    _build,
    _grants,
    _logical_id,
    _policies,
    _resources,
    _statements_for,
)


def test_tagged_images_never_expire_by_lifecycle(default_stacks: Stacks) -> None:
    """A lifecycle rule selects an image by ANY matching tag, so any rule on tagged images could
    delete a deployed or pinned build: every repo only expires untagged images (deploy.sh prunes
    old coordinator build tags). The backend writes no images or tags and copies none (imports
    do); it only reads a tag's manifest (BatchGetImage) to pin it."""
    t = Template.from_stack(default_stacks["data"])
    repos = t.find_resources("AWS::ECR::Repository")
    assert len(repos) == 3
    for repo in repos.values():
        rules = json.loads(repo["Properties"]["LifecyclePolicy"]["LifecyclePolicyText"])["rules"]
        assert [r["selection"]["tagStatus"] for r in rules] == ["untagged"], rules
    api = default_stacks["api"]
    t = Template.from_stack(api)
    # Both backend Lambdas: the API and the sweep (which launches parked runs).
    for fn in (api.backend_lambda, api.sweep_lambda):
        statements = _statements_for(t, api, fn.role)
        assert statements, fn
        for write_or_copy in (
            "ecr:PutImage",
            "ecr:BatchDeleteImage",
            "ecr:InitiateLayerUpload",
            "ecr:UploadLayerPart",
            "ecr:CompleteLayerUpload",
            "ecr:GetDownloadUrlForLayer",
        ):
            assert not any(_grants(st, write_or_copy) for st in statements), (fn, write_or_copy)


def test_the_image_import_project_runs_the_repo_import_script(default_stacks: Stacks) -> None:
    build = default_stacks["build"]
    t = Template.from_stack(build)
    projects = {
        p["Properties"]["Name"]: p["Properties"]
        for p in t.find_resources("AWS::CodeBuild::Project").values()
    }
    props = projects["biffix-acme-image-import"]
    assert props["Source"]["Type"] == "NO_SOURCE"
    assert props["Environment"]["PrivilegedMode"] is False
    spec = json.loads(props["Source"]["BuildSpec"])
    exported = set(spec["env"]["exported-variables"])
    assert {"IMAGE_DIGEST", "CAPABILITIES", "KEEP_TAG", "BASE_DIGEST"} <= exported
    packed = spec["phases"]["build"]["commands"][0].split()[1]
    assert gzip.decompress(base64.b64decode(packed)).decode() == bundled_script(IMAGE_IMPORT_SCRIPT)
    granted = _actions_for_role(t, build, build.import_project.role)
    assert {"ecr:PutImage", "s3:PutObject", "s3:GetObject*", "ecr:BatchGetImage"} <= granted
    statements = _statements_for(t, build, build.import_project.role)
    # The archive is append-only for the import: it never deletes.
    assert not any(_grants(st, "s3:DeleteObject") for st in statements), granted
    # A derive pulls a runtime's layer image from the coordinator repository (read-only).
    coordinator_repo_statements = [
        statement
        for statement in statements
        if "CoordinatorRepo" in json.dumps(statement["Resource"])
    ]
    assert coordinator_repo_statements, "the import project cannot pull the coordinator repository"
    for statement in coordinator_repo_statements:
        assert not _grants(statement, "ecr:PutImage"), statement
    # It imports into the workflow repository, and indexes (and derives built images) into the
    # builds repository: it is told both, and may push to both.
    env = {v["Name"]: v["Value"] for v in props["Environment"]["EnvironmentVariables"]}
    assert "WorkflowRepo" in json.dumps(env["BIFFIX_IMAGE_REPO"]), env
    assert "BuildsRepo" in json.dumps(env["BIFFIX_BUILDS_REPO"]), env
    for repo in ("WorkflowRepo", "BuildsRepo"):
        assert any(
            _grants(st, "ecr:PutImage") and repo in json.dumps(st["Resource"]) for st in statements
        ), repo


_ECR_PUSH = ("ecr:PutImage", "ecr:InitiateLayerUpload", "ecr:CompleteLayerUpload")


def _pushes(statement: Json) -> bool:
    """Whether `statement` grants any ECR push action (wildcards included)."""
    return statement.get("Effect", "Allow") == "Allow" and any(
        _grants(statement, a) for a in _ECR_PUSH
    )


# A repository ARN as a synthesized one renders: a wildcard resource that covers it (`*`,
# `…:repository/*`) covers the builds repository too.
_SOME_REPOSITORY = "arn:aws:ecr:{Ref:AWS::Region}:{Ref:AWS::AccountId}:repository/some-repo"


def _covers(resource: str, name: str) -> bool:
    """Whether a rendered resource names the `name` resource (by its logical id) or is a wildcard
    that covers it."""
    return name in resource or fnmatch.fnmatchcase(_SOME_REPOSITORY, resource)


def _builds_repo_pushers(stacks: Stacks) -> set[tuple[str, str]]:
    """(stack, role logical id) of every role any policy in the graph lets push to the builds
    repository, by name or by a wildcard."""
    pushers: set[tuple[str, str]] = set()
    for name, value in stacks.items():
        for stack in value if isinstance(value, list) else [value]:
            if not isinstance(stack, cdk.Stack):
                continue
            for pol in _policies(Template.from_stack(stack)):
                for st in pol["Properties"]["PolicyDocument"]["Statement"]:
                    if _pushes(st) and any(_covers(r, "BuildsRepo") for r in _resources(st)):
                        pushers |= {(name, r["Ref"]) for r in pol["Properties"].get("Roles", [])}
    return pushers


def test_only_the_build_and_import_projects_can_push_to_the_builds_repo(tmp_path: Path) -> None:
    # Security contract (codebuild/image/build.sh reuses a build-* tag it finds there unchecked):
    # nothing third-party workflow CI can assume (the registration role) may push to the builds
    # repository, and neither may biffix's own GitHub image-build role (it builds the coordinator).
    config = dataclasses.replace(
        CONFIG,
        github_subjects=["repo:acme/pipelines:ref:refs/heads/main"],
        image_build_trust=ImageBuildTrust(subject="repo:example/biffix:environment:image-build"),
    )
    stacks = _build(tmp_path, config)
    build, identity = stacks["build"], stacks["identity"]
    assert identity.registration_role is not None and identity.image_build_role is not None
    expected = {
        ("build", _logical_id(build, role))
        for role in (build.import_project.role, *(p.role for p in build.build_projects.values()))
    }
    assert _builds_repo_pushers(stacks) == expected
    # The identity stack's roles are not given the builds repository at all (not even pull: a
    # workflow's CI pushes its own images, and never reads biffix's builds).
    assert "BuildsRepo" not in json.dumps(Template.from_stack(identity).to_json())


def test_the_image_build_projects_build_natively_and_privileged(default_stacks: Stacks) -> None:
    build = default_stacks["build"]
    t = Template.from_stack(build)
    projects = {
        p["Properties"]["Name"]: p["Properties"]
        for p in t.find_resources("AWS::CodeBuild::Project").values()
    }
    expected_type = {"amd64": "LINUX_CONTAINER", "arm64": "ARM_CONTAINER"}
    for arch, env_type in expected_type.items():
        props = projects[f"biffix-acme-image-build-{arch}"]
        assert props["Source"]["Type"] == "NO_SOURCE"
        assert props["Environment"]["PrivilegedMode"] is True
        assert props["Environment"]["Type"] == env_type
        env = {v["Name"]: v["Value"] for v in props["Environment"]["EnvironmentVariables"]}
        assert env["BIFFIX_BUILD_ARCH"] == arch
        # It pushes into the builds repository (the variable build.sh requires), and knows no other.
        assert "BuildsRepo" in json.dumps(env["BIFFIX_BUILDS_REPO"]), env
        assert "BIFFIX_IMAGE_REPO" not in env, env
        assert '"${BIFFIX_BUILDS_REPO:?}"' in IMAGE_BUILD_SCRIPT.read_text()
        spec = json.loads(props["Source"]["BuildSpec"])
        assert set(spec["env"]["exported-variables"]) == {"IMAGE", "IMAGE_DIGEST", "ARCH"}
        packed = spec["phases"]["build"]["commands"][0].split()[1]
        assert gzip.decompress(base64.b64decode(packed)).decode() == bundled_script(
            IMAGE_BUILD_SCRIPT
        )
        role = build.build_projects[arch].role
        granted = _actions_for_role(t, build, role)
        assert {"ecr:PutImage", "ecr:BatchGetImage"} <= granted
        # The privileged builds never touch the archive or the coordinator repository.
        statements = _statements_for(t, build, role)
        for statement in statements:
            resource = json.dumps(statement["Resource"])
            assert "ImageArchiveBucket" not in resource, statement
            assert "CoordinatorRepo" not in resource, statement
            # Nor through a wildcard: they push to the builds repository only, and write objects
            # in no bucket a wildcard could make the archive.
            if _pushes(statement):
                assert all("BuildsRepo" in r for r in _resources(statement)), statement
            if any(_grants(statement, a) for a in ("s3:PutObject", "s3:DeleteObject")):
                assert not any(
                    fnmatch.fnmatchcase("arn:aws:s3:::some-bucket/x", r)
                    for r in _resources(statement)
                ), statement
            # The workflow repository (a base image its CI pushed) is only read.
            if "WorkflowRepo" in resource:
                assert not _pushes(statement), statement
        assert any(
            _pushes(st) and "BuildsRepo" in json.dumps(st["Resource"]) for st in statements
        ), statements
        # They read build contexts from the staged workflow sources (the default project bucket).
        assert any(
            "s3:GetObject*" in _actions(st) and "workflow-sources/*" in json.dumps(st["Resource"])
            for st in statements
        ), statements


def test_build_logs_expire(default_stacks: Stacks) -> None:
    """Every build project logs to a group of its stack that expires: a project without its own
    group would write to a default one that never does."""
    t = Template.from_stack(default_stacks["build"])
    groups = t.find_resources("AWS::Logs::LogGroup")
    assert groups
    for group in groups.values():
        assert group["Properties"]["RetentionInDays"] == 365
    projects = t.find_resources("AWS::CodeBuild::Project")
    assert len(projects) == len(CONFIG.build_project_names), projects.keys()
    for lid, project in projects.items():
        logs = project["Properties"]["LogsConfig"]["CloudWatchLogs"]
        assert logs["Status"] == "ENABLED", (lid, logs)
        assert logs["GroupName"] in [{"Ref": gid} for gid in groups], (lid, logs)


def test_backend_and_sweep_may_start_image_builds(default_stacks: Stacks) -> None:
    api = default_stacks["api"]
    t = Template.from_stack(api)
    wired = [
        f["Properties"]["Environment"]["Variables"]
        for f in t.find_resources("AWS::Lambda::Function").values()
        if "BIFFIX_IMAGE_BUILD_PROJECT_AMD64" in f["Properties"]["Environment"]["Variables"]
    ]
    assert len(wired) == 2
    for w in wired:
        assert w["BIFFIX_IMAGE_BUILD_PROJECT_AMD64"] == "biffix-acme-image-build-amd64"
        assert w["BIFFIX_IMAGE_BUILD_PROJECT_ARM64"] == "biffix-acme-image-build-arm64"
        # The backend trusts a built image only from the builds repository.
        assert "BuildsRepo" in json.dumps(w["BIFFIX_BUILDS_IMAGE_REPO"]), w
        assert "WorkflowRepo" in json.dumps(w["BIFFIX_WORKER_IMAGE_REPO"]), w


def test_the_keep_tag_prefix_matches_the_workflow_crate() -> None:
    # codebuild/lib/common.sh applies the keep-tags biffix-workflow names (images::KEEP_TAG_PREFIX).
    images = (REPO_ROOT / "crates/biffix-workflow/src/images/mod.rs").read_text()
    assert 'pub const KEEP_TAG_PREFIX: &str = "biffix-keep-";' in images
    assert "KEEP_TAG_PREFIX=biffix-keep-" in COMMON_SCRIPT.read_text()


def _archive_bucket(data_stack: DataStack) -> Json:
    t = Template.from_stack(data_stack)
    (bucket,) = [
        r
        for lid, r in t.find_resources("AWS::S3::Bucket").items()
        if lid.startswith("ImageArchiveBucket")
    ]
    props: Json = bucket["Properties"]
    return props


def test_the_image_archive_tiers_only_large_blobs(default_stacks: Stacks) -> None:
    props = _archive_bucket(default_stacks["data"])
    assert props["VersioningConfiguration"]["Status"] == "Enabled"
    assert "ObjectLockEnabled" not in props
    (tier,) = [r for r in props["LifecycleConfiguration"]["Rules"] if r.get("Id") == "tier-blobs"]
    assert tier["Prefix"] == "blobs/"
    assert tier["ObjectSizeGreaterThan"] == 1024 * 1024
    assert [(x["StorageClass"], x["TransitionInDays"]) for x in tier["Transitions"]] == [
        ("GLACIER_IR", 0),
        ("DEEP_ARCHIVE", 90),
    ]


def test_the_image_archive_config_skips_null_tiers_and_can_lock_objects() -> None:
    archive = parse_image_archive(
        {
            "instant_retrieval_after_days": None,
            "deep_archive_after_days": 30,
            "object_lock": {"mode": "compliance", "days": 2555},
        }
    )
    app = cdk.App()
    props = _archive_bucket(DataStack(app, "data", image_archive=archive))
    (tier,) = [r for r in props["LifecycleConfiguration"]["Rules"] if r.get("Id") == "tier-blobs"]
    assert [x["StorageClass"] for x in tier["Transitions"]] == ["DEEP_ARCHIVE"]
    assert props["ObjectLockEnabled"] is True
    retention = props["ObjectLockConfiguration"]["Rule"]["DefaultRetention"]
    assert retention == {"Mode": "COMPLIANCE", "Days": 2555}
    with pytest.raises(ValueError, match="deep_archive_after_days"):
        parse_image_archive({"instant_retrieval_after_days": 90})
    with pytest.raises(ValueError, match="object_lock needs mode"):
        parse_image_archive({"object_lock": {"mode": "x", "days": 1}})


def test_backend_and_sweep_may_start_image_imports(default_stacks: Stacks) -> None:
    api = default_stacks["api"]
    t = Template.from_stack(api)
    funcs = t.find_resources("AWS::Lambda::Function").values()
    wired = [
        f["Properties"]["Environment"]["Variables"]
        for f in funcs
        if "BIFFIX_IMAGE_IMPORT_PROJECT" in f["Properties"]["Environment"]["Variables"]
    ]
    assert len(wired) == 2
    assert {w["BIFFIX_IMAGE_IMPORT_PROJECT"] for w in wired} == {"biffix-acme-image-import"}
