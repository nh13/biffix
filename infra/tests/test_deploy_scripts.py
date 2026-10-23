"""deploy.sh, teardown.sh and their config reader, run for real against stand-ins: an `aws` that
answers from a rule table and records every call, a `cdk` that records its arguments and prints a
canned log, and a `uv` that runs this interpreter (so the scripts read the config through the real
biffix_infra.config). Nothing here reaches AWS."""

from __future__ import annotations

import hashlib
import json
import re
import shutil
import subprocess
import sys
from pathlib import Path
from typing import Required, TypedDict

import pytest

from biffix_infra import script_config
from biffix_infra.config import DeployConfig
from biffix_infra.image_retention import protect_tag
from biffix_infra.paths import COORDINATOR_DIR, INFRA_DIR
from biffix_infra.version import biffix_version
from tests.deploy_config_file import FIXTURE
from tests.helpers import executable
from tests.release_fixture import DIGEST, IMAGE, publish
from tests.release_fixture import TAG as RELEASE

BASH32 = Path("/bin/bash")

ACCT = "111122223333"
TENANT = "acme"
STACKS = [
    f"biffix-{TENANT}-{part}"
    for part in (
        "network",
        "data",
        "identity",
        "compute",
        "build",
        "project-demo",
        "persistence",
        "ingest",
        "api",
        "edge",
    )
]


def _config_text(*, tenant: str = f'"{TENANT}"   # quoted', extra: str = "") -> str:
    text = FIXTURE.read_text()
    text = re.sub(r"(?m)^tenant: .*$", f"tenant: {tenant}", text)
    text = re.sub(
        r"(?m)^coordinator_image_tag: .*$", 'coordinator_image_tag: "abc123"  # pinned', text
    )
    return text + extra


# ── The config reader ─────────────────────────────────────────────────────────────


def test_script_config_reads_values_as_the_app_does(tmp_path: Path) -> None:
    """Quoted values and trailing comments come back as YAML reads them, not as the raw text."""
    cfg = tmp_path / "c.yaml"
    cfg.write_text(_config_text(extra="account: 012345678901\n"))
    lines = script_config.values(
        DeployConfig.load(cfg), ["tenant", "coordinator_image_tag", "account", "app_domain"]
    )
    # An unquoted id with an 8 or 9 in it is a string to YAML, leading zero and all.
    assert lines == [TENANT, "abc123", "012345678901", ""]


@pytest.mark.parametrize(
    ("account", "loaded"),
    [
        # A 12-digit number with no leading zero is the id itself.
        ("123456789012", "123456789012"),
        # A leading zero and only octal digits makes YAML read an octal number, another id
        # entirely; any number short of 12 digits has lost its leading zeros. Neither is guessed.
        ("012345670123", None),
        ("000000000001", None),
    ],
)
def test_an_unquoted_account_is_read_only_when_yaml_kept_it(
    tmp_path: Path, account: str, loaded: str | None
) -> None:
    cfg = tmp_path / "c.yaml"
    cfg.write_text(_config_text(extra=f"account: {account}\n"))
    if loaded:
        assert DeployConfig.load(cfg).account == loaded
    else:
        with pytest.raises(ValueError, match="quote it"):
            DeployConfig.load(cfg)


@pytest.mark.parametrize("unset", ["", "coordinator_image_tag: null\n"])
def test_script_config_defaults_the_image_tag_like_the_app(tmp_path: Path, unset: str) -> None:
    """Left out or null, the tag is this checkout's release tag (the one --release imports)."""
    cfg = tmp_path / "c.yaml"
    cfg.write_text(re.sub(r"(?m)^coordinator_image_tag: .*\n", unset, _config_text()))
    tag = f"v{biffix_version()}"
    assert script_config.values(DeployConfig.load(cfg), ["coordinator_image_tag"]) == [tag]


def test_script_config_rejects_a_malformed_account(tmp_path: Path) -> None:
    cfg = tmp_path / "c.yaml"
    cfg.write_text(_config_text(extra='account: "12345"\n'))
    with pytest.raises(ValueError, match="12-digit"):
        DeployConfig.load(cfg)


def test_script_config_lists_exact_stacks_and_imported_buckets(tmp_path: Path) -> None:
    cfg = tmp_path / "c.yaml"
    cfg.write_text(
        _config_text().replace("  - id: demo\n", "  - id: demo\n    bucket_name: shared-bkt\n", 1)
    )
    stacks, imported = script_config.values(
        DeployConfig.load(cfg), ["stack_names", "imported_buckets"]
    )
    assert stacks.split() == STACKS
    assert imported == "shared-bkt"


def test_script_config_cli_fails_on_unknown_key(tmp_path: Path) -> None:
    cfg = tmp_path / "c.yaml"
    cfg.write_text(_config_text())
    assert script_config.main([str(cfg), "tenant", "nope"]) == 1


# ── Stand-ins ────────────────────────────────────────────────────────────────────

_AWS = r'''#!{python}
"""Answer from the rule table in $AWS_RULES (the first rule whose `match` tokens are all in the
call, whose `env` the call's environment has, and whose `times` is not used up); record every call
in $AWS_LOG, and in $CALL_LOG with the cdk calls."""
import json, os, sys
args = sys.argv[1:]
line = " ".join(args)
with open(os.environ["AWS_LOG"], "a") as log:
    log.write(line + "\n")
with open(os.environ["CALL_LOG"], "a") as log:
    log.write("aws " + line + "\n")
state_path = os.environ["AWS_RULES"] + ".state"
state = json.load(open(state_path)) if os.path.exists(state_path) else {{}}
for i, rule in enumerate(json.load(open(os.environ["AWS_RULES"]))):
    if not all(tok in line for tok in rule["match"]):
        continue
    if any(os.environ.get(k) != v for k, v in rule.get("env", {{}}).items()):
        continue
    used = state.get(str(i), 0)
    if "times" in rule and used >= rule["times"]:
        continue
    state[str(i)] = used + 1
    json.dump(state, open(state_path, "w"))
    sys.stdout.write(rule.get("out", ""))
    sys.stderr.write(rule.get("err", ""))
    sys.exit(rule.get("rc", 0))
sys.exit(0)
'''

_CDK = """#!/bin/bash
printf '%s\\n' "$*" >> "$CDK_LOG"
printf 'cdk %s\\n' "$*" >> "$CALL_LOG"
case "$1" in
  deploy)
    printf '%s' "${CDK_OUT:-}"
    [ -z "${CDK_OUT_FILE:-}" ] || cat "$CDK_OUT_FILE"
    exit "${CDK_RC:-0}" ;;
  # A stack CloudFormation cannot delete (CDK_UNDELETABLE) fails a destroy that includes it.
  destroy)
    if [ -n "${CDK_UNDELETABLE:-}" ]; then
      case " $* " in *" --all "*|*" $CDK_UNDELETABLE "*) exit 1 ;; esac
    fi ;;
esac
exit 0
"""

_UV = """#!/bin/bash
# `uv run [--quiet] python ...` -> this interpreter, with the real biffix_infra importable.
[ "$1" = run ] || exit 2
shift
[ "$1" = --quiet ] && shift
[ "$1" = python ] || exit 2
shift
PYTHONPATH="$REAL_INFRA" exec "$PYTHON" "$@"
"""


# `curl -fsSL <awscli url> -o <file>`: the "zip" is its own name, so its sha256 is known.
_CURL = """#!/bin/bash
printf '%s\\n' "$*" >> "$CURL_LOG"
url="" out=""
while [ $# -gt 0 ]; do
  case "$1" in -o) out="$2"; shift 2 ;; https://*) url="$1"; shift ;; *) shift ;; esac
done
case "$url" in
  https://awscli.amazonaws.com/awscli-exe-linux-*.zip) printf 'zip:%s' "${url##*/}" > "$out" ;;
  *) exit 22 ;;
esac
"""


class Rule(TypedDict, total=False):
    """One answer of the stand-in aws (`_AWS`): to the first call holding every `match` token whose
    environment has `env`, while used fewer than `times` times: `out`, `err` and exit code `rc`."""

    match: Required[list[str]]
    env: dict[str, str]
    times: int
    out: str
    err: str
    rc: int


