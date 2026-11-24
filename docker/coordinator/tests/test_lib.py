"""The entrypoint's libraries (lib/*.sh) and helpers (lib/*.py), one function at a time: a shell
function runs in `bash` with only its library (and common.sh) sourced, a helper as the entrypoint
runs it (`python3 <lib>/<helper>.py …`) over the stand-in boto3 (fakes/py). test_entrypoint.py
drives the whole script; these pin down the edges it does not reach."""

from __future__ import annotations

import io
import json
import os
import subprocess
import sys
import tarfile
import time
import urllib.error
from collections.abc import Iterator
from pathlib import Path
from types import ModuleType
from typing import ClassVar

import pytest
from helpers import FAKES, LIB, RealSnakemake, bash, helper, module
from processes import stop_group

# ── common.sh ──


def test_runtime_error_exits_with_ex_config() -> None:
    got = bash("runtime_error the thing is wrong; echo unreachable")
    assert got.returncode == 78
    assert got.stderr == "biffix-coordinator: the thing is wrong\n"
    assert "unreachable" not in got.stdout


@pytest.mark.parametrize(
    ("url", "logged"),
    [
        ("https://user:tok@github.com/o/r.git", "https://github.com/o/r.git"),
        ("https://tok@github.com/o/r", "https://github.com/o/r"),
        ("https://github.com/o/r", "https://github.com/o/r"),
        # An @ past the host is part of the path, not credentials.
        ("https://github.com/o/r@v1", "https://github.com/o/r@v1"),
    ],
)
def test_redact_url_drops_only_the_credentials(url: str, logged: str) -> None:
    got = bash(f"redact_url {url!r}")
    assert got.returncode == 0, got.stderr
    assert got.stdout == logged


@pytest.mark.parametrize(
    ("text", "key", "value"),
    [
        ('{"kind": "git", "repo": "r"}', "kind", "git"),
        ('{"kind": "git"}', "repo", ""),
        ('{"sha256": 7}', "sha256", ""),  # not a string
        ('["kind"]', "kind", ""),  # not an object
        ("{not json", "kind", ""),
        ("", "kind", ""),
        ('{"k": "it\'s $(x) `y`"}', "k", "it's $(x) `y`"),  # never evaluated
    ],
)
def test_jget_reads_one_string_field_or_nothing(text: str, key: str, value: str) -> None:
    got = bash('jget "$JSON" "$KEY"', JSON=text, KEY=key)
    assert got.returncode == 0, got.stderr
    assert got.stdout.rstrip("\n") == value
    assert module("json_field").field(text, key) == value


# ── runtime.sh ──


def test_select_runtime_fails_on_an_unknown_engine() -> None:
    got = bash("ENGINE=cromwell RUNTIME_ROOT=/nonexistent select_runtime", "runtime")
    assert got.returncode == 78
    assert "unknown engine 'cromwell'" in got.stderr


# ── signals.sh ──


def test_run_engine_leaves_the_exit_status_in_rc_and_logs_the_quoted_command() -> None:
    got = bash(
        "install_signal_traps; set +e; run_engine TERM sh -c 'exit 7' 'a b'; set -e\n"
        'echo "rc=$rc"',
        "signals",
    )
    assert got.returncode == 0, got.stderr
    assert got.stdout == "rc=7\n"
    assert got.stderr == "biffix-coordinator: running sh -c exit\\ 7 a\\ b\n"


def test_a_signal_before_the_engine_starts_ends_the_run() -> None:
    got = bash("install_signal_traps; kill -TERM $$; sleep 5; echo unreachable", "signals")
    assert got.returncode == 143
    assert got.stderr == "biffix-coordinator: received SIGTERM\n"
    assert "unreachable" not in got.stdout


def test_a_signal_after_the_engine_exits_lets_the_post_engine_steps_finish() -> None:
    # The final metadata sync, the events upload and the replay still run, and the run exits with
    # the engine's status, not the signal's.
    got = bash(
        "install_signal_traps; set +e; run_engine TERM sh -c 'exit 7'; set -e\n"
        'kill -TERM $$; echo "post-engine step ran"; exit "$rc"',
        "signals",
    )
    assert got.returncode == 7, got.stderr
    assert got.stdout == "post-engine step ran\n"
    assert "received SIGTERM" in got.stderr


def test_a_run_a_signal_stops_exits_with_the_signal_s_status_not_the_engine_s() -> None:
    # Nextflow exits 1 on SIGTERM; a cancelled run must not read as a plain failure.
    got = bash(
        "install_signal_traps; (sleep 0.5; kill -TERM $$) &\n"
        "set +e; run_engine TERM sh -c 'trap \"exit 1\" TERM; while :; do sleep 0.1; done'\n"
        "set -e\n"
        'echo "rc=$rc"',
        "signals",
    )
    assert got.returncode == 0, got.stderr
    assert got.stdout == "rc=143\n"
    assert "the engine stopped on the signal (its status 1); the run exits 143" in got.stderr


def test_run_in_runs_the_command_from_the_directory(tmp_path: Path) -> None:
    got = bash(f"(run_in {str(tmp_path)!r} pwd)", "signals")
    assert got.returncode == 0, got.stderr
    assert Path(got.stdout.strip()).resolve() == tmp_path.resolve()


# ── source.sh ──


@pytest.mark.parametrize(
    ("repo", "url"),
    [
        ("git@github.com:o/r.git", "https://github.com/o/r.git"),
        ("ssh://git@github.com/o/r", "https://github.com/o/r"),
        ("http://example.org/r", "http://example.org/r"),
        ("https://example.org/r", "https://example.org/r"),
        ("github.com/o/r", "https://github.com/o/r"),
    ],
)
def test_git_origins_are_cloned_over_anonymous_https(repo: str, url: str) -> None:
    got = bash(f"git_https_url {repo!r}", "source")
    assert got.returncode == 0, got.stderr
    assert got.stdout == url


@pytest.mark.parametrize(
    ("source", "message"),
    [
        ('{"kind": "git"}', "the workflow source (kind=git) names no repo"),
        ('{"kind": "git", "repo": ""}', "the workflow source (kind=git) names no repo"),
        ('{"kind": "s3", "sha256": "x"}', "the workflow source (kind=s3) names no uri"),
    ],
)
def test_a_source_without_its_location_fails_with_ex_config(
    tmp_path: Path, source: str, message: str
) -> None:
    got = bash(
        "stage_workflow_source",
        "source",
        SRC=str(tmp_path / "src"),
        BIFFIX_WORKFLOW_SOURCE=source,
    )
    assert got.returncode == 78
    assert f"biffix-coordinator: {message}" in got.stderr


def test_an_s3_tarball_source_needs_a_well_formed_sha256() -> None:
    for sha, message in (
        ("", "records no sha256 for its source tarball s3://b/w.tgz"),
        ("ABC", "recorded sha256 is not a sha256: ABC"),
        ("A" * 64, "recorded sha256 is not a sha256"),  # lowercase hex only
    ):
        got = bash(f"SRC=/nonexistent stage_s3_source s3://b/w.tgz {sha!r}", "source")
        assert got.returncode == 78, sha
        assert message in got.stderr


def test_params_are_written_as_given_or_as_an_empty_object(tmp_path: Path) -> None:
    # Into the source root, Snakemake's working directory: a remote job gets only what is below it.
    for params, written in (("", "{}"), ('{"a": 1}', '{"a": 1}')):
        got = bash(
            'materialize_params; printf "%s|%s" "$CONFIG" "$PARAMS_JSON"',
            "source",
            SRC=str(tmp_path),
            BIFFIX_PARAMS=params,
        )
        assert got.returncode == 0, got.stderr
        assert got.stdout == f"{tmp_path}/.biffix-config.json|{written}"
        assert (tmp_path / ".biffix-config.json").read_text() == written


def _git(repo: Path, *args: str) -> str:
    # Without the caller's `GIT_*` environment (`git rebase --exec` exports `GIT_DIR`), which would
    # point git at another repository than `repo`.
    env = {k: v for k, v in os.environ.items() if not k.startswith("GIT_")}
    out = subprocess.run(
        ["git", "-C", str(repo), "-c", "user.name=t", "-c", "user.email=t@t", *args],
        capture_output=True,
        text=True,
        env=env,
        check=True,
    )
    return out.stdout


def _tracked(work: Path) -> list[str]:
    return _git(work, "ls-files").split()


def _track(work: Path) -> subprocess.CompletedProcess[str]:
    """track_staged_tree over `work` (the source, and the params in it, in src/)."""
    return bash(
        "track_staged_tree",
        "source",
        WORK=str(work),
        SRC=str(work / "src"),
        CONFIG=str(work / "src/.biffix-config.json"),
    )


def test_a_git_source_is_checked_out_at_its_sha_without_its_git_dir(tmp_path: Path) -> None:
    # The pinned SHA wins over the ref, and the clone's .git goes (track_staged_tree then tracks
    # the tree as it does an S3 one).
    repo = tmp_path / "repo"
    repo.mkdir()
    _git(repo, "init", "-q", "-b", "main")
    (repo / "Snakefile").write_text("first\n")
    _git(repo, "add", "Snakefile")
    _git(repo, "-c", "commit.gpgsign=false", "commit", "-q", "-m", "first")
    first = _git(repo, "rev-parse", "HEAD").strip()
    (repo / "Snakefile").write_text("second\n")
    _git(repo, "-c", "commit.gpgsign=false", "commit", "-q", "-am", "second")
    src = tmp_path / "src"
    got = bash(
        # A local repo: the anonymous-HTTPS rewrite is for a remote one.
        'git_https_url() { printf "%s" "$1"; }; stage_git_source "$REPO" main',
        "source",
        SRC=str(src),
        REPO=f"file://{repo}",
        BIFFIX_GIT_SHA=first,
    )
    assert got.returncode == 0, got.stderr
    assert (src / "Snakefile").read_text() == "first\n"
    assert not (src / ".git").exists()
    got = bash(
        'git_https_url() { printf "%s" "$1"; }; stage_git_source "$REPO" main',
        "source",
        SRC=str(tmp_path / "src2"),
        REPO=f"file://{repo}",
    )
    assert got.returncode == 0, got.stderr
    assert (tmp_path / "src2/Snakefile").read_text() == "second\n"


def test_the_staged_tree_and_params_are_git_tracked_for_worker_deployment(tmp_path: Path) -> None:
    (tmp_path / "src/config").mkdir(parents=True)
    (tmp_path / "src/Snakefile").write_text("rule all:\n")
    (tmp_path / "src/config/x.yaml").write_text("a: 1\n")
    (tmp_path / "src/.biffix-config.json").write_text("{}")
    got = _track(tmp_path)
    assert got.returncode == 0, got.stderr
    assert _tracked(tmp_path) == [
        "src/.biffix-config.json",
        "src/Snakefile",
        "src/config/x.yaml",
    ]
    # Snakemake lists them from the source root, its working directory: every one below it.
    assert _git(tmp_path / "src", "ls-files", ".").split() == [
        ".biffix-config.json",
        "Snakefile",
        "config/x.yaml",
    ]


def test_the_staged_tree_ships_whole_whatever_its_ignore_rules(tmp_path: Path) -> None:
    # The staged source is what registration recorded (the CLI's tarball is built from
    # `git ls-files`, a clone has only tracked files), so a file the workflow's repo tracks despite
    # its own .gitignore ships too. What else the work root holds (the image map) does not.
    (tmp_path / "src/resources").mkdir(parents=True)
    (tmp_path / "src/Snakefile").write_text("rule all:\n")
    (tmp_path / "src/.gitignore").write_text("resources/*.fa\n*.json\n")
    (tmp_path / "src/resources/ref.fa").write_text(">chr1\n")
    (tmp_path / ".gitignore").write_text("src/\n")
    (tmp_path / "src/.biffix-config.json").write_text("{}")
    (tmp_path / "images.json").write_text("{}")
    got = _track(tmp_path)
    assert got.returncode == 0, got.stderr
    assert _tracked(tmp_path) == [
        "src/.biffix-config.json",
        "src/.gitignore",
        "src/Snakefile",
        "src/resources/ref.fa",
    ]


