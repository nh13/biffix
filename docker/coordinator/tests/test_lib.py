"""The entrypoint's libraries (lib/*.sh) and helpers (lib/*.py), one function at a time: a shell
function runs in `bash` with only its library (and common.sh) sourced, a helper as the entrypoint
runs it (`python3 <lib>/<helper>.py …`) over the stand-in boto3 (fakes/py). test_entrypoint.py
drives the whole script; these pin down the edges it does not reach."""

from __future__ import annotations

import importlib.util
import io
import json
import os
import subprocess
import sys
import tarfile
import urllib.error
from pathlib import Path
from types import ModuleType
from typing import ClassVar

import pytest

LIB = Path(__file__).resolve().parents[1] / "lib"
FAKES = Path(__file__).parent / "fakes" / "py"


def bash(code: str, *libs: str, **env: str) -> subprocess.CompletedProcess[str]:
    """`code` in bash under the entrypoint's options, with common.sh and `libs` sourced."""
    sources = "".join(f'. "$LIB_DIR/{lib}.sh"\n' for lib in ("common", *libs))
    return subprocess.run(
        ["bash", "-c", f"set -euo pipefail\n{sources}{code}"],
        env={"PATH": os.environ["PATH"], "LIB_DIR": str(LIB), "PYTHONPATH": str(FAKES)} | env,
        capture_output=True,
        text=True,
        timeout=60,
    )


def helper(name: str, *args: str, stdin: str = "", **env: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [sys.executable, str(LIB / f"{name}.py"), *args],
        input=stdin,
        env={"PATH": os.environ["PATH"], "PYTHONPATH": str(FAKES)} | env,
        capture_output=True,
        text=True,
        timeout=60,
    )


def module(name: str) -> ModuleType:
    spec = importlib.util.spec_from_file_location(name, LIB / f"{name}.py")
    assert spec is not None and spec.loader is not None, name
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


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
