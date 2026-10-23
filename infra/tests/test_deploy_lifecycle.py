"""Deployment-lifecycle tests for the complete CDK app graph."""

from __future__ import annotations

import json
import os
import subprocess
import sys
from pathlib import Path
from typing import Any

import pytest

from biffix_infra import script_config
from biffix_infra.config import DeployConfig
from biffix_infra.paths import INFRA_DIR
from tests.deploy_config_file import FIXTURE

CDK = INFRA_DIR / "node_modules" / ".bin" / "cdk"
CONFIG = FIXTURE

# cdk.out's JSON (its manifest, an artifact, a template, a resource): the CDK's shapes, read
# unchecked.
Json = dict[str, Any]
# The synthesized app: its manifest and its cdk.out directory.
Synth = tuple[Json, Path]


@pytest.fixture(scope="module")
def synth(tmp_path_factory: pytest.TempPathFactory) -> Synth:
    """The complete app, synthesized once for the module: its manifest and its cdk.out."""
    if not CDK.exists():
        pytest.skip(f"no cdk at {CDK} (run `npm ci` in infra/)")
    output = tmp_path_factory.mktemp("cdk") / "cdk.out"
    return _synth_app(output), output


def _synth_app(output: Path) -> Json:
    """Synthesize the complete app into `output`, without inheriting ambient tenant credentials."""
    env = os.environ.copy()
    for name in (
        "AWS_ACCESS_KEY_ID",
        "AWS_SECRET_ACCESS_KEY",
        "AWS_SESSION_TOKEN",
        "AWS_SECURITY_TOKEN",
    ):
        env.pop(name, None)
    env["JSII_SILENCE_WARNING_UNTESTED_NODE_VERSION"] = "1"

    subprocess.run(
        [
            str(CDK),
            "synth",
            "--quiet",
            "--output",
            str(output),
            "-c",
            f"configFile={CONFIG}",
        ],
        cwd=INFRA_DIR,
        env=env,
        check=True,
        capture_output=True,
        text=True,
    )
    manifest: Json = json.loads((output / "manifest.json").read_text())
    return manifest


def _stack_artifacts(manifest: Json) -> dict[str, Json]:
    """Return stack artifacts keyed by their stable stack names."""
    return {
        name: artifact
        for name, artifact in manifest["artifacts"].items()
        if artifact["type"] == "aws:cloudformation:stack"
    }


def test_the_app_has_no_default_config() -> None:
    """A synth that names no config refuses, rather than picking up whatever file is nearby."""
    env = {k: v for k, v in os.environ.items() if k != "BIFFIX_DEPLOY_CONFIG"}
    r = subprocess.run(
        [sys.executable, "app.py"], cwd=INFRA_DIR, env=env, capture_output=True, text=True
    )
    assert r.returncode != 0
    assert "no deploy config" in r.stderr


def test_stateful_stacks_enable_termination_protection(synth: Synth) -> None:
    """Every stack owning retained tenant state is protected from stack deletion; the stateless
    stacks (which redeploy freely) are not. Aurora + the capability secret live in `persistence`.
    Every stack is one or the other."""
    stacks = _stack_artifacts(synth[0])
    protected = ("-data", "-identity", "-persistence", "-project-demo")
    unprotected = ("-network", "-compute", "-build", "-ingest", "-api", "-edge")

    for suffix in (*protected, *unprotected):
        [stack_name] = [name for name in stacks if name.endswith(suffix)]
        want = suffix in protected
        assert stacks[stack_name]["properties"]["terminationProtection"] is want, stack_name
    assert len(stacks) == len(protected) + len(unprotected), sorted(stacks)


def test_dependency_graph_is_inferred_not_hand_authored(synth: Synth) -> None:
    """The deploy ordering is what CDK infers from resource tokens — grants attach to resources and
    the reference direction is natural, so app.py declares NO manual stack dependency."""
    # app.py synthesizes the graph `app_graph` wires.
    for source in (INFRA_DIR / "app.py", INFRA_DIR / "biffix_infra" / "app_graph.py"):
        app_src = source.read_text()
        # Match the call syntax, not prose: the docstring legitimately mentions `add_dependency`.
        assert "add_stack_dependency(" not in app_src, source
        assert ".add_dependency(" not in app_src, source

    stacks = _stack_artifacts(synth[0])

    def stack(suffix: str) -> tuple[str, Json]:
        return next((name, artifact) for name, artifact in stacks.items() if name.endswith(suffix))

    network_name, _ = stack("-network")
    data_name, data = stack("-data")
    identity_name, identity = stack("-identity")
    compute_name, compute = stack("-compute")
    persistence_name, persistence = stack("-persistence")
    ingest_name, ingest = stack("-ingest")
    api_name, api = stack("-api")
    _, edge = stack("-edge")
    project_name, project = stack("-project-demo")

    # Roots feed leaves; api is the hub, edge the sink. All inferred from tokens. (identity→data is
    # conditional on OIDC registration roles, which the dev config doesn't configure, so it's not
    # asserted here.)
    assert {network_name, data_name} <= set(compute["dependencies"])
    assert identity_name in project["dependencies"]  # permissions boundary
    assert network_name in persistence["dependencies"]  # VPC
    assert persistence_name in ingest["dependencies"]  # DB secret + SG
    assert {persistence_name, identity_name, compute_name, data_name, project_name} <= set(
        api["dependencies"]
    )
    assert api_name in edge["dependencies"]  # CloudFront /api origin


def test_scripts_see_the_stacks_resources_and_tags_the_app_synthesizes(synth: Synth) -> None:
    """deploy.sh and teardown.sh never match by name prefix: they use the exact stack names from
    `script_config`, the data stack's resources by logical id, and the `biffix:tenant` tag on the
    buckets and Aurora cluster. Each of those must match what the app really synthesizes."""
    config = DeployConfig.load(CONFIG)
    manifest, out = synth
    stacks = _stack_artifacts(manifest)
    stack_names, data_stack = script_config.values(config, ["stack_names", "data_stack"])
    assert sorted(stacks) == sorted(stack_names.split())

    def resources(stack: str) -> dict[str, Json]:
        template: Json = json.loads((out / f"{stack}.template.json").read_text())
        found: dict[str, Json] = template["Resources"]
        return found

    data = resources(data_stack)
    for type_, logical in (
        ("AWS::S3::Bucket", "ReferenceBucket"),
        ("AWS::S3::Bucket", "ImageArchiveBucket"),
        ("AWS::ECR::Repository", "CoordinatorRepo"),
    ):
        matches = [k for k, r in data.items() if r["Type"] == type_ and k.startswith(logical)]
        assert len(matches) == 1, (type_, logical, matches)

    tenant_tag = {"Key": "biffix:tenant", "Value": config.tenant}
    tagged_types = {"AWS::S3::Bucket", "AWS::RDS::DBCluster"}
    seen = set()
    for stack in stacks:
        for logical, r in resources(stack).items():
            tags = r.get("Properties", {}).get("Tags", [])
            if r["Type"] in tagged_types:
                assert tenant_tag in tags, (stack, logical)
                seen.add(r["Type"])
            elif isinstance(tags, list):
                # A tag change replaces some types (e.g. Batch compute environments), so only the
                # types teardown deletes carry it.
                assert tenant_tag not in tags, (stack, logical)
    assert seen == tagged_types
