"""The repository paths (`biffix_infra.paths`) name what is really there."""

from __future__ import annotations

from biffix_infra import paths


def test_every_repo_path_names_what_the_infra_reads() -> None:
    assert (paths.REPO_ROOT / "Cargo.toml").is_file()
    assert (paths.INFRA_DIR / "app.py").is_file()
    assert (paths.HOST_DIR / "install_aws_cli.sh").is_file()
    assert (paths.CRATES_DIR / "biffix-core").is_dir()
    assert paths.UI_BUILD.parent == paths.REPO_ROOT / "ui"
    assert (paths.COORDINATOR_DIR / "entrypoint.sh").is_file()
    assert (paths.CODEBUILD_DIR / "lib/common.sh").is_file()
    assert paths.CDK_OUT.parent == paths.INFRA_DIR