class Harness:
    """A copy of infra/'s scripts in a scratch repo, with stand-ins on PATH."""

    def __init__(self, tmp_path: Path, rules: list[Rule]):
        self.root = tmp_path / "repo"
        self.infra = self.root / "infra"
        self.infra.mkdir(parents=True)
        for name in ("deploy.sh", "teardown.sh", "script_lib.sh"):
            shutil.copy2(INFRA_DIR / name, self.infra / name)
        executable(self.infra / "node_modules/.bin/cdk", _CDK)
        self.stubs = stubs = tmp_path / "stubs"
        executable(stubs / "aws", _AWS.format(python=sys.executable))
        executable(stubs / "uv", _UV)
        # deploy.sh polls ACM with `sleep`; the stand-ins answer at once, so the waits need not.
        executable(stubs / "sleep", "#!/bin/sh\nexit 0\n")
        self.config = self.infra / "cfg.yaml"
        self.config.write_text(_config_text())
        self.aws_log = tmp_path / "aws.log"
        self.cdk_log = tmp_path / "cdk.log"
        self.call_log = tmp_path / "calls.log"
        self.curl_log = tmp_path / "curl.log"
        for log in (self.aws_log, self.cdk_log, self.call_log, self.curl_log):
            log.touch()
        self.rules = tmp_path / "rules.json"
        self.rules.write_text(json.dumps(rules))
        self.env = {
            "PATH": f"{stubs}:/usr/bin:/bin:/usr/sbin:/sbin",
            "HOME": str(tmp_path),
            "TMPDIR": str(tmp_path),
            "AWS_LOG": str(self.aws_log),
            "AWS_RULES": str(self.rules),
            "CDK_LOG": str(self.cdk_log),
            "CALL_LOG": str(self.call_log),
            "CURL_LOG": str(self.curl_log),
            "PYTHON": sys.executable,
            "REAL_INFRA": str(INFRA_DIR),
        }

    def ui_build(self, *, real: bool) -> None:
        build = self.root / "ui/build"
        build.mkdir(parents=True, exist_ok=True)
        (build / "index.html").write_text("<!doctype html>\n")
        if real:
            (build / "_app").mkdir(exist_ok=True)

    def run(
        self, script: str, *args: str, bash: str = "bash", input: str | None = None, **env: str
    ) -> subprocess.CompletedProcess[str]:
        stdin = {"input": input} if input is not None else {"stdin": subprocess.DEVNULL}
        return subprocess.run(
            [bash, str(self.infra / script), *args],
            env={**self.env, **env},
            cwd=self.root,
            capture_output=True,
            text=True,
            timeout=120,
            **stdin,
        )

    def add_rules(self, *rules: Rule, first: bool = False) -> None:
        table = json.loads(self.rules.read_text())
        table = [*rules, *table] if first else [*table, *rules]
        self.rules.write_text(json.dumps(table))

    def aws_calls(self) -> list[str]:
        return self.aws_log.read_text().splitlines()

    def cdk_calls(self) -> list[str]:
        return self.cdk_log.read_text().splitlines()

    def calls(self) -> list[str]:
        """Every aws and cdk call, in the order they were made."""
        return self.call_log.read_text().splitlines()


def _identity(account: str = ACCT) -> list[Rule]:
    return [
        {"match": ["sts get-caller-identity", "Account"], "out": f"{account}\n"},
        {"match": ["sts get-caller-identity", "Arn"], "out": f"arn:aws:iam::{account}:user/me\n"},
        {
            "match": ["organizations describe-organization"],
            "rc": 254,
            "err": "An error occurred (AWSOrganizationsNotInUseException)\n",
        },
    ]


def _data_stack_resources() -> list[Rule]:
    return [
        {
            "match": ["list-stack-resources", f"biffix-{TENANT}-data", "ReferenceBucket"],
            "out": "acme-ref-bucket\n",
        },
        {
            "match": ["list-stack-resources", f"biffix-{TENANT}-data", "CoordinatorRepo"],
            "out": "acme-coord-repo\n",
        },
    ]


def _ok_lines(stacks: list[str]) -> str:
    return "".join(f"\n \u2705  {s}\n" for s in stacks)


# ── deploy.sh: the account guard ───────────────────────────────────────────────────


@pytest.fixture
def deploy(tmp_path: Path) -> Harness:
    h = Harness(tmp_path, _identity() + _data_stack_resources())
    h.ui_build(real=True)
    return h


DEPLOY_FLAGS = ("--skip-ui", "--skip-lambdas", "--no-verify", "-c", "cfg.yaml")


def test_deploy_requires_a_target_account(deploy: Harness) -> None:
    r = deploy.run("deploy.sh", *DEPLOY_FLAGS)
    assert r.returncode != 0
    assert "no target account" in r.stderr
    assert deploy.cdk_calls() == []


@pytest.mark.parametrize(
    ("script", "flags"),
    [("deploy.sh", ("--skip-ui", "--skip-lambdas", "--no-verify")), ("teardown.sh", ("--yes",))],
)
def test_the_scripts_have_no_default_config(
    deploy: Harness, script: str, flags: tuple[str, ...]
) -> None:
    """Even with a config at the old default path, a deploy or teardown that names none refuses
    before touching AWS: it must never act on a tenant the command line did not name."""
    (deploy.infra / "biffix-deploy.yaml").write_text(_config_text(extra=f'account: "{ACCT}"\n'))
    r = deploy.run(script, *flags, "--account", ACCT)
    assert r.returncode != 0
    assert "no deploy config: pass --config <file>" in r.stderr
    assert deploy.calls() == []


def test_deploy_takes_the_account_from_config_and_checks_the_flag_agrees(deploy: Harness) -> None:
    deploy.config.write_text(_config_text(extra=f'account: "{ACCT}"\n'))
    r = deploy.run("deploy.sh", *DEPLOY_FLAGS, "--account", "999999999999")
    assert r.returncode != 0
    assert "disagrees" in r.stderr
    assert deploy.cdk_calls() == []


def test_deploy_refuses_a_different_caller_account(deploy: Harness) -> None:
    r = deploy.run("deploy.sh", *DEPLOY_FLAGS, "--account", "999999999999")
    assert r.returncode != 0
    assert "WRONG ACCOUNT" in r.stderr
    assert deploy.cdk_calls() == []


def test_deploy_refuses_the_organization_management_account(tmp_path: Path) -> None:
    rules: list[Rule] = [
        {"match": ["organizations describe-organization"], "out": f"{ACCT}\n"},
        *_identity(),
    ]
    h = Harness(tmp_path, rules)
    h.ui_build(real=True)
    r = h.run("deploy.sh", *DEPLOY_FLAGS, "--account", ACCT)
    assert r.returncode != 0
    assert "management account" in r.stderr
    assert h.cdk_calls() == []


ASSUMED = {"AWS_PROFILE": "biffix-assume-role"}


def _assumed_identity(session: str = "biffix-deploy") -> list[Rule]:
    """The target account answers only through the profile that assumes the role."""
    return [
        {
            "match": ["sts get-caller-identity", "Arn"],
            "env": ASSUMED,
            "out": f"arn:aws:sts::{ACCT}:assumed-role/Deployer/{session}\n",
        },
        {"match": ["sts get-caller-identity", "Account"], "env": ASSUMED, "out": f"{ACCT}\n"},
        {"match": ["sts get-caller-identity", "Account"], "out": "999999999999\n"},
        *_identity(),
        *_data_stack_resources(),
    ]


def test_deploy_guards_the_account_the_assumed_role_is_in(tmp_path: Path) -> None:
    role = "arn:aws:iam::111122223333:role/Deployer"
    h = Harness(tmp_path / "assumed", _assumed_identity())
    h.ui_build(real=True)
    r = h.run("deploy.sh", *DEPLOY_FLAGS, "--account", ACCT, "--assume-role", role)
    assert r.returncode == 0, r.stderr
    # The SDKs assume (and renew) the role through the profile; no one-off session is exported.
    assert not any(c.startswith("sts assume-role") for c in h.aws_calls())
    # The private config holding the profile is gone once the deploy exits.
    assert list((tmp_path / "assumed").glob("biffix-aws-config.*")) == []
    # Without the role, the caller is elsewhere: refused before any change.
    h = Harness(tmp_path / "caller", _assumed_identity())
    h.ui_build(real=True)
    r = h.run("deploy.sh", *DEPLOY_FLAGS, "--account", ACCT)
    assert r.returncode != 0
    assert "WRONG ACCOUNT: caller is 999999999999" in r.stderr
    assert h.cdk_calls() == []


