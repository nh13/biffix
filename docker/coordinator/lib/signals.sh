# shellcheck shell=bash
# biffix coordinator: running the engine as a child that a cancel reaches. Sourced by
# entrypoint.sh; defines functions only.
#   Reads: BIFFIX_STOP_GRACE_SECS (how long a stopping engine has before it is killed; default 20).
#   Sets:  ENGINE_PID, ENGINE_STOP_SIGNAL and GOT_SIGNAL (the running engine), ENGINE_KILLER (its
#          stop deadline), ENGINE_STOPPED_BY (the status a signal ends the run with), ENGINE_DONE
#          (it has exited), rc (the run's exit status).

# Cancel (Batch TerminateJob) and the job timeout reach this script, PID 1 in the job, as SIGTERM
# (then SIGKILL after the stop timeout, 30 s). The engine runs as a child (run_engine below) and is
# passed the signal that makes IT stop at once, cancelling its task jobs, so the post-engine steps
# (the final metadata sync, the events.jsonl upload and replay) still run within that grace:
#   - Nextflow stops its tasks and exits on SIGTERM.
#   - Snakemake does not: its scheduler takes SIGTERM as "exit after the running jobs finish" and
#     keeps waiting on them until the SIGKILL. SIGINT is its prompt stop: the scheduler's
#     KeyboardInterrupt calls the executor's cancel, which the AWS Batch executor implements as a
#     TerminateJob of every active job (Snakemake 9 job_scheduler.py; the executor's cancel_jobs).
# Should the engine still be running BIFFIX_STOP_GRACE_SECS after the signal, it is killed, so the
# post-engine steps keep the rest of the grace (the backend's cancel and its sweep terminate any
# task job the engine left behind). Before the engine starts there is nothing to pass the signal
# to, so the run just ends. A run the signal stopped exits 128 + the signal's number (143 for a
# cancel's SIGTERM), whatever the engine itself exited with (Nextflow exits 1), so a cancelled run
# never reads as a plain failure. After the engine has exited, the signal is only logged: the
# post-engine steps finish (or Batch's SIGKILL ends them) and the run still exits with the engine's
# own status.
# shellcheck disable=SC2329 # invoked by the traps below
on_signal() {
  echo "biffix-coordinator: received SIG$1" >&2
  if [ "$ENGINE_DONE" = 1 ]; then
    return 0
  fi
  if [ -z "$ENGINE_PID" ]; then
    exit "$2"
  fi
  GOT_SIGNAL=1
  [ -n "$ENGINE_STOPPED_BY" ] || ENGINE_STOPPED_BY=$2
  echo "biffix-coordinator: stopping the engine (SIG$ENGINE_STOP_SIGNAL)" >&2
  kill -"$ENGINE_STOP_SIGNAL" "$ENGINE_PID" 2>/dev/null || true
  if [ -z "$ENGINE_KILLER" ]; then
    local grace="${BIFFIX_STOP_GRACE_SECS:-20}" pid=$ENGINE_PID
    # The sleep holds none of the job's output open: a killer stopped early (the engine exited in
    # time) leaves nothing behind that a reader of that output would wait on.
    (
      sleep "$grace" >/dev/null 2>&1
      echo "biffix-coordinator: the engine did not stop within ${grace}s; killing it" >&2
      kill -KILL "$pid" 2>/dev/null
    ) &
    ENGINE_KILLER=$!
  fi
}

# install_signal_traps: from here on, SIGTERM and SIGINT go through on_signal.
install_signal_traps() {
  ENGINE_PID=""
  ENGINE_STOP_SIGNAL=TERM
  ENGINE_KILLER=""
  ENGINE_STOPPED_BY=""
  ENGINE_DONE=0
  GOT_SIGNAL=0
  require_count BIFFIX_STOP_GRACE_SECS "${BIFFIX_STOP_GRACE_SECS:-20}"
  trap 'on_signal TERM 143' TERM
  trap 'on_signal INT 130' INT
}

# run_in <dir> <command…>: run the command from <dir> (exec, so run_engine's signal goes to it).
# SIGINT is restored to its default first: bash starts a background command with SIGINT ignored,
# and a Python engine (Snakemake) started so never raises KeyboardInterrupt.
# shellcheck disable=SC2329 # invoked through run_engine
run_in() {
  cd "$1" || exit 1
  shift
  trap - INT
  exec "$@"
}

# run_engine <stop signal> <command…>: log the command, run it in the background, and wait for it
# (again after a signal interrupts the wait), leaving its exit status in `rc`, or the signal's when
# one stopped it. A cancel stops it with <stop signal> (on_signal). errexit is off while it runs,
# so a failing engine still reaches the post-engine steps.
run_engine() {
  set +e
  ENGINE_STOP_SIGNAL=$1
  shift
  {
    printf 'biffix-coordinator: running'
    printf ' %q' "$@"
    printf '\n'
  } >&2
  "$@" &
  ENGINE_PID=$!
  while :; do
    GOT_SIGNAL=0
    wait "$ENGINE_PID"
    # shellcheck disable=SC2034 # read by entrypoint.sh, which exits with it
    rc=$?
    [ "$GOT_SIGNAL" = 1 ] || break
  done
  if [ -n "$ENGINE_STOPPED_BY" ]; then
    echo "biffix-coordinator: the engine stopped on the signal (its status $rc); the run exits $ENGINE_STOPPED_BY" >&2
    rc=$ENGINE_STOPPED_BY
  fi
  ENGINE_PID=""
  ENGINE_DONE=1
  if [ -n "$ENGINE_KILLER" ]; then
    kill "$ENGINE_KILLER" 2>/dev/null
    wait "$ENGINE_KILLER" 2>/dev/null
  fi
  set -e
}
