"""The edge stack: CloudFront, the UI build and the SPA rewrite."""

import dataclasses
import json
import shutil
import subprocess
from pathlib import Path

import aws_cdk as cdk
import pytest
from aws_cdk import aws_apigatewayv2 as apigw
from aws_cdk.assertions import Match, Template

from biffix_infra.app_graph import Stacks
from biffix_infra.constructs.ui_distribution import spa_rewrite_code
from biffix_infra.stacks.edge_stack import EdgeStack
from tests.synth.synth_helpers import (
    CONFIG,
    _build,
)


def test_edge_serves_ui_via_cloudfront(default_stacks: Stacks) -> None:
    stacks = default_stacks
    t = Template.from_stack(stacks["edge"])
    t.resource_count_is("AWS::CloudFront::Distribution", 1)
    t.resource_count_is("AWS::CloudFront::Function", 2)
    functions = t.find_resources("AWS::CloudFront::Function")
    codes = [r["Properties"]["FunctionCode"] for r in functions.values()]
    assert any("/index.html" in c for c in codes)
    assert any("substring(4)" in c for c in codes)
    t.resource_count_is("AWS::CloudFront::OriginAccessControl", 1)
    # The UI bucket is DESTROY + auto-delete (reproducible build), private behind OAC.
    t.has_resource("AWS::S3::Bucket", {"DeletionPolicy": "Delete"})
    t.has_resource_properties(
        "AWS::S3::Bucket",
        Match.object_like(
            {
                "PublicAccessBlockConfiguration": {
                    "BlockPublicAcls": True,
                    "BlockPublicPolicy": True,
                    "IgnorePublicAcls": True,
                    "RestrictPublicBuckets": True,
                }
            }
        ),
    )
    distribution = next(iter(t.find_resources("AWS::CloudFront::Distribution").values()))
    cfg = distribution["Properties"]["DistributionConfig"]
    api_behavior = next(b for b in cfg["CacheBehaviors"] if b["PathPattern"] == "/api/*")
    assert cfg["DefaultCacheBehavior"]["ViewerProtocolPolicy"] == "redirect-to-https"
    assert api_behavior["CachePolicyId"] != cfg["DefaultCacheBehavior"]["CachePolicyId"]
    t.has_resource_properties(
        "Custom::CDKBucketDeployment",
        Match.object_like({"DistributionPaths": ["/*"], "Prune": True, "RetainOnDelete": False}),
    )


def test_edge_defers_custom_domain_until_certificate_is_validated(tmp_path: Path) -> None:
    cfg = dataclasses.replace(CONFIG, app_domain="biffix.example.org")
    stacks = _build(tmp_path, config=cfg)
    t = Template.from_stack(stacks["edge"])
    t.resource_count_is("AWS::CertificateManager::Certificate", 0)
    props = next(iter(t.find_resources("AWS::CloudFront::Distribution").values()))["Properties"][
        "DistributionConfig"
    ]
    assert "Aliases" not in props


def test_edge_attaches_validated_certificate_by_arn(tmp_path: Path) -> None:
    cert_arn = "arn:aws:acm:us-east-1:222233334444:certificate/11111111-2222-3333-4444-555555555555"
    cfg = dataclasses.replace(
        CONFIG,
        app_domain="biffix.example.org",
        app_certificate_arn=cert_arn,
    )
    stacks = _build(tmp_path, config=cfg)
    t = Template.from_stack(stacks["edge"])
    t.resource_count_is("AWS::CertificateManager::Certificate", 0)  # imported, not created
    t.has_resource_properties(
        "AWS::CloudFront::Distribution",
        Match.object_like(
            {
                "DistributionConfig": Match.object_like(
                    {
                        "Aliases": ["biffix.example.org"],
                        "ViewerCertificate": Match.object_like({"AcmCertificateArn": cert_arn}),
                    }
                )
            }
        ),
    )


def test_edge_requires_a_real_sveltekit_build(tmp_path: Path) -> None:
    # A placeholder UI would make a successful deploy unusable and hide a broken release pipeline.
    app = cdk.App()
    holder = cdk.Stack(app, "holder")
    http_api = apigw.HttpApi(holder, "H")
    with pytest.raises(FileNotFoundError, match="SvelteKit build not found"):
        EdgeStack(app, "edge", http_api=http_api, ui_asset_path=tmp_path)


def _rewrite(tmp_path: Path, uris: list[str]) -> list[str]:
    """Run the SPA rewrite function on `uris` (node), against a build with a root file, the
    shell and an `_app/` directory."""
    node = shutil.which("node")
    if node is None:
        pytest.skip("node is not installed")
    build = tmp_path / "build"
    (build / "_app/immutable").mkdir(parents=True)
    (build / "index.html").write_text("<!doctype html>")
    (build / "favicon.svg").write_text("<svg/>")
    (build / "_app/immutable/app.js").write_text("")
    script = (
        spa_rewrite_code(build)
        + "var uris = "
        + json.dumps(uris)
        + ";\nconsole.log(JSON.stringify(uris.map(function (u) {"
        + " return handler({request: {uri: u}}).uri; })));\n"
    )
    out = subprocess.run([node, "-e", script], check=True, capture_output=True, text=True)
    rewritten: list[str] = json.loads(out.stdout)
    return rewritten


def test_spa_rewrite_serves_the_shell_for_every_client_route(tmp_path: Path) -> None:
    routes = [
        "/runs/abc",
        "/data/refs.grch38",  # a data source id with a dot
        "/workflows/x/v1.2.3",
    ]
    assert _rewrite(tmp_path, routes) == ["/index.html"] * len(routes)


def test_spa_rewrite_passes_through_what_the_build_publishes(tmp_path: Path) -> None:
    assets = ["/", "/index.html", "/favicon.svg", "/_app/immutable/app.js", "/_app/missing.js"]
    assert _rewrite(tmp_path, assets) == assets