# Sources script_lib.sh, assumes the role, and prints the config it wrote and the AWS_* environment
# it left (the EXIT trap a real script sets is not set, so the config outlives the call).
_ASSUME_DRIVER = """#!/bin/bash
set -euo pipefail
log() { :; }; warn() { :; }; die() { printf '%s\\n' "$*" >&2; exit 1; }
INFRA_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$INFRA_DIR/script_lib.sh"
assume_role "$1" biffix-deploy
stat -f %Lp "$ROLE_CONFIG" 2>/dev/null || stat -c %a "$ROLE_CONFIG"
cat "$ROLE_CONFIG"
printf -- '---\\n'
env | grep '^AWS_' | sort
"""

_ROLE = "arn:aws:iam::111122223333:role/ops/Deployer"


def _assume(tmp_path: Path, rules: list[Rule], **env: str) -> tuple[str, list[str], list[str]]:
    """Run the driver; return the config's mode, its lines, and the AWS_* environment after."""
    h = Harness(tmp_path, rules)
    executable(h.infra / "assume.sh", _ASSUME_DRIVER)
    aws_dir = tmp_path / ".aws"
    aws_dir.mkdir()
    (aws_dir / "config").write_text("[profile ops]\nregion = us-west-2\n")
    r = h.run("assume.sh", _ROLE, **env)
    assert r.returncode == 0, r.stderr
    mode, rest = r.stdout.split("\n", 1)
    config, environ = rest.split("---\n")
    return mode, config.splitlines(), environ.splitlines()


@pytest.mark.parametrize(
    ("env", "profiles", "source"),
    [
        # A named profile is the role's source, resolved from the caller's config copied in.
        ({"AWS_PROFILE": "ops"}, "", "source_profile = ops"),
        ({"AWS_DEFAULT_PROFILE": "ops"}, "", "source_profile = ops"),
        # No profile named: the default one, when it is configured...
        ({}, "default\nops\n", "source_profile = default"),
        # ...otherwise the container's or the instance's role.
        (
            {"AWS_CONTAINER_CREDENTIALS_RELATIVE_URI": "/v2/creds"},
            "ops\n",
            "credential_source = EcsContainer",
        ),
        ({}, "ops\n", "credential_source = Ec2InstanceMetadata"),
    ],
)
def test_assume_role_sources_the_role_from_the_caller_s_profile(
    tmp_path: Path, env: dict[str, str], profiles: str, source: str
) -> None:
    rules: list[Rule] = [
        {"match": ["configure list-profiles"], "out": profiles},
        *_assumed_identity(),
    ]
    mode, config, environ = _assume(tmp_path, rules, **env)
    assert mode == "600"
    # The caller's own config first, then the profile that assumes (and renews) the role.
    assert config[:2] == ["[profile ops]", "region = us-west-2"]
    assert config[-4:] == [
        "[profile biffix-assume-role]",
        f"role_arn = {_ROLE}",
        source,
        "role_session_name = biffix-deploy",
    ]
    assert not any("duration_seconds" in line for line in config)
    assert "AWS_PROFILE=biffix-assume-role" in environ
    assert any(e.startswith("AWS_CONFIG_FILE=") and "biffix-aws-config." in e for e in environ)
    assert not any(e.startswith("AWS_DEFAULT_PROFILE=") for e in environ)


def test_assume_role_moves_exported_keys_into_a_source_profile(tmp_path: Path) -> None:
    """Exported keys would outrank the profile for the aws CLI, so they leave the environment for a
    source profile in the private config (even when a profile is named too: the CLI's order)."""
    keys = {
        "AWS_ACCESS_KEY_ID": "AKIA-CALLER",
        "AWS_SECRET_ACCESS_KEY": "caller-secret",
        "AWS_SESSION_TOKEN": "caller-token",
        "AWS_PROFILE": "ops",
    }
    _, config, environ = _assume(tmp_path, _assumed_identity(), **keys)
    assert config[-9:] == [
        "[profile biffix-assume-role-source]",
        "aws_access_key_id = AKIA-CALLER",
        "aws_secret_access_key = caller-secret",
        "aws_session_token = caller-token",
        "",
        "[profile biffix-assume-role]",
        f"role_arn = {_ROLE}",
        "source_profile = biffix-assume-role-source",
        "role_session_name = biffix-deploy",
    ]
    assert not any(
        e.split("=", 1)[0] in ("AWS_ACCESS_KEY_ID", "AWS_SECRET_ACCESS_KEY", "AWS_SESSION_TOKEN")
        for e in environ
    )
    assert "AWS_PROFILE=biffix-assume-role" in environ


def test_assume_role_fails_unless_the_calls_act_as_the_role(tmp_path: Path) -> None:
    h = Harness(tmp_path, _identity())
    executable(h.infra / "assume.sh", _ASSUME_DRIVER)
    r = h.run("assume.sh", _ROLE, AWS_PROFILE="ops")
    assert r.returncode != 0
    assert f"not role {_ROLE}" in r.stderr


def test_deploy_warns_when_the_management_account_check_cannot_run(tmp_path: Path) -> None:
    rules: list[Rule] = [
        {
            "match": ["organizations describe-organization"],
            "rc": 254,
            "err": "An error occurred (AccessDeniedException)\n",
        },
        *_identity(),
        *_data_stack_resources(),
    ]
    h = Harness(tmp_path, rules)
    h.ui_build(real=True)
    r = h.run("deploy.sh", *DEPLOY_FLAGS, "--account", ACCT)
    assert r.returncode == 0, r.stderr
    assert f"could not check whether {ACCT} is an Organizations management account" in r.stderr
    assert "AccessDeniedException" in r.stderr


def test_deploy_option_without_a_value_names_the_option(deploy: Harness) -> None:
    r = deploy.run("deploy.sh", "--account")
    assert r.returncode != 0
    assert "--account requires a value" in r.stderr


# ── deploy.sh: escape hatches ──────────────────────────────────────────────────────


def test_deploy_refuses_the_unbuilt_lambda_escape_hatch(deploy: Harness) -> None:
    r = deploy.run("deploy.sh", *DEPLOY_FLAGS, "--account", ACCT, BIFFIX_ALLOW_UNBUILT_LAMBDA="1")
    assert r.returncode != 0
    assert "BIFFIX_ALLOW_UNBUILT_LAMBDA" in r.stderr
    assert deploy.cdk_calls() == []


def test_deploy_skip_ui_rejects_a_placeholder_build(tmp_path: Path) -> None:
    h = Harness(tmp_path, _identity())
    h.ui_build(real=False)
    r = h.run("deploy.sh", *DEPLOY_FLAGS, "--account", ACCT)
    assert r.returncode != 0
    assert "not a real UI build" in r.stderr
    assert h.cdk_calls() == []


# ── deploy.sh: the Rust builds ─────────────────────────────────────────────────────

# `cargo lambda build … --bin X` / `cargo build … --bin X`: writes X where cargo would, under
# CARGO_TARGET_DIR (relative to the working directory) or ./target. The built CLI logs its calls.
_CARGO = """#!/bin/bash
printf 'cargo %s\\n' "$*" >> "$CALL_LOG"
target="${CARGO_TARGET_DIR:-target}" bin="" prev=""
for a in "$@"; do [ "$prev" = --bin ] && bin="$a"; prev="$a"; done
case "$1" in
  lambda) mkdir -p "$target/lambda/$bin" && : > "$target/lambda/$bin/bootstrap" ;;
  build)
    out="$target/release/$bin"
    mkdir -p "$target/release"
    printf '#!/bin/bash\\nprintf "%s %%s\\\\n" "$*" >> "$CALL_LOG"\\n' "$bin" > "$out"
    chmod +x "$out" ;;
esac
"""


@pytest.mark.parametrize("target", [None, "relative", "absolute"])
def test_deploy_builds_and_runs_the_rust_binaries_in_cargo_s_target_dir(
    deploy: Harness, tmp_path: Path, target: str | None
) -> None:
    """The Lambdas are touched and the health-check CLI is run where cargo put them: target/, or
    CARGO_TARGET_DIR (a relative one under the repo root, where cargo runs)."""
    executable(deploy.stubs / "cargo", _CARGO)
    executable(deploy.stubs / "cargo-lambda", "#!/bin/bash\nexit 0\n")
    env = {}
    if target == "relative":
        env["CARGO_TARGET_DIR"] = "build/cargo"
    elif target == "absolute":
        env["CARGO_TARGET_DIR"] = str(tmp_path / "cargo-target")
    flags = ("--skip-ui", "--url", "https://api.example", "-c", "cfg.yaml", "--account", ACCT)
    r = deploy.run("deploy.sh", *flags, **env)
    assert r.returncode == 0, r.stderr
    built = {
        None: deploy.root / "target",
        "relative": deploy.root / "build/cargo",
        "absolute": tmp_path / "cargo-target",
    }[target]
    assert (built / "lambda/biffix-backend-lambda/bootstrap").is_file()
    assert (built / "release/biffix").is_file()
    assert "biffix health --url https://api.example" in deploy.calls()


