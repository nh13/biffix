"""What a Batch host runs at boot, assembled from the scripts in `biffix_infra/host/`.

Every host's user data turns off privileged containers in the ECS agent and installs the pinned
host AWS CLI (`host/install_aws_cli.sh`) from the reference bucket's `host/` prefix; Nextflow
bind-mounts the CLI into each task (`aws.batch.cliPath`), so task images need none.

A single-job host (`QueueClass.single_job`: one job is a whole run, its tasks in apptainer nested
in the job's container) boots with two more steps, as a cloud-init boothook (`single_job_boothook`
says why): Docker's default seccomp profile becomes moby's default plus
mount/umount2/unshare/setns (`host/seccomp_profile.sh`), and the instance-store disks are mounted
at `SCRATCH_DIR` (`host/scratch_disks.sh`), which then writes `INSTANCE_STORE_MARKER`.

The paths a single-job job relies on (`SCRATCH_DIR`, `HOST_STATE_DIR`, `INSTANCE_STORE_MARKER`)
are defined here once: the single-job job definition template mounts and names the same ones
(`constructs/coordinator.py`), and a test checks the coordinator's own fallbacks against them.
"""

from __future__ import annotations

import base64
import gzip
import json
import posixpath
import re
from pathlib import Path
from typing import Any

from aws_cdk import aws_ec2 as ec2

from biffix_infra.config import HOST_AWS_CLI_PATH, HostAwsCli
from biffix_infra.paths import HOST_DIR

INSTALL_AWS_CLI = HOST_DIR / "install_aws_cli.sh"
SECCOMP_PROFILE_SCRIPT = HOST_DIR / "seccomp_profile.sh"
SCRATCH_DISKS_SCRIPT = HOST_DIR / "scratch_disks.sh"
# The ECS agent setting that makes it refuse to start a privileged container, appended to its config
# before the agent starts (on the ECS-optimized AMI the agent starts after cloud-init has run the
# user data).
DISABLE_PRIVILEGED = "ECS_DISABLE_PRIVILEGED=true"
# Moby's default seccomp profile, vendored byte for byte from moby's
# `profiles/seccomp/default.json` at tag v25.0.18 (the dockerd the ECS-optimized AL2023 AMI ran
# when single-job mode was tested on it; the file is the same from v25.0.14 through v28.0.0).
# Vendored, not read from the host: dockerd compiles its default in and the AMI ships no copy, and
# a pinned base keeps what the hosts run reviewable here. Moving to a newer moby release is
# replacing this file (and its checksum in tests/test_host_single_job.py). Moby's Apache-2.0
# LICENSE and its NOTICE are beside it (`moby-LICENSE`, `moby-NOTICE`).
MOBY_SECCOMP_DEFAULT = HOST_DIR / "moby-seccomp-default-v25.0.18.json"
# What apptainer needs beyond moby's default to run a task's container nested in the job's: user
# namespaces (unshare, setns) and mounts in them (mount, umount2). Each was shown necessary on EC2;
# clone and pivot_root were not.
SINGLE_JOB_SYSCALLS = ("mount", "umount2", "unshare", "setns")
# Where single-job hosts keep the profile their Docker daemon.json names.
SINGLE_JOB_SECCOMP_PROFILE = "/etc/docker/seccomp-biffix-single-job.json"
# The host's own biffix state on a single-job host, on the root volume and root's. The single-job
# template mounts it into the job READ-ONLY at the same path, so a run can read what is here but
# never change it.
HOST_STATE_DIR = "/var/lib/biffix"
# Where a single-job host's boothook unpacks its boot script (`single_job_boothook`), kept for
# inspection.
SINGLE_JOB_BOOT_SCRIPT = f"{HOST_STATE_DIR}/single-job-boot.sh"
# Where single-job hosts mount their instance-store disks; the single-job template mounts it into
# the job read-write at the same path, and names it in BIFFIX_SCRATCH_ROOT.
SCRATCH_DIR = "/scratch"
# The file the boothook writes once the instance store is mounted at `SCRATCH_DIR`, and only then
# (`host/scratch_disks.sh`). It is in `HOST_STATE_DIR`, not on the run-writable scratch, so no run
# can remove it (and make every later run on the host refuse to start) or fake it. The single-job
# template names it in BIFFIX_INSTANCE_STORE_MARKER; a single-job run refuses to start without it,
# rather than run on the root volume.
INSTANCE_STORE_MARKER = f"{HOST_STATE_DIR}/instance-store"


