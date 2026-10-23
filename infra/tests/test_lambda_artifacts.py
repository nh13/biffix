"""The prebuilt Rust Lambda guards: a missing artifact and a stale one are both hard failures.

conftest opts the whole session out of both guards (BIFFIX_ALLOW_UNBUILT_LAMBDA=1); these tests
turn them back on against a throwaway repo tree.
"""

import os
from pathlib import Path

import pytest

from biffix_infra import lambda_artifacts
from biffix_infra.lambda_artifacts import _LAMBDA_SOURCE_CRATES, rust_lambda_code

BIN = "biffix-backend-lambda"


def _touch(path: Path, mtime: float) -> Path:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("x")
    os.utime(path, (mtime, mtime))
    return path


@pytest.fixture
def repo(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
    """A repo whose sources are all older (t=1000) than a built bootstrap (t=2000)."""
    monkeypatch.delenv("BIFFIX_ALLOW_UNBUILT_LAMBDA", raising=False)
    monkeypatch.delenv("CARGO_TARGET_DIR", raising=False)
    monkeypatch.setattr(lambda_artifacts, "_REPO_ROOT", tmp_path)
    for crate in _LAMBDA_SOURCE_CRATES[BIN]:
        _touch(tmp_path / "crates" / crate / "src/lib.rs", 1000)
        _touch(tmp_path / "crates" / crate / "Cargo.toml", 1000)
    for name in ("Cargo.lock", "Cargo.toml", "rust-toolchain.toml"):
        _touch(tmp_path / name, 1000)
    _touch(tmp_path / "target/lambda" / BIN / "bootstrap", 2000)
    return tmp_path


@pytest.mark.usefixtures("repo")
def test_a_fresh_artifact_is_used() -> None:
    rust_lambda_code(BIN)


@pytest.mark.parametrize("relative", [False, True])
def test_cargo_target_dir_moves_where_the_artifact_is_read(
    repo: Path, monkeypatch: pytest.MonkeyPatch, relative: bool
) -> None:
    # A relative CARGO_TARGET_DIR is under the repo root, where deploy.sh runs cargo.
    target = "build/cargo" if relative else str(repo.parent / "elsewhere")
    monkeypatch.setenv("CARGO_TARGET_DIR", target)
    with pytest.raises(FileNotFoundError, match="not found"):
        rust_lambda_code(BIN)  # the one under target/ is not this build's
    _touch(repo / target / "lambda" / BIN / "bootstrap", 2000)
    rust_lambda_code(BIN)


def test_a_missing_artifact_is_a_hard_failure(repo: Path) -> None:
    (repo / "target/lambda" / BIN / "bootstrap").unlink()
    with pytest.raises(FileNotFoundError, match="not found"):
        rust_lambda_code(BIN)


@pytest.mark.parametrize(
    "newer",
    [
        "crates/biffix-backend/src/lib.rs",
        "crates/biffix-core/Cargo.toml",
        "crates/biffix-workflow/src/lib.rs",
        # Embedded at compile time by sqlx::migrate! / include_str!, not .rs or .toml.
        "crates/biffix-db/migrations/0099_new.sql",
        "crates/biffix-db/build.rs",
        # Workspace-wide inputs outside every crate.
        "Cargo.lock",
        "Cargo.toml",
        "rust-toolchain.toml",
    ],
)
def test_an_input_newer_than_the_artifact_makes_it_stale(repo: Path, newer: str) -> None:
    _touch(repo / newer, 3000)
    with pytest.raises(RuntimeError, match="STALE"):
        rust_lambda_code(BIN)


def test_only_a_binary_s_own_inputs_count(repo: Path) -> None:
    # Integration tests are not linked into a binary; another binary's crate is not its input.
    _touch(repo / "crates/biffix-backend/tests/api.rs", 3000)
    _touch(repo / "crates/biffix-cli/src/main.rs", 3000)
    rust_lambda_code(BIN)
    # No binary embeds the coordinator entrypoint (the backend's include_str! of it is test-only).
    _touch(repo / "docker/coordinator/entrypoint.sh", 3000)
    rust_lambda_code(BIN)
    for crate in _LAMBDA_SOURCE_CRATES["biffix-ingest-lambda"]:
        _touch(repo / "crates" / crate / "src/lib.rs", 1000)
    _touch(repo / "target/lambda/biffix-ingest-lambda/bootstrap", 2000)
    rust_lambda_code("biffix-ingest-lambda")
    # The ingest binaries do not link the workflow crate.
    _touch(repo / "crates/biffix-workflow/src/lib.rs", 3000)
    rust_lambda_code("biffix-ingest-lambda")


@pytest.mark.usefixtures("repo")
def test_an_unknown_binary_is_refused() -> None:
    with pytest.raises(ValueError, match="unknown Rust Lambda binary"):
        rust_lambda_code("biffix-new-lambda")


def test_every_binary_the_stacks_deploy_is_known() -> None:
    stacks = Path(lambda_artifacts.__file__).parent / "stacks"
    used = {
        line.split('rust_lambda_code("', 1)[1].split('"', 1)[0]
        for path in stacks.glob("*.py")
        for line in path.read_text().splitlines()
        if 'rust_lambda_code("' in line
    }
    assert used == set(_LAMBDA_SOURCE_CRATES)
