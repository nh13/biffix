"""Commit files to a branch through the GitHub API, for the release PR publish.yml opens by hand.

    python3 .github/scripts/commit_files.py BRANCH MESSAGE FILE...

Commits each FILE's working-tree contents to BRANCH, which must already exist on GitHub with its
head at the local HEAD. A `git commit` on the runner would be unsigned, and main's ruleset requires
signed commits; GitHub signs a commit made through its API (`createCommitOnBranch`) and attributes
it to the token's owner, as it does release-plz's. Run it from the repository root: each FILE is a
path from there. It adds or updates files only (no deletions) and sends no file mode, so it suits
regular files. Needs `gh` with GH_TOKEN set; prints the new commit's id.
"""

import argparse
import base64
import json
import os
import pathlib
import subprocess
import sys

MUTATION = """
mutation($input: CreateCommitOnBranchInput!) {
  createCommitOnBranch(input: $input) { commit { oid } }
}
"""


def run(*command: str, stdin: str | None = None) -> str:
    """`command`'s stdout; exits with its output when it fails."""
    result = subprocess.run(command, input=stdin, capture_output=True, text=True)
    if result.returncode != 0:
        sys.exit(f"{' '.join(command[:3])} failed: {result.stdout}{result.stderr}")
    return result.stdout.strip()


def check_files(files: list[pathlib.Path]) -> None:
    """Exits unless each of `files` is a file, named by a path from the repository root."""
    for f in files:
        if f.is_absolute() or ".." in f.parts:
            sys.exit(f"{f}: not a path from the repository root")
        if not f.is_file():
            sys.exit(f"{f}: no such file")


def commit_input(
    repo: str, branch: str, message: str, head: str, files: list[pathlib.Path]
) -> dict[str, object]:
    """The `createCommitOnBranch` input that commits `files` onto `branch` at `head`."""
    headline, _, body = message.partition("\n")
    return {
        "branch": {"repositoryNameWithOwner": repo, "branchName": branch},
        "message": {"headline": headline, "body": body.strip()},
        # GitHub refuses the commit if the branch has moved since it was read.
        "expectedHeadOid": head,
        "fileChanges": {
            "additions": [
                # The API takes a path from the repository root, with no `./`.
                {"path": f.as_posix(), "contents": base64.b64encode(f.read_bytes()).decode()}
                for f in files
            ]
        },
    }


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("branch", help="the branch to commit to")
    parser.add_argument("message", help="the commit message: its first line is the headline")
    parser.add_argument("files", nargs="+", type=pathlib.Path, help="the files to commit")
    args = parser.parse_args()
    root = pathlib.Path(run("git", "rev-parse", "--show-toplevel")).resolve()
    if root != pathlib.Path.cwd().resolve():
        sys.exit("run this from the repository root")
    check_files(args.files)
    if not run("git", "status", "--porcelain", "--", *map(str, args.files)):
        sys.exit(f"nothing to commit: {' '.join(map(str, args.files))} match HEAD")
    # Set on every Actions runner; `gh` works it out from the remote elsewhere.
    repo = os.environ.get("GITHUB_REPOSITORY") or run(
        "gh", "repo", "view", "--json", "nameWithOwner", "--jq", ".nameWithOwner"
    )
    head = run("git", "rev-parse", "HEAD")
    payload = {
        "query": MUTATION,
        "variables": {"input": commit_input(repo, args.branch, args.message, head, args.files)},
    }
    response = json.loads(run("gh", "api", "graphql", "--input", "-", stdin=json.dumps(payload)))
    commit = ((response.get("data") or {}).get("createCommitOnBranch") or {}).get("commit") or {}
    if "oid" not in commit:
        sys.exit(f"could not commit to {args.branch}: {json.dumps(response)}")
    print(commit["oid"])


if __name__ == "__main__":
    main()
