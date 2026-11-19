"""A single-job host's boothook scripts (`biffix_infra/host/seccomp_profile.sh` and
`scratch_disks.sh`), run for real against the header the stack emits, with stand-ins for the
commands that touch the host's devices (dmsetup, mkfs.xfs, mount, mountpoint, udevadm) and a fake
`/sys/block`."""

from __future__ import annotations

import hashlib
import json
import os
import subprocess
from pathlib import Path
from typing import Any

import pytest

from biffix_infra.host_boot import (
    INSTANCE_STORE_MARKER,
    MOBY_SECCOMP_DEFAULT,
    SCRATCH_DIR,
    SINGLE_JOB_SECCOMP_PROFILE,
    SINGLE_JOB_SYSCALLS,
    single_job_boot_header,
    single_job_boot_script,
    single_job_profile_json,
    single_job_seccomp_profile,
)
from biffix_infra.paths import HOST_DIR
from tests.helpers import executable

SECCOMP = HOST_DIR / "seccomp_profile.sh"
SCRATCH = HOST_DIR / "scratch_disks.sh"

# moby/moby v25.0.18 profiles/seccomp/default.json, byte for byte: replacing the vendored base is a
# deliberate change, made here too.
MOBY_V25_0_18_SHA256 = "9c1025c88ccaa517b648da571961838744ea2137f176bfe6a48b21294cae9c76"

INSTANCE_STORE = "Amazon EC2 NVMe Instance Storage                 \n"
EBS = "Amazon Elastic Block Store                        \n"


def test_the_vendored_moby_profile_is_moby_s() -> None:
    assert hashlib.sha256(MOBY_SECCOMP_DEFAULT.read_bytes()).hexdigest() == MOBY_V25_0_18_SHA256


def test_the_profile_is_moby_s_default_plus_one_allow_rule() -> None:
    moby = json.loads(MOBY_SECCOMP_DEFAULT.read_text())
    profile = single_job_seccomp_profile()
    *rules, extra = profile["syscalls"]
    assert rules == moby["syscalls"]
    assert extra == {"names": ["mount", "umount2", "unshare", "setns"], "action": "SCMP_ACT_ALLOW"}
    assert {k: v for k, v in profile.items() if k != "syscalls"} == {
        k: v for k, v in moby.items() if k != "syscalls"
    }
    # Default deny stays: only the four syscalls are added.
    assert profile["defaultAction"] == "SCMP_ACT_ERRNO"


def test_the_header_carries_the_profile_as_the_shell_reads_it() -> None:
    header = "\n".join(single_job_boot_header())
    echoed = subprocess.run(
        ["bash", "-c", f'{header}\nprintf %s "$BIFFIX_SECCOMP_PROFILE_JSON"'],
        capture_output=True,
        text=True,
        check=True,
    ).stdout
    assert echoed == single_job_profile_json()


def test_the_boot_script_runs_both_scripts_without_their_comments() -> None:
    script = single_job_boot_script()
    assert script.startswith("#!/bin/bash\n")
    assert "\n#" not in script
    for function in ("biffix_write_seccomp_profile", "biffix_mount_scratch"):
        assert f"\n{function}() {{\n" in script, function
        assert f"set -euo pipefail; {function} )" in script, function
    assert script.index("biffix_write_seccomp_profile") < script.index("biffix_mount_scratch")


