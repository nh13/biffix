# shellcheck shell=bash
# The coordinator's combined output: preserve the live job log and keep archive segments.
# Reads: LIB_DIR, BIFFIX_DRY_RUN, BIFFIX_RUN_ID, AWS_BATCH_JOB_ID, AWS_BATCH_JOB_ATTEMPT.
# Sets: CAPTURE_DIR, CAPTURE_PID, CAPTURE_FINISHED, ORIG_STDOUT_FD, ORIG_STDERR_FD.
# Copier diagnostics use original stderr and never become their own archive input.

capture_stream() {
  local failures=0 status
  # This shell retains stdin's read end while each copier runs: a dead copier never leaves
  # writers without a reader. A bounded restart budget also handles a missing interpreter.
  while true; do
    if python3 "$LIB_DIR/segment_tee.py" "$1"; then
      return 0
    else
      status=$?
    fi
    failures=$((failures + 1))
    if [ "$failures" -ge 3 ]; then
      printf 'biffix-coordinator: archive copier exited %s; forwarding only\n' "$status" >&2 || true
      exec cat
    fi
    printf 'biffix-coordinator: archive copier exited %s; restarting\n' "$status" >&2 || true
  done
}

start_coordinator_capture() {
  [ -z "${BIFFIX_DRY_RUN:-}" ] || return 0
  [ -z "${CAPTURE_PID:-}" ] || return 0
  local directory writer="" saved_out="" saved_err="" pid
  directory=$(mktemp -d /tmp/biffix-coordinator-logs.XXXXXX) || return 0
  # /tmp is a symlink on macOS. The uploader deliberately refuses symlinked ancestors.
  directory=$(cd "$directory" && pwd -P) || return 0
  # Acquire privately before replacing stdio or publishing state. Logging is optional, including
  # when descriptors are exhausted or an original stream is closed.
  if exec {saved_out}>&1 && exec {saved_err}>&2 \
    && exec {writer}> >(capture_stream "$directory" 1>&"$saved_err" 2>&"$saved_err"); then
    pid=$!
    if exec 1>&"$writer" 2>&1; then
      ORIG_STDOUT_FD=$saved_out ORIG_STDERR_FD=$saved_err
      CAPTURE_PID=$pid
      # shellcheck disable=SC2034 # consumed by the archive uploader, lib/logs.sh
      CAPTURE_DIR=$directory
      CAPTURE_FINISHED=0
      exec {writer}>&-
      printf 'biffix-coordinator: output archive run=%s attempt=%s job=%s\n' \
        "${BIFFIX_RUN_ID:-unknown}" "${AWS_BATCH_JOB_ATTEMPT:-1}" "${AWS_BATCH_JOB_ID:-unknown}" >&2
      return 0
    fi
    exec 1>&"$saved_out" 2>&"$saved_err" || true
  fi
  [ -z "$writer" ] || exec {writer}>&- || true
  [ -z "$saved_out" ] || exec {saved_out}>&- || true
  [ -z "$saved_err" ] || exec {saved_err}>&- || true
  rm -rf -- "$directory" || true
  printf 'biffix-coordinator: output archive unavailable; keeping original output\n' >&2 || true
  return 0
}

finish_coordinator_capture() {
  [ -n "${CAPTURE_PID:-}" ] || return 0
  [ "${CAPTURE_FINISHED:-0}" = 0 ] || return 0
  CAPTURE_FINISHED=1
  exec 1>&"$ORIG_STDOUT_FD" 2>&"$ORIG_STDERR_FD"
  local i
  for ((i=0; i<20; i++)); do
    if ! kill -0 "$CAPTURE_PID" 2>/dev/null; then
      wait "$CAPTURE_PID" 2>/dev/null || true
      return 0
    fi
    sleep 0.05
  done
  # A descendant may still hold a writer. Do not turn its next write into SIGPIPE by killing
  # the reader; the final uploader takes a snapshot of the remaining open segment instead.
  printf 'biffix-coordinator: output archive drain incomplete; keeping live forwarding\n' >&2 || true
  return 0
}