def test_a_tree_that_is_already_a_repo_is_left_alone(tmp_path: Path) -> None:
    (tmp_path / "src/.git").mkdir(parents=True)
    got = _track(tmp_path)
    assert got.returncode == 0, got.stderr
    assert not (tmp_path / ".git").exists()


# ── tasks.sh ──


QUEUES = json.dumps({"spot": "arn:q/spot", "cpu": "arn:q/cpu"})


@pytest.mark.parametrize(
    ("env", "queue"),
    [
        ({}, "arn:q/spot"),  # the map's first entry
        ({"BIFFIX_QUEUE_CLASS": "cpu"}, "arn:q/cpu"),
        ({"SNAKEMAKE_AWS_BATCH_JOB_QUEUE": "arn:q/preset"}, "arn:q/preset"),
    ],
)
def test_the_worker_queue_resolves_from_the_queue_class_map(
    env: dict[str, str], queue: str
) -> None:
    got = bash(
        'resolve_worker_queue; printf "%s" "$SNAKEMAKE_AWS_BATCH_JOB_QUEUE"',
        "tasks",
        BIFFIX_QUEUE_CLASS_MAP=QUEUES,
        SNAKEMAKE_AWS_BATCH_JOB_ROLE="arn:role",
        **env,
    )
    assert got.returncode == 0, got.stderr
    assert got.stdout == queue


@pytest.mark.parametrize(
    ("raw", "want", "queue"),
    [
        (QUEUES, "", "arn:q/spot"),
        (QUEUES, "gpu", ""),
        ("", "", ""),
        ("[1, 2]", "", ""),
        ("{bad", "cpu", ""),
        # A non-string entry is no queue ARN (it once printed as `None` or a dict's repr).
        ('{"spot": null}', "", ""),
        ('{"cpu": {"arn": "arn:q/cpu"}}', "cpu", ""),
    ],
)
def test_worker_queue_resolve(raw: str, want: str, queue: str) -> None:
    assert module("worker_queue").resolve(raw, want) == queue


@pytest.mark.parametrize(
    ("raw", "want", "why"),
    [
        ("{}", "", "BIFFIX_QUEUE_CLASS_MAP is missing or empty"),
        ("", "", "BIFFIX_QUEUE_CLASS_MAP is missing or empty"),
        ("{bad", "", "BIFFIX_QUEUE_CLASS_MAP is not a JSON object"),
        ("[1]", "cpu", "BIFFIX_QUEUE_CLASS_MAP is not a JSON object"),
        (QUEUES, "gpu", "no queue-class 'gpu' in BIFFIX_QUEUE_CLASS_MAP (it has: spot, cpu)"),
        (
            '{"cpu": 1}',
            "cpu",
            "queue-class 'cpu' in BIFFIX_QUEUE_CLASS_MAP has no queue ARN string",
        ),
        (
            '{"spot": null}',
            "",
            "the first queue-class in BIFFIX_QUEUE_CLASS_MAP has no queue ARN string",
        ),
    ],
)
def test_no_worker_queue_says_why(raw: str, want: str, why: str) -> None:
    got = bash("resolve_worker_queue", "tasks", BIFFIX_QUEUE_CLASS_MAP=raw, BIFFIX_QUEUE_CLASS=want)
    assert got.returncode == 78
    assert f"biffix-coordinator: {why}\n" in got.stderr
    assert f"no worker queue resolved (BIFFIX_QUEUE_CLASS='{want}')" in got.stderr


def test_no_worker_queue_or_role_fails_with_ex_config() -> None:
    got = bash("resolve_worker_queue", "tasks", BIFFIX_QUEUE_CLASS_MAP="{}")
    assert got.returncode == 78
    assert "no worker queue resolved (BIFFIX_QUEUE_CLASS='')" in got.stderr
    got = bash("resolve_worker_queue", "tasks", BIFFIX_QUEUE_CLASS_MAP=QUEUES)
    assert got.returncode == 78
    assert "SNAKEMAKE_AWS_BATCH_JOB_ROLE required" in got.stderr


ASSIGNMENT = json.dumps(
    {
        "by_label": {"process_high": "cpu", "size=large": "spot"},
        "by_name": {"ALIGN": "cpu", "WF:SUB:CALL": "spot"},
    }
)


def selector_lines(
    engine: str, assignment: str, queues: str = QUEUES, selectors: ModuleType | None = None
) -> list[str]:
    """queue_selectors.py's lines for `assignment` (in `selectors`, a loaded copy of the script)."""
    selectors = selectors or module("queue_selectors")
    lines: list[str] = selectors.settings(engine, *selectors.parse_assignment(assignment), queues)
    return lines


def test_a_nextflow_assignment_is_withlabel_then_withname_lines() -> None:
    assert selector_lines("nextflow", ASSIGNMENT) == [
        "withLabel\tprocess_high\tarn:q/cpu",
        "withLabel\tsize=large\tarn:q/spot",
        "withName\tALIGN\tarn:q/cpu",
        "withName\tWF:SUB:CALL\tarn:q/spot",
    ]


def test_nextflow_s_lines_become_config_selectors_with_the_queue_quoted() -> None:
    lines = selector_lines("nextflow", ASSIGNMENT, json.dumps({"cpu": "arn:q/'x\\", "spot": "s"}))
    got = bash("queue_selector_config", "nextflow", stdin="\n".join(lines) + "\n")
    assert got.returncode == 0, got.stderr
    assert got.stdout.splitlines() == [
        "process { withLabel: 'process_high' { queue = 'arn:q/\\'x\\\\' } }",
        "process { withLabel: 'size=large' { queue = 's' } }",
        "process { withName: 'ALIGN' { queue = 'arn:q/\\'x\\\\' } }",
        "process { withName: 'WF:SUB:CALL' { queue = 's' } }",
    ]
    # No assignment, no selectors.
    assert bash("queue_selector_config", "nextflow", stdin="").stdout == ""


def test_a_snakemake_assignment_is_the_executor_s_rule_queue_map() -> None:
    got = selector_lines("snakemake", json.dumps({"by_name": {"call": "spot", "align": "cpu"}}))
    assert got == ['{"align": "arn:q/cpu", "call": "arn:q/spot"}']
    # No rule named, no map.
    assert selector_lines("snakemake", "{}") == []


def test_a_snakemake_rule_name_is_any_name_snakemake_gives() -> None:
    # A `name:` directive's string, or an unnamed rule's number: the map is matched exactly, and
    # JSON quotes the name.
    names = {"1": "cpu", "align-bwa-mem": "spot", "a:b (x)": "cpu", "a'b": "cpu"}
    (got,) = selector_lines("snakemake", json.dumps({"by_name": names}))
    assert json.loads(got) == {
        "1": "arn:q/cpu",
        "align-bwa-mem": "arn:q/spot",
        "a:b (x)": "arn:q/cpu",
        "a'b": "arn:q/cpu",
    }


SELECTOR_CASES = json.loads(
    (Path(__file__).resolve().parents[3] / "schemas" / "queue-selector-cases.json").read_text()
)


@pytest.mark.parametrize(
    "case", SELECTOR_CASES, ids=lambda c: f"{c['engine']}-{c['key']}-{c['selector']!r}"
)
def test_the_selector_grammars_agree_with_registration(case: dict[str, object]) -> None:
    # biffix-workflow's cases (schemas/queue-selector-cases.json, from queues.rs's validation):
    # this script accepts exactly the selectors registration does.
    selectors = module("queue_selectors")
    assignment = json.dumps({case["key"]: {case["selector"]: "cpu"}})
    try:
        selector_lines(str(case["engine"]), assignment, selectors=selectors)
    except selectors.AssignmentError:
        assert not case["accepted"], case
    else:
        assert case["accepted"], case


@pytest.mark.parametrize(
    ("engine", "assignment", "why"),
    [
        ("nextflow", '{"by_name": {"A": "gpu"}}', "queue-class 'gpu' (by_name.A) has no queue"),
        ("snakemake", '{"by_label": {"x": "cpu"}}', "by_label applies only to Nextflow"),
        ("nextflow", '{"by_label": {"pro.*": "cpu"}}', "'pro.*' is not a Nextflow label"),
        ("nextflow", '{"by_name": {"A|B": "cpu"}}', "'A|B' is not a Nextflow process name"),
        ("snakemake", '{"by_name": {"": "cpu"}}', "'' is not a Snakemake rule name"),
        ("snakemake", '{"by_name": {"a\\nb": "cpu"}}', "'a\\nb' is not a Snakemake rule name"),
        ("snakemake", '{"by_name": {"a\\u0085": "cpu"}}', "is not a Snakemake rule name"),
        ("nextflow", '{"by_rule": {}}', "unknown keys: by_rule"),
        ("nextflow", '{"by_name": ["A"]}', "by_name is not an object of strings"),
        ("nextflow", "[1]", "BIFFIX_QUEUE_ASSIGNMENT is not a JSON object"),
        ("nextflow", '{"by_name": {"A": 1}}', "by_name is not an object of strings"),
        ("cromwell", "{}", "unknown engine 'cromwell'"),
    ],
)
def test_an_assignment_that_cannot_be_applied_says_why(
    engine: str, assignment: str, why: str
) -> None:
    got = helper(
        "queue_selectors", engine, BIFFIX_QUEUE_ASSIGNMENT=assignment, BIFFIX_QUEUE_CLASS_MAP=QUEUES
    )
    assert got.returncode == 1
    assert why in got.stderr, got.stderr


@pytest.mark.parametrize(
    ("queues", "why"),
    [
        ('{"cpu": "arn:q/\\ncpu"}', "the worker queue of 'cpu' has a control character"),
        ('{"cpu": "arn:q/\\tcpu"}', "the worker queue of 'cpu' has a control character"),
        ('{"cpu": ""}', "queue-class 'cpu' (by_name.A) has no queue in BIFFIX_QUEUE_CLASS_MAP"),
        ("{not json", "BIFFIX_QUEUE_CLASS_MAP is not JSON"),
        ('["arn:q/cpu"]', "BIFFIX_QUEUE_CLASS_MAP is not a JSON object"),
    ],
)
def test_a_queue_class_map_that_cannot_be_applied_says_why(queues: str, why: str) -> None:
    got = helper(
        "queue_selectors",
        "nextflow",
        BIFFIX_QUEUE_ASSIGNMENT='{"by_name": {"A": "cpu"}}',
        BIFFIX_QUEUE_CLASS_MAP=queues,
    )
    assert got.returncode == 1
    assert why in got.stderr, got.stderr


@pytest.mark.parametrize("engine", ["nextflow", "snakemake"])
def test_no_assignment_selects_nothing(tmp_path: Path, engine: str) -> None:
    got = bash(
        'resolve_queue_selectors; cat "$QUEUE_SELECTORS_FILE"',
        "tasks",
        WORK=str(tmp_path),
        ENGINE=engine,
        BIFFIX_QUEUE_CLASS_MAP=QUEUES,
    )
    assert got.returncode == 0, got.stderr
    assert got.stdout == ""


