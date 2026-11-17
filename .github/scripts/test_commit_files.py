"""Tests for commit_files.py, run by CI's infra job (`uv run pytest ../.github/scripts`).

`gh` is a stand-in on PATH that records the request and answers as the test sets, so nothing
reaches GitHub.
"""

import base64
import json
import os
import pathlib
import subprocess
import sys

import pytest

sys.path.insert(0, str(pathlib.Path(__file__).parent))
import commit_files  # noqa: E402

SCRIPT = pathlib.Path(__file__).with_name("commit_files.py")


def test_commit_input_splits_the_message_and_encodes_each_file(
    tmp_path: pathlib.Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.chdir(tmp_path)
    (tmp_path / "dir").mkdir()
    (tmp_path / "dir" / "a.txt").write_bytes(b"one\x00two\n")
    (tmp_path / "Cargo.toml").write_text("[workspace]\n")
    files = [pathlib.Path("dir/a.txt"), pathlib.Path("./Cargo.toml")]
    got = commit_files.commit_input("o/r", "release-v1.0.0", "head line\n\nbody\n", "abc", files)
    assert got["branch"] == {"repositoryNameWithOwner": "o/r", "branchName": "release-v1.0.0"}
    assert got["message"] == {"headline": "head line", "body": "body"}
    assert got["expectedHeadOid"] == "abc"
    additions = got["fileChanges"]["additions"]  # type: ignore[index]
    assert [a["path"] for a in additions] == ["dir/a.txt", "Cargo.toml"]
    assert base64.b64decode(additions[0]["contents"]) == b"one\x00two\n"
    assert base64.b64decode(additions[1]["contents"]) == b"[workspace]\n"


@pytest.mark.parametrize(
    ("path", "error"),
    [
        ("/etc/hosts", "not a path from the repository root"),
        # It exists (the fixture makes it), so only the `..` check refuses it.
        ("../outside.txt", "not a path from the repository root"),
        ("missing.txt", "no such file"),
    ],
)
def test_check_files_refuses_a_path_outside_the_repository_or_missing(
    tmp_path: pathlib.Path, monkeypatch: pytest.MonkeyPatch, path: str, error: str
) -> None:
    (tmp_path / "outside.txt").write_text("x\n")
    (tmp_path / "repo").mkdir()
    monkeypatch.chdir(tmp_path / "repo")
    with pytest.raises(SystemExit, match=error):
        commit_files.check_files([pathlib.Path(path)])


@pytest.fixture
def repo(tmp_path: pathlib.Path) -> pathlib.Path:
    root = tmp_path / "repo"
    root.mkdir()
    env = {"GIT_CONFIG_GLOBAL": os.devnull, "GIT_CONFIG_NOSYSTEM": "1"}
    for args in (
        ["init", "-q", "-b", "main"],
        ["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "--allow-empty", "-m", "0"],
    ):
        subprocess.run(["git", *args], cwd=root, check=True, env={**os.environ, **env})
    (root / "CHANGELOG.md").write_text("old\n")
    subprocess.run(["git", "add", "CHANGELOG.md"], cwd=root, check=True)
    subprocess.run(
        ["git", "-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "1"],
        cwd=root,
        check=True,
        env={**os.environ, **env},
    )
    return root


def _run(
    repo: pathlib.Path,
    tmp_path: pathlib.Path,
    answer: str,
    status: int = 0,
    cwd: str = ".",
    files: tuple[str, ...] = ("CHANGELOG.md",),
    github_repository: str | None = "o/r",
) -> tuple[subprocess.CompletedProcess[str], pathlib.Path]:
    """Runs the script in `repo` with a `gh` that answers `answer` and exits `status`.

    `gh repo view` answers `from/remote`; any other call records its stdin as the request.
    """
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir(exist_ok=True)
    request = tmp_path / "request.json"
    (bin_dir / "gh").write_text(
        "#!/bin/sh\n"
        'if [ "$1" = repo ]; then echo from/remote; exit 0; fi\n'
        f"cat > '{request}'\nprintf '%s' '{answer}'\nexit {status}\n"
    )
    (bin_dir / "gh").chmod(0o755)
    env = {**os.environ, "PATH": f"{bin_dir}{os.pathsep}{os.environ['PATH']}"}
    env.pop("GITHUB_REPOSITORY", None)
    if github_repository is not None:
        env["GITHUB_REPOSITORY"] = github_repository
    result = subprocess.run(
        [sys.executable, str(SCRIPT), "release-v1.0.0", "chore(release): v1.0.0\n\nwhy", *files],
        cwd=repo / cwd,
        env=env,
        capture_output=True,
        text=True,
    )
    return result, request


def test_it_commits_the_changed_file_at_head_and_prints_the_new_commit(
    repo: pathlib.Path, tmp_path: pathlib.Path
) -> None:
    (repo / "CHANGELOG.md").write_text("new\n")
    answer = '{"data":{"createCommitOnBranch":{"commit":{"oid":"f00d"}}}}'
    result, request = _run(repo, tmp_path, answer)
    assert result.returncode == 0, result.stderr
    assert result.stdout.strip() == "f00d"
    body = json.loads(request.read_text())
    assert body["query"] == commit_files.MUTATION
    sent = body["variables"]["input"]
    head = subprocess.run(
        ["git", "rev-parse", "HEAD"], cwd=repo, capture_output=True, text=True, check=True
    )
    assert sent["expectedHeadOid"] == head.stdout.strip()
    assert sent["branch"] == {"repositoryNameWithOwner": "o/r", "branchName": "release-v1.0.0"}
    assert sent["message"] == {"headline": "chore(release): v1.0.0", "body": "why"}
    [addition] = sent["fileChanges"]["additions"]
    assert addition["path"] == "CHANGELOG.md"
    assert base64.b64decode(addition["contents"]) == b"new\n"


def test_it_fails_with_github_s_answer_when_the_commit_is_refused(
    repo: pathlib.Path, tmp_path: pathlib.Path
) -> None:
    (repo / "CHANGELOG.md").write_text("new\n")
    result, _ = _run(repo, tmp_path, '{"errors":[{"message":"Expected branch to point to x"}]}', 1)
    assert result.returncode == 1
    assert "gh api graphql failed" in result.stderr
    assert "Expected branch to point to x" in result.stderr


@pytest.mark.parametrize("answer", ['{"data":{"createCommitOnBranch":null}}', "{}"])
def test_it_fails_when_github_answers_without_a_commit(
    repo: pathlib.Path, tmp_path: pathlib.Path, answer: str
) -> None:
    (repo / "CHANGELOG.md").write_text("new\n")
    result, _ = _run(repo, tmp_path, answer)
    assert result.returncode == 1
    assert "could not commit to release-v1.0.0" in result.stderr


def test_it_refuses_files_that_match_head(repo: pathlib.Path, tmp_path: pathlib.Path) -> None:
    result, request = _run(repo, tmp_path, "{}")
    assert result.returncode == 1
    assert "nothing to commit: CHANGELOG.md match HEAD" in result.stderr
    assert not request.exists()


def test_it_refuses_to_run_below_the_repository_root(
    repo: pathlib.Path, tmp_path: pathlib.Path
) -> None:
    (repo / "sub").mkdir()
    (repo / "CHANGELOG.md").write_text("new\n")
    result, request = _run(repo, tmp_path, "{}", cwd="sub")
    assert result.returncode == 1
    assert "run this from the repository root" in result.stderr
    assert not request.exists()


def test_it_asks_gh_for_the_repository_outside_actions(
    repo: pathlib.Path, tmp_path: pathlib.Path
) -> None:
    (repo / "CHANGELOG.md").write_text("new\n")
    answer = '{"data":{"createCommitOnBranch":{"commit":{"oid":"f00d"}}}}'
    result, request = _run(repo, tmp_path, answer, github_repository=None)
    assert result.returncode == 0, result.stderr
    sent = json.loads(request.read_text())["variables"]["input"]
    assert sent["branch"]["repositoryNameWithOwner"] == "from/remote"


def test_it_names_a_missing_file_rather_than_finding_nothing_to_commit(
    repo: pathlib.Path, tmp_path: pathlib.Path
) -> None:
    result, request = _run(repo, tmp_path, "{}", files=("CHANGELOG.mdx",))
    assert result.returncode == 1
    assert "CHANGELOG.mdx: no such file" in result.stderr
    assert not request.exists()