def test_the_boot_script_as_shipped_sets_up_the_profile_and_scratch(tmp_path: Path) -> None:
    # The comment-stripped script the user data carries, with its paths moved under tmp_path.
    profile = tmp_path / "etc/docker/seccomp-biffix-single-job.json"
    scratch = tmp_path / "scratch"
    marker = _marker(tmp_path)
    script = (
        single_job_boot_script()
        .replace(
            f"BIFFIX_SECCOMP_PROFILE={SINGLE_JOB_SECCOMP_PROFILE}\n",
            f"BIFFIX_SECCOMP_PROFILE={profile}\n",
        )
        .replace(f"BIFFIX_SCRATCH_DIR={SCRATCH_DIR}\n", f"BIFFIX_SCRATCH_DIR={scratch}\n")
        .replace(
            f"BIFFIX_INSTANCE_STORE_MARKER={INSTANCE_STORE_MARKER}\n",
            f"BIFFIX_INSTANCE_STORE_MARKER={marker}\n",
        )
    )
    assert str(profile) in script and str(scratch) in script and str(marker) in script
    result, ran = _scratch(tmp_path, {"nvme1n1": (INSTANCE_STORE, 900_000_000)}, script=script)
    assert result.returncode == 0, result
    assert "default seccomp profile is" in result.stdout
    assert "scratch mounted" in result.stdout
    assert json.loads(profile.read_text()) == single_job_seccomp_profile()
    assert ran[-1] == f"mount -o noatime /dev/nvme1n1 {scratch}"
    assert marker.read_text() == "/dev/nvme1n1\n"


def _seccomp(tmp_path: Path, daemon_json: str | None = None) -> subprocess.CompletedProcess[str]:
    docker = tmp_path / "etc/docker"
    docker.mkdir(parents=True)
    if daemon_json is not None:
        (docker / "daemon.json").write_text(daemon_json)
    header = "\n".join(
        single_job_boot_header(
            scratch_dir=str(tmp_path / "scratch"),
            profile_path=str(docker / "seccomp-biffix-single-job.json"),
        )
    )
    boot = tmp_path / "boot.sh"
    boot.write_text(f"#!/bin/bash\n{header}\n{SECCOMP.read_text()}")
    env = {
        **os.environ,
        "BIFFIX_DOCKER_DAEMON_JSON": str(docker / "daemon.json"),
        "BIFFIX_SECCOMP_LOG": str(tmp_path / "seccomp.log"),
    }
    return subprocess.run(["bash", str(boot)], env=env, capture_output=True, text=True)


def test_the_profile_is_written_and_named_in_daemon_json(tmp_path: Path) -> None:
    result = _seccomp(tmp_path)
    assert result.returncode == 0, result
    assert "default seccomp profile is" in result.stdout
    profile_path = tmp_path / "etc/docker/seccomp-biffix-single-job.json"
    assert json.loads(profile_path.read_text()) == single_job_seccomp_profile()
    daemon = json.loads((tmp_path / "etc/docker/daemon.json").read_text())
    assert daemon == {"seccomp-profile": str(profile_path)}


def test_daemon_json_keeps_its_other_settings(tmp_path: Path) -> None:
    result = _seccomp(tmp_path, daemon_json='{"debug": true, "seccomp-profile": "/old.json"}')
    assert result.returncode == 0, result
    daemon = json.loads((tmp_path / "etc/docker/daemon.json").read_text())
    profile_path = tmp_path / "etc/docker/seccomp-biffix-single-job.json"
    assert daemon == {"debug": True, "seccomp-profile": str(profile_path)}


def test_running_twice_writes_the_same_files(tmp_path: Path) -> None:
    # A boothook runs on every boot.
    assert _seccomp(tmp_path).returncode == 0
    docker = tmp_path / "etc/docker"
    first = {p.name: p.read_bytes() for p in docker.iterdir()}
    boot = tmp_path / "boot.sh"
    env = {
        **os.environ,
        "BIFFIX_DOCKER_DAEMON_JSON": str(docker / "daemon.json"),
        "BIFFIX_SECCOMP_LOG": str(tmp_path / "seccomp.log"),
    }
    assert subprocess.run(["bash", str(boot)], env=env, capture_output=True).returncode == 0
    assert {p.name: p.read_bytes() for p in docker.iterdir()} == first