def host_install_header(
    cli: HostAwsCli,
    *,
    bucket_name: str,
    region: str,
    cli_dir: str = posixpath.dirname(posixpath.dirname(HOST_AWS_CLI_PATH)),
) -> list[str]:
    """The variable assignments the host boot script (`install_aws_cli.sh`) reads, as the user data
    sets them. One function, so the boot-script test runs the script against exactly the names and
    per-arch values the stack emits."""
    return [
        f"BIFFIX_HOST_BUCKET={bucket_name}",
        f"BIFFIX_REGION={region}",
        f"BIFFIX_AWS_CLI_DIR={cli_dir}",
        f"BIFFIX_AWS_CLI_KEY_PREFIX={posixpath.dirname(cli.key('x86_64'))}/",
        f"BIFFIX_AWS_CLI_SHA256_X86_64={cli.sha256_x86_64}",
        f"BIFFIX_AWS_CLI_SHA256_AARCH64={cli.sha256_aarch64}",
    ]


def host_user_data(
    cli: HostAwsCli, *, bucket_name: str, region: str, single_job: bool = False
) -> ec2.MultipartUserData:
    """The boot script every host runs, as the MIME multipart user data Batch requires (Batch
    merges its own ECS agent part in). Two parts: turning off privileged containers, then the
    host AWS CLI install; a single-job host's starts with a third, its boothook
    (`single_job_boothook`)."""
    # Nothing biffix runs on Batch is privileged: the coordinator job definitions are not
    # (constructs/coordinator.py), the backend never registers a privileged task definition
    # (and IAM refuses one, ControlPlaneGrants.launch), and the image builds that need a
    # privileged Docker daemon run in CodeBuild, not on Batch. So every host, coordinator and
    # worker alike, refuses one: a job definition registered outside biffix's control still
    # cannot get a privileged container on hosts every project shares.
    harden = ec2.UserData.for_linux()
    harden.add_commands(f"echo {DISABLE_PRIVILEGED} >> /etc/ecs/ecs.config")
    install = ec2.UserData.for_linux()
    install.add_commands(
        *host_install_header(cli, bucket_name=bucket_name, region=region),
        INSTALL_AWS_CLI.read_text(),
    )
    user_data = ec2.MultipartUserData()
    if single_job:
        user_data.add_part(single_job_boothook())
    user_data.add_part(ec2.MultipartBody.from_user_data(harden))
    user_data.add_part(ec2.MultipartBody.from_user_data(install))
    return user_data


def single_job_seccomp_profile() -> dict[str, Any]:
    """Docker's default seccomp profile on single-job hosts: moby's default
    (`MOBY_SECCOMP_DEFAULT`) plus one rule allowing `SINGLE_JOB_SYSCALLS` unconditionally.

    Moby's default allows these only to a container holding CAP_SYS_ADMIN, which no job gets. The
    rule lets any container on these hosts create user namespaces and mount inside them: no host
    capability, and container root still cannot get past `/proc` masking, but it exposes the kernel
    code reachable through user namespaces. So only a single-job queue-class's own compute
    environment (one run per host, on-demand) boots with it."""
    profile: dict[str, Any] = json.loads(MOBY_SECCOMP_DEFAULT.read_text())
    profile["syscalls"].append({"names": list(SINGLE_JOB_SYSCALLS), "action": "SCMP_ACT_ALLOW"})
    return profile


def single_job_profile_json() -> str:
    """The profile as the boot script writes it: compact JSON."""
    return json.dumps(single_job_seccomp_profile(), separators=(",", ":"))


def single_job_boot_header(
    *,
    scratch_dir: str = SCRATCH_DIR,
    marker: str = INSTANCE_STORE_MARKER,
    profile_path: str = SINGLE_JOB_SECCOMP_PROFILE,
) -> list[str]:
    """The variable assignments the single-job boot scripts (`seccomp_profile.sh`,
    `scratch_disks.sh`) read, as the user data sets them (and the tests run them with)."""
    profile = single_job_profile_json()
    # Single-quoted as is: moby's profile has no quote of that kind to escape.
    if "'" in profile:
        raise ValueError("the seccomp profile has a single quote; quote it for the shell")
    return [
        f"BIFFIX_SECCOMP_PROFILE={profile_path}",
        f"BIFFIX_SECCOMP_PROFILE_JSON='{profile}'",
        f"BIFFIX_SCRATCH_DIR={scratch_dir}",
        f"BIFFIX_INSTANCE_STORE_MARKER={marker}",
    ]


