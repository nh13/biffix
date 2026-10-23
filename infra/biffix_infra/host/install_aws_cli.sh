# shellcheck shell=bash
# Install the pinned AWS CLI v2 on a Batch host at boot, at $BIFFIX_AWS_CLI_DIR/bin/aws.
#
# Nextflow's `aws.batch.cliPath` points at that path: it bind-mounts $BIFFIX_AWS_CLI_DIR read-only
# into every task container and stages the task's S3 I/O with it, so task images (a stock
# BioContainer) need no AWS CLI of their own.
#
# compute_stack.py prepends the pinned values:
#   BIFFIX_HOST_BUCKET, BIFFIX_REGION, BIFFIX_AWS_CLI_DIR (/opt/biffix/aws-cli),
#   BIFFIX_AWS_CLI_KEY_PREFIX (host/aws-cli/<version>/),
#   BIFFIX_AWS_CLI_SHA256_X86_64, BIFFIX_AWS_CLI_SHA256_AARCH64
#
# The ECS AMI may have neither the AWS CLI nor unzip, so curl signs the S3 request itself (SigV4
# with the instance role's credentials) and python3 (always present: dnf needs it) unpacks the zip.
#
# A failed install is logged but does not fail the boot: the host still joins ECS, and only a
# Nextflow task placed on it fails (its launcher cannot find `aws`). Failing the boot instead would
# leave Batch launching replacement hosts in a loop.

BIFFIX_HOST_AWS_CLI_LOG="${BIFFIX_HOST_AWS_CLI_LOG:-/var/log/biffix-host-aws-cli.log}"

biffix_install_host_aws_cli() {
  local prefix="$BIFFIX_AWS_CLI_DIR"
  local arch sha
  case "$(uname -m)" in
    x86_64) arch=x86_64 sha="$BIFFIX_AWS_CLI_SHA256_X86_64" ;;
    aarch64) arch=aarch64 sha="$BIFFIX_AWS_CLI_SHA256_AARCH64" ;;
    *) echo "unsupported architecture: $(uname -m)"; return 1 ;;
  esac

  local work
  work="$(mktemp -d)"
  # shellcheck disable=SC2064 # expand now: $work is local to this function
  trap "rm -rf '$work'" EXIT

  # The instance role's credentials (IMDSv2), handed to curl in a config file so the keys never
  # appear in a process listing.
  local imds=http://169.254.169.254/latest token role
  token="$(curl -fsS --retry 5 -X PUT "$imds/api/token" \
    -H 'X-aws-ec2-metadata-token-ttl-seconds: 300')"
  role="$(curl -fsS --retry 5 -H "X-aws-ec2-metadata-token: $token" \
    "$imds/meta-data/iam/security-credentials/")"
  curl -fsS --retry 5 -H "X-aws-ec2-metadata-token: $token" \
    "$imds/meta-data/iam/security-credentials/$role" > "$work/credentials.json"
  (umask 077 && python3 - "$work/credentials.json" > "$work/curl.cfg") <<'PY'
import json
import sys

creds = json.load(open(sys.argv[1]))
print(f'user = "{creds["AccessKeyId"]}:{creds["SecretAccessKey"]}"')
print(f'header = "x-amz-security-token: {creds["Token"]}"')
PY

  curl -fsS --retry 5 -K "$work/curl.cfg" \
    --aws-sigv4 "aws:amz:${BIFFIX_REGION}:s3" \
    -H 'x-amz-content-sha256: UNSIGNED-PAYLOAD' \
    -o "$work/awscli.zip" \
    "https://${BIFFIX_HOST_BUCKET}.s3.${BIFFIX_REGION}.amazonaws.com/${BIFFIX_AWS_CLI_KEY_PREFIX}${arch}.zip"
  echo "$sha  $work/awscli.zip" | sha256sum -c -

  # zipfile.extract drops the mode bits, so restore them (the installer and binaries need +x).
  python3 - "$work/awscli.zip" "$work" <<'PY'
import os
import sys
import zipfile

with zipfile.ZipFile(sys.argv[1]) as archive:
    for info in archive.infolist():
        path = archive.extract(info, sys.argv[2])
        mode = (info.external_attr >> 16) & 0o777
        if mode:
            os.chmod(path, mode)
PY

  rm -rf "$prefix"
  "$work/aws/install" --install-dir "$prefix/dist" --bin-dir "$prefix/bin"

  # The CLI loads shared libraries it ships next to its binary (libz, libffi, …) but only finds them
  # through the system's paths, which a task image may lack (BioContainers' busybox base has no
  # libz: "libz.so.1: cannot open shared object file"). So bin/aws is a wrapper pointing the loader
  # at the bundle; plain sh, as that is all a task image is sure to have.
  local dist="$prefix/dist/v2/current/dist"
  rm -f "$prefix/bin/aws"
  # shellcheck disable=SC2016 # $LD_LIBRARY_PATH and $@ expand when the wrapper runs
  printf '#!/bin/sh\nexport LD_LIBRARY_PATH="%s${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"\nexec "%s/aws" "$@"\n' \
    "$dist" "$dist" > "$prefix/bin/aws"
  chmod 755 "$prefix/bin/aws"
  "$prefix/bin/aws" --version
}

# A subshell so `set -e` applies inside it (it is ignored in a function called from an `if`/`||`).
( set -euo pipefail; biffix_install_host_aws_cli ) >> "$BIFFIX_HOST_AWS_CLI_LOG" 2>&1
biffix_rc=$?
if [ "$biffix_rc" -eq 0 ]; then
  echo "biffix: host AWS CLI installed at $BIFFIX_AWS_CLI_DIR/bin/aws"
else
  echo "biffix: host AWS CLI install FAILED (exit $biffix_rc); see $BIFFIX_HOST_AWS_CLI_LOG"
  tail -n 20 "$BIFFIX_HOST_AWS_CLI_LOG"
fi
