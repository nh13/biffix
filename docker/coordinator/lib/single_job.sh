# shellcheck shell=bash
# biffix coordinator: the run's execution mode, and a single-job run's host. Sourced by
# entrypoint.sh; defines functions only.
#   Reads: LIB_DIR, WORK, ENGINE, BIFFIX_EXECUTION, BIFFIX_SINGLE_JOB_CPUS,
#          BIFFIX_SINGLE_JOB_MEMORY_MIB, BIFFIX_SCRATCH_ROOT and BIFFIX_INSTANCE_STORE_MARKER (both
#          set by the single-job job definition), BIFFIX_RESUME, BIFFIX_DRY_RUN,
#          BIFFIX_IMAGE_MAP_FILE, BIFFIX_WORKER_IMAGE, BIFFIX_SIF_PARALLEL, ENGINE_STOPPED_BY
#          (lib/signals.sh), BIFFIX_OOM_SCORE_ADJ_FILE (a test hook: the file a task's oom score is
#          written to, /proc/self/oom_score_adj).
#   Sets:  EXECUTION; for a single-job run WORK (under the scratch root), SCRATCH, SJ_MARKER, SJ_CPUS,
#          SJ_MEMORY_MIB, SJ_TASK_MEMORY_MIB and SJ_JVM_HEAP_MIB (single_job_memory), SJ_IMAGES
#          (the task image files), SJ_TASK_TMP, SJ_BIN (the apptainer wrapper's directory, which
#          the engine gets first on PATH), APPTAINER_CACHEDIR and APPTAINER_TMPDIR; FAILURE_FILE,
#          FAILURE_LABEL and rc (when a task image cannot be converted, or a step before the engine
#          fails: run_setup_step).

# A single-job run (BIFFIX_EXECUTION=single_job) is the whole run in this one Batch job, on a host of
# its own: the engine runs every task itself (its local executor), each in the image its task on
# AWS Batch would run (Nextflow: the process's container; Snakemake: the rule's
# aws_batch_container_image, else the version's default image), through Apptainer nested in this
# container, with the host's instance store (/scratch) as the work root. A task with no image (a
# Snakemake local rule, a rule with no image when the version has no default one, a Nextflow
# `exec:` process) runs in this container, as it would beside a Batch run.
# The job runs as the project's run role and submits no Batch jobs, so it needs no worker queue
# and no task job definitions. Apptainer runs without setuid (the image's apptainer.conf), in user
# namespaces the single-job hosts' seccomp profile allows; the job gets /dev/fuse for its image
# mounts. What a task does NOT get, as it would on AWS Batch: a CPU or memory limit of its own (the
# container's cgroups are read-only, so `apptainer --memory/--cpus` cannot work), a PID namespace of
# its own (Apptainer's --pid cannot mount /proc under Docker's masked paths), or an environment of
# its own beyond what each engine passes (Nextflow's `env -`; Snakemake's --cleanenv, below). Those
# keep the job's environment from being inherited, but not from being read: a task runs as the
# engine's user in the engine's PID namespace, so it can read the engine's /proc/<pid>/environ,
# the run's capability token with it (which authorizes only this run's event POSTs).
#
# The engine's share of memory: each engine is given the job's size less a reserve for itself
# (lib/nextflow.sh, lib/snakemake.sh), and every task raises its own oom_score_adj (the apptainer
# wrapper below), so when the job runs out of memory the kernel kills a task, which the engine
# retries or reports, rather than the engine.

# is_single_job: whether this is a single-job run.
is_single_job() {
  [ "$EXECUTION" = single_job ]
}