def test_a_bad_profile_never_reaches_daemon_json(tmp_path: Path) -> None:
    # Dockerd refuses to start on a `seccomp-profile` it cannot load, so the host would never join:
    # a corrupt payload leaves daemon.json alone and the boot goes on.
    docker = tmp_path / "etc/docker"
    docker.mkdir(parents=True)
    boot = tmp_path / "boot.sh"
    boot.write_text(
        "#!/bin/bash\n"
        f"BIFFIX_SECCOMP_PROFILE={docker}/seccomp-biffix-single-job.json\n"
        "BIFFIX_SECCOMP_PROFILE_JSON='{\"defaultAction\": '\n"
        f"{SECCOMP.read_text()}"
    )
    env = {
        **os.environ,
        "BIFFIX_DOCKER_DAEMON_JSON": str(docker / "daemon.json"),
        "BIFFIX_SECCOMP_LOG": str(tmp_path / "seccomp.log"),
    }
    result = subprocess.run(["bash", str(boot)], env=env, capture_output=True, text=True)
    assert result.returncode == 0
    assert "seccomp profile FAILED" in result.stdout
    assert not (docker / "daemon.json").exists()
    assert not (docker / "seccomp-biffix-single-job.json").exists()


def _check_profile(tmp_path: Path, profile: dict[str, Any]) -> subprocess.CompletedProcess[str]:
    """Run seccomp_profile.sh with `profile` as its payload."""
    docker = tmp_path / "etc/docker"
    docker.mkdir(parents=True)
    payload = json.dumps(profile, separators=(",", ":"))
    assert "'" not in payload
    boot = tmp_path / "boot.sh"
    boot.write_text(
        "#!/bin/bash\n"
        f"BIFFIX_SECCOMP_PROFILE={docker}/seccomp-biffix-single-job.json\n"
        f"BIFFIX_SECCOMP_PROFILE_JSON='{payload}'\n"
        f"{SECCOMP.read_text()}"
    )
    env = {
        **os.environ,
        "BIFFIX_DOCKER_DAEMON_JSON": str(docker / "daemon.json"),
        "BIFFIX_SECCOMP_LOG": str(tmp_path / "seccomp.log"),
    }
    return subprocess.run(["bash", str(boot)], env=env, capture_output=True, text=True)


def test_moby_s_own_rule_does_not_pass_for_the_biffix_rule(tmp_path: Path) -> None:
    # Moby's default allows the four syscalls too, but only with CAP_SYS_ADMIN (`includes`), which
    # no job holds: moby's profile unmodified is refused, and daemon.json is left alone.
    result = _check_profile(tmp_path, json.loads(MOBY_SECCOMP_DEFAULT.read_text()))
    assert result.returncode == 0
    assert "seccomp profile FAILED" in result.stdout
    assert "no unconditional rule" in result.stdout
    docker = tmp_path / "etc/docker"
    assert not (docker / "daemon.json").exists()
    assert not (docker / "seccomp-biffix-single-job.json").exists()


@pytest.mark.parametrize(
    "rule",
    [
        # Conditioned on an argument, on a capability or an architecture, or missing a syscall.
        {"names": list(SINGLE_JOB_SYSCALLS), "action": "SCMP_ACT_ALLOW", "args": [{"index": 0}]},
        {
            "names": list(SINGLE_JOB_SYSCALLS),
            "action": "SCMP_ACT_ALLOW",
            "includes": {"caps": ["CAP_SYS_ADMIN"]},
        },
        {
            "names": list(SINGLE_JOB_SYSCALLS),
            "action": "SCMP_ACT_ALLOW",
            "excludes": {"arches": ["amd64"]},
        },
        {"names": ["mount", "umount2", "unshare"], "action": "SCMP_ACT_ALLOW"},
        {"names": list(SINGLE_JOB_SYSCALLS), "action": "SCMP_ACT_ERRNO"},
    ],
)
def test_only_an_unconditional_rule_for_all_four_passes(
    tmp_path: Path, rule: dict[str, Any]
) -> None:
    profile = json.loads(MOBY_SECCOMP_DEFAULT.read_text())
    profile["syscalls"].append(rule)
    result = _check_profile(tmp_path, profile)
    assert "seccomp profile FAILED" in result.stdout, result
    assert not (tmp_path / "etc/docker/daemon.json").exists()


def test_the_profile_check_wants_every_syscall() -> None:
    # The script's own check names the same four the stack adds.
    for name in SINGLE_JOB_SYSCALLS:
        assert f'"{name}"' in SECCOMP.read_text()


# ── scratch ──


