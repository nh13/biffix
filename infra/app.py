#!/usr/bin/env python
"""biffix CDK app entry: load the deploy config and synthesize the stack graph
`biffix_infra.app_graph.build_stacks` wires (see its module docstring for the graph)."""

from __future__ import annotations

import dataclasses
import json
import os
import sys
from pathlib import Path

import aws_cdk as cdk

from biffix_infra.app_graph import build_stacks
from biffix_infra.config import (
    DeployConfig,
    bucket_cors_instructions,
    noncurrent_versions_lifecycle_rule,
)
from biffix_infra.paths import INFRA_DIR

app = cdk.App()
# There is no default config: every synth names the tenant it is for. A relative path resolves
# against infra/.
_config_arg = app.node.try_get_context("configFile") or os.environ.get("BIFFIX_DEPLOY_CONFIG")
if not _config_arg:
    sys.exit(
        "biffix: no deploy config: pass `-c configFile=<file>` or set BIFFIX_DEPLOY_CONFIG "
        "(copy infra/biffix-deploy.example.yaml to start one)"
    )
_config_path = Path(_config_arg)
if not _config_path.is_absolute():
    _config_path = INFRA_DIR / _config_path
config = DeployConfig.load(_config_path)
# deploy.sh injects the ACM cert ARN it provisioned for a custom-domain deploy: app_domain is
# declared in config, but the cert is imperative (requested + DNS-validated at deploy time), so the
# script fills it in via context rather than mutating the config file. An arn set in config wins if
# the context is absent.
_cert_override = app.node.try_get_context("appCertificateArn")
if _cert_override:
    config = dataclasses.replace(config, app_certificate_arn=_cert_override)
env = cdk.Environment(region=config.region)
# teardown.sh synthesizes only to destroy, so it points the edge stack at a placeholder UI build
# outside the source tree rather than requiring (or faking) ui/build.
_ui_asset_path = app.node.try_get_context("uiAssetPath")

build_stacks(
    app,
    config,
    name=config.stack_name,
    env=env,
    termination_protection=True,
    ui_asset_path=_ui_asset_path,
)

# Buckets biffix does not own and that lack the CORS rules browser uploads and the alignment viewer
# need: biffix will not change them, so say what their owners would apply (stderr: synth output
# stays clean).
for instruction in bucket_cors_instructions(config):
    print(instruction, file=sys.stderr)

for project in config.projects:
    # A created bucket expires superseded versions itself; an imported one's owner must, or (if it
    # is versioned) the intermediates and cache sweeps' deletes free no space.
    if project.bucket_name and config.noncurrent_versions_days is not None:
        rule = noncurrent_versions_lifecycle_rule(config.noncurrent_versions_days)
        print(
            f"[biffix] project `{project.id}`: if the imported bucket {project.bucket_name} is "
            f"versioned, the retention and cache sweeps' deletes only hide objects until its owner "
            f"adds this lifecycle rule (merged with the bucket's existing rules):\n"
            f"  {json.dumps(rule)}",
            file=sys.stderr,
        )

app.synth()