# ── deploy.sh: config, region, exit status ─────────────────────────────────────────


def test_deploy_reads_the_config_through_config_py_and_targets_its_region(
    deploy: Harness, tmp_path: Path
) -> None:
    """An absolute -c path and a quoted tenant both work; every CloudFormation call names the
    config's region, and retention gets the unquoted tag and the data stack's own repo."""
    image = {
        "imageDigest": "sha256:d",
        "imageTags": ["abc123"],
        "imagePushedAt": "2026-09-01T00:00:00+00:00",
        "imageManifestMediaType": "application/vnd.oci.image.manifest.v1+json",
    }
    deploy.add_rules(
        {
            "match": ["ecr describe-images", "--repository-name acme-coord-repo"],
            "out": json.dumps({"imageDetails": [image]}),
        },
        {
            "match": ["ecr batch-get-image", "--repository-name acme-coord-repo"],
            "out": json.dumps(
                {
                    "images": [
                        {
                            "imageManifest": "{}",
                            "imageManifestMediaType": image["imageManifestMediaType"],
                        }
                    ]
                }
            ),
        },
    )
    elsewhere = tmp_path / "elsewhere.yaml"
    elsewhere.write_text(_config_text().replace("region: us-east-1", "region: eu-west-2"))
    r = deploy.run(
        "deploy.sh",
        "--skip-ui",
        "--skip-lambdas",
        "--no-verify",
        "-c",
        str(elsewhere),
        "--account",
        ACCT,
    )
    assert r.returncode == 0, r.stderr
    cfn = [c for c in deploy.aws_calls() if c.startswith("cloudformation")]
    assert cfn and all("--region eu-west-2" in c for c in cfn)
    assert any(f"--stack-name biffix-{TENANT}-data" in c for c in cfn)
    [deploy_call] = [c for c in deploy.cdk_calls() if c.startswith("deploy")]
    assert f"configFile={elsewhere}" in deploy_call and "--all" in deploy_call
    # image_retention ran against the stack's repo and found the deployed image by the unquoted
    # tag (a quoted one names no image, and retention then warns and prunes nothing).
    assert "acme-coord-repo: 1 to protect, 0 tags to remove" in r.stdout
    assert "image retention for" not in r.stderr
    [put] = [c for c in deploy.aws_calls() if c.startswith("ecr put-image")]
    assert "--repository-name acme-coord-repo" in put and "--image-digest sha256:d" in put
    assert f"--image-tag {protect_tag(f'deployed-{TENANT}-', 'sha256:d')}" in put


def test_deploy_fails_on_a_nonzero_cdk_exit_that_deployed_nothing(deploy: Harness) -> None:
    """Asset publishing failed: every stack still sits in its old UPDATE_COMPLETE, which must not
    read as success."""
    rules = json.loads(deploy.rules.read_text())
    rules.append({"match": ["describe-stacks"], "out": "UPDATE_COMPLETE\n"})
    deploy.rules.write_text(json.dumps(rules))
    r = deploy.run(
        "deploy.sh", *DEPLOY_FLAGS, "--account", ACCT,
        CDK_RC="1", CDK_OUT="failed to publish assets\n",
    )  # fmt: skip
    assert r.returncode != 0
    assert "without reporting" in r.stderr


def test_deploy_fails_on_a_failed_stack_marker(deploy: Harness) -> None:
    r = deploy.run(
        "deploy.sh", *DEPLOY_FLAGS, "--account", ACCT,
        CDK_RC="1", CDK_OUT=_ok_lines(STACKS[:-1]) + f"\n \u274c  {STACKS[-1]} failed: boom\n",
    )  # fmt: skip
    assert r.returncode != 0
    assert "cdk deploy failed (exit 1; see the \u274c lines above)" in r.stderr


def test_deploy_reads_a_failure_marker_early_in_a_long_cdk_log(
    deploy: Harness, tmp_path: Path
) -> None:
    """Under pipefail, `printf "$log" | grep -q` fails when grep stops at an early match before
    printf has written a log longer than the pipe buffer (SIGPIPE): the marker must still count."""
    out = tmp_path / "cdk-out.txt"
    out.write_text(f" ❌  {STACKS[0]} failed: boom\n" + "progress line\n" * 50_000)
    r = deploy.run("deploy.sh", *DEPLOY_FLAGS, "--account", ACCT, CDK_RC="1", CDK_OUT_FILE=str(out))
    assert r.returncode != 0
    assert "cdk deploy failed (exit 1; see the ❌ lines above)" in r.stderr


def test_deploy_fails_when_the_cdk_app_did_not_synthesize(deploy: Harness) -> None:
    r = deploy.run(
        "deploy.sh", *DEPLOY_FLAGS, "--account", ACCT,
        CDK_RC="1", CDK_OUT="Error: boom\nSubprocess exited with error 1\n",
    )  # fmt: skip
    assert r.returncode != 0
    assert "the CDK app did not synthesize" in r.stderr


def test_deploy_fails_on_a_reported_stack_that_rolled_back(deploy: Harness) -> None:
    deploy.add_rules(
        {"match": ["describe-stacks", "StackStatus"], "out": "UPDATE_ROLLBACK_COMPLETE\n"}
    )
    r = deploy.run(
        "deploy.sh", *DEPLOY_FLAGS, "--account", ACCT, CDK_RC="1", CDK_OUT=_ok_lines(STACKS)
    )
    assert r.returncode != 0
    assert f"cdk deploy failed \u2014 {STACKS[0]} is UPDATE_ROLLBACK_COMPLETE" in r.stderr
    assert "advisory-warning quirk" not in r.stderr


def test_deploy_fails_when_the_stack_state_cannot_be_read(deploy: Harness) -> None:
    rules = json.loads(deploy.rules.read_text())
    rules.append({"match": ["describe-stacks"], "rc": 255, "err": "ExpiredToken\n"})
    deploy.rules.write_text(json.dumps(rules))
    r = deploy.run(
        "deploy.sh", *DEPLOY_FLAGS, "--account", ACCT, CDK_RC="1", CDK_OUT=_ok_lines(STACKS)
    )
    assert r.returncode != 0
    assert "could not be read" in r.stderr


def test_deploy_accepts_the_advisory_warning_exit_only_with_positive_evidence(
    deploy: Harness,
) -> None:
    rules = json.loads(deploy.rules.read_text())
    rules.append({"match": ["describe-stacks", "StackStatus"], "out": "UPDATE_COMPLETE\n"})
    deploy.rules.write_text(json.dumps(rules))
    r = deploy.run(
        "deploy.sh", *DEPLOY_FLAGS, "--account", ACCT, "--stack", f"biffix-{TENANT}-api",
        CDK_RC="1", CDK_OUT=f"\033[32m \u2705  biffix-{TENANT}-api (no changes)\033[0m\n",
    )  # fmt: skip
    assert r.returncode == 0, r.stderr
    assert "advisory-warning quirk" in r.stderr


def _google_harness(tmp_path: Path) -> Harness:
    """A deploy whose config enables Google sign-in, with its client secret not yet stored."""
    h = Harness(
        tmp_path,
        [
            *_identity(),
            *_data_stack_resources(),
            {
                "match": ["secretsmanager describe-secret"],
                "rc": 254,
                "err": "ResourceNotFoundException\n",
            },
        ],
    )
    h.ui_build(real=True)
    h.config.write_text(
        _config_text(
            extra="cognito_domain_prefix: acme\n"
            "google_oidc:\n  client_id: id.apps.googleusercontent.com\n"
        )
    )
    return h


_MKTEMP = """#!/bin/bash
# The real mktemp, recording each path it makes in $MKTEMP_LOG.
path="$(/usr/bin/mktemp "$@")" || exit 1
printf '%s\\n' "$path" >> "$MKTEMP_LOG"
printf '%s\\n' "$path"
"""