def _marker(tmp_path: Path) -> Path:
    """Where the tests put the instance-store marker: `INSTANCE_STORE_MARKER` under tmp_path."""
    return tmp_path / INSTANCE_STORE_MARKER.lstrip("/")


_STUB = """#!/bin/bash
printf '%s %s\\n' "$(basename "$0")" "$*" >> "$STUB_LOG"
"""
# mkfs.xfs refuses a label longer than an XFS label holds (12 bytes), as the real one does.
_MKFS_XFS_LABEL_CHECK = """while [ $# -gt 0 ]; do
  if [ "$1" = -L ] && [ "${#2}" -gt 12 ]; then echo "label $2 is too long" >&2; exit 1; fi
  shift
done
"""


def _scratch(
    tmp_path: Path,
    disks: dict[str, tuple[str, int | str]],
    *,
    mounted: bool = False,
    script: str | None = None,
    failing: tuple[str, ...] = (),
) -> tuple[subprocess.CompletedProcess[str], list[str]]:
    """Run scratch_disks.sh (or `script`) with `/sys/block` holding `disks` (name → (model,
    sectors)) and the device commands stubbed (each in `failing` exits 1); return the result and
    the stubbed commands it ran, in order."""
    sys_block = tmp_path / "sys/block"
    for name, (model, sectors) in disks.items():
        (sys_block / name / "device").mkdir(parents=True)
        (sys_block / name / "device/model").write_text(model)
        (sys_block / name / "size").write_text(f"{sectors}\n")
    stubs = tmp_path / "stubs"
    for cmd in ("dmsetup", "mkfs.xfs", "mount", "udevadm"):
        check = _MKFS_XFS_LABEL_CHECK if cmd == "mkfs.xfs" else ""
        executable(stubs / cmd, _STUB + check + ("exit 1\n" if cmd in failing else ""))
    executable(stubs / "mountpoint", _STUB + ("exit 0\n" if mounted else "exit 1\n"))
    scratch = tmp_path / "scratch"
    header = "\n".join(
        single_job_boot_header(scratch_dir=str(scratch), marker=str(_marker(tmp_path)))
    )
    boot = tmp_path / "boot.sh"
    boot.write_text(script or f"#!/bin/bash\n{header}\n{SCRATCH.read_text()}")
    log = tmp_path / "stub.log"
    env = {
        **os.environ,
        "PATH": f"{stubs}:{os.environ['PATH']}",
        "STUB_LOG": str(log),
        "BIFFIX_SYS_BLOCK": str(sys_block),
        "BIFFIX_DEV_DIR": "/dev",
        "BIFFIX_SCRATCH_LOG": str(tmp_path / "scratch.log"),
        "BIFFIX_DOCKER_DAEMON_JSON": str(tmp_path / "etc/docker/daemon.json"),
        "BIFFIX_SECCOMP_LOG": str(tmp_path / "seccomp.log"),
    }
    result = subprocess.run(["bash", str(boot)], env=env, capture_output=True, text=True)
    ran = log.read_text().splitlines() if log.exists() else []
    return result, [line for line in ran if not line.startswith(("udevadm", "mountpoint"))]


def test_one_instance_store_disk_is_formatted_and_mounted_directly(tmp_path: Path) -> None:
    result, ran = _scratch(
        tmp_path, {"nvme0n1": (EBS, 60_000_000), "nvme1n1": (INSTANCE_STORE, 900_000_000)}
    )
    assert result.returncode == 0, result
    assert "scratch mounted" in result.stdout
    scratch = tmp_path / "scratch"
    assert ran == [
        "mkfs.xfs -f -K -L bfx-scratch /dev/nvme1n1",
        f"mount -o noatime /dev/nvme1n1 {scratch}",
    ]
    assert scratch.stat().st_mode & 0o7777 == 0o1777
    # The marker, written last, outside the run-writable scratch: the coordinator refuses a
    # single-job run without it. Readable by all, writable by its owner (root, on a host) only,
    # naming the device it vouches for; nothing is written on the scratch itself.
    marker = _marker(tmp_path)
    assert marker.read_text() == "/dev/nvme1n1\n"
    assert marker.stat().st_mode & 0o777 == 0o644
    assert marker.parent.stat().st_mode & 0o777 == 0o755
    assert list(scratch.iterdir()) == []


