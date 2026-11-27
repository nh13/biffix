# shellcheck shell=bash
# biffix coordinator: running the engine (and a single-job run's scratch wipe and image conversion)
# as a child that a cancel reaches. Sourced by entrypoint.sh; defines functions only.
#   Reads: LIB_DIR, BIFFIX_STOP_GRACE_SECS (how long a stopping child has before it is killed;
#          default 20), ENGINE_OUTPUT (a file the engine's output is also copied to, when set),
#          SJ_LOCK_FD (a single-job run's scratch lock, lib/single_job.sh, which run_in closes),
#          FINALIZING (the EXIT owner), ORIG_STDOUT_FD and ORIG_STDERR_FD (background diagnostics).
#   Sets:  ENGINE_PID, ENGINE_STOP_SIGNAL, CHILD_WHAT and GOT_SIGNAL (the running child),
#          ENGINE_KILLER (its stop deadline), ENGINE_STOPPED_BY (the status a signal ends the run
#          with) and STOPPED_AT (the script's SECONDS when that signal came), ENGINE_DONE (the
#          engine has exited), rc (the run's exit status), COORDINATOR_SIGNAL_SEEN (cleanup latch).

# Cancel (Batch TerminateJob) and the job timeout reach this script as SIGTERM (then SIGKILL after
# the stop timeout, 30 s): as PID 1 of a tasks run's job, and in a single-job run's through Docker's
# init (its job definition sets initProcessEnabled, to reap the tasks' processes), which is PID 1
# there and forwards the signal to this script, its child. The engine runs as a child (run_engine
# below) and is passed the signal that makes IT stop at once, cancelling its task jobs, so the
# post-engine steps (the final metadata sync, the events.jsonl upload and replay) still run within
# that grace:
#   - Nextflow stops its tasks and exits on SIGTERM.
#   - Snakemake does not: its scheduler takes SIGTERM as "exit after the running jobs finish" and
#     keeps waiting on them until the SIGKILL. SIGINT is its prompt stop: the scheduler's
#     KeyboardInterrupt calls the executor's cancel, which the AWS Batch executor implements as a
#     TerminateJob of every active job (Snakemake 9 job_scheduler.py; the executor's cancel_jobs).
# Should the engine still be running BIFFIX_STOP_GRACE_SECS after the signal, it is killed, so the
# post-engine steps keep the rest of the grace (the backend's cancel and its sweep terminate any
# task job the engine left behind). A single-job run's scratch wipe and image conversion, before
# the engine, are stopped the same way (with SIGTERM), and the run then ends at once. With no child
# running there is nothing to pass the signal to, so the run just ends. A run the signal stopped
# exits 128 + the signal's number (143 for a cancel's SIGTERM), whatever the child itself exited
# with (Nextflow exits 1), so a cancelled run never reads as a plain failure. After the engine has
# exited, the signal is only logged: the post-engine steps finish (or Batch's SIGKILL ends them) and
# the run still exits with the engine's own status.
# shellcheck disable=SC2329 # invoked by the traps below
on_signal() {
  # shellcheck disable=SC2034 # skip scratch cleanup even when the workflow outcome is preserved
  COORDINATOR_SIGNAL_SEEN=1
  echo "biffix-coordinator: received SIG$1" >&2
  if [ "${FINALIZING:-0}" = 1 ] || [ "${ENGINE_DONE:-0}" = 1 ]; then
    return 0
  fi
  if [ -z "$ENGINE_STOPPED_BY" ]; then
    ENGINE_STOPPED_BY=$2
    STOPPED_AT=$SECONDS
  fi
  if [ -z "$ENGINE_PID" ]; then
    exit "$ENGINE_STOPPED_BY"
  fi
  GOT_SIGNAL=1
  echo "biffix-coordinator: stopping $CHILD_WHAT (SIG$ENGINE_STOP_SIGNAL)" >&2
  kill -"$ENGINE_STOP_SIGNAL" "$ENGINE_PID" 2>/dev/null || true
  if [ -z "$ENGINE_KILLER" ]; then
    local grace="${BIFFIX_STOP_GRACE_SECS:-20}" pid=$ENGINE_PID what=$CHILD_WHAT
    # The sleep holds none of the job's output open: a killer stopped early (the child exited in
    # time) leaves nothing behind that a reader of that output would wait on.
    (
      # Stdio already points at the original job log. Do not let an orphaned sleep retain
      # the saved duplicates after this killer shell is stopped early.
      [ -z "${ORIG_STDOUT_FD:-}" ] || exec {ORIG_STDOUT_FD}>&-
      [ -z "${ORIG_STDERR_FD:-}" ] || exec {ORIG_STDERR_FD}>&-
      sleep "$grace" >/dev/null 2>&1
      echo "biffix-coordinator: $what did not stop within ${grace}s; killing it" >&2
      kill -KILL "$pid" 2>/dev/null
    ) 1>&"${ORIG_STDOUT_FD:-1}" 2>&"${ORIG_STDERR_FD:-2}" &
    ENGINE_KILLER=$!
  fi
}