def test_deploy_stores_the_google_secret_from_a_file_it_then_removes(tmp_path: Path) -> None:
    h = _google_harness(tmp_path)
    executable(h.stubs / "mktemp", _MKTEMP)
    made = tmp_path / "mktemp.log"
    r = h.run(
        "deploy.sh", *DEPLOY_FLAGS, "--account", ACCT,
        BIFFIX_GOOGLE_CLIENT_SECRET="s3kr1t", MKTEMP_LOG=str(made),
    )  # fmt: skip
    assert r.returncode == 0, r.stderr
    [create] = [c for c in h.aws_calls() if "create-secret" in c]
    assert "--secret-string file://" in create and "s3kr1t" not in create
    # Nothing the deploy wrote to a temp file outlives it: not the secret, not the cdk log.
    paths = made.read_text().split()
    assert paths and not [p for p in paths if Path(p).exists()], paths
    # No secret and no terminal to ask for it: it stops before deploying.
    h = _google_harness(tmp_path / "no-tty")
    r = h.run("deploy.sh", *DEPLOY_FLAGS, "--account", ACCT)
    assert r.returncode != 0
    assert "set BIFFIX_GOOGLE_CLIENT_SECRET" in r.stderr
    assert h.cdk_calls() == []


CERT = "arn:aws:acm:us-east-1:123456789012:certificate/c"


def _domain_harness(tmp_path: Path, *rules: Rule) -> Harness:
    """A deploy of a config with an app_domain and no certificate ARN."""
    h = Harness(tmp_path, [*rules, *_identity(), *_data_stack_resources()])
    h.ui_build(real=True)
    h.config.write_text(_config_text(extra="app_domain: app.example.com\n"))
    return h


def test_deploy_reuses_an_issued_certificate_for_the_app_domain(tmp_path: Path) -> None:
    h = _domain_harness(
        tmp_path, {"match": ["acm list-certificates", "ISSUED"], "out": f"{CERT}\n"}
    )
    r = h.run("deploy.sh", *DEPLOY_FLAGS, "--account", ACCT)
    assert r.returncode == 0, r.stderr
    [deploy_call] = [c for c in h.cdk_calls() if c.startswith("deploy")]
    assert f"appCertificateArn={CERT}" in deploy_call
    assert not [c for c in h.aws_calls() if "request-certificate" in c]


@pytest.mark.parametrize("status", ["ISSUED", "PENDING_VALIDATION"])
def test_deploy_fails_when_the_certificate_lookup_fails(tmp_path: Path, status: str) -> None:
    # An AccessDenied is not "no certificate": requesting another one would be wrong.
    h = _domain_harness(
        tmp_path,
        {"match": ["acm list-certificates", status], "rc": 254, "err": "AccessDeniedException\n"},
        {"match": ["acm list-certificates", "ISSUED"], "out": "None\n"},
    )
    r = h.run("deploy.sh", *DEPLOY_FLAGS, "--account", ACCT)
    assert r.returncode != 0
    assert "AccessDeniedException" in r.stderr
    assert not [c for c in h.aws_calls() if "request-certificate" in c]
    assert h.cdk_calls() == []


def test_deploy_stops_waiting_on_a_certificate_that_failed(tmp_path: Path) -> None:
    h = _domain_harness(
        tmp_path,
        {"match": ["acm list-certificates", "ISSUED"], "out": "None\n"},
        {"match": ["acm list-certificates", "PENDING_VALIDATION"], "out": f"{CERT}\n"},
        {"match": ["acm describe-certificate", "ResourceRecord"], "out": "_a.\tCNAME\t_b.\n"},
        {"match": ["acm describe-certificate", "Status"], "out": "VALIDATION_TIMED_OUT\n"},
    )
    r = h.run("deploy.sh", *DEPLOY_FLAGS, "--account", ACCT)
    assert r.returncode != 0
    assert "VALIDATION_TIMED_OUT" in r.stderr
    assert len([c for c in h.aws_calls() if "Certificate.Status" in c]) == 1
    assert h.cdk_calls() == []


def test_deploy_waits_for_a_new_certificate_s_validation_record(tmp_path: Path) -> None:
    # ACM fills the record in a moment after the request: the deploy prints it once it is there.
    h = _domain_harness(
        tmp_path,
        {"match": ["acm list-certificates"], "out": "None\n"},
        {"match": ["acm request-certificate"], "out": f"{CERT}\n"},
        {"match": ["acm describe-certificate", "ResourceRecord"], "out": "None\n", "times": 1},
        {"match": ["acm describe-certificate", "ResourceRecord"], "out": "_a.\tCNAME\t_b.\n"},
        {"match": ["acm describe-certificate", "Status"], "out": "ISSUED\n"},
    )
    r = h.run("deploy.sh", *DEPLOY_FLAGS, "--account", ACCT)
    assert r.returncode == 0, r.stderr
    assert "_a.\tCNAME\t_b." in r.stderr
    [deploy_call] = [c for c in h.cdk_calls() if c.startswith("deploy")]
    assert f"appCertificateArn={CERT}" in deploy_call


def test_deploy_takes_the_account_from_the_config_alone(deploy: Harness) -> None:
    deploy.config.write_text(_config_text(extra=f'account: "{ACCT}"\n'))
    r = deploy.run("deploy.sh", *DEPLOY_FLAGS)
    assert r.returncode == 0, r.stderr
    assert [c.split()[0] for c in deploy.cdk_calls()] == ["deploy"]


def test_a_failed_image_retention_only_warns(deploy: Harness) -> None:
    deploy.add_rules({"match": ["ecr describe-images"], "rc": 254, "err": "AccessDenied\n"})
    r = deploy.run("deploy.sh", *DEPLOY_FLAGS, "--account", ACCT)
    assert r.returncode == 0, r.stderr
    assert "image retention for acme-coord-repo failed" in r.stderr


def test_deploying_one_stack_skips_image_retention(deploy: Harness) -> None:
    r = deploy.run("deploy.sh", *DEPLOY_FLAGS, "--account", ACCT, "--stack", STACKS[0])
    assert r.returncode == 0, r.stderr
    assert not [c for c in deploy.aws_calls() if c.startswith("ecr ")]


_BIFFIX = """#!/bin/bash
printf 'biffix %s\\n' "$*" >> "$CALL_LOG"
"""

_UI_URL = ["describe-stacks", f"biffix-{TENANT}-edge", "UiUrl"]


@pytest.mark.parametrize(
    ("rule", "url"),
    [
        # The edge stack's own UiUrl output.
        ({"match": _UI_URL, "out": "https://d1.cloudfront.net\n"}, "https://d1.cloudfront.net/api"),
        # No edge stack yet: the config's app_domain.
        (
            {"match": _UI_URL, "rc": 254, "err": "Stack with id x does not exist\n"},
            "https://app.example.com/api",
        ),
        # Any other failure to read it stops the deploy rather than guess.
        ({"match": _UI_URL, "rc": 254, "err": "ExpiredToken\n"}, None),
    ],
)
def test_deploy_verifies_against_the_deployed_api(
    tmp_path: Path, rule: Rule, url: str | None
) -> None:
    cert: Rule = {"match": ["acm list-certificates"], "out": f"{CERT}\n"}
    h = Harness(tmp_path, [rule, cert, *_identity(), *_data_stack_resources()])
    h.ui_build(real=True)
    h.config.write_text(_config_text(extra="app_domain: app.example.com\n"))
    executable(h.stubs / "biffix", _BIFFIX)
    r = h.run("deploy.sh", "--skip-ui", "--skip-lambdas", "-c", "cfg.yaml", "--account", ACCT)
    health = [c for c in h.calls() if c.startswith("biffix ")]
    if url is None:
        assert r.returncode != 0
        assert "could not read the" in r.stderr and "ExpiredToken" in r.stderr
        assert health == []
    else:
        assert r.returncode == 0, r.stderr
        assert health == [f"biffix health --url {url}"]


