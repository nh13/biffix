"""The biffix version this checkout is, read from the Cargo workspace (`[workspace.package]`).

One version names the whole release: the Lambdas, the UI and the coordinator image are built from
the same tree, and the release tag is `v<version>` (.github/workflows/release.yml refuses a tag that
disagrees). The infra code reads it to default `coordinator_image_tag` to the image that release
imports, and `deploy.sh --release` refuses a release other than this checkout's.
"""

from __future__ import annotations

import tomllib
from pathlib import Path

from biffix_infra.paths import REPO_ROOT


def _workspace_package(root: Path = REPO_ROOT) -> dict[str, object]:
    with (root / "Cargo.toml").open("rb") as f:
        package: dict[str, object] = tomllib.load(f)["workspace"]["package"]
    return package


def biffix_version(root: Path = REPO_ROOT) -> str:
    """The workspace version, e.g. `0.1.0`."""
    return str(_workspace_package(root)["version"])


def release_tag(root: Path = REPO_ROOT) -> str:
    """The release tag of this checkout's version, e.g. `v0.1.0`."""
    return f"v{biffix_version(root)}"


def repository_url(root: Path = REPO_ROOT) -> str:
    """The repository releases are published from (`https://github.com/<owner>/<repo>`)."""
    return str(_workspace_package(root)["repository"]).rstrip("/")