# install_signal_traps: from here on, SIGTERM and SIGINT go through on_signal.
install_signal_traps() {
  ENGINE_PID=""
  ENGINE_STOP_SIGNAL=TERM
  CHILD_WHAT="the engine"
  ENGINE_KILLER=""
  ENGINE_STOPPED_BY=""
  # shellcheck disable=SC2034 # read by lib/logs.sh
  STOPPED_AT=""
  ENGINE_DONE=0
  GOT_SIGNAL=0
  # shellcheck disable=SC2034 # consumed by scratch cleanup, including signals during finalization
  COORDINATOR_SIGNAL_SEEN=0
  trap 'on_signal TERM 143' TERM
  trap 'on_signal INT 130' INT
  require_count BIFFIX_STOP_GRACE_SECS "${BIFFIX_STOP_GRACE_SECS:-20}"
}

# run_in <dir> <command…>: run the command from <dir> (exec, so run_engine's signal goes to it).
# SIGINT is restored to its default first: bash starts a background command with SIGINT ignored,
# and a Python engine (Snakemake) started so never raises KeyboardInterrupt. A single-job run's
# scratch lock is closed in it too: the engine runs workflow code, which could otherwise release
# the lock this shell holds (flock LOCK_UN on the shared open file) while the run uses the scratch
# root.
# shellcheck disable=SC2329 # invoked through run_engine
run_in() {
  cd "$1" || exit 1
  shift
  trap - INT
  if [ -n "${SJ_LOCK_FD:-}" ]; then
    exec {SJ_LOCK_FD}<&-
  fi
  exec "$@"
}

# run_stoppable <what> <stop signal> <command…>: run the command in the background and wait for it
# (again after a signal interrupts the wait), leaving its exit status in `rc`, or the signal's when
# one stopped it (ENGINE_STOPPED_BY). A cancel stops it with <stop signal> (on_signal), naming it
# <what>. errexit is off while it runs, so a failing command still reaches the steps after it.
# With ENGINE_OUTPUT set, its output is also copied there, capped (capped_tee.py), for failure.txt
# (lib/logs.sh): stdout and stderr both, to the job's log on stderr (CloudWatch merges the two
# anyway). The copy has taken the command's last lines when this returns (drain_copy).
run_stoppable() {
  set +e
  CHILD_WHAT=$1 ENGINE_STOP_SIGNAL=$2
  shift 2
  local copy_fd="" copy_pid=""
  if [ -n "${ENGINE_OUTPUT:-}" ]; then
    # The copy's pipe is opened in this shell, so it can be closed and its reader waited on below.
    # The command gets it as its stdout and stderr only (and is still its own PID: the
    # redirections are made in the forked child, which runs it).
    exec {copy_fd}> >(exec python3 "$LIB_DIR/capped_tee.py" "$ENGINE_OUTPUT" >&2)
    copy_pid=$!
    "$@" >&"$copy_fd" 2>&1 {copy_fd}>&- &
  else
    "$@" &
  fi
  ENGINE_PID=$!
  while :; do
    GOT_SIGNAL=0
    wait "$ENGINE_PID"
    # shellcheck disable=SC2034 # read by entrypoint.sh, which exits with it
    rc=$?
    [ "$GOT_SIGNAL" = 1 ] || break
  done
  # The engine has exited: from here on a signal is only logged (on_signal), so a late one cannot
  # skip the cleanup below or the post-engine steps, nor make a finished run read as cancelled.
  [ "$CHILD_WHAT" != "the engine" ] || ENGINE_DONE=1
  ENGINE_PID=""
  # First: the killer, a fork of this shell, holds the copy's pipe open too.
  if [ -n "$ENGINE_KILLER" ]; then
    kill "$ENGINE_KILLER" 2>/dev/null
    wait "$ENGINE_KILLER" 2>/dev/null
    ENGINE_KILLER=""
  fi
  if [ -n "$copy_fd" ]; then
    exec {copy_fd}>&-
    drain_copy "$copy_pid"
  fi
  if [ -n "$ENGINE_STOPPED_BY" ]; then
    echo "biffix-coordinator: $CHILD_WHAT stopped on the signal (its status $rc); the run exits $ENGINE_STOPPED_BY" >&2
    rc=$ENGINE_STOPPED_BY
  fi
  set -e
}

# drain_copy <pid>: wait for the output copy to read the end of its pipe and exit, so failure.txt
# quotes the command's last lines too. At most ~1 s: a process the command left behind (a task the
# killed engine never reaped) may still hold the pipe open; the copy then ends with the job.
drain_copy() {
  local i
  for ((i = 0; i < 20; i++)); do
    kill -0 "$1" 2>/dev/null || return 0
    sleep 0.05
  done
  echo "biffix-coordinator: the copy of the engine's output is still open; failure.txt may miss its last lines" >&2
}

# run_engine <stop signal> <command…>: log the command and run it as the engine (run_stoppable).
# Once it has exited (ENGINE_DONE, set by run_stoppable as soon as it is reaped), a signal no
# longer ends the run (on_signal).
run_engine() {
  {
    printf 'biffix-coordinator: running'
    printf ' %q' "${@:2}"
    printf '\n'
  } >&2
  run_stoppable "the engine" "$@"
}

# exit_if_stopped: before the engine, end a run a signal stopped (in run_stoppable) at once, with
# the signal's status.
exit_if_stopped() {
  [ -z "$ENGINE_STOPPED_BY" ] || exit "$ENGINE_STOPPED_BY"
}
