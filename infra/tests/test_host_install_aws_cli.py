"""The host boot script (`biffix_infra/host/install_aws_cli.sh`), run for real with stand-ins for
the instance metadata service and S3: a `curl` that answers IMDS and serves a locally built zip, and
a `uname` that reports the host architecture. The zip mimics the AWS CLI bundle: an `aws/install`
that lays out `<install-dir>` and links `<bin-dir>/aws`."""

from __future__ import annotations

import hashlib
import os
import shutil
import stat
import subprocess
import zipfile
from pathlib import Path

import pytest

from biffix_infra.config import HostAwsCli
from biffix_infra.paths import HOST_DIR
from biffix_infra.stacks.compute_stack import host_install_header
from tests.helpers import executable

SCRIPT = HOST_DIR / "install_aws_cli.sh"

# The bundle's installer, reduced to what the boot script relies on: `--install-dir` and
# `--bin-dir`, and an `aws` that runs.
_INSTALLER = """#!/bin/bash
set -eu
while [ $# -gt 0 ]; do
  case "$1" in
    --install-dir) install_dir="$2"; shift 2 ;;
    --bin-dir) bin_dir="$2"; shift 2 ;;
    *) exit 2 ;;
  esac
done
# The real installer's layout: v2/<version>/{dist,bin/aws -> ../dist/aws}, v2/current -> <version>.
mkdir -p "$install_dir/v2/2.37.4/bin" "$bin_dir"
cp -r "$(dirname "$0")/dist" "$install_dir/v2/2.37.4/dist"
ln -s ../dist/aws "$install_dir/v2/2.37.4/bin/aws"
ln -snf "$install_dir/v2/2.37.4" "$install_dir/v2/current"
ln -sf "$install_dir/v2/current/bin/aws" "$bin_dir/aws"
"""

_CURL = """#!/bin/bash
# Record the request, then answer it like IMDS or S3 would.
printf '%s\\n' "$*" >> "$STUB_LOG"
url="" out=""
while [ $# -gt 0 ]; do
  case "$1" in -o) out="$2"; shift 2 ;; http*) url="$1"; shift ;; *) shift ;; esac
done
case "$url" in
  */api/token) echo token ;;
  */security-credentials/) echo host-role ;;
  */security-credentials/host-role)
    echo '{"AccessKeyId":"AKID","SecretAccessKey":"SECRET","Token":"SESSION"}' ;;
  https://*.amazonaws.com/*) cp "$STUB_ZIP" "$out" ;;
  *) exit 22 ;;
esac
"""


def _bundle(path: Path) -> str:
    """Write a stand-in CLI zip (with Unix modes, as AWS ships it) and return its sha256."""
    with zipfile.ZipFile(path, "w") as archive:
        for name, text in {
            "aws/install": _INSTALLER,
            "aws/dist/aws": '#!/bin/bash\necho "aws-cli/2.37.4 LD_LIBRARY_PATH=$LD_LIBRARY_PATH"\n',
        }.items():
            info = zipfile.ZipInfo(name)
            info.external_attr = (stat.S_IFREG | 0o755) << 16
            archive.writestr(info, text)
    return hashlib.sha256(path.read_bytes()).hexdigest()


def _run(
    tmp_path: Path, *, machine: str, sha256: str | None = None
) -> subprocess.CompletedProcess[str]:
    stubs = tmp_path / "stubs"
    stubs.mkdir()
    real_sha = _bundle(tmp_path / "bundle.zip")
    executable(stubs / "curl", _CURL)
    executable(stubs / "uname", f"#!/bin/bash\necho {machine}\n")
    # macOS has no sha256sum on every version; the host (Linux) always does.
    if not shutil.which("sha256sum"):
        executable(stubs / "sha256sum", '#!/bin/bash\nexec shasum -a 256 "$@"\n')
    prefix = tmp_path / "opt/biffix/aws-cli"
    # The header the stack itself emits (its variable names and per-arch binding). Only the host's
    # own arch gets the bundle's checksum, so a swapped per-arch binding fails the install.
    wrong = "f" * 64
    good = sha256 or real_sha
    cli = HostAwsCli(
        version="2.37.4",
        sha256_x86_64=good if machine == "x86_64" else wrong,
        sha256_aarch64=good if machine == "aarch64" else wrong,
    )
    header = "\n".join(
        host_install_header(cli, bucket_name="ref-bucket", region="us-east-1", cli_dir=str(prefix))
    )
    boot = tmp_path / "boot.sh"
    boot.write_text(f"#!/bin/bash\n{header}\n{SCRIPT.read_text()}")
    env = {
        **os.environ,
        "PATH": f"{stubs}:{os.environ['PATH']}",
        "STUB_LOG": str(tmp_path / "curl.log"),
        "STUB_ZIP": str(tmp_path / "bundle.zip"),
        "BIFFIX_HOST_AWS_CLI_LOG": str(tmp_path / "install.log"),
    }
    return subprocess.run(["bash", str(boot)], env=env, capture_output=True, text=True)


@pytest.mark.parametrize("machine", ["x86_64", "aarch64"])
def test_installs_the_cli_for_the_host_arch(tmp_path: Path, machine: str) -> None:
    result = _run(tmp_path, machine=machine)
    assert result.returncode == 0
    assert "host AWS CLI installed" in result.stdout, result.stdout
    # bin/aws runs the bundled binary with the loader pointed at the bundle's own libraries.
    aws = tmp_path / "opt/biffix/aws-cli/bin/aws"
    version = subprocess.run(
        [aws], capture_output=True, text=True, env={**os.environ, "LD_LIBRARY_PATH": "/x"}
    ).stdout
    dist = tmp_path / "opt/biffix/aws-cli/dist/v2/current/dist"
    assert version.startswith("aws-cli/"), version
    assert f"LD_LIBRARY_PATH={dist}:/x" in version
    s3_get = (tmp_path / "curl.log").read_text().splitlines()[-1]
    key = f"host/aws-cli/2.37.4/{machine}.zip"
    assert s3_get.endswith(f"https://ref-bucket.s3.us-east-1.amazonaws.com/{key}")
    assert "--aws-sigv4 aws:amz:us-east-1:s3" in s3_get
    # The keys reach curl through its config file, never its arguments.
    assert "SECRET" not in (tmp_path / "curl.log").read_text()


def test_a_checksum_mismatch_installs_nothing_but_does_not_fail_the_boot(tmp_path: Path) -> None:
    result = _run(tmp_path, machine="x86_64", sha256="0" * 64)
    assert result.returncode == 0
    assert "host AWS CLI install FAILED" in result.stdout
    assert not (tmp_path / "opt/biffix/aws-cli").exists()


def test_an_unknown_arch_is_reported(tmp_path: Path) -> None:
    result = _run(tmp_path, machine="ppc64le")
    # Reported, and the boot still succeeds (a failing boot would replace the host in a loop).
    assert result.returncode == 0
    assert "unsupported architecture: ppc64le" in result.stdout
    assert "host AWS CLI install FAILED" in result.stdout
