# shellcheck shell=bash
# Mount a single-job host's instance-store NVMe disks at $BIFFIX_SCRATCH_DIR as one XFS file system:
# one disk directly, several striped (RAID0). The single-job template mounts it into the job as its
# work root (`/scratch`): the engine's work dir, the SIF cache, every intermediate.
#
# Runs as a cloud-init boothook, so before Docker and the ECS agent start (host_boot.py says
# why): no job can be placed on the host before its scratch is mounted.
#
# host_boot.py prepends BIFFIX_SCRATCH_DIR (/scratch) and BIFFIX_INSTANCE_STORE_MARKER
# (/var/lib/biffix/instance-store): the file a successful mount leaves, so it is there only while
# the instance store is mounted. It is root's, on the root volume, outside the run-writable scratch;
# the single-job template mounts its directory into the job read-only. A single-job run refuses to
# start without it rather than fill the root volume.
#
# Only what the ECS-optimized AL2023 AMI already has, so a boot needs no package install: neither
# mdadm nor lvm2 is on it, so disks are striped with device-mapper's `striped` target (dmsetup,
# from the device-mapper package systemd needs), formatted with xfsprogs (AL2023's root file
# system is XFS), and found through sysfs (no nvme-cli).
#
# Every boot builds it afresh: a device-mapper table is not persistent, NVMe names may come back in
# another order, and nothing on it outlives its run (a run on a rebooted host has lost its job, and
# Batch's retry starts the run from scratch). Already mounted (the hook run twice in one boot), it
# is left as it is.
#
# A failure is logged but does not fail the boot (a failing boot would have Batch replace the host
# in a loop): the mount point stays a directory on the root volume, with no marker
# ($BIFFIX_INSTANCE_STORE_MARKER, written only once the file system is mounted), so a run placed on
# the host refuses to start instead of filling the root volume.

BIFFIX_SCRATCH_LOG="${BIFFIX_SCRATCH_LOG:-/var/log/biffix-scratch.log}"
# Where the disks are found; tests point these elsewhere.
BIFFIX_SYS_BLOCK="${BIFFIX_SYS_BLOCK:-/sys/block}"
BIFFIX_DEV_DIR="${BIFFIX_DEV_DIR:-/dev}"
# The device-mapper device the disks are striped into.
BIFFIX_SCRATCH_DM_NAME=biffix-scratch
# The stripe's chunk, in 512-byte sectors (512 KiB: mdadm's default chunk for RAID0).
BIFFIX_SCRATCH_CHUNK_SECTORS=1024

# The instance-store disks, one device path per line, by name. Nitro reports each NVMe
# controller's model: instance store is "Amazon EC2 NVMe Instance Storage", EBS (the root
# volume) "Amazon Elastic Block Store".
biffix_instance_store_disks() {
  local block model
  for block in "$BIFFIX_SYS_BLOCK"/nvme*n1; do
    [ -e "$block/device/model" ] || continue
    model="$(tr -s ' ' < "$block/device/model")"
    model="${model% }"
    if [ "$model" = "Amazon EC2 NVMe Instance Storage" ]; then
      echo "$BIFFIX_DEV_DIR/$(basename "$block")"
    fi
  done
}

# The size of `$1` (a device path), in 512-byte sectors: a positive whole number, or a failure.
biffix_disk_sectors() {
  local sectors
  sectors="$(cat "$BIFFIX_SYS_BLOCK/$(basename "$1")/size")" || return 1
  case "$sectors" in
    '' | *[!0-9]* | 0)
      echo "$1: size '$sectors' is not a sector count" >&2
      return 1
      ;;
  esac
  echo "$sectors"
}