def test_deploy_dry_run_changes_nothing_in_aws(tmp_path: Path) -> None:
    rules: list[Rule] = [
        *_identity(),
        {
            "match": ["secretsmanager describe-secret"],
            "rc": 254,
            "err": "ResourceNotFoundException\n",
        },
        {"match": ["acm list-certificates"], "out": "None\n"},
    ]
    h = Harness(tmp_path, rules)
    h.ui_build(real=True)
    h.config.write_text(
        _config_text(
            extra="app_domain: app.example.com\ncognito_domain_prefix: acme\n"
            "google_oidc:\n  client_id: id.apps.googleusercontent.com\n"
        )
    )
    r = h.run("deploy.sh", *DEPLOY_FLAGS, "--account", ACCT, "--dry-run")
    assert r.returncode == 0, r.stderr
    # Every call reads: an allow-list, so a new mutating call fails here whatever it is named.
    reads = ("get-", "list-", "describe-", "head-")
    calls = h.aws_calls()
    assert calls and not [c for c in calls if not c.split()[1].startswith(reads)], calls
    assert [c.split()[0] for c in h.cdk_calls()] == ["synth"]


# ── deploy.sh --release ──────────────────────────────────────────────────────────

# `crane digest` answers from $CRANE_STATE (no file: no such tag); `crane copy` records the source's
# digest there; `crane auth login` reads the password from stdin.
_CRANE = """#!/bin/bash
printf 'crane %s\\n' "$*" >> "$CALL_LOG"
case "$1" in
  auth) cat > /dev/null ;;
  digest) [ -f "$CRANE_STATE" ] && cat "$CRANE_STATE" ;;
  copy) printf '%s\\n' "${2##*@}" > "$CRANE_STATE" ;;
  *) exit 2 ;;
esac
"""

REGISTRY = f"{ACCT}.dkr.ecr.us-east-1.amazonaws.com"


def _release_harness(tmp_path: Path, *rules: Rule, crane: bool = True) -> Harness:
    """A deploy of this checkout's release, published under a file:// base URL, with the config's
    coordinator_image_tag left at its default (the release tag). No UI build, no cargo."""
    h = Harness(tmp_path, [*rules, *_identity(), *_data_stack_resources()])
    h.config.write_text(re.sub(r"(?m)^coordinator_image_tag: .*\n", "", _config_text()))
    publish(tmp_path / "releases")
    h.env["BIFFIX_RELEASE_BASE_URL"] = (tmp_path / "releases").as_uri()
    h.env["CRANE_STATE"] = str(tmp_path / "crane-state")
    if crane:
        executable(h.stubs / "crane", _CRANE)
    executable(h.infra / "node_modules/.bin/cdk", _CDK_RELEASE)
    return h


# The cdk stand-in, also recording the release directory the app is handed.
_CDK_RELEASE = _CDK.replace(
    'case "$1" in',
    'printf \'cdk-env %s\\n\' "${BIFFIX_RELEASE_DIR:-}" >> "$CALL_LOG"\ncase "$1" in',
)


RELEASE_FLAGS = ("--release", RELEASE, "--no-verify", "-c", "cfg.yaml", "--account", ACCT)


def test_deploy_release_deploys_the_release_and_imports_its_image_first(tmp_path: Path) -> None:
    """Nothing is built; the app reads the fetched release, and the image is copied by digest into
    the data stack's coordinator repo under the release tag before the deploy that names it."""
    h = _release_harness(tmp_path)
    r = h.run("deploy.sh", *RELEASE_FLAGS)
    assert r.returncode == 0, r.stderr
    release_dir = h.infra / ".release" / RELEASE
    assert (release_dir / "release.json").is_file()
    calls = h.calls()
    target = f"{REGISTRY}/acme-coord-repo:{RELEASE}"
    copy = f"crane copy {IMAGE}@{DIGEST} {target}"
    assert f"crane auth login {REGISTRY} -u AWS --password-stdin" in calls
    deploy_at = next(i for i, c in enumerate(calls) if c.startswith("cdk deploy"))
    assert calls.index(copy) < deploy_at
    assert f"cdk-env {release_dir}" in calls
    assert not [c for c in calls if c.startswith("cargo")]
    assert f"{target} = {DIGEST}" in r.stdout


def test_deploy_release_imports_after_a_first_deploy_creates_the_repo(tmp_path: Path) -> None:
    no_repo_yet: Rule = {
        "match": ["list-stack-resources", f"biffix-{TENANT}-data", "CoordinatorRepo"],
        "out": "None\n",
        "times": 1,
    }
    h = _release_harness(tmp_path, no_repo_yet)
    r = h.run("deploy.sh", *RELEASE_FLAGS)
    assert r.returncode == 0, r.stderr
    calls = h.calls()
    deploy_at = next(i for i, c in enumerate(calls) if c.startswith("cdk deploy"))
    copy_at = next(i for i, c in enumerate(calls) if c.startswith("crane copy"))
    assert copy_at > deploy_at
    assert "importing the image after the deploy" in r.stdout


def test_deploy_release_reuses_an_image_already_imported(tmp_path: Path) -> None:
    h = _release_harness(tmp_path)
    Path(h.env["CRANE_STATE"]).write_text(DIGEST + "\n")
    r = h.run("deploy.sh", *RELEASE_FLAGS)
    assert r.returncode == 0, r.stderr
    assert not [c for c in h.calls() if c.startswith("crane copy")]


def test_deploy_release_dry_run_fetches_and_synthesizes_only(tmp_path: Path) -> None:
    h = _release_harness(tmp_path, crane=False)
    r = h.run("deploy.sh", *RELEASE_FLAGS, "--dry-run")
    assert r.returncode == 0, r.stderr
    assert f"would import {IMAGE}@{DIGEST}" in r.stdout
    assert [c.split()[0] for c in h.cdk_calls()] == ["synth"]
    assert f"cdk-env {h.infra / '.release' / RELEASE}" in h.calls()
    assert not [c for c in h.calls() if c.startswith("crane")]


def test_deploy_release_refuses_a_config_naming_another_image_tag(tmp_path: Path) -> None:
    h = _release_harness(tmp_path)
    h.config.write_text(_config_text())  # coordinator_image_tag: "abc123"
    r = h.run("deploy.sh", *RELEASE_FLAGS)
    assert r.returncode != 0
    assert f"imports the coordinator image as {RELEASE}" in r.stderr
    assert h.cdk_calls() == []


@pytest.mark.parametrize(
    ("args", "error"),
    [
        (("--release", "1.0.0"), "takes a release tag"),
        (("--release", RELEASE, "--skip-ui"), "drop --skip-ui/--skip-lambdas"),
        (("--release", RELEASE, "--skip-lambdas"), "drop --skip-ui/--skip-lambdas"),
        (("--release",), "--release requires a value"),
    ],
)
def test_deploy_release_rejects_bad_arguments(
    tmp_path: Path, args: tuple[str, ...], error: str
) -> None:
    h = _release_harness(tmp_path)
    r = h.run("deploy.sh", "-c", "cfg.yaml", "--account", ACCT, *args)
    assert r.returncode != 0
    assert error in r.stderr
    assert h.cdk_calls() == []


def test_deploy_release_needs_crane_for_a_real_deploy(tmp_path: Path) -> None:
    h = _release_harness(tmp_path, crane=False)
    r = h.run("deploy.sh", *RELEASE_FLAGS)
    assert r.returncode != 0
    assert "crane not found" in r.stderr


def test_deploy_release_stops_on_a_release_that_fails_verification(tmp_path: Path) -> None:
    h = _release_harness(tmp_path)
    (tmp_path / "releases" / RELEASE / "biffix-ui.tar.gz").write_bytes(b"tampered")
    r = h.run("deploy.sh", *RELEASE_FLAGS)
    assert r.returncode != 0
    assert "biffix-ui.tar.gz: sha256 is" in r.stderr
    assert h.cdk_calls() == []
    assert not (h.infra / ".release" / RELEASE).exists()


# ── deploy.sh: the host AWS CLI ───────────────────────────────────────────────────


HOST_CLI = "2.99.0"


def _host_cli_harness(
    tmp_path: Path, *, x86_sha: str | None = None, staged: tuple[str, ...] = ()
) -> Harness:
    """A deploy whose reference bucket has none of the pinned zips (but those in `staged`)."""
    sha = {
        arch: hashlib.sha256(f"zip:awscli-exe-linux-{arch}-{HOST_CLI}.zip".encode()).hexdigest()
        for arch in ("x86_64", "aarch64")
    }
    head: list[Rule] = [
        {"match": ["s3api head-object", f"--key host/aws-cli/{HOST_CLI}/{arch}.zip"]}
        for arch in staged
    ]
    missing: Rule = {"match": ["s3api head-object"], "rc": 254, "err": "An error occurred (404)\n"}
    h = Harness(tmp_path, [*_identity(), *_data_stack_resources(), *head, missing])
    h.ui_build(real=True)
    executable(h.stubs / "curl", _CURL)
    h.config.write_text(
        _config_text(
            extra=f'host_aws_cli:\n  version: "{HOST_CLI}"\n'
            f'  sha256_x86_64: "{x86_sha or sha["x86_64"]}"\n'
            f'  sha256_aarch64: "{sha["aarch64"]}"\n'
        )
    )
    return h