def _strip_comments(path: Path) -> str:
    """`path`'s shell without its whole-line comments. Lines inside a here-document are kept as
    they are: they are its input, not shell."""
    kept: list[str] = []
    heredoc_end: str | None = None
    for line in path.read_text().splitlines(keepends=True):
        if heredoc_end is not None:
            kept.append(line)
            if line.strip() == heredoc_end:
                heredoc_end = None
            continue
        if line.lstrip().startswith("#"):
            continue
        kept.append(line)
        opened = re.search(r"<<-?\s*'?\"?(\w+)", line)
        if opened:
            heredoc_end = opened.group(1)
    return "".join(kept)


def single_job_boot_script() -> str:
    """The script a single-job host runs at boot: the header, then `seccomp_profile.sh` and
    `scratch_disks.sh` without their whole-line shell comments (they stay in the source; the user
    data only has 16 KB)."""
    return "\n".join(
        [
            "#!/bin/bash",
            *single_job_boot_header(),
            _strip_comments(SECCOMP_PROFILE_SCRIPT) + _strip_comments(SCRATCH_DISKS_SCRIPT),
        ]
    )


def single_job_boothook() -> ec2.MultipartBody:
    """A single-job host's seccomp profile and `/scratch`, as one cloud-init BOOTHOOK part.

    Both must be in place before Docker starts (dockerd reads daemon.json's `seccomp-profile` only
    when it starts) and before the ECS agent can place a job (whose `/scratch` host volume would
    otherwise be a bare directory on the root volume). A boothook runs in cloud-init's network stage
    (cloud-init.service; cloud-init-network.service in newer cloud-init), which is ordered
    `Before=sysinit.target`; docker.service and docker.socket cannot start before sysinit.target
    (systemd's default dependencies; docker.service is also `After=network-online.target`), and the
    agent's ecs.service is `After=docker.service cloud-final.service`. So both are done before
    Docker or the agent runs, and Docker never restarts. A shell-script part (what Batch's own part
    and the two parts here are) runs later, in cloud-final, after Docker has started: AWS's own
    daemon.json example must then `systemctl restart docker --no-block`, which restarts the agent
    with it (`PartOf=docker.service`) mid-boot. AWS Batch's launch-template guide names this very
    split: "a cloud boothook that configures the Docker daemon" beside "a user data shell script"
    for the agent. Boothooks run on every boot; both scripts are written for that.

    EC2 caps user data at 16 KB (the MIME text, which Batch's own part joins), and the profile
    alone is 9 KB of JSON. So the part is a short shim, not base64-transfer-encoded, carrying the
    boot script (`single_job_boot_script`) gzipped (with no timestamp, so the launch template is the
    same bytes on every synth) and base64-encoded; it unpacks it to `SINGLE_JOB_BOOT_SCRIPT` and
    runs it."""
    gz = gzip.compress(single_job_boot_script().encode(), compresslevel=9, mtime=0)
    encoded = base64.encodebytes(gz).decode()
    shim = "\n".join(
        [
            "#!/bin/bash",
            "# biffix single-job host: seccomp profile and /scratch, before Docker starts",
            "# (infra/biffix_infra/host/seccomp_profile.sh and scratch_disks.sh, gzipped).",
            # A script that did not unpack whole is never run.
            "set -o pipefail",
            f"mkdir -p {posixpath.dirname(SINGLE_JOB_BOOT_SCRIPT)}",
            f"if base64 -d <<'EOF' | gunzip > {SINGLE_JOB_BOOT_SCRIPT}; then",
            encoded + "EOF",
            f"  bash {SINGLE_JOB_BOOT_SCRIPT}",
            "else",
            # Only the boot script removes a marker an earlier boot left (on the root volume, it
            # outlives a reboot), so without it the marker would vouch for unmounted scratch.
            f"  rm -f {INSTANCE_STORE_MARKER}",
            '  echo "biffix: single-job boot script did not unpack"',
            "fi",
            "",
        ]
    )
    return ec2.MultipartBody.from_raw_body(content_type=ec2.MultipartBody.CLOUD_BOOTHOOK, body=shim)