# Stripe the disks given as arguments into one device-mapper device; print its path. Called in a
# command substitution, which does not inherit errexit: every step returns on its own failure.
biffix_stripe() {
  local n=$# chunk=$BIFFIX_SCRATCH_CHUNK_SECTORS smallest="" sectors disk
  for disk in "$@"; do
    sectors="$(biffix_disk_sectors "$disk")" || return 1
    if [ -z "$smallest" ] || [ "$sectors" -lt "$smallest" ]; then
      smallest=$sectors
    fi
  done
  # Each disk contributes the same whole number of chunks.
  local length=$(( n * (smallest / chunk) * chunk ))
  if [ "$length" -le 0 ]; then
    echo "the smallest disk ($smallest sectors) holds no whole $chunk-sector chunk" >&2
    return 1
  fi
  local table="0 $length striped $n $chunk"
  for disk in "$@"; do
    table="$table $disk 0"
  done
  echo "striping $n disks: $table" >&2
  # No udev sync: a boothook runs before udev is sure to be up, and the node is made below.
  dmsetup create "$BIFFIX_SCRATCH_DM_NAME" --noudevsync --table "$table" >&2 || return 1
  dmsetup mknodes "$BIFFIX_SCRATCH_DM_NAME" >&2 || return 1
  echo "$BIFFIX_DEV_DIR/mapper/$BIFFIX_SCRATCH_DM_NAME"
}

biffix_mount_scratch() {
  # Command substitutions (biffix_stripe's) keep errexit too.
  shopt -s inherit_errexit
  mkdir -p "$BIFFIX_SCRATCH_DIR"
  if mountpoint -q "$BIFFIX_SCRATCH_DIR"; then
    echo "$BIFFIX_SCRATCH_DIR is already mounted"
    return 0
  fi
  # A marker left from an earlier boot (it is on the root volume) would vouch for scratch that is
  # not there.
  rm -f "$BIFFIX_INSTANCE_STORE_MARKER"
  # The kernel has found the NVMe controllers long before cloud-init runs; this only waits out
  # any udev work still queued for them.
  udevadm settle --timeout=30 || true
  local disks=() disk
  while IFS= read -r disk; do
    disks+=("$disk")
  done < <(biffix_instance_store_disks)
  echo "instance-store disks: ${disks[*]:-none}"
  local device
  case "${#disks[@]}" in
    0)
      echo "no instance-store disk: $BIFFIX_SCRATCH_DIR is on the root volume"
      return 1
      ;;
    1) device="${disks[0]}" ;;
    *) device="$(biffix_stripe "${disks[@]}")" ;;
  esac
  # -K: skip the discard pass, as a new instance-store disk reads as zeros already.
  mkfs.xfs -f -K -L "$BIFFIX_SCRATCH_DM_NAME" "$device"
  mount -o noatime "$device" "$BIFFIX_SCRATCH_DIR"
  # Whichever user a job's tasks run as may write to it (like /tmp).
  chmod 1777 "$BIFFIX_SCRATCH_DIR"
  # Last, so it exists only when every step above succeeded: root's, readable by all, naming the
  # device it vouches for. Written whole, then moved into place.
  local marker_dir
  marker_dir="$(dirname "$BIFFIX_INSTANCE_STORE_MARKER")"
  mkdir -p "$marker_dir"
  chmod 0755 "$marker_dir"
  printf '%s\n' "$device" > "$BIFFIX_INSTANCE_STORE_MARKER.tmp"
  chmod 0644 "$BIFFIX_INSTANCE_STORE_MARKER.tmp"
  mv -f "$BIFFIX_INSTANCE_STORE_MARKER.tmp" "$BIFFIX_INSTANCE_STORE_MARKER"
}

( set -euo pipefail; biffix_mount_scratch ) >> "$BIFFIX_SCRATCH_LOG" 2>&1
biffix_rc=$?
if [ "$biffix_rc" -eq 0 ]; then
  echo "biffix: scratch mounted at $BIFFIX_SCRATCH_DIR"
else
  echo "biffix: scratch FAILED (exit $biffix_rc); $BIFFIX_SCRATCH_DIR is on the root volume;" \
    "see $BIFFIX_SCRATCH_LOG"
  tail -n 20 "$BIFFIX_SCRATCH_LOG"
fi