def _uploads(h: Harness) -> list[str]:
    return [c.split()[3] for c in h.aws_calls() if c.startswith("s3 cp")]


def test_deploy_stages_the_pinned_host_aws_cli_in_the_reference_bucket(tmp_path: Path) -> None:
    h = _host_cli_harness(tmp_path)
    r = h.run("deploy.sh", *DEPLOY_FLAGS, "--account", ACCT)
    assert r.returncode == 0, r.stderr
    keys = {f"s3://acme-ref-bucket/host/aws-cli/{HOST_CLI}/{a}.zip" for a in ("x86_64", "aarch64")}
    assert set(_uploads(h)) == keys
    downloads = h.curl_log.read_text()
    for arch in ("x86_64", "aarch64"):
        assert f"https://awscli.amazonaws.com/awscli-exe-linux-{arch}-{HOST_CLI}.zip" in downloads
    # Staged before the deploy (the bucket exists), so no host boots looking for a missing zip.
    calls = h.calls()
    first_upload = next(i for i, c in enumerate(calls) if c.startswith("aws s3 cp"))
    assert first_upload < next(i for i, c in enumerate(calls) if c.startswith("cdk deploy"))


def test_deploy_leaves_a_staged_host_aws_cli_alone(tmp_path: Path) -> None:
    h = _host_cli_harness(tmp_path, staged=("x86_64",))
    r = h.run("deploy.sh", *DEPLOY_FLAGS, "--account", ACCT)
    assert r.returncode == 0, r.stderr
    assert set(_uploads(h)) == {f"s3://acme-ref-bucket/host/aws-cli/{HOST_CLI}/aarch64.zip"}
    assert "x86_64" not in h.curl_log.read_text()


def test_deploy_refuses_a_host_aws_cli_that_is_not_the_pinned_one(tmp_path: Path) -> None:
    h = _host_cli_harness(tmp_path, x86_sha="0" * 64)
    r = h.run("deploy.sh", *DEPLOY_FLAGS, "--account", ACCT)
    assert r.returncode != 0
    assert f"AWS CLI {HOST_CLI} (x86_64) sha256 is" in r.stderr
    assert f"config pins {'0' * 64}" in r.stderr
    assert _uploads(h) == []
    assert not [c for c in h.cdk_calls() if c.startswith("deploy")]


# ── teardown.sh ────────────────────────────────────────────────────────────────────


def _teardown_rules(*, bucket_errors: bool = False, present: int = 2) -> list[Rule]:
    """Tenant `acme` with a sibling tenant `acme-dev` in the same account and region, whose
    resources answer every lookup that is not by this tenant's exact names or tag. `present` buckets
    are found by the purge; each is gone when checked afterwards."""
    sibling = [f"biffix-{TENANT}-dev-{p}" for p in ("data", "persistence")]
    # A project since removed from the config, whose stack is still live.
    sibling.append(f"biffix-{TENANT}-project-gone")
    rules: list[Rule] = [
        *_identity(),
        {
            "match": ["cloudformation list-stacks"],
            "out": "\t".join(STACKS + sibling) + "\n",
            "times": 1,
        },
        {"match": ["cloudformation list-stacks"], "out": "\t".join(sibling) + "\n"},
        {
            "match": ["list-stack-resources", f"biffix-{TENANT}-data"],
            "out": "AWS::S3::Bucket\tReferenceBucketABC\tbiffix-acme-data-referencebucket1\n"
            "AWS::S3::Bucket\tImageArchiveBucketABC\tbiffix-acme-data-imagearchivebucket1\n"
            "AWS::ECR::Repository\tCoordinatorRepoABC\tbiffix-acme-data-coordinatorrepo1\n",
        },
        {
            "match": ["list-stack-resources", f"biffix-{TENANT}-persistence"],
            "out": "AWS::RDS::DBCluster\tEventStoreABC\tbiffix-acme-persistence-eventstore1\n",
        },
        # Found only by their tenant tag (ARN, biffix:role, and the stack name and logical id
        # CloudFormation tagged them with): an orphan from an earlier partial teardown; the
        # project's imported bucket, which is never biffix's to delete even when tagged; and two
        # image archives whose data stack is gone, under truncated generated names that do not
        # spell `imagearchivebucket`: one with its role tag, one from before the tag existed.
        {
            "match": ["resourcegroupstaggingapi get-resources", "Values=acme "],
            "out": "arn:aws:s3:::biffix-acme-project-demo-orphan1\tNone\t"
            "biffix-acme-project-demo\tDataBucket1\n"
            "arn:aws:s3:::shared-bkt\tNone\tNone\tNone\n"
            "arn:aws:s3:::biffix-acme-data-imagea-tagged1\timage-archive\tNone\tNone\n"
            "arn:aws:s3:::biffix-acme-data-imagea-pretag1\tNone\t"
            "biffix-acme-data\tImageArchiveBucket13FCE476\n"
            # The bucket of that removed project's live stack: not this teardown's to delete.
            "arn:aws:s3:::biffix-acme-project-gone-bucket1\tNone\t"
            "biffix-acme-project-gone\tDataBucket1\n",
        },
        {
            "match": ["resourcegroupstaggingapi get-resources", "Values=acme-dev "],
            "out": "arn:aws:s3:::biffix-acme-dev-data-referencebucket9\tNone\tNone\tNone\n",
        },
        {
            "match": ["describe-db-clusters", "DBClusterMembers"],
            "out": "biffix-acme-persistence-writer1\n",
        },
        {
            "match": ["describe-db-clusters", "biffix-acme-persistence-eventstore1"],
            "rc": 254,
            "err": "DBClusterNotFoundFault\n",
        },
        # Listing by name would find the sibling's: teardown must never discover that way.
        {"match": ["s3api list-buckets"], "out": "biffix-acme-dev-data-referencebucket9\n"},
        {"match": ["rds describe-db-clusters"], "out": "biffix-acme-dev-persistence-eventstore9\n"},
        {"match": ["head-bucket"], "times": present},
        {
            "match": ["head-bucket"],
            "rc": 254,
            "err": "An error occurred (404) when calling HeadBucket: Not Found\n",
        },
        {
            "match": ["list-object-versions"],
            "out": '{"Objects": [{"Key": "k", "VersionId": "v"}], "Quiet": true}\n',
            "times": 2 if bucket_errors else 1,
        },
        {"match": ["list-object-versions"], "out": '{"Objects": [], "Quiet": true}\n'},
    ]
    if bucket_errors:
        rules.insert(0, {"match": ["delete-objects"], "out": "AccessDenied\tk\tlocked\n"})
        rules.insert(0, {"match": ["head-bucket"]})  # the buckets stay
    return rules


def _teardown(tmp_path: Path, *, bucket_errors: bool = False, present: int = 2) -> Harness:
    h = Harness(tmp_path, _teardown_rules(bucket_errors=bucket_errors, present=present))
    h.config.write_text(
        _config_text().replace("  - id: demo\n", "  - id: demo\n    bucket_name: shared-bkt\n", 1)
    )
    return h


def _run_teardown(h: Harness, *extra: str, bash: str = "bash") -> subprocess.CompletedProcess[str]:
    return h.run("teardown.sh", "-c", "cfg.yaml", "--account", ACCT, "--yes", *extra, bash=bash)


