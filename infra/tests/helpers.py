"""Helpers shared across the infra tests."""

from __future__ import annotations

from pathlib import Path

from biffix_infra.paths import REPO_ROOT

__all__ = ["REPO_ROOT", "executable"]


def executable(path: Path, text: str) -> None:
    """Write `text` to `path` (making its directory) as an executable script."""
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text)
    path.chmod(0o755)
