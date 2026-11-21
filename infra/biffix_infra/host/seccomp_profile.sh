# shellcheck shell=bash
# Make Docker's default seccomp profile on a single-job host the one in $BIFFIX_SECCOMP_PROFILE: moby's
# default plus one rule allowing mount, umount2, unshare and setns, which apptainer needs to run
# each task in its own container nested in the job's (see host_boot.single_job_seccomp_profile).
#
# Runs as a cloud-init boothook, so before Docker starts (host_boot.py says why), on every
# boot: both files are rewritten each time, to the same bytes.
#
# host_boot.py prepends:
#   BIFFIX_SECCOMP_PROFILE (/etc/docker/seccomp-biffix-single-job.json),
#   BIFFIX_SECCOMP_PROFILE_JSON (the profile, compact JSON)
#
# The profile is written and checked before daemon.json names it: dockerd refuses to start when
# its `seccomp-profile` cannot be read, and a host without Docker never joins the cluster. A
# failure is logged and leaves daemon.json alone, so the host still joins with Docker's built-in
# profile; a single-job run placed on it then fails when apptainer is refused, not silently.

BIFFIX_DOCKER_DAEMON_JSON="${BIFFIX_DOCKER_DAEMON_JSON:-/etc/docker/daemon.json}"
BIFFIX_SECCOMP_LOG="${BIFFIX_SECCOMP_LOG:-/var/log/biffix-seccomp-profile.log}"

biffix_write_seccomp_profile() {
  mkdir -p "$(dirname "$BIFFIX_SECCOMP_PROFILE")" "$(dirname "$BIFFIX_DOCKER_DAEMON_JSON")"
  local tmp="$BIFFIX_SECCOMP_PROFILE.tmp"
  printf '%s\n' "$BIFFIX_SECCOMP_PROFILE_JSON" > "$tmp"
  # It parses and has the rule: a truncated or wrong profile must not reach dockerd. One rule
  # allows all four unconditionally. Moby's own rule for them is gated on CAP_SYS_ADMIN
  # (`includes`), which no job holds, so it does not count. (No comment inside the here-document:
  # the boot script keeps its lines as they are.)
  python3 - "$tmp" <<'PY'
import json
import sys

profile = json.load(open(sys.argv[1]))
wanted = {"mount", "umount2", "unshare", "setns"}
if not any(
    rule.get("action") == "SCMP_ACT_ALLOW"
    and not rule.get("args")
    and not rule.get("includes")
    and not rule.get("excludes")
    and wanted <= set(rule.get("names", []))
    for rule in profile["syscalls"]
):
    sys.exit(f"the profile has no unconditional rule allowing {sorted(wanted)}")
PY
  mv -f "$tmp" "$BIFFIX_SECCOMP_PROFILE"

  # daemon.json gains the one key; any other setting already there is kept (the ECS AMI ships
  # none today, so this is usually the whole file).
  python3 - "$BIFFIX_DOCKER_DAEMON_JSON" "$BIFFIX_SECCOMP_PROFILE" <<'PY'
import json
import os
import sys

path, profile = sys.argv[1], sys.argv[2]
config = {}
if os.path.exists(path):
    with open(path) as f:
        config = json.load(f)
config["seccomp-profile"] = profile
with open(path + ".tmp", "w") as f:
    json.dump(config, f, indent=2, sort_keys=True)
    f.write("\n")
os.replace(path + ".tmp", path)
PY
}

( set -euo pipefail; biffix_write_seccomp_profile ) >> "$BIFFIX_SECCOMP_LOG" 2>&1
biffix_rc=$?
if [ "$biffix_rc" -eq 0 ]; then
  echo "biffix: Docker's default seccomp profile is $BIFFIX_SECCOMP_PROFILE"
else
  echo "biffix: seccomp profile FAILED (exit $biffix_rc); Docker keeps its built-in profile;" \
    "see $BIFFIX_SECCOMP_LOG"
  tail -n 20 "$BIFFIX_SECCOMP_LOG"
fi
