"""Tests for set_version.py, run by CI's infra job (`uv run pytest ../.github/scripts`)."""

import pathlib
import subprocess
import sys

SCRIPT = pathlib.Path(__file__).with_name("set_version.py")

CARGO_TOML = """\
[workspace]
members = ["crates/*"]

[workspace.package]
edition = "2024"
version = "0.1.0"

[workspace.dependencies]
biffix-core = { version = "0.1.0", path = "crates/biffix-core" }
biffix-db = { path = "crates/biffix-db" }
serde = { version = "0.1.0" }
"""

CHANGELOG = """\
# Changelog

All notable changes to this project will be documented in this file.

## [0.1.0] - 2026-10-01

- first
"""


def _run(root: pathlib.Path, *args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [sys.executable, str(SCRIPT), *args], cwd=root, capture_output=True, text=True
    )


def test_it_sets_the_workspace_and_path_dependency_versions_and_adds_the_section(
    tmp_path: pathlib.Path,
) -> None:
    (tmp_path / "Cargo.toml").write_text(CARGO_TOML)
    (tmp_path / "CHANGELOG.md").write_text(CHANGELOG)
    (tmp_path / "section.md").write_text("\n## [0.2.0] - 2026-11-17\n\n- second\n\n")
    result = _run(tmp_path, "0.2.0", "--changelog-section", "section.md")
    assert result.returncode == 0, result.stderr
    # Only the workspace's own version and its path dependencies' change; a registry dependency
    # that happens to share the old version keeps it.
    assert (tmp_path / "Cargo.toml").read_text() == CARGO_TOML.replace(
        'version = "0.1.0"\n', 'version = "0.2.0"\n', 1
    ).replace(
        '{ version = "0.1.0", path = "crates/biffix-core" }',
        '{ version = "0.2.0", path = "crates/biffix-core" }',
    )
    assert (tmp_path / "CHANGELOG.md").read_text() == CHANGELOG.replace(
        "## [0.1.0]", "## [0.2.0] - 2026-11-17\n\n- second\n\n## [0.1.0]"
    )


def test_it_starts_a_changelog_with_release_plz_s_header(tmp_path: pathlib.Path) -> None:
    (tmp_path / "Cargo.toml").write_text(CARGO_TOML)
    (tmp_path / "section.md").write_text("## [0.2.0]\n\n- second\n")
    result = _run(tmp_path, "0.2.0", "--changelog-section", "section.md")
    assert result.returncode == 0, result.stderr
    assert (tmp_path / "CHANGELOG.md").read_text() == (
        "# Changelog\n\nAll notable changes to this project will be documented in this file.\n\n"
        "## [0.2.0]\n\n- second\n\n"
    )


def test_it_needs_the_changelog_section(tmp_path: pathlib.Path) -> None:
    (tmp_path / "Cargo.toml").write_text(CARGO_TOML)
    result = _run(tmp_path, "0.2.0")
    assert result.returncode == 2
    assert "--changelog-section" in result.stderr
    assert (tmp_path / "Cargo.toml").read_text() == CARGO_TOML