def test_an_assignment_is_resolved_for_the_engine(tmp_path: Path) -> None:
    got = bash(
        'resolve_queue_selectors; cat "$QUEUE_SELECTORS_FILE"',
        "tasks",
        WORK=str(tmp_path),
        ENGINE="snakemake",
        BIFFIX_QUEUE_CLASS_MAP=QUEUES,
        BIFFIX_QUEUE_ASSIGNMENT='{"by_name": {"align": "cpu", "call": "spot"}}',
    )
    assert got.returncode == 0, got.stderr
    assert got.stdout == '{"align": "arn:q/cpu", "call": "arn:q/spot"}\n'
    # Counted by selector, though Snakemake's map is one line.
    assert "2 task selector(s) on other worker queues" in got.stderr


def test_an_unresolvable_assignment_fails_with_ex_config(tmp_path: Path) -> None:
    got = bash(
        "resolve_queue_selectors",
        "tasks",
        WORK=str(tmp_path),
        ENGINE="nextflow",
        BIFFIX_QUEUE_CLASS_MAP=QUEUES,
        BIFFIX_QUEUE_ASSIGNMENT='{"by_label": {"big": "gpu"}}',
    )
    assert got.returncode == 78
    assert "queue-class 'gpu' (by_label.big) has no queue" in got.stderr
    assert "could not resolve the run's queue assignment" in got.stderr


def test_no_image_map_is_no_change() -> None:
    got = bash('fetch_image_map; echo "${SNAKEMAKE_AWS_BATCH_CONTAINER_IMAGE_MAP-unset}"', "tasks")
    assert got.returncode == 0, got.stderr
    assert got.stdout == "unset\n"


def test_an_image_map_must_be_an_s3_json_object(tmp_path: Path) -> None:
    s3 = tmp_path / "s3"
    (s3 / "b").mkdir(parents=True)
    (s3 / "b/list.json").write_text("[]")
    dest = str(tmp_path / "images.json")
    got = helper("image_map", "s3://b/list.json", dest, FAKE_S3=str(s3))
    assert got.returncode == 1
    assert "the image map is not a JSON object" in got.stderr
    got = helper("image_map", "https://b/images.json", dest, FAKE_S3=str(s3))
    assert got.returncode == 1
    assert "not an s3 uri: https://b/images.json" in got.stderr


# ── stage_source.py ──


def _tarball(members: dict[str, bytes]) -> bytes:
    buf = io.BytesIO()
    with tarfile.open(fileobj=buf, mode="w:gz") as tar:
        for name, data in members.items():
            info = tarfile.TarInfo(name)
            info.size = len(data)
            tar.addfile(info, io.BytesIO(data))
    return buf.getvalue()


def _put(s3: Path, key: str, body: bytes) -> None:
    path = s3 / key
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(body)


def test_a_tarball_member_cannot_escape_the_staging_dir(tmp_path: Path) -> None:
    import hashlib

    s3 = tmp_path / "s3"
    body = _tarball({"Snakefile": b"x", "../escape": b"y"})
    _put(s3, "b/w.tar.gz", body)
    sha = hashlib.sha256(body).hexdigest()
    dest, tmp = tmp_path / "src", tmp_path / "tmp"
    tmp.mkdir()
    got = helper(
        "stage_source",
        "tarball",
        "s3://b/w.tar.gz",
        str(dest),
        sha,
        FAKE_S3=str(s3),
        TMPDIR=str(tmp),
    )
    assert got.returncode == 1
    assert "tar member escapes staging dir: ../escape" in got.stderr
    assert not (tmp_path / "escape").exists()
    assert not (dest / "Snakefile").exists()  # checked before anything is extracted
    assert not list(tmp.iterdir())  # nor is the download left behind


def test_a_refused_tarball_is_not_left_behind(tmp_path: Path) -> None:
    s3, tmp = tmp_path / "s3", tmp_path / "tmp"
    tmp.mkdir()
    _put(s3, "b/w.tar.gz", _tarball({"Snakefile": b"x"}))
    args = ("tarball", "s3://b/w.tar.gz", str(tmp_path / "src"), "0" * 64)
    got = helper("stage_source", *args, FAKE_S3=str(s3), TMPDIR=str(tmp))
    assert got.returncode == 78
    assert "refusing to run it" in got.stderr
    assert not list(tmp.iterdir())


def test_a_tarball_uri_needs_a_key(tmp_path: Path) -> None:
    got = helper("stage_source", "tarball", "s3://b", str(tmp_path / "src"), "0" * 64)
    assert got.returncode == 1
    assert "s3 tarball uri has no key: s3://b" in got.stderr


@pytest.mark.parametrize(
    ("uri", "staged"),
    [
        ("s3://b/src/", ["Snakefile", "config/x.yaml"]),
        ("s3://b/src/Snakefile", ["Snakefile"]),  # a bare single object
        ("s3://b", ["src/Snakefile", "src/config/x.yaml", "src2/stray"]),  # the whole bucket
    ],
)
def test_a_prefix_source_stages_paths_relative_to_the_prefix(
    tmp_path: Path, uri: str, staged: list[str]
) -> None:
    s3 = tmp_path / "s3"
    for key in ("src/Snakefile", "src/config/x.yaml", "src2/stray"):
        _put(s3, f"b/{key}", b"x")
    dest = tmp_path / "out"
    got = helper("stage_source", "prefix", uri, str(dest), FAKE_S3=str(s3))
    assert got.returncode == 0, got.stderr
    assert sorted(str(p.relative_to(dest)) for p in dest.rglob("*") if p.is_file()) == staged
    assert f"staged {len(staged)} file(s)" in got.stderr


def test_a_prefix_key_cannot_escape_the_staging_dir(tmp_path: Path) -> None:
    # The stand-in lists keys by path, which has no `..` key: plant a listing that does.
    (tmp_path / "py/boto3").mkdir(parents=True)
    (tmp_path / "py/boto3/__init__.py").write_text(
        "class _P:\n"
        "    def paginate(self, Bucket, Prefix=''):\n"
        "        yield {'Contents': [{'Key': 'src/../../escape'}]}\n"
        "class _S3:\n"
        "    def get_paginator(self, name):\n"
        "        return _P()\n"
        "def client(service):\n"
        "    return _S3()\n"
    )
    got = helper(
        "stage_source",
        "prefix",
        "s3://b/src",
        str(tmp_path / "out"),
        PYTHONPATH=str(tmp_path / "py"),
    )
    assert got.returncode == 1
    assert "s3 key escapes staging dir: src/../../escape" in got.stderr


def test_stage_source_refuses_an_unknown_mode() -> None:
    got = helper("stage_source", "zip", "s3://b/x")
    assert got.returncode == 1
    assert "Usage:" in got.stderr


# ── events.sh / events_jsonl.py ──


def test_the_events_file_is_read_from_the_engine_s_launch_dir(tmp_path: Path) -> None:
    # The source root, where both engines start.
    def found(**env: str) -> str:
        got = bash("events_file", "events", SRC=str(tmp_path / "src"), **env)
        assert got.returncode == 0, got.stderr
        return got.stdout

    launch = tmp_path / "src"
    other = tmp_path
    (tmp_path / "src/sub").mkdir(parents=True)
    assert found() == ""
    # A same-named file anywhere else (committed in the staged source, say) is not this run's.
    (other / ".biffix_events.jsonl").write_text("{}\n")
    (tmp_path / "src/sub/.biffix_events.jsonl").write_text("{}\n")
    assert found() == ""
    (launch / ".biffix_events.jsonl").write_text("{}\n")
    assert found() == f"{launch}/.biffix_events.jsonl"
    # A configured name, with directories in it, relative to the launch dir or absolute.
    (launch / "logs").mkdir()
    (launch / "logs/ev.jsonl").write_text("{}\n")
    assert found(BIFFIX_EVENT_FILE="logs/ev.jsonl") == f"{launch}/logs/ev.jsonl"
    absolute = tmp_path / "abs.jsonl"
    absolute.write_text("{}\n")
    assert found(BIFFIX_EVENT_FILE=str(absolute)) == str(absolute)


def test_no_events_file_is_no_upload_and_no_replay(tmp_path: Path) -> None:
    got = bash(
        "upload_events_jsonl; replay_events_jsonl",
        "events",
        SRC=str(tmp_path / "src"),
        BIFFIX_INGEST_URL="http://127.0.0.1:9",
        BIFFIX_INGEST_TOKEN="t",
    )
    assert got.returncode == 0, got.stderr
    assert got.stderr == "biffix-coordinator: no events.jsonl to upload\n"


def test_a_failed_upload_is_swallowed(tmp_path: Path) -> None:
    src = tmp_path / "events.jsonl"
    src.write_text("{}\n")
    blocker = tmp_path / "s3"
    blocker.write_text("not a directory")
    got = helper(
        "events_jsonl", "upload", str(src), "s3://b/runs/r/events.jsonl", FAKE_S3=str(blocker)
    )
    assert got.returncode == 0
    assert "events.jsonl upload failed (swallowed)" in got.stderr


@pytest.mark.parametrize("dest", ["https://b/k", "s3://b", "s3:///k"])
def test_the_upload_refuses_anything_but_an_s3_object_uri(tmp_path: Path, dest: str) -> None:
    src = tmp_path / "events.jsonl"
    src.write_text("{}\n")
    got = helper("events_jsonl", "upload", str(src), dest, FAKE_S3=str(tmp_path / "s3"))
    assert got.returncode == 0
    assert f"upload failed (swallowed): not an s3 object uri: {dest}" in got.stderr
    assert not (tmp_path / "s3").exists()


