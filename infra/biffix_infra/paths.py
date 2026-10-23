"""The repository paths the infra code reads, in one place.

Stacks, the deploy helpers and the tests import these instead of counting `parents[N]` from their
own location, so moving a module or a directory is a change here and nowhere else.
"""

from __future__ import annotations

import os
from pathlib import Path

# The repository root: infra/biffix_infra/paths.py -> parents[2].
REPO_ROOT = Path(__file__).resolve().parents[2]

# The CDK app (app.py, the example deploy config, deploy.sh) and its synth output.
INFRA_DIR = REPO_ROOT / "infra"
CDK_OUT = INFRA_DIR / "cdk.out"
# Scripts the stacks ship to Batch hosts (the compute stack's launch template).
HOST_DIR = Path(__file__).resolve().parent / "host"

# The Rust workspace's crates (the Lambda staleness check reads their sources).
CRATES_DIR = REPO_ROOT / "crates"
# The UI's static build, which the edge stack uploads.
UI_BUILD = REPO_ROOT / "ui" / "build"
# The coordinator image's sources (entrypoint.sh, the Dockerfile).
COORDINATOR_DIR = REPO_ROOT / "docker" / "coordinator"
# The CodeBuild scripts the build stack inlines into its buildspecs.
CODEBUILD_DIR = REPO_ROOT / "codebuild"


def cargo_target_dir(root: Path = REPO_ROOT) -> Path:
    """Cargo's target directory for the workspace at `root`.

    `CARGO_TARGET_DIR` when set, else `root/target`. Cargo resolves a relative `CARGO_TARGET_DIR`
    against the directory it runs in, and deploy.sh runs cargo from the repository root, so a
    relative one is taken under `root`.
    """
    target = os.environ.get("CARGO_TARGET_DIR")
    return root / target if target else root / "target"


def lambda_target_dir(root: Path = REPO_ROOT) -> Path:
    """Where `cargo lambda build` puts each Lambda binary (`<dir>/<bin>/bootstrap`)."""
    return cargo_target_dir(root) / "lambda"


def release_dir() -> Path | None:
    """The fetched release the artifacts come from instead of this tree's builds, or None.

    `BIFFIX_RELEASE_DIR` (set by `deploy.sh --release`) names a directory `biffix_infra.release`
    laid out: the Lambda zips under `lambda/`, the UI build under `ui/`.
    """
    value = os.environ.get("BIFFIX_RELEASE_DIR")
    return Path(value).resolve() if value else None


def ui_build_dir() -> Path:
    """The UI build the edge stack uploads: the release's when deploying one, else `UI_BUILD`."""
    release = release_dir()
    return release / "ui" if release is not None else UI_BUILD