# select_execution: the run's execution mode, checked before anything is staged (exit 78 for a
# single-job run without its size, one asked to resume, or an image without Apptainer). A
# single-job run's work root is under the host's scratch (the instance store), not the container's
# own /work.
select_execution() {
  EXECUTION="${BIFFIX_EXECUTION:-tasks}"
  case "$EXECUTION" in
    tasks) return 0 ;;
    single_job) ;;
    *) runtime_error "unknown execution mode BIFFIX_EXECUTION='$EXECUTION' (tasks or single_job)" ;;
  esac
  require_env BIFFIX_SINGLE_JOB_CPUS "the vCPUs a single-job run was given"
  require_env BIFFIX_SINGLE_JOB_MEMORY_MIB "the memory a single-job run was given"
  SJ_CPUS=$BIFFIX_SINGLE_JOB_CPUS SJ_MEMORY_MIB=$BIFFIX_SINGLE_JOB_MEMORY_MIB
  require_count BIFFIX_SINGLE_JOB_CPUS "$SJ_CPUS"
  require_count BIFFIX_SINGLE_JOB_MEMORY_MIB "$SJ_MEMORY_MIB"
  [ "$SJ_CPUS" -ge 1 ] || runtime_error "BIFFIX_SINGLE_JOB_CPUS must be at least 1: $SJ_CPUS"
  require_count BIFFIX_SIF_PARALLEL "${BIFFIX_SIF_PARALLEL:-4}"
  [ "${BIFFIX_SIF_PARALLEL:-4}" -ge 1 ] \
    || runtime_error "BIFFIX_SIF_PARALLEL must be at least 1: $BIFFIX_SIF_PARALLEL"
  single_job_memory
  # Its work dir dies with the job (and the host), so there is nothing to resume from.
  [ "${BIFFIX_RESUME:-}" != 1 ] || runtime_error "a single-job run cannot be resumed (BIFFIX_RESUME=1)"
  # The arm64 coordinator image has no Apptainer (apptainer publishes no arm64 package; see the
  # Dockerfile): a single-job queue-class of Graviton instances cannot run.
  command -v apptainer >/dev/null 2>&1 \
    || runtime_error "this coordinator image ($(uname -m)) has no apptainer: a single-job run needs an x86_64 instance type"
  # Canonical (no `..`, `//` or symlink in it), as everything under it is removed (clear_scratch).
  local given="${BIFFIX_SCRATCH_ROOT:-/scratch}"
  case "$given" in
    /*) SCRATCH=$(canonical_path "$given") ;;
    *) SCRATCH="" ;;
  esac
  case "$SCRATCH" in
    /?*) ;;
    *) runtime_error "the scratch root must be an absolute path other than /: '$given'" ;;
  esac
  WORK="$SCRATCH/run"
}

# canonical_path <path>: <path> absolute, with every `.`, `..`, repeated `/` and symlink resolved,
# whether or not it exists (GNU `realpath -m`; Python's where there is none, as on macOS).
canonical_path() {
  realpath -m -- "$1" 2>/dev/null \
    || python3 -c 'import os, sys; print(os.path.realpath(sys.argv[1]))' "$1"
}

# single_job_memory: split the job's memory between the engine and its tasks (exit 78 when the
# tasks would get less than 1 GiB): SJ_TASK_MEMORY_MIB, what the engine may give its tasks, and for
# Nextflow SJ_JVM_HEAP_MIB, its capped heap. The engine's reserve: Nextflow's heap (1/16 of the
# job, 1 to 8 GiB; the JVM would otherwise take a quarter of the job) and room for the JVM's own
# overhead and the image mounts (1/32, at least 1 GiB); Snakemake's (its DAG, the logger, the
# storage plugin) 1/16, 2 to 8 GiB.
single_job_memory() {
  local reserve
  if [ "$ENGINE" = nextflow ]; then
    local heap=$((SJ_MEMORY_MIB / 16)) slack=$((SJ_MEMORY_MIB / 32))
    [ "$heap" -ge 1024 ] || heap=1024
    [ "$heap" -le 8192 ] || heap=8192
    [ "$slack" -ge 1024 ] || slack=1024
    # shellcheck disable=SC2034 # read by lib/nextflow.sh
    SJ_JVM_HEAP_MIB=$heap
    reserve=$((heap + slack))
  else
    reserve=$((SJ_MEMORY_MIB / 16))
    [ "$reserve" -ge 2048 ] || reserve=2048
    [ "$reserve" -le 8192 ] || reserve=8192
  fi
  SJ_TASK_MEMORY_MIB=$((SJ_MEMORY_MIB - reserve))
  [ "$SJ_TASK_MEMORY_MIB" -ge 1024 ] \
    || runtime_error "BIFFIX_SINGLE_JOB_MEMORY_MIB=$SJ_MEMORY_MIB leaves ${SJ_TASK_MEMORY_MIB} MiB for tasks after the engine's ${reserve} MiB"
}

# The instance-store marker: the file the single-job hosts' boot script
# (infra/biffix_infra/host/scratch_disks.sh) writes once it has striped, formatted and mounted the
# instance store at the scratch root, outside the scratch root and owned by the host: the single-job
# job definition mounts its directory read-only and names it in BIFFIX_INSTANCE_STORE_MARKER.
# Without it, the scratch root is a directory of the host's root volume: too small for a run, and
# the host's own. A run cannot remove or forge it, as it could a file on the scratch root it writes
# to.

# scratch_lock: the file on the scratch root a run holds an exclusive lock on for its life
# (prepare_scratch), so that two runs never share one scratch root; kept when the scratch root is
# cleared.
scratch_lock() {
  printf '%s' .biffix-lock
}

# clear_scratch: remove everything under the scratch root but its lock file (a regular file of that
# name: anything else a run left there under the name goes too).
clear_scratch() {
  local lock
  lock=$(scratch_lock)
  find "$SCRATCH" -mindepth 1 -maxdepth 1 ! \( -name "$lock" -type f \) -exec rm -rf -- {} + \
    2>/dev/null
  [ -z "$(find "$SCRATCH" -mindepth 1 -maxdepth 1 ! \( -name "$lock" -type f \) -print -quit \
    2>/dev/null)" ]
}

# The single-job hosts are reused, by runs of every project: what a run leaves under the scratch
# root is that run's data, and its image files are ones its workflow could have changed. So the
# scratch root is cleared before this run stages anything there, and again (best-effort; the next
# run clears it anyway) when this one exits. Not when a signal stopped the run: a large work dir
# can take longer to remove than Batch's stop timeout leaves (its SIGKILL would cut the wipe short
# anyway), and the steps before it (the logs, the events replay) matter more.
# shellcheck disable=SC2329 # invoked by the EXIT trap below
clear_scratch_at_exit() {
  if [ -n "${ENGINE_STOPPED_BY:-}" ]; then
    echo "biffix-coordinator: stopped by a signal: leaving $SCRATCH for the next run to clear" >&2
    return 0
  fi
  cd / || return 0
  clear_scratch || echo "biffix-coordinator: WARNING: could not clear $SCRATCH at exit" >&2
  return 0
}

# prepare_scratch: for a single-job run, clear the scratch root (exit 78 when it is not a writable
# directory, the host has no instance-store marker, another run holds it, or it cannot be cleared)
# and lay out the run's directories under it. The wipe runs as a child a cancel stops
# (run_stoppable): a previous run's leftovers (a run a signal stopped leaves its own) can take a
# while to remove.
prepare_scratch() {
  is_single_job || return 0
  [ -d "$SCRATCH" ] && [ -w "$SCRATCH" ] \
    || runtime_error "the scratch root $SCRATCH is not a writable directory (the single-job host's instance store)"
  SJ_MARKER="${BIFFIX_INSTANCE_STORE_MARKER:-/var/lib/biffix/instance-store}"
  [ -f "$SJ_MARKER" ] \
    || runtime_error "$SCRATCH is not the host's instance store (no marker $SJ_MARKER): the instance type has no NVMe instance storage or the host's scratch setup failed"
  # The deploy gives each run a host of its own (one size per queue-class, a whole-host request);
  # should two runs ever share one, the second must not wipe the first's work. An exclusive flock
  # on the lock file, taken on a descriptor this shell keeps open (the lock belongs to the open
  # file, so it outlives the helper that takes it), holds for the life of the job and dies with it.
  # A previous run could have left anything under the lock file's name (it writes to the scratch
  # root): a link or a directory is removed, so it can neither redirect the lock nor block it, and
  # the file is emptied once held, so nothing a run wrote in it reaches the next.
  local lock
  lock="$SCRATCH/$(scratch_lock)"
  if [ -L "$lock" ] || { [ -e "$lock" ] && [ ! -f "$lock" ]; }; then
    rm -rf -- "$lock"
  fi
  exec {SJ_LOCK_FD}>>"$lock" \
    || runtime_error "cannot open the scratch root's lock file $lock"
  python3 -c '
import fcntl, os, sys
fd = int(sys.argv[1])
fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
os.ftruncate(fd, 0)' "$SJ_LOCK_FD" 2>/dev/null \
    || runtime_error "another run holds the scratch root $SCRATCH: a single-job host must run one run at a time"
  echo "biffix-coordinator: clearing $SCRATCH (a host is reused across runs)" >&2
  run_stoppable "the scratch root's wipe" TERM clear_scratch
  exit_if_stopped
  [ "$rc" = 0 ] || runtime_error "could not clear the scratch root $SCRATCH"
  trap 'clear_scratch_at_exit' EXIT
  SJ_IMAGES="$SCRATCH/apptainer/images" SJ_TASK_TMP="$SCRATCH/tmp" SJ_BIN="$WORK/bin"
  # Apptainer's own layer cache and build space, and the engine's temporary files (and those of
  # tasks that run in the engine's own environment), on the instance store rather than the
  # container's root volume.
  export APPTAINER_CACHEDIR="$SCRATCH/apptainer/cache" APPTAINER_TMPDIR="$SCRATCH/apptainer/tmp"
  export TMPDIR="$SJ_TASK_TMP" NXF_TEMP="$SJ_TASK_TMP"
  mkdir -p "$WORK" "$SJ_IMAGES" "$SJ_TASK_TMP" "$SJ_BIN" "$APPTAINER_CACHEDIR" "$APPTAINER_TMPDIR"
  echo "biffix-coordinator: single-job run: ${SJ_CPUS} vCPUs, ${SJ_MEMORY_MIB} MiB, work root $SCRATCH" >&2
}

# prepare_task_images: for a single-job run, convert every task image the run may name to an
# image file (single_job.py convert), before the engine starts: an image that cannot be pulled
# fails the run now, not hours into it, and none is pulled while tasks run. The conversion runs as
# a child a cancel stops (run_stoppable), so a cancelled run still ends within Batch's stop
# timeout. Then install the apptainer wrapper the engine's tasks run through. A dry run converts
# nothing (no task runs).
prepare_task_images() {
  is_single_job || return 0
  install_apptainer_wrapper
  if [ -n "${BIFFIX_DRY_RUN:-}" ]; then
    echo "biffix-coordinator: dry run: not converting the task images" >&2
    return 0
  fi
  local failure="$WORK/image-conversion-failure.txt"
  # The worker image is in the map whenever there is one; named too for a run without a map.
  run_stoppable "the task images' conversion" TERM \
    python3 "$LIB_DIR/single_job.py" convert "${BIFFIX_IMAGE_MAP_FILE:-}" "$SJ_IMAGES" \
    "$WORK/image-logs" "$failure" ${BIFFIX_WORKER_IMAGE:+"$BIFFIX_WORKER_IMAGE"}
  [ "$rc" != 0 ] || return 0
  if [ -z "$ENGINE_STOPPED_BY" ]; then
    # A run failure (an image the run needs cannot be had), not a misconfiguration: exit 1, with
    # the reason in the run's logs.
    # shellcheck disable=SC2034 # read by upload_run_logs (lib/logs.sh)
    rc=1 FAILURE_FILE=$failure FAILURE_LABEL="Converting the task images failed"
  fi
  upload_run_logs final || true
  exit "$rc"
}

# run_setup_step <label> <command…>: run a step of a single-job run's setup, before the engine. On
# failure end the run (exit 1) with the step's error output as its failure.txt, under <label>, as
# prepare_task_images does: the scratch root is wiped at exit, so nothing else would explain it.
run_setup_step() {
  local label=$1 failure="$WORK/setup-step-failure.txt" status=0
  shift
  "$@" 2>"$failure" || status=$?
  cat "$failure" >&2
  [ "$status" != 0 ] || return 0
  # shellcheck disable=SC2034 # read by upload_run_logs (lib/logs.sh)
  rc=1 FAILURE_FILE=$failure FAILURE_LABEL=$label
  upload_run_logs final || true
  exit 1
}

# install_apptainer_wrapper: an `apptainer` first on the engine's PATH, which each task's container
# starts through (Nextflow's .command.run runs `apptainer exec` from PATH, under `env - PATH=…`;
# Snakemake finds it with shutil.which and runs it by name). Before it runs the real one, it
#   - raises its own oom_score_adj to 500 (the engine's is 0; at most 1000), which every process of
#     the task inherits: raising one's own needs no privilege. So the kernel picks a task, not the
#     engine, when the job is out of memory. A task that runs
#     no container (a Snakemake `run:` block, which runs in the engine itself, or a rule with no
#     image when the version has no default one; a Nextflow `exec:` process) is not covered.
#   - binds the instance store's task tmp dir at /tmp in the container, so a task's temporary
#     files go to the instance store, not to this container's own (Docker's root) volume; and at
#     its own path too, which is the TMPDIR the engines hand a task (prepare_scratch exports it;
#     Nextflow passes it into the container, and Snakemake's default resources.tmpdir is it).
#   - passes the per-job thread counts Snakemake sets for a job (OMP_NUM_THREADS and the like, its
#     `threads`) into the container, which --cleanenv would drop: with no per-task CPU limit, a
#     BLAS or OpenMP task would otherwise start a thread per vCPU of the host.
install_apptainer_wrapper() {
  local real score="${BIFFIX_OOM_SCORE_ADJ_FILE:-/proc/self/oom_score_adj}"
  real=$(command -v apptainer)
  {
    echo '#!/bin/bash'
    echo '# Written by biffix for a single-job run (lib/single_job.sh install_apptainer_wrapper).'
    printf 'echo 500 2>/dev/null >%q || true\n' "$score"
    # shellcheck disable=SC2016 # expanded by the wrapper, not here
    printf 'export APPTAINER_BINDPATH="${APPTAINER_BINDPATH:+$APPTAINER_BINDPATH,}"%q\n' \
      "$SJ_TASK_TMP:/tmp,$SJ_TASK_TMP"
    local var
    for var in OMP_NUM_THREADS GOTO_NUM_THREADS OPENBLAS_NUM_THREADS MKL_NUM_THREADS \
      VECLIB_MAXIMUM_THREADS NUMEXPR_NUM_THREADS; do
      # shellcheck disable=SC2016 # expanded by the wrapper, not here
      printf '[ -z "${%s+x}" ] || export APPTAINERENV_%s="$%s"\n' "$var" "$var" "$var"
    done
    printf 'exec %q "$@"\n' "$real"
  } > "$SJ_BIN/apptainer"
  chmod +x "$SJ_BIN/apptainer"
}