def test_the_replay_reads_every_well_formed_event(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    src = tmp_path / "events.jsonl"
    src.write_text('{"kind": "a"}\n\nnot json\n  {"kind": "b", "n": 1}  \n[1]\n{"kind": "c"')
    items = module("events_jsonl").read_events(str(src))
    assert [event for event, _ in items] == [{"kind": "a"}, {"kind": "b", "n": 1}]
    assert items[1][1] == b'{"kind":"b","n":1}'
    # Not silently: the replayed count alone would not show them.
    assert f"skipped 3 malformed line(s) of {src}" in capsys.readouterr().err


def test_replay_chunks_are_cut_at_the_route_s_event_count() -> None:
    ev = module("events_jsonl")
    items = [({"i": i}, b"{}") for i in range(ev.MAX_EVENTS * 2 + 1)]
    sizes = [len(chunk) for chunk in ev.chunks(items)]
    assert sizes == [ev.MAX_EVENTS, ev.MAX_EVENTS, 1]
    assert [item for chunk in ev.chunks(items) for item in chunk] == items
    assert list(ev.chunks([])) == []


INGEST_LIMITS = Path(__file__).resolve().parents[3] / "schemas" / "ingest-limits.json"


def test_the_replay_s_limits_and_retry_policy_are_biffix_core_s() -> None:
    # biffix_core::ingest_limits is the one source, written to schemas/ingest-limits.json; the
    # engine plugins' posters are pinned to it the same way.
    limits = json.loads(INGEST_LIMITS.read_text())
    ev = module("events_jsonl")
    assert ev.MAX_EVENTS == limits["route"]["max_batch_events"]
    assert ev.ROUTE_MAX_BYTES == limits["route"]["max_body_bytes"]
    poster = limits["poster"]
    assert ev.MAX_BYTES == poster["max_batch_bytes"]
    assert ev.SPLIT_BUDGET == poster["split_post_budget"]
    assert ev.ATTEMPTS == poster["max_attempts"]
    assert ev.RETRY_BACKOFF * 1000 == poster["retry_backoff_ms"]
    assert ev.MAX_RETRY_AFTER * 1000 == poster["max_retry_after_ms"]


def _http_error(code: int, retry_after: str | None = None) -> urllib.error.HTTPError:
    import email.message

    headers = email.message.Message()
    if retry_after is not None:
        headers["Retry-After"] = retry_after
    return urllib.error.HTTPError("http://ingest/events", code, "x", headers, io.BytesIO(b""))


@pytest.mark.parametrize(
    ("code", "retry_after", "outcome", "sleeps"),
    [
        (408, None, "sent", [0.5]),  # transient, as the posters treat it
        (429, "2", "sent", [2.0]),  # after the answer's Retry-After
        (503, "999", "sent", [30.0]),  # capped
        (503, None, "down", [0.5, 1.0]),  # a doubling backoff, then ingest is down
        (404, None, "final", []),
    ],
)
def test_the_replay_retries_as_the_posters_do(
    monkeypatch: pytest.MonkeyPatch,
    code: int,
    retry_after: str | None,
    outcome: str,
    sleeps: list[float],
) -> None:
    ev = module("events_jsonl")
    answers = [_http_error(code, retry_after)] * (3 if outcome == "down" else 1)
    slept: list[float] = []

    class Ok:
        def __enter__(self) -> Ok:
            return self

        def __exit__(self, *_: object) -> None:
            pass

        def read(self, _n: int) -> bytes:
            return b"{}"

    class Opener:
        def open(self, _req: object, timeout: float) -> Ok:  # noqa: ARG002 (urllib's keyword)
            if answers:
                raise answers.pop(0)
            return Ok()

    monkeypatch.setattr(ev, "OPENER", Opener())
    monkeypatch.setattr(ev.time, "sleep", slept.append)
    replay = ev.Replay("http://ingest/events", "t", ev.time.monotonic() + 300)
    got, _ = replay.post([({"kind": "a"}, b"{}")])
    assert got == outcome
    assert slept == sleeps


def test_the_replay_never_follows_a_redirect() -> None:
    # urllib re-sends a redirected POST as a GET without the events, and the GET's 2xx read as
    # the chunk delivered.
    from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
    from threading import Thread

    methods: list[str] = []

    class Handler(BaseHTTPRequestHandler):
        def _answer(self, code: int) -> None:
            methods.append(self.command)
            self.send_response(code)
            if code == 302:
                self.send_header("Location", "/elsewhere")
            self.send_header("Content-Length", "0")
            self.end_headers()

        def do_POST(self) -> None:
            self.rfile.read(int(self.headers["Content-Length"]))
            self._answer(302)

        def do_GET(self) -> None:
            self._answer(200)

        def log_message(self, *args: object) -> None:
            pass

    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    Thread(target=server.serve_forever, daemon=True).start()
    try:
        ev = module("events_jsonl")
        url = f"http://127.0.0.1:{server.server_address[1]}/events"
        replay = ev.Replay(url, "t", ev.time.monotonic() + 30)
        outcome, detail = replay.post([({"kind": "a"}, b"{}")])
    finally:
        server.shutdown()
        server.server_close()
    assert (outcome, methods) == ("final", ["POST"])
    assert detail.startswith("HTTP 302")


def test_an_empty_events_file_replays_nothing(tmp_path: Path) -> None:
    src = tmp_path / "events.jsonl"
    src.write_text("\n")
    got = helper(
        "events_jsonl",
        "replay",
        str(src),
        BIFFIX_INGEST_URL="http://127.0.0.1:9",
        BIFFIX_INGEST_TOKEN="t",
    )
    assert got.returncode == 0
    assert got.stderr == ""


# ── snakemake.sh / sm_metadata.py ──


@pytest.mark.parametrize(
    ("sent", "tags"),
    [
        ("", "biffix:run=r1,biffix:project=p1"),
        ("team=x", "team=x,biffix:run=r1,biffix:project=p1"),
        ("biffix:run=r1", "biffix:run=r1,biffix:project=p1"),
        ("biffix:project=p1,biffix:run=r1", "biffix:project=p1,biffix:run=r1"),
        # A key that merely ends like a biffix tag is not one.
        ("x-biffix:run=r9", "x-biffix:run=r9,biffix:run=r1,biffix:project=p1"),
    ],
)
def test_snakemake_job_tags_adds_the_run_and_project_when_missing(sent: str, tags: str) -> None:
    got = bash(f"snakemake_job_tags {sent!r}", "snakemake", BIFFIX_RUN_ID="r1", BIFFIX_PROJECT="p1")
    assert got.returncode == 0, got.stderr
    assert got.stdout == tags


def _metadata_dir(root: Path) -> Path:
    meta = root / ".snakemake"
    (meta / "metadata").mkdir(parents=True)
    (meta / "metadata/out").write_text("a")
    return meta


def test_a_metadata_sync_uploads_only_what_changed_unless_final(tmp_path: Path) -> None:
    s3, meta, fp = tmp_path / "s3", _metadata_dir(tmp_path), tmp_path / "fp"
    obj = s3 / "b/store/.snakemake/biffix-metadata.tar.gz"
    args = ("sync", "s3://b/store/.snakemake/biffix-metadata.tar.gz", str(meta), str(fp))

    def sync(*final: str) -> None:
        got = helper("sm_metadata", *args, *final, FAKE_S3=str(s3))
        assert got.returncode == 0 and got.stderr == "", got.stderr

    sync()
    with tarfile.open(obj) as tar:
        assert tar.getnames() == ["metadata", "metadata/out"]
    obj.unlink()
    sync()  # nothing changed: no upload
    assert not obj.exists()
    sync("final")  # always uploads
    assert obj.exists()
    obj.unlink()
    (meta / "incomplete").mkdir()
    (meta / "incomplete/x").write_text("")
    sync()  # changed: uploads again, with the incomplete markers
    with tarfile.open(obj) as tar:
        assert "incomplete/x" in tar.getnames()


def test_a_failed_metadata_sync_is_swallowed(tmp_path: Path) -> None:
    meta = _metadata_dir(tmp_path)
    blocker = tmp_path / "s3"
    blocker.write_text("not a directory")
    got = helper(
        "sm_metadata",
        "sync",
        "s3://b/m.tar.gz",
        str(meta),
        str(tmp_path / "fp"),
        FAKE_S3=str(blocker),
    )
    assert got.returncode == 0
    assert "snakemake metadata sync failed (swallowed)" in got.stderr


def test_the_metadata_fingerprint_follows_sizes_and_paths(tmp_path: Path) -> None:
    fingerprint = module("sm_metadata").fingerprint
    meta = _metadata_dir(tmp_path)
    first = fingerprint(str(meta))
    assert fingerprint(str(meta)) == first
    (meta / "metadata/out").write_text("ab")
    assert fingerprint(str(meta)) != first
    (meta / "other").mkdir()
    (meta / "other/ignored").write_text("x")  # only metadata/ and incomplete/ count
    second = fingerprint(str(meta))
    (meta / "other/ignored").write_text("xy")
    assert fingerprint(str(meta)) == second


def test_restoring_no_metadata_is_quiet_unless_resuming(tmp_path: Path) -> None:
    args = ("restore", "s3://b/none.tar.gz", str(tmp_path / ".snakemake"))
    got = helper("sm_metadata", *args, FAKE_S3=str(tmp_path))
    assert got.returncode == 0 and got.stderr == ""
    got = helper("sm_metadata", *args, FAKE_S3=str(tmp_path), BIFFIX_RESUME="1")
    assert got.returncode == 0
    assert "no snakemake metadata to restore" in got.stderr
    assert "resuming from the existing outputs alone" in got.stderr
    assert not (tmp_path / ".snakemake").exists()


@pytest.mark.parametrize("uri", ["https://b/m.tar.gz", "s3://b", "s3:///m.tar.gz"])
def test_the_metadata_helpers_refuse_anything_but_an_s3_object_uri(
    tmp_path: Path, uri: str
) -> None:
    meta = _metadata_dir(tmp_path)
    s3 = tmp_path / "s3"
    got = helper(
        "sm_metadata", "sync", uri, str(meta), str(tmp_path / "fp"), "final", FAKE_S3=str(s3)
    )
    assert got.returncode == 0
    assert f"sync failed (swallowed): not an s3 object uri: {uri}" in got.stderr
    got = helper("sm_metadata", "restore", uri, str(meta), FAKE_S3=str(s3))
    assert got.returncode == 1
    assert f"restore failed: not an s3 object uri: {uri}" in got.stderr
    assert not s3.exists()


def test_a_failed_metadata_restore_fails_loudly(tmp_path: Path) -> None:
    args = ("restore", "s3://b/m.tar.gz", str(tmp_path / ".snakemake"))
    got = helper("sm_metadata", *args, FAKE_S3=str(tmp_path), FAKE_S3_DENY_GET="1")
    assert got.returncode == 1
    assert "snakemake metadata restore failed" in got.stderr and "AccessDenied" in got.stderr
    # A tarball that is not one fails the same way.
    _put(tmp_path, "b/m.tar.gz", b"not a tarball")
    got = helper("sm_metadata", *args, FAKE_S3=str(tmp_path))
    assert got.returncode == 1
    assert "snakemake metadata restore failed" in got.stderr


@pytest.mark.parametrize("deny", ["", "1"])
def test_a_failed_metadata_restore_keeps_the_store_s_record(tmp_path: Path, deny: str) -> None:
    # The STORE has a record this run could not read: syncing this run's metadata over it would
    # replace it with this run's jobs alone, so no sync uploads. A missing record syncs as usual.
    s3, work = tmp_path / "s3", tmp_path / "work"
    _metadata_dir(work)
    if deny:
        _put(s3, "b/store/m.tar.gz", b"the STORE's record")
    got = bash(
        "restore_snakemake_metadata; sync_snakemake_metadata; sync_snakemake_metadata final",
        "snakemake",
        SRC=str(work),
        SM_META_URI="s3://b/store/m.tar.gz",
        SM_META_FINGERPRINT=str(tmp_path / "fp"),
        FAKE_S3=str(s3),
        FAKE_S3_DENY_GET=deny,
    )
    assert got.returncode == 0, got.stderr
    obj = s3 / "b/store/m.tar.gz"
    if deny:
        assert "this run's metadata is not synced" in got.stderr
        assert obj.read_bytes() == b"the STORE's record"
    else:
        assert got.stderr == ""
        with tarfile.open(obj) as tar:
            assert "metadata/out" in tar.getnames()


class _Rule:
    def __init__(self, name: str, **attrs: object):
        self.name = name
        self.__dict__.update(attrs)


class _Cache:
    def __init__(self, output: object):
        self.output = output


def test_the_cache_rule_helper_reads_both_snakemake_apis() -> None:
    # Loading the helper imports no Snakemake until main() runs.
    names = module("snakemake_cache_rules").cache_rule_names

    class Before917:  # the marking is the workflow's name -> mode map
        rules: ClassVar[list[_Rule]] = [_Rule("a"), _Rule("b")]
        cache_rules: ClassVar[dict[str, str]] = {"a": "omit-software"}

    class From917:  # the marking is on the rule
        rules: ClassVar[list[_Rule]] = [
            _Rule("a", cache=_Cache("omit-software")),
            _Rule("b", cache=None),
        ]

    assert names(Before917()) == ["a"]
    assert names(From917()) == ["a"]


# ── nextflow.sh ──


@pytest.mark.parametrize(
    ("value", "literal"),
    [
        ("plain", "'plain'"),
        ("it's", r"'it\'s'"),
        ("a\\b", r"'a\\b'"),
        ("\\'", r"'\\\''"),
        ("", "''"),
        ("$x ${y}", "'$x ${y}'"),  # single-quoted Groovy strings do not interpolate
    ],
)
def test_groovy_str_escapes_backslashes_and_quotes(value: str, literal: str) -> None:
    got = bash('groovy_str "$V" what', "nextflow", V=value)
    assert got.returncode == 0, got.stderr
    assert got.stdout == literal


@pytest.mark.parametrize("value", ["a\nb", "a\rb", "a\tb"])
def test_groovy_str_refuses_a_control_character(value: str) -> None:
    got = bash('groovy_str "$V" "the value"', "nextflow", V=value)
    assert got.returncode == 78
    assert "the value has a control character; it cannot go into nextflow.config" in got.stderr


def test_layer_plugin_version_reads_the_one_plugin_dir(tmp_path: Path) -> None:
    for d in ("nf-amazon-3.4.4", "nf-amazon-2fa-1.0.0", "nf-biffix-0.5.0+build.1"):
        (tmp_path / d).mkdir()
    # A stray file next to the plugin dir is a second candidate, which fails the run.
    (tmp_path / "nf-amazon-9.9.9.zip").write_text("")

    def version(plugin: str) -> subprocess.CompletedProcess[str]:
        return bash(f"layer_plugin_version {plugin}", "nextflow", NXF_PLUGINS_DIR=str(tmp_path))

    got = version("nf-biffix")
    assert got.returncode == 0, got.stderr
    assert got.stdout == "0.5.0+build.1"
    got = version("nf-amazon")
    assert got.returncode == 78
    assert "must carry exactly one nf-amazon plugin" in got.stderr
    got = version("nf-cloudcache")
    assert got.returncode == 78
    assert "must carry exactly one nf-cloudcache plugin" in got.stderr


# ── single_job.sh / single_job.py ──


@pytest.mark.parametrize(
    ("image", "name"),
    [
        # Nextflow's SingularityCache.simpleName of docker://<image>.
        ("quay.io/biocontainers/fastqc:0.12.1--h1", "quay.io-biocontainers-fastqc-0.12.1--h1.img"),
        ("123.dkr.ecr.r.amazonaws.com/w@sha256:ab", "123.dkr.ecr.r.amazonaws.com-w@sha256-ab.img"),
        ("ubuntu", "ubuntu.img"),
        ("reg/x.sif", "reg-x.sif"),
        ("reg/x.sif:1", "reg-x-1.sif"),
    ],
)
def test_image_files_are_named_as_nextflow_looks_for_them(image: str, name: str) -> None:
    assert module("single_job").image_file_name(image) == name


def test_the_tenant_images_are_the_map_s_distinct_values_and_the_extras() -> None:
    sj = module("single_job")
    assert sj.tenant_images({"a": "x", "b": "x", "c": "y"}, ["z", "", "x"]) == ["x", "y", "z"]


@pytest.mark.parametrize(
    ("image", "registry"),
    [
        ("123.dkr.ecr.r.amazonaws.com/w:1", "123.dkr.ecr.r.amazonaws.com"),
        ("localhost/w", "localhost"),
        ("localhost:5000/w", "localhost:5000"),
        ("biocontainers/fastqc:1", ""),
        ("ubuntu:24.04", ""),
    ],
)
def test_registry_of(image: str, registry: str) -> None:
    assert module("single_job").registry_of(image) == registry


def test_only_the_region_s_ecr_gets_the_login() -> None:
    ecr = module("single_job").ecr_host("us-east-1")
    assert ecr.match("123456789012.dkr.ecr.us-east-1.amazonaws.com")
    assert not ecr.match("123456789012.dkr.ecr.us-west-2.amazonaws.com")
    assert not ecr.match("123456789012.dkr.ecr.us-east-1.amazonaws.com.evil.example")
    assert not ecr.match("quay.io")


def test_each_rule_runs_the_image_its_batch_job_would() -> None:
    sj = module("single_job")
    rule = sj.RuleImage
    image_map = {
        "tool:1": "T",
        "docker.io/library/other:2": "O",
        "W": "W",
        "$default": "W",
    }
    images, problems = sj.plan_rule_images(
        [
            rule("as_written", image="tool:1"),
            # Looked up in its normalized spelling, as the AWS Batch executor does.
            rule("normalized", image="docker.io/library/tool:1"),
            rule("short", image="other:2"),
            rule("default"),
            rule("local", image="tool:1", local=True),
            rule("unmapped", image="docker://quay.io/x:1"),
            rule("reserved", image="$default"),
            rule("dynamic", problem="set by a function"),
        ],
        image_map,
        "W",
    )
    assert images == {
        "as_written": "T",
        "normalized": "T",
        "short": "O",
        "default": "W",
        "local": None,
    }
    assert [p.split(":")[0] for p in problems] == [
        "rule unmapped",
        "rule reserved",
        "rule dynamic",
    ]
    assert "not one the run's image map names" in problems[0]
    # No default image: such a rule runs in the job itself, as its Batch job would run in the
    # coordinator's own image.
    images, problems = sj.plan_rule_images([rule("default")], {}, "")
    assert images == {"default": None} and problems == []


def test_a_grouped_rule_without_an_image_runs_in_its_group_s() -> None:
    """On AWS Batch a group's rules run as one job, in the image its rules name (Snakemake merges
    the resource across the group job), so a member that names none takes the group's image."""
    sj = module("single_job")
    rule = sj.RuleImage
    planned, problems = sj.group_images(
        [
            rule("tool", image="tool:1"),
            rule("bare"),
            rule("alone"),
            rule("other_group"),
            rule("dynamic", problem="set by a function"),
            rule("mixed_a", image="tool:1"),
            rule("mixed_b", image="other:2"),
            rule("mixed_bare"),
        ],
        {
            "tool": "g",
            "bare": "g",
            "alone": None,
            "other_group": "h",
            "dynamic": "g",
            "mixed_a": "m",
            "mixed_b": "m",
            "mixed_bare": "m",
        },
    )
    assert {r.name: r.image for r in planned} == {
        "tool": "tool:1",
        "bare": "tool:1",
        "alone": None,
        "other_group": None,
        "dynamic": None,
        "mixed_a": "tool:1",
        "mixed_b": "other:2",
        "mixed_bare": None,
    }
    assert [r.name for r in planned if r.problem] == ["dynamic"]
    # Snakemake refuses a group job whose rules name different images; so does a single-job run.
    assert len(problems) == 1
    assert problems[0].startswith("group m: its rules name different images (other:2, tool:1)")


def test_a_rule_image_resource_must_be_a_plain_string() -> None:
    sj = module("single_job")

    class Resource:
        def __init__(self, value: object, evaluable: bool = False):
            self.value, self._evaluable = value, evaluable

        def is_evaluable(self) -> bool:
            return self._evaluable

    assert sj.rule_image("a", None, False) == sj.RuleImage("a")
    assert sj.rule_image("a", Resource(None), True) == sj.RuleImage("a", local=True)
    assert sj.rule_image("a", Resource(None, True), True) == sj.RuleImage("a", local=True)
    assert sj.rule_image("a", Resource(3), True) == sj.RuleImage("a", local=True)
    assert sj.rule_image("a", Resource("t:1"), False) == sj.RuleImage("a", image="t:1")
    assert "set by a function" in (sj.rule_image("a", Resource(None, True), False).problem or "")
    assert "not a plain string (3)" in (sj.rule_image("a", Resource(3), False).problem or "")


_MODULE_SNAKEFILE = """\
container: "docker://module-global:1"
rule m:
    output: "m.txt"
    resources: aws_batch_container_image="tool:1"
    shell: "echo m > {output}"
rule mdef:
    output: "mdef.txt"
    shell: "echo mdef > {output}"
"""

_SNAKEFILE = """\
container: "docker://global:1"
localrules: loc, grouped
module mm:
    snakefile: "mod/Snakefile"
use rule * from mm as mm_*
rule all:
    input: "a.txt", "b.txt", "c.txt", "l.txt", "m.txt", "mdef.txt", "g.txt"
rule grouped:
    input: "gt.txt"
    output: "g.txt"
    group: "g"
    shell: "echo g > {output}"
rule gtool:
    output: "gt.txt"
    group: "g"
    resources: aws_batch_container_image="tool:1"
    shell: "echo gt > {output}"
rule a:
    output: "a.txt"
    resources: aws_batch_container_image="docker.io/library/tool:1"
    shell: "echo a > {output}"
rule b:
    output: "b.txt"
    shell: "echo b > {output}"
rule c:
    output: "c.txt"
    container: "docker://ubuntu:24.04"
    shell: "echo c > {output}"
rule loc:
    output: "l.txt"
    resources: aws_batch_container_image=lambda wildcards: "ignored-local-image:1"
    shell: "echo l > {output}"
"""


def test_a_real_snakemake_runs_every_rule_in_the_image_its_batch_job_would(tmp_path: Path) -> None:
    pytest.importorskip("snakemake")
    sm = RealSnakemake(tmp_path)
    (sm.src / "mod/Snakefile").write_text(_MODULE_SNAKEFILE)
    workflow = _SNAKEFILE
    for name in ("a", "b", "c", "l", "g", "gt"):
        workflow = workflow.replace(f"echo {name} >", "echo ${{BIFFIX_TEST_IMAGE:-}} >")
    module_workflow = _MODULE_SNAKEFILE.replace("echo m >", "echo $BIFFIX_TEST_IMAGE >").replace(
        "echo mdef >", "echo $BIFFIX_TEST_IMAGE >"
    )
    (sm.src / "mod/Snakefile").write_text(module_workflow)
    got = sm.run(workflow, "all")
    assert got.returncode == 0, got.stderr
    tool = str(sm.images / "reg.io-t@sha256-1.img")
    worker = str(sm.images / "reg.io-w@sha256-2.img")
    # One job per image: the rules with the resource (a module rule's included) run the mapped
    # image; the rest the default image, whatever their own or the workflow's `container:`; the
    # local rule runs no container, unless it is in a group, whose Batch job runs the image a rule
    # of the group names (its own naming none). Nothing is pulled.
    assert {
        name: (sm.src / f"{name}.txt").read_text().strip()
        for name in ("a", "b", "c", "l", "m", "mdef", "g", "gt")
    } == {
        "a": tool,
        "b": worker,
        "c": worker,
        "l": "",
        "m": tool,
        "mdef": worker,
        "g": tool,
        "gt": tool,
    }
    assert json.loads(sm.task_images.read_text()) == {
        "a": "reg.io/t@sha256:1",
        "b": "reg.io/w@sha256:2",
        "c": "reg.io/w@sha256:2",
        "grouped": "reg.io/t@sha256:1",
        "gtool": "reg.io/t@sha256:1",
        "mm_m": "reg.io/t@sha256:1",
        "mm_mdef": "reg.io/w@sha256:2",
    }
    assert "Pulling" not in got.stderr
    prefix = tmp_path / "apptainer/snakemake"
    assert not prefix.exists() or not any(prefix.iterdir())


def test_only_the_main_snakemake_process_writes_the_task_image_report(tmp_path: Path) -> None:
    """A job Snakemake spawns (a `run:` job) reloads the wrapper Snakefile too: it sets its rules'
    containers, but two such jobs both publishing the report through one temporary file race, and
    the loser's rename fails its job. The main process alone writes it, before any job starts."""
    pytest.importorskip("snakemake")
    from types import SimpleNamespace

    sj = module("single_job")
    report = tmp_path / "task-images.json"
    images = tmp_path / "images.json"
    tenant = "reg.io/t@sha256:1"
    images.write_text(json.dumps({"tool": tenant}))
    for main, written in [(False, False), (True, True)]:
        rule = SimpleNamespace(
            name="mapped",
            resources={
                sj.IMAGE_RESOURCE: SimpleNamespace(value="tool", is_evaluable=lambda: False)
            },
            norun=False,
            is_template_engine=False,
            is_run=False,
            _group=None,
            container_img=None,
            is_containerized=True,
        )
        workflow = SimpleNamespace(
            rules=[rule],
            _localrules=set(),
            group_settings=SimpleNamespace(overwrite_groups={}),
            is_main_process=main,
        )
        sj.set_rule_containers(workflow, str(images), str(tmp_path), "", str(report), False)
        # Either process sets the rule's container to its converted image file...
        assert rule.container_img == str(tmp_path / sj.image_file_name(tenant))
        assert rule.is_containerized is False
        # ...but only the main one publishes the report.
        assert report.exists() == written
    assert json.loads(report.read_text()) == {"mapped": tenant}
    assert sorted(p.name for p in tmp_path.iterdir()) == ["images.json", "task-images.json"]


def test_a_local_native_run_block_runs_in_the_engine_without_an_image(tmp_path: Path) -> None:
    """A local Python run: block executes in the engine, so it reports no task image (one with an
    image is refused: test_snakemake_single_job.py)."""
    pytest.importorskip("snakemake")
    sm = RealSnakemake(tmp_path)
    got = sm.run("""localrules: native
rule native:
    output: "native.txt"
    run:
        import sys
        with open(output[0], "w") as out:
            out.write(sys.executable)
""")
    assert got.returncode == 0, got.stderr
    assert Path((sm.src / "native.txt").read_text()).resolve() == Path(sys.executable).resolve()
    assert not sm.recorded.exists()
    assert json.loads(sm.task_images.read_text()) == {}


def test_a_real_snakemake_refuses_a_rule_whose_image_the_run_cannot_have(tmp_path: Path) -> None:
    pytest.importorskip("snakemake")
    sm = RealSnakemake(tmp_path)
    snakefile = """\
rule all:
    input: "u.txt", "f.txt", "p.txt"
rule plain:
    output: "p.txt"
    shell: "echo p > {output}"
rule unmapped:
    output: "u.txt"
    resources: aws_batch_container_image="docker://quay.io/evil:1"
    shell: "echo u > {output}"
rule function:
    output: "f.txt"
    resources: aws_batch_container_image=lambda wildcards: "tool:1"
    shell: "echo f > {output}"
"""
    got = sm.run(snakefile, convert=False)
    assert got.returncode != 0
    assert "rule unmapped: its image 'docker://quay.io/evil:1'" in got.stderr
    assert "rule function: its aws_batch_container_image is set by a function" in got.stderr
    # The default image, which `plain` runs, was not converted either.
    assert "the image file of reg.io/w@sha256:2" in got.stderr
    assert "it was not converted" in got.stderr
    # Before any job ran.
    assert not sm.recorded.exists()
    assert not (sm.src / "u.txt").exists() and not sm.task_images.exists()


def test_a_conversion_failure_names_two_images_that_would_share_a_file(tmp_path: Path) -> None:
    map_file = tmp_path / "images.json"
    map_file.write_text(json.dumps({"a": "reg/a:1", "b": "reg/a/1"}))
    failure = tmp_path / "failure.txt"
    got = helper(
        "single_job",
        "convert",
        str(map_file),
        str(tmp_path / "i"),
        str(tmp_path / "l"),
        str(failure),
    )
    assert got.returncode == 1
    assert "would share the image file reg-a-1.img" in failure.read_text()


def test_the_single_job_mode_and_its_scratch_root_are_checked(tmp_path: Path) -> None:
    got = bash("select_execution", "single_job", ENGINE="snakemake", BIFFIX_EXECUTION="x")
    assert got.returncode == 78
    got = bash("select_execution; echo $EXECUTION", "single_job", ENGINE="snakemake")
    assert got.returncode == 0 and got.stdout == "tasks\n"
    # Not a directory: refused before anything is cleared.
    scratch = tmp_path / "scratch"
    scratch.write_text("a file")
    got = bash(
        "install_signal_traps; EXECUTION=single_job; prepare_scratch",
        "signals",
        "single_job",
        SCRATCH=str(scratch),
        WORK=str(scratch / "run"),
    )
    assert got.returncode == 78
    assert "is not a writable directory" in got.stderr


# The backend mirrors this split to size a run (runs/single_job.rs task_memory_mib), its test pinned
# to the same values: change the two together.
@pytest.mark.parametrize(
    ("engine", "memory", "tasks", "heap"),
    [
        ("nextflow", "65536", "59392", "4096"),
        ("nextflow", "524288", "499712", "8192"),
        ("nextflow", "8192", "6144", "1024"),
        # A c6id.8xlarge's whole-host job.
        ("nextflow", "61184", "55448", "3824"),
        ("snakemake", "61184", "57360", ""),
        ("snakemake", "8192", "6144", ""),
        ("snakemake", "65536", "61440", ""),
        ("snakemake", "524288", "516096", ""),
    ],
)
def test_the_engine_keeps_a_reserve_of_the_job_s_memory(
    engine: str, memory: str, tasks: str, heap: str
) -> None:
    got = bash(
        'single_job_memory; echo "$SJ_TASK_MEMORY_MIB ${SJ_JVM_HEAP_MIB:-}"',
        "single_job",
        ENGINE=engine,
        SJ_MEMORY_MIB=memory,
    )
    assert got.returncode == 0, got.stderr
    assert got.stdout == f"{tasks} {heap}\n"


# ── run_logs.py ──


def _task(work: Path, name: str, status: str, mtime: int) -> Path:
    task = work / name[:2] / name[2:]
    task.mkdir(parents=True)
    (task / ".exitcode").write_text(status)
    (task / ".command.err").write_text(f"{name} err")
    os.utime(task / ".exitcode", (mtime, mtime))
    return task


def test_only_failed_tasks_files_are_collected_the_most_recent_first(tmp_path: Path) -> None:
    rl = module("run_logs")
    _task(tmp_path, "aa1111", "0", 100)
    _task(tmp_path, "bb2222", "1", 200)
    _task(tmp_path, "cc3333", "137\n", 300)
    names = [u.name for u in rl.failed_tasks(tmp_path)]
    assert names == ["tasks/cc/3333/.command.err", "tasks/bb/2222/.command.err"]
    total = rl.MAX_FAILED_TASKS + 5
    for i in range(total):
        _task(tmp_path / "many", f"d{i:05d}", "1", 400 + i)
    # The cap keeps the newest, newest first: the 5 oldest are the ones left out.
    names = [u.name for u in rl.failed_tasks(tmp_path / "many")]
    assert names == [
        f"tasks/d0/{i:04d}/.command.err"
        for i in range(total - 1, total - 1 - rl.MAX_FAILED_TASKS, -1)
    ]


def test_failure_txt_is_the_status_and_the_end_of_the_output(tmp_path: Path) -> None:
    rl = module("run_logs")
    out = tmp_path / "out"
    out.write_text("".join(f"line {i}\n" for i in range(1000)))
    text = rl.failure_text(out, "2")
    assert text.startswith("The engine exited with status 2.")
    assert "line 999" in text and "line 799\n" not in text and "line 800" in text
    assert rl.failure_text(tmp_path / "none", "1") == "The engine exited with status 1.\n"


def test_a_state_file_that_cannot_be_written_does_not_stop_the_uploads(tmp_path: Path) -> None:
    a, b, s3 = tmp_path / "a.log", tmp_path / "b.log", tmp_path / "s3"
    a.write_text("one")
    b.write_text("two")
    state = tmp_path / "no-such-dir" / "state.json"
    got = helper(
        "run_logs", "s3://b/logs/", str(state), "30", "--file", f"a.log={a}", "--file",
        f"b.log={b}", FAKE_S3=str(s3),
    )  # fmt: skip
    assert got.returncode == 0, got.stderr
    assert "could not record the logs sent" in got.stderr
    assert "uploaded 2 log file(s)" in got.stderr
    assert (s3 / "b/logs/a.log").read_text() == "one"
    assert (s3 / "b/logs/b.log").read_text() == "two"


class _Growing:
    """A log whose writer appends while it is read: just before the first read (after the reader
    has observed its size), `extra` is appended to the file, so a read past that size gets it."""

    def __init__(self, path: Path, extra: bytes) -> None:
        self.path = path
        self.fh = open(path, "rb")  # noqa: SIM115 - closed by __exit__
        self.extra = extra
        self.grown = False

    def __enter__(self) -> _Growing:
        return self

    def __exit__(self, *exc: object) -> None:
        self.fh.close()

    def __getattr__(self, name: str) -> object:
        return getattr(self.fh, name)

    def read(self, size: int = -1) -> bytes:
        if not self.grown:
            self.grown = True
            with open(self.path, "ab") as writer:
                writer.write(self.extra)
        return self.fh.read(size)


def test_a_tail_never_reads_past_its_cap_while_the_log_grows(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    rl = module("run_logs")
    log = tmp_path / "engine.log"
    log.write_bytes(b"x" * 100)
    opened: list[Path] = []

    def growing(path: Path) -> _Growing:
        # Each open sees the log as written, before this open's own append.
        path.write_bytes(b"x" * 100)
        opened.append(path)
        return _Growing(path, b"y" * 1000)

    monkeypatch.setattr(rl, "_open_regular", growing)
    assert rl.tail_bytes(log, 40) == b"x" * 40
    monkeypatch.setattr(rl, "FAILURE_BYTES", 40)
    assert rl.failure_text(log, "1").endswith("x" * 40 + "\n")
    assert opened == [log, log]


def test_logs_are_uploaded_only_when_they_changed(tmp_path: Path) -> None:
    log, state, s3 = tmp_path / "engine.log", tmp_path / "state.json", tmp_path / "s3"
    log.write_text("one")

    def upload() -> subprocess.CompletedProcess[str]:
        return helper(
            "run_logs", "s3://b/runs/r/logs/", str(state), "30", "--file", f"e.log={log}",
            FAKE_S3=str(s3),
        )  # fmt: skip

    assert "uploaded 1 log file(s)" in upload().stderr
    assert (s3 / "b/runs/r/logs/e.log").read_text() == "one"
    assert "uploaded" not in upload().stderr
    log.write_text("one two")
    assert "uploaded 1 log file(s)" in upload().stderr
    assert (s3 / "b/runs/r/logs/e.log").read_text() == "one two"


def test_a_log_upload_never_fails_and_never_outlasts_its_deadline(tmp_path: Path) -> None:
    log = tmp_path / "engine.log"
    log.write_text("x")
    args = ["s3://b/logs/", str(tmp_path / "state"), "1", "--file", f"e.log={log}"]
    # S3 unreachable: logged, exit 0.
    got = helper("run_logs", *args, FAKE_S3=str(tmp_path / "s3"), FAKE_S3_DENY_PUT="1")
    assert got.returncode == 0
    assert "log upload of e.log failed (skipped)" in got.stderr
    # S3 hanging: abandoned at the deadline, exit 0, well before the 30 s PUT would finish (a
    # deadline that only logged would let it finish, and time out here) and without its object.
    got = subprocess.run(
        [sys.executable, str(LIB / "run_logs.py"), *args],
        env={
            "PATH": os.environ["PATH"],
            "PYTHONPATH": str(FAKES),
            "FAKE_S3": str(tmp_path / "s3"),
            "FAKE_S3_SLOW_PUT": "30",
        },
        capture_output=True,
        text=True,
        timeout=10,
    )
    assert got.returncode == 0
    assert "still running after 1s; abandoning it" in got.stderr
    assert not (tmp_path / "s3/b/logs/e.log").exists()
    # Not an s3 prefix: logged, exit 0.
    got = helper("run_logs", "s3://b/no-slash", str(tmp_path / "state"), "5")
    assert got.returncode == 0 and "not an s3 prefix" in got.stderr


@pytest.mark.parametrize("uri", ["s3://b/logs/", "s3://b/missing-slash"])
def test_returning_from_log_upload_cancels_its_deadline(tmp_path: Path, uri: str) -> None:
    """A finished or handled-error upload must not exit its caller later."""
    got = subprocess.run(
        [
            sys.executable,
            "-c",
            "import run_logs, sys, time; "
            "assert run_logs.main(sys.argv[1:]) == 0; "
            "print('returned', flush=True); "
            "time.sleep(0.6); "
            "print('caller survived')",
            uri,
            str(tmp_path / "state.json"),
            "0.3",
        ],
        env={
            "PATH": os.environ["PATH"],
            "PYTHONPATH": os.pathsep.join((str(LIB), str(FAKES))),
            "FAKE_S3": str(tmp_path / "s3"),
        },
        capture_output=True,
        text=True,
        timeout=10,
    )
    assert got.returncode == 0, got.stderr
    assert got.stdout.splitlines() == ["returned", "caller survived"]
    assert "abandoning it" not in got.stderr


# ── review follow-ups: the scratch wipe, the conversion's failures, the logs' deadline ──


def test_the_scratch_wipe_removes_everything_under_the_scratch_root(tmp_path: Path) -> None:
    # An older run's lock file on the scratch root (the lock is now on the host's marker) goes too,
    # whatever it is.
    (tmp_path / ".biffix-lock").write_text("")
    (tmp_path / "run/src").mkdir(parents=True)
    (tmp_path / ".hidden").write_text("x")
    got = bash("clear_scratch", "signals", "single_job", SCRATCH=str(tmp_path))
    assert got.returncode == 0, got.stderr
    assert list(tmp_path.iterdir()) == []


def test_a_conversion_never_passes_on_a_registry_login_the_caller_had() -> None:
    sj = module("single_job")
    caller = {
        "PATH": "/bin",
        "APPTAINER_DOCKER_USERNAME": "u",
        "APPTAINER_DOCKER_PASSWORD": "p",
        "SINGULARITY_DOCKER_USERNAME": "u",
        "SINGULARITY_DOCKER_PASSWORD": "p",
        "DOCKER_USERNAME": "u",
        "DOCKER_PASSWORD": "p",
    }
    assert sj.pull_env(caller) == {"PATH": "/bin"}


@pytest.mark.parametrize("failing", [1, 2], ids=["deletion", "verification"])
def test_a_scratch_wipe_whose_find_fails_fails(tmp_path: Path, failing: int) -> None:
    # A stand-in `find` that fails on one pass (after deleting, on the deletion pass): a failed
    # check must not read as an empty scratch root.
    (tmp_path / "run").mkdir()
    stub = (
        'n=0; find() { n=$((n + 1)); command find "$@" || return; '
        f'[ "$n" -ne {failing} ] || return 1; }}; '
    )
    got = bash(
        stub + "if clear_scratch; then echo wiped; else echo refused; fi",
        "signals",
        "single_job",
        SCRATCH=str(tmp_path),
    )
    assert got.stdout.strip() == "refused", got.stderr


def test_a_stopped_run_leaves_its_scratch_at_exit(tmp_path: Path) -> None:
    (tmp_path / "run").mkdir()
    got = bash("ENGINE_STOPPED_BY=143 clear_scratch_at_exit", "single_job", SCRATCH=str(tmp_path))
    assert got.returncode == 0, got.stderr
    assert (tmp_path / "run").exists()
    got = bash("ENGINE_STOPPED_BY= clear_scratch_at_exit", "single_job", SCRATCH=str(tmp_path))
    assert got.returncode == 0, got.stderr
    assert list(tmp_path.iterdir()) == []


@pytest.mark.parametrize(
    ("content", "env", "why"),
    [
        ("[1, 2]", {}, "is not a JSON object"),
        ("{not json", {}, "could not convert the task images: "),
        ('{"a": "reg/a:1"}', {"BIFFIX_SIF_PARALLEL": "lots"}, "BIFFIX_SIF_PARALLEL must be"),
        ('{"a": "reg/a:1"}', {"BIFFIX_SIF_PARALLEL": "0"}, "BIFFIX_SIF_PARALLEL must be"),
    ],
)
def test_any_conversion_error_is_the_run_s_failure(
    tmp_path: Path, content: str, env: dict[str, str], why: str
) -> None:
    map_file = tmp_path / "images.json"
    map_file.write_text(content)
    failure = tmp_path / "failure.txt"
    args = [str(map_file), str(tmp_path / "i"), str(tmp_path / "l"), str(failure)]
    got = helper("single_job", "convert", *args, **env)
    assert got.returncode == 1, got.stderr
    assert why in failure.read_text()
    assert "Traceback" not in got.stderr


def test_failure_txt_names_what_failed_when_it_is_not_the_engine(tmp_path: Path) -> None:
    out = tmp_path / "out"
    out.write_text("could not convert the task image x\n")
    text = module("run_logs").failure_text(out, "1", "Converting the task images failed")
    assert text.startswith("Converting the task images failed (exit status 1).")
    assert "could not convert the task image x" in text


def _final_deadline(**env: str) -> str:
    # SECONDS is set here, not inherited: the shell's own count starts when it does, which a slow
    # start would advance.
    got = bash(
        'unset SECONDS; SECONDS=0; logs_command final; echo "${LOGS_CMD[3]}"',
        "logs",
        WORK="/w",
        SRC="/w/src",
        BIFFIX_PROJECT_BUCKET="b",
        BIFFIX_RUN_PREFIX="runs/r/",
        **env,
    )
    assert got.returncode == 0, got.stderr
    return got.stdout.strip()


def test_the_final_logs_upload_fits_in_what_a_cancel_leaves() -> None:
    # SECONDS starts at 0 in the test's shell: the signal came 20 s, or 29 s, ago.
    assert _final_deadline() == "8"
    assert _final_deadline(BIFFIX_LOGS_FINAL_SECS="5") == "5"
    stopped = {"ENGINE_STOPPED_BY": "143", "rc": "143"}
    # 30 s of Batch's stop timeout, less the 20 s gone, less 4 s for the replay.
    assert _final_deadline(STOPPED_AT="-20", **stopped) == "6"
    assert _final_deadline(STOPPED_AT="-29", **stopped) == "1"
    # Never more than asked for.
    assert _final_deadline(STOPPED_AT="0", **stopped) == "8"


def test_the_engine_s_output_copy_has_its_last_line_when_run_engine_returns(
    tmp_path: Path,
) -> None:
    out = tmp_path / "engine-output.log"
    got = bash(
        "install_signal_traps\n"
        "run_engine TERM sh -c 'i=0; while [ $i -lt 20000 ]; do echo line $i; i=$((i+1)); done; "
        "echo last >&2; exit 3'\n"
        'echo "rc=$rc"; tail -n 1 "$ENGINE_OUTPUT"',
        "signals",
        ENGINE_OUTPUT=str(out),
    )
    assert got.returncode == 0, got.stderr
    assert got.stdout == "rc=3\nlast\n"
    # And the job's log still has all of it.
    assert "line 19999\nlast\n" in got.stderr


def test_the_output_copy_keeps_only_its_end(tmp_path: Path) -> None:
    ct = module("capped_tee")
    out = tmp_path / "copy.log"
    data = b"".join(b"line %d\n" % i for i in range(400_000))
    got = subprocess.run(
        [sys.executable, str(LIB / "capped_tee.py"), str(out)],
        input=data,
        capture_output=True,
        timeout=60,
    )
    assert got.returncode == 0, got.stderr
    # Everything passes through; the copy is its end, within the cap.
    assert got.stdout == data
    copy = out.read_bytes()
    assert len(copy) <= ct.ROTATE_AT
    assert data.endswith(copy) and len(copy) >= ct.KEEP


def test_a_stdout_that_goes_away_stops_only_the_forwarding(tmp_path: Path) -> None:
    ct = module("capped_tee")
    out = tmp_path / "copy.log"
    data = b"".join(b"line %d\n" % i for i in range(200_000))
    proc = subprocess.Popen(
        [sys.executable, str(LIB / "capped_tee.py"), str(out)],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    assert proc.stdin is not None and proc.stdout is not None
    try:
        # A probe the child must forward before the reader goes away, so the reader leaves only
        # once the forwarding is under way.
        proc.stdin.write(data[: ct.CHUNK])
        proc.stdin.flush()
        assert proc.stdout.read(1) == data[:1]
        proc.stdout.close()  # the reader goes away
        proc.stdout = None  # so communicate() reads only stderr
        proc.communicate(input=data[ct.CHUNK :], timeout=60)
        assert proc.returncode == 0
    finally:
        if proc.poll() is None:
            proc.kill()
            proc.wait()
    # The copy still holds the end of everything.
    assert data.endswith(out.read_bytes()) and len(out.read_bytes()) >= ct.KEEP


def test_a_copy_that_cannot_be_written_stops_only_the_copy(tmp_path: Path) -> None:
    data = b"".join(b"line %d\n" % i for i in range(10_000))
    got = subprocess.run(
        [sys.executable, str(LIB / "capped_tee.py"), str(tmp_path / "missing/copy.log")],
        input=data,
        capture_output=True,
        timeout=60,
    )
    assert got.returncode == 0, got.stderr
    assert got.stdout == data
    assert b"no copy of the engine's output kept" in got.stderr


def test_a_full_volume_ends_the_copy_without_an_error(tmp_path: Path) -> None:
    ct = module("capped_tee")

    class Full(io.BytesIO):
        """A buffered file on a full volume: every write, flush and close fails."""

        def write(self, _data: object) -> int:
            raise OSError(28, "No space left on device")

        def close(self) -> None:
            raise OSError(28, "No space left on device")

    copy = ct.CappedFile(str(tmp_path / "copy.log"), ct.KEEP, ct.ROTATE_AT)
    copy.out = Full()
    copy.write(b"x")  # no error: the copy is given up
    assert copy.out is None
    copy.write(b"y")
    copy.close()
    copy.out = Full()
    copy.close()  # nor here
    assert copy.out is None


# ── s3_uri.py ──


def test_an_s3_uri_is_split_into_its_bucket_and_key() -> None:
    s3 = module("s3_uri")
    assert s3.split("s3://b/k/x") == ("b", "k/x")
    assert s3.split("s3://b") == ("b", "")
    assert s3.split_object("s3://b/k") == ("b", "k")
    assert s3.split_prefix("s3://b/p/") == ("b", "p/")
    for bad, fn, why in [
        ("https://b/k", "split", "not an s3 uri"),
        ("s3:///k", "split", "not an s3 uri"),
        ("s3://b", "split_object", "not an s3 object uri"),
        ("s3://b/", "split_object", "not an s3 object uri"),
        ("b/k", "split_object", "not an s3 object uri"),
        ("s3://b/p", "split_prefix", "not an s3 prefix"),
        ("s3://b", "split_prefix", "not an s3 prefix"),
    ]:
        with pytest.raises(ValueError, match=why):
            getattr(s3, fn)(bad)


def test_failure_txt_and_the_named_files_go_before_the_failed_tasks_scan(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    rl = module("run_logs")
    log = tmp_path / "engine.log"
    log.write_text("log")
    out = tmp_path / "out"
    out.write_text("boom\n")
    for i in range(3):
        _task(tmp_path / "work", f"a{i:05d}", "1", 100 + i)
    sent: list[str] = []
    scanned_after: list[int] = []

    class S3:
        def put_object(self, **kwargs: object) -> None:
            sent.append(str(kwargs["Key"]))

    class Recorder(rl.Uploader):  # type: ignore[name-defined]
        def __init__(self, uri: str, state_file: str, max_bytes: int) -> None:
            self.uri, self.state_file, self.max_bytes = uri, state_file, max_bytes
            self.bucket, self.prefix = "b", "p/"
            self.state: dict[str, str] = {}
            self.s3, self.sent = S3(), 0

    real_failed_tasks = rl.failed_tasks

    def failed_tasks(work: Path, stop_at: float | None = None) -> object:
        scanned_after.append(len(sent))
        assert stop_at is not None
        return real_failed_tasks(work, stop_at)

    monkeypatch.setattr(rl, "Uploader", Recorder)
    monkeypatch.setattr(rl, "failed_tasks", failed_tasks)
    rl.main(
        ["s3://b/p/", str(tmp_path / "state"), "30", "--file", f"engine.log={log}",
         "--failed-tasks", str(tmp_path / "work"), "--failure", str(out), "1"]
    )  # fmt: skip
    assert sent[:2] == ["p/failure.txt", "p/engine.log"]
    assert scanned_after == [2]
    assert len(sent) == 2 + 3


def test_the_failed_tasks_scan_stops_at_its_time_limit(tmp_path: Path) -> None:
    rl = module("run_logs")
    _task(tmp_path, "aa1111", "1", 100)
    assert rl.failed_tasks(tmp_path, stop_at=0.0) == []
    assert len(rl.failed_tasks(tmp_path, stop_at=None)) == 1


def test_a_log_sent_only_in_part_is_sent_again_by_a_larger_cap(tmp_path: Path) -> None:
    log, state, s3 = tmp_path / "engine.log", tmp_path / "state.json", tmp_path / "s3"
    log.write_text("0123456789")

    def upload(*extra: str) -> subprocess.CompletedProcess[str]:
        return helper(
            "run_logs", "s3://b/l/", str(state), "30", "--file", f"e.log={log}", *extra,
            FAKE_S3=str(s3),
        )  # fmt: skip

    assert "uploaded 1" in upload("--max-bytes", "4").stderr
    assert (s3 / "b/l/e.log").read_text() == "6789"
    assert "uploaded" not in upload("--max-bytes", "4").stderr
    # The final upload's cap is larger: the whole log goes, though it has not changed.
    assert "uploaded 1" in upload().stderr
    assert (s3 / "b/l/e.log").read_text() == "0123456789"
    assert "uploaded" not in upload().stderr


# ── single_job.sh: the scratch root's marker and lock, a setup step's failure ──


def _prepare_scratch(tmp_path: Path, **env: str) -> subprocess.CompletedProcess[str]:
    marker = tmp_path / "host/instance-store"
    scratch = tmp_path / "scratch"
    return bash(
        "install_signal_traps; EXECUTION=single_job; prepare_scratch",
        "signals",
        "single_job",
        **{
            "SCRATCH": str(scratch),
            "WORK": str(scratch / "run"),
            "BIFFIX_INSTANCE_STORE_MARKER": str(marker),
            "SJ_CPUS": "8",
            "SJ_MEMORY_MIB": "65536",
        }
        | env,
    )


def test_a_scratch_root_without_the_host_s_marker_is_refused(tmp_path: Path) -> None:
    (tmp_path / "scratch").mkdir()
    (tmp_path / "scratch/host-file").write_text("the host's own")
    # A marker on the scratch root itself, which a run could write, does not count.
    (tmp_path / "scratch/instance-store").write_text("")
    got = _prepare_scratch(tmp_path)
    assert got.returncode == 78, got.stderr
    assert f"(no marker {tmp_path}/host/instance-store)" in got.stderr
    assert (tmp_path / "scratch/host-file").exists()


@pytest.fixture
def scratch_host(tmp_path: Path) -> Iterator[Path]:
    """A scratch root, and the host's instance-store marker read-only in a read-only directory, as
    the single-job job definition mounts it. The marker's directory is made writable again
    afterwards, so the test's directory can be removed."""
    (tmp_path / "scratch").mkdir()
    marker = tmp_path / "host/instance-store"
    marker.parent.mkdir()
    marker.write_text("")
    marker.chmod(0o444)
    marker.parent.chmod(0o555)
    yield tmp_path / "scratch"
    marker.parent.chmod(0o755)


def test_a_scratch_root_another_run_holds_is_refused_and_left_alone(scratch_host: Path) -> None:
    import fcntl

    scratch = scratch_host
    (scratch / "run").mkdir()
    (scratch / "run/theirs.txt").write_text("another run's work")
    # Something not a file where an older run kept its lock: nothing under the scratch root is
    # removed before the lock on the host's marker is held, so it is still there.
    (scratch / ".biffix-lock/x").mkdir(parents=True)
    # The other run's lock: on the host's marker, opened read-only.
    with open(scratch.parent / "host/instance-store") as held:
        fcntl.flock(held.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
        got = _prepare_scratch(scratch.parent)
    assert got.returncode == 78, got.stderr
    assert "another run holds the scratch root" in got.stderr
    assert (scratch / "run/theirs.txt").read_text() == "another run's work"
    assert (scratch / ".biffix-lock/x").is_dir()


def test_the_lock_on_a_read_only_marker_is_held_for_the_life_of_the_job(
    scratch_host: Path,
) -> None:
    import fcntl

    scratch = scratch_host
    marker = scratch.parent / "host/instance-store"
    # The marker stays open for the flock (a temporary closes once fileno() returns, and flock on
    # it fails with EBADF). Only a lock someone else holds prints "held"; any other error exits
    # non-zero, which the return-code assertion below catches.
    probe = (
        'python3 -c "import fcntl, sys\n'
        "marker = open(sys.argv[1])\n"
        "try:\n"
        "    fcntl.flock(marker.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)\n"
        "except BlockingIOError:\n"
        "    print('held')\n"
        "else:\n"
        "    print('free')\n"
        '" "$BIFFIX_INSTANCE_STORE_MARKER"'
    )
    got = bash(
        f"install_signal_traps; EXECUTION=single_job; prepare_scratch; {probe}",
        "signals",
        "single_job",
        SCRATCH=str(scratch),
        WORK=str(scratch / "run"),
        BIFFIX_INSTANCE_STORE_MARKER=str(marker),
        SJ_CPUS="8",
        SJ_MEMORY_MIB="65536",
    )
    assert got.returncode == 0, got.stderr
    # Held after prepare_scratch returns (on a descriptor the shell keeps), against any other
    # opener of the marker...
    assert got.stdout.strip() == "held"
    # ...and released when the job ends.
    with open(marker) as f:
        fcntl.flock(f.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)


def test_a_second_coordinator_never_wipes_the_first_s_scratch_root(
    scratch_host: Path, tmp_path: Path
) -> None:
    """Two coordinators on one host, one paused at its first `rm` (its checks done, its wipe not
    begun) until the other has had its chance at the scratch root: one of them gets it, and the
    other never wipes that run. Under a lock file on the scratch root itself, the paused one went on
    to remove the other's live lock file (a stale entry it had seen there), lock a new one, and wipe
    the other's run."""
    scratch = scratch_host
    (scratch / ".biffix-lock/x").mkdir(parents=True)
    shim = tmp_path / "shim"
    shim.mkdir()
    paused, go, once = tmp_path / "paused", tmp_path / "go", tmp_path / "once"
    real_rm = subprocess.run(
        ["sh", "-c", "command -v rm"], capture_output=True, text=True, check=True
    ).stdout.strip()
    (shim / "rm").write_text(
        "#!/bin/sh\n"
        f'if [ ! -e "{once}" ]; then : > "{once}"; : > "{paused}"\n'
        f'  while [ ! -e "{go}" ]; do sleep 0.05; done\nfi\n'
        f'exec {real_rm} "$@"\n'
    )
    (shim / "rm").chmod(0o755)
    env = {
        "LIB_DIR": str(LIB),
        "PYTHONPATH": str(FAKES),
        "SCRATCH": str(scratch),
        "WORK": str(scratch / "run"),
        "BIFFIX_INSTANCE_STORE_MARKER": str(tmp_path / "host/instance-store"),
        "SJ_CPUS": "8",
        "SJ_MEMORY_MIB": "65536",
    }
    sources = "".join(f'. "$LIB_DIR/{lib}.sh"\n' for lib in ("common", "signals", "single_job"))
    prologue = f"set -euo pipefail\n{sources}install_signal_traps; EXECUTION=single_job\n"
    ready = tmp_path / "other-ready"
    paused_one = subprocess.Popen(
        ["bash", "-c", f"{prologue}prepare_scratch"],
        env=env | {"PATH": f"{shim}:{os.environ['PATH']}"},
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        start_new_session=True,
    )
    other: subprocess.Popen[str] | None = None
    try:
        deadline = time.monotonic() + 20
        while not paused.exists():
            assert paused_one.poll() is None, stop_group(paused_one)
            assert time.monotonic() < deadline, "the paused coordinator never reached an rm"
            time.sleep(0.05)
        other = subprocess.Popen(
            [
                "bash",
                "-c",
                f'{prologue}prepare_scratch\necho mine > "$WORK/mine.txt"\n: > "{ready}"\nsleep 60',
            ],
            env=env | {"PATH": os.environ["PATH"]},
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            start_new_session=True,
        )
        deadline = time.monotonic() + 20
        while not ready.exists() and other.poll() is None:
            assert time.monotonic() < deadline, "the other coordinator neither ran nor stopped"
            time.sleep(0.05)
        go.write_text("")
        _, paused_err = paused_one.communicate(timeout=30)
        if ready.exists():
            # The other got the scratch root: the paused one must be refused, its run left alone.
            assert paused_one.returncode == 78, paused_err
            assert (scratch / "run/mine.txt").read_text() == "mine\n"
        else:
            # The paused one held it already: the other was refused.
            _, other_err = other.communicate(timeout=30)
            assert other.returncode == 78, other_err
            assert "another run holds the scratch root" in other_err
            assert paused_one.returncode == 0, paused_err
    finally:
        go.write_text("")
        # Each coordinator's whole tree: the other's `sleep 60` (or the paused one's rm shim)
        # would otherwise hold its pipes, and the scratch lock, after its bash is gone.
        for proc in (paused_one, other):
            if proc is not None:
                stop_group(proc)


def test_a_failed_setup_step_ends_the_run_with_its_error_as_the_failure(tmp_path: Path) -> None:
    got = bash(
        'upload_run_logs() { printf "%s|%s|%s" "$FAILURE_LABEL" "$(cat "$FAILURE_FILE")" "$rc"; }\n'
        "run_setup_step 'Linking failed' sh -c 'echo why it failed >&2; exit 3'\n"
        "echo not reached",
        "single_job",
        WORK=str(tmp_path),
    )
    assert got.returncode == 1, got.stderr
    assert got.stdout == "Linking failed|why it failed|1"
    assert "why it failed" in got.stderr
    # A step that succeeds goes on.
    got = bash("run_setup_step 'X failed' true; echo went on", "single_job", WORK=str(tmp_path))
    assert got.returncode == 0 and got.stdout == "went on\n"


@pytest.mark.parametrize("left", ["link", "directory", "content"])
def test_an_older_run_s_lock_file_on_the_scratch_root_is_cleared(
    scratch_host: Path, tmp_path: Path, left: str
) -> None:
    scratch = scratch_host
    lock = scratch / ".biffix-lock"
    if left == "link":
        (tmp_path / "elsewhere").write_text("not the run's")
        lock.symlink_to(tmp_path / "elsewhere")
    elif left == "directory":
        (lock / "x").mkdir(parents=True)
    else:
        lock.write_text("a note for the next run")
    got = _prepare_scratch(tmp_path)
    assert got.returncode == 0, got.stderr
    assert "another run holds" not in got.stderr, got.stderr
    assert not lock.exists() and not lock.is_symlink()
    if left == "link":
        assert (tmp_path / "elsewhere").read_text() == "not the run's"