@pytest.mark.parametrize(
    "bash",
    [
        "bash",
        pytest.param(
            str(BASH32), marks=pytest.mark.skipif(not BASH32.exists(), reason="no /bin/bash")
        ),
    ],
)
def test_teardown_touches_only_this_tenant(tmp_path: Path, bash: str) -> None:
    """The sibling tenant `acme-dev` shares the `biffix-acme-` prefix and must be left alone; the
    tagged orphan is found; the imported bucket and the image archive are never touched; Aurora
    goes before the stacks; runs on macOS's bash 3.2 too."""
    h = _teardown(tmp_path)
    r = _run_teardown(h, bash=bash)
    assert r.returncode == 0, r.stderr
    calls = h.aws_calls()
    archives = ("imagearchivebucket1", "imagea-tagged1", "imagea-pretag1")
    for untouchable in ("acme-dev", "shared-bkt", "project-gone", *archives):
        assert not [c for c in calls if untouchable in c], untouchable
    assert "biffix-acme-project-gone-bucket1: its stack biffix-acme-project-gone" in r.stderr
    assert all(f"{a} (image archive: KEPT" in r.stdout for a in archives), r.stdout
    unscoped = [
        c
        for c in calls
        if "list-buckets" in c
        or ("describe-db-clusters" in c and "--db-cluster-identifier" not in c)
    ]
    assert not unscoped
    protected = [c for c in calls if "update-termination-protection" in c]
    assert len(protected) == len(STACKS)
    # A batch of up to 1000 versions goes through a file: as one argument it can exceed Linux's
    # 128 KiB limit on a single argv string.
    batches = [c for c in calls if "delete-objects" in c]
    assert batches and all("--delete file://" in c for c in batches), batches
    deleted = [c for c in calls if "delete-bucket" in c]
    assert any("referencebucket1" in c for c in deleted)
    assert any("project-demo-orphan1" in c for c in deleted)
    assert any("delete-db-cluster" in c and "eventstore1" in c for c in calls)
    # The retained cluster holds the persistence stack's security group: it goes first.
    order = h.calls()
    destroy = order.index(next(c for c in order if c.startswith("cdk destroy --all")))
    deletes = ("aws rds modify-", "aws rds delete-", "aws rds wait")
    assert max(i for i, c in enumerate(order) if c.startswith(deletes)) < destroy
    assert max(i for i, c in enumerate(order) if "update-termination-protection" in c) < destroy
    assert "coordinatorrepo1" in r.stdout  # reported as retained by design
    # The synth placeholder UI is a temp directory, never ui/build, and is removed on exit.
    assert not (h.root / "ui/build").exists()
    [destroy_call] = h.cdk_calls()
    assert destroy_call.startswith("destroy --all")
    stub = destroy_call.split("uiAssetPath=")[1].split()[0]
    assert not stub.startswith(str(h.root)) and not Path(stub).exists(), stub


def test_teardown_requires_the_account(tmp_path: Path) -> None:
    h = _teardown(tmp_path)
    r = h.run("teardown.sh", "-c", "cfg.yaml", "--yes")
    assert r.returncode != 0
    assert "--account <id> is required" in r.stderr
    assert h.calls() == []


def test_teardown_acts_as_the_assumed_role_and_removes_its_config(tmp_path: Path) -> None:
    h = _teardown(tmp_path)
    h.add_rules(*_assumed_identity("biffix-teardown")[:2], first=True)
    r = _run_teardown(h, "--assume-role", "arn:aws:iam::111122223333:role/Deployer")
    assert r.returncode == 0, r.stderr
    assert any(c.startswith("cdk destroy") for c in h.calls())
    assert list(tmp_path.glob("biffix-aws-config.*")) == []


def test_teardown_keep_data_destroys_only_the_stacks(tmp_path: Path) -> None:
    """The kept cluster holds the persistence stack's security group and sits in the network
    stack's subnets, so CloudFormation cannot delete either: both are kept, and the rest go."""
    h = _teardown(tmp_path)
    persistence, network = f"biffix-{TENANT}-persistence", f"biffix-{TENANT}-network"
    r = h.run(
        "teardown.sh", "-c", "cfg.yaml", "--account", ACCT, "--yes", "--keep-data",
        CDK_UNDELETABLE=persistence,
    )  # fmt: skip
    assert r.returncode == 0, r.stderr
    touched = ("rds ", "s3api ")
    assert not [c for c in h.aws_calls() if c.startswith(touched)]
    [destroy] = h.cdk_calls()
    destroyed = set(destroy.split()) & set(STACKS)
    assert destroyed == set(STACKS) - {persistence, network}, destroy
    unprotected = {
        c.split("--stack-name ")[1].split()[0] for c in h.aws_calls() if "termination" in c
    }
    assert unprotected == destroyed
    assert f"kept: {network} {persistence}" in r.stdout or f"kept: {persistence} {network}" in (
        r.stdout
    )
    assert "(--keep-data: Aurora and buckets kept)" in r.stdout


def test_teardown_keep_data_destroys_every_stack_once_no_cluster_is_left(tmp_path: Path) -> None:
    h = _teardown(tmp_path)
    h.add_rules(
        {"match": ["list-stack-resources", f"biffix-{TENANT}-persistence"], "out": ""}, first=True
    )
    r = _run_teardown(h, "--keep-data")
    assert r.returncode == 0, r.stderr
    [destroy] = h.cdk_calls()
    assert destroy.startswith("destroy --all")


def test_teardown_delete_image_archive_purges_and_deletes_it(tmp_path: Path) -> None:
    h = _teardown(tmp_path, present=5)
    r = _run_teardown(h, "--delete-image-archive")
    assert r.returncode == 0, r.stderr
    for name in ("imagearchivebucket1", "imagea-tagged1", "imagea-pretag1"):
        archive = [c for c in h.aws_calls() if name in c]
        assert any(c.startswith("s3api delete-bucket") for c in archive), archive
    assert "image archive kept" not in r.stdout


def test_teardown_stops_unless_the_tenant_is_typed(tmp_path: Path) -> None:
    h = _teardown(tmp_path / "wrong")
    r = h.run("teardown.sh", "-c", "cfg.yaml", "--account", ACCT, input="acme-dev\n")
    assert r.returncode != 0
    assert "confirmation did not match" in r.stderr
    assert not [c for c in h.aws_calls() if "update-termination-protection" in c]
    assert h.cdk_calls() == []
    h = _teardown(tmp_path / "typed")
    r = h.run("teardown.sh", "-c", "cfg.yaml", "--account", ACCT, input=f"{TENANT}\n")
    assert r.returncode == 0, r.stderr
    assert [c.split()[0] for c in h.cdk_calls()] == ["destroy"]
    # With nothing to read (no terminal, no --yes), it says why it stopped.
    h = _teardown(tmp_path / "closed")
    r = h.run("teardown.sh", "-c", "cfg.yaml", "--account", ACCT)
    assert r.returncode != 0
    assert "pass --yes" in r.stderr
    assert h.cdk_calls() == []


def test_teardown_stops_purging_a_bucket_whose_versions_cannot_be_deleted(tmp_path: Path) -> None:
    h = _teardown(tmp_path, bucket_errors=True)
    r = _run_teardown(h)
    assert r.returncode != 0
    assert "cannot be deleted" in r.stderr
    # One attempt per bucket, not an endless loop.
    assert len([c for c in h.aws_calls() if "delete-objects" in c]) == 2


def test_teardown_fails_when_discovery_fails(tmp_path: Path) -> None:
    rules: list[Rule] = [
        *_identity(),
        {"match": ["cloudformation list-stacks"], "rc": 255, "err": "ExpiredToken\n"},
    ]
    h = Harness(tmp_path, rules)
    r = _run_teardown(h)
    assert r.returncode != 0
    assert "nothing to tear down" not in r.stdout
    assert "could not list CloudFormation stacks" in r.stderr
    assert h.cdk_calls() == []


def test_teardown_fails_when_the_aurora_cluster_is_not_deleted(tmp_path: Path) -> None:
    h = _teardown(tmp_path)
    h.add_rules(
        {"match": ["wait db-cluster-deleted"], "rc": 255, "err": "Waiter failed\n"}, first=True
    )
    r = _run_teardown(h)
    assert r.returncode != 0
    assert "was not deleted" in r.stderr
    assert "Aurora cluster biffix-acme-persistence-eventstore1 deleted" not in r.stdout
    assert h.cdk_calls() == []


def test_teardown_keeps_an_existing_ui_build(tmp_path: Path) -> None:
    h = _teardown(tmp_path)
    h.ui_build(real=True)
    r = _run_teardown(h)
    assert r.returncode == 0, r.stderr
    assert (h.root / "ui/build/index.html").exists()


def test_scripts_pass_shellcheck() -> None:
    if not shutil.which("shellcheck"):
        pytest.skip("shellcheck not installed")
    scripts = [INFRA_DIR / "deploy.sh", INFRA_DIR / "teardown.sh", INFRA_DIR / "script_lib.sh"]
    scripts.append(COORDINATOR_DIR / "build-and-push.sh")
    r = subprocess.run(["shellcheck", "-x", *map(str, scripts)], capture_output=True, text=True)
    assert r.returncode == 0, r.stdout