def test_several_disks_are_striped_with_device_mapper(tmp_path: Path) -> None:
    # Never the EBS root; each disk gives the same whole number of 1024-sector chunks.
    result, ran = _scratch(
        tmp_path,
        {
            "nvme0n1": (EBS, 60_000_000),
            "nvme1n1": (INSTANCE_STORE, 1_000_000_500),
            "nvme2n1": (INSTANCE_STORE, 1_000_000_000),
        },
    )
    assert result.returncode == 0, result
    assert "scratch mounted" in result.stdout
    length = 2 * (1_000_000_000 // 1024) * 1024
    table = f"0 {length} striped 2 1024 /dev/nvme1n1 0 /dev/nvme2n1 0"
    device = "/dev/mapper/biffix-scratch"
    assert ran == [
        f"dmsetup create biffix-scratch --noudevsync --table {table}",
        "dmsetup mknodes biffix-scratch",
        f"mkfs.xfs -f -K -L bfx-scratch {device}",
        f"mount -o noatime {device} {tmp_path / 'scratch'}",
    ]
    assert _marker(tmp_path).read_text() == f"{device}\n"


def test_a_mounted_scratch_is_left_alone(tmp_path: Path) -> None:
    result, ran = _scratch(tmp_path, {"nvme1n1": (INSTANCE_STORE, 900_000_000)}, mounted=True)
    assert result.returncode == 0, result
    assert ran == []


@pytest.mark.parametrize("disks", [{}, {"nvme0n1": (EBS, 60_000_000)}])
def test_no_instance_store_is_reported_but_does_not_fail_the_boot(
    tmp_path: Path, disks: dict[str, tuple[str, int | str]]
) -> None:
    # A marker left from an earlier boot (it is on the root volume) is removed: it would vouch for
    # instance store that is not there.
    (tmp_path / "scratch").mkdir()
    _marker(tmp_path).parent.mkdir(parents=True)
    _marker(tmp_path).touch()
    result, ran = _scratch(tmp_path, disks)
    assert result.returncode == 0
    assert "scratch FAILED" in result.stdout
    assert "no instance-store disk" in result.stdout
    assert ran == []
    assert (tmp_path / "scratch").is_dir()
    assert not _marker(tmp_path).exists()


def _two_disks(size: int | str = 1_000_000_000) -> dict[str, tuple[str, int | str]]:
    return {"nvme1n1": (INSTANCE_STORE, 1_000_000_000), "nvme2n1": (INSTANCE_STORE, size)}


def test_a_failed_stripe_is_never_formatted(tmp_path: Path) -> None:
    # biffix_stripe runs in a command substitution: its dmsetup failing fails the mount, never
    # hands mkfs.xfs a device that does not exist.
    result, ran = _scratch(tmp_path, _two_disks(), failing=("dmsetup",))
    assert result.returncode == 0
    assert "scratch FAILED" in result.stdout, result
    assert [line.split()[0] for line in ran] == ["dmsetup"]
    assert not _marker(tmp_path).exists()


@pytest.mark.parametrize("size", ["", "garbage", "0", "512"])
def test_a_disk_of_no_usable_size_is_never_striped(tmp_path: Path, size: str) -> None:
    # Not a count, or smaller than one stripe chunk: nothing is created or formatted.
    result, ran = _scratch(tmp_path, _two_disks(size))
    assert "scratch FAILED" in result.stdout, result
    assert ran == []
    assert not _marker(tmp_path).exists()


@pytest.mark.parametrize("failing", ["mkfs.xfs", "mount"])
def test_a_failed_format_or_mount_leaves_no_marker(tmp_path: Path, failing: str) -> None:
    result, _ = _scratch(tmp_path, {"nvme1n1": (INSTANCE_STORE, 900_000_000)}, failing=(failing,))
    assert "scratch FAILED" in result.stdout, result
    assert not _marker(tmp_path).exists()
