# shellcheck shell=bash
# biffix coordinator: the run's logs, under its own prefix (s3://<bucket>/<run prefix>logs/), while
# staging and engine execution, and once at exit. Sourced by entrypoint.sh; defines functions only.
#   Reads: WORK, SRC, LIB_DIR, NXF_HOME, BIFFIX_PROJECT_BUCKET, BIFFIX_RUN_PREFIX, BIFFIX_DRY_RUN,
#          BIFFIX_LOGS_SYNC_SECS, BIFFIX_LOGS_FINAL_SECS, rc, ENGINE_STOPPED_BY and STOPPED_AT (the
#          run's status, and whether and when a signal stopped it; lib/signals.sh), FAILURE_FILE and
#          FAILURE_LABEL (what failure.txt quotes, default the engine's output, and what it says
#          failed, default the engine), LOCAL_WORK_DIR (a Nextflow work dir on this host, when it is
#          one), CAPTURE_DIR (coordinator segments), AWS_BATCH_JOB_ATTEMPT, RUN_LOGS_STATE_FILE
#          (optional upload-state path independent of WORK), ENGINE_DONE (an engine actually ran).
#          ORIG_STDOUT_FD and ORIG_STDERR_FD (background diagnostics), EXECUTION / BIFFIX_EXECUTION
#          and COORDINATOR_SCRATCH_PREPARED (workspace ownership before collecting scratch files).
#   Sets:  ENGINE_OUTPUT (init_engine_output), LOGS_PID, LOGS_CMD, COORDINATOR_LOGS_ARCHIVED.

# What is uploaded, when it exists (run_logs.py): the engine's log (.nextflow.log, Snakemake's
# newest .snakemake/log/*.snakemake.log), the trace of a pipeline that turns Nextflow's on without
# naming a file for it (the newest trace*.txt in the launch dir, the source root; biffix never turns
# it on: its metrics need tools such as `ps` in every task image), the engine config biffix
# generated (Nextflow's -c and default configs, Snakemake's command line and wrapper Snakefile), and
# the .command.* files of failed tasks in a Nextflow work dir on this host (a single-job run's). At
# exit, a run that failed (not one a signal stopped) also gets failure.txt, the end of the engine's
# output; an attempt's first upload that writes none removes an earlier attempt's (a Batch retry
# runs under the same prefix). Only what changed since the last upload is sent. Best-effort: a failure is logged and
# never changes the run's outcome. A dry run writes nothing to S3, so it uploads nothing.

# logs_command [final]: the upload's run_logs.py command line, in LOGS_CMD. Each upload is abandoned
# after a deadline: 60 s while the engine runs, and at exit BIFFIX_LOGS_FINAL_SECS (default 8), or,
# in a run a signal stopped, what is left of Batch's stop timeout (30 s from the SIGTERM to its
# SIGKILL) less 4 s for the events.jsonl replay after it, when that is less (at least 1 s): the
# engine's own stop (BIFFIX_STOP_GRACE_SECS; lib/signals.sh) and the events.jsonl upload have
# already taken their share. A periodic upload sends at most a file's last 8 MiB (each re-sends a
# growing log whole, held in memory), the final one up to run_logs.py's MAX_FILE_BYTES.
logs_command() {
  local deadline=60 batch_stop_secs=30 replay_reserve_secs=4
  # A refused single-job run must not read a previous project's scratch files. Only the private
  # capture directory is eligible until this invocation owns and has initialized its workspace.
  local workspace=1
  if [ "${EXECUTION:-${BIFFIX_EXECUTION:-tasks}}" = single_job ] \
    && [ "${COORDINATOR_SCRATCH_PREPARED:-0}" != 1 ]; then
    workspace=0
  fi
  local state="${RUN_LOGS_STATE_FILE:-}"
  if [ -z "$state" ] && [ "$workspace" = 1 ] && [ -n "${WORK:-}" ]; then
    state="$WORK/.biffix-logs-uploaded.json"
  fi
  [ -n "$state" ] || return 1
  local -a ARGS=()
  if [ "$workspace" = 1 ] && [ -n "${WORK:-}" ]; then
    ARGS+=(--file "nextflow.log=$WORK/.nextflow.log"
      --file "nextflow.config=$WORK/nextflow.config"
      --file "snakemake.args=$WORK/snakemake.args"
      --file "Snakefile.biffix=$WORK/Snakefile.biffix")
  fi
  if [ "$workspace" = 1 ] && [ -n "${SRC:-}" ]; then
    ARGS+=(--newest "trace.txt=$SRC/trace*.txt"
      --newest "snakemake.log=$SRC/.snakemake/log/*.snakemake.log")
  fi
  if [ -n "${CAPTURE_DIR:-}" ]; then
    local attempt="${AWS_BATCH_JOB_ATTEMPT:-1}"
    if [[ "$attempt" =~ ^[1-9][0-9]*$ ]]; then
      ARGS+=(--segments "coordinator/attempt-$attempt=$CAPTURE_DIR")
      [ "${1:-}" != final ] || ARGS+=(--final)
    fi
  fi
  if [ "$workspace" = 1 ]; then
    [ -z "${NXF_HOME:-}" ] || ARGS+=(--file "nextflow-defaults.config=$NXF_HOME/config")
    [ -z "${LOCAL_WORK_DIR:-}" ] || ARGS+=(--failed-tasks "$LOCAL_WORK_DIR")
  fi
  if [ "${1:-}" = final ]; then
    deadline="${BIFFIX_LOGS_FINAL_SECS:-8}"
    if [ -n "${ENGINE_STOPPED_BY:-}" ] && [ -n "${STOPPED_AT:-}" ]; then
      local left=$((batch_stop_secs - (SECONDS - STOPPED_AT) - replay_reserve_secs))
      [ "$left" -ge 1 ] || left=1
      [ "$left" -ge "$deadline" ] || deadline=$left
    fi
    if [ "$workspace" = 1 ] && [ "${rc:-0}" != 0 ] && [ -z "${ENGINE_STOPPED_BY:-}" ] \
      && { [ "${ENGINE_DONE:-0}" = 1 ] || [ -n "${FAILURE_LABEL:-}" ]; }; then
      ARGS+=(--failure "${FAILURE_FILE:-${ENGINE_OUTPUT:-}}" "$rc")
      [ -z "${FAILURE_LABEL:-}" ] || ARGS+=(--failure-label "$FAILURE_LABEL")
    fi
  else
    ARGS+=(--max-bytes $((8 * 1024 * 1024)))
  fi
  LOGS_CMD=("$LIB_DIR/run_logs.py" "s3://${BIFFIX_PROJECT_BUCKET:-}/${BIFFIX_RUN_PREFIX:-}logs/"
    "$state" "$deadline" "${ARGS[@]}")
}

# upload_run_logs [final]: upload the logs once.
upload_run_logs() {
  [ -z "${BIFFIX_DRY_RUN:-}" ] || return 0
  [ -n "${BIFFIX_PROJECT_BUCKET:-}" ] && [ -n "${BIFFIX_RUN_PREFIX:-}" ] || return 0
  logs_command "$@" || return 0
  (run_logs_uploader) || true
}

# Scrub before exec (not inside Python, where /proc/self/environ retains the initial environment).
# Keep ECS credential discovery for S3, and leave the parent's token intact for event replay.
# Call only in an explicit subshell, so exec replaces the tracked child, not an extra function shell.
run_logs_uploader() {
  unset BIFFIX_INGEST_TOKEN AWS_ACCESS_KEY_ID AWS_SECRET_ACCESS_KEY AWS_SESSION_TOKEN AWS_SECURITY_TOKEN
  exec python3 "${LOGS_CMD[@]}"
}

# Keep the engine-only failure.txt source separate from early coordinator capture. run_engine
# copies the engine's output here through capped_tee.py, retaining only its end.
init_engine_output() {
  ENGINE_OUTPUT="$WORK/engine-output.log"
}

# Upload periodically during staging and engine execution (every BIFFIX_LOGS_SYNC_SECS, default
# 120). Restart to pick up late settings such as LOCAL_WORK_DIR. TERM reaps an upload in flight
# before stop_logs_sync returns, preventing an older upload from racing the final snapshot.
start_logs_sync() {
  stop_logs_sync
  local every="${BIFFIX_LOGS_SYNC_SECS:-120}"
  require_count BIFFIX_LOGS_SYNC_SECS "$every"
  [[ "$every" =~ [1-9] ]] || runtime_error "BIFFIX_LOGS_SYNC_SECS must be positive: $every"
  require_count BIFFIX_LOGS_FINAL_SECS "${BIFFIX_LOGS_FINAL_SECS:-8}"
  LOGS_PID=""
  [ -z "${BIFFIX_DRY_RUN:-}" ] || return 0
  (
    child=""
    # Consult the job table too: TERM can arrive after launch but before child=$!.
    trap '
      for child in $(jobs -p); do
        kill "$child" 2>/dev/null || true
        wait_logs_child "$child"
      done
      exit 0
    ' TERM
    while :; do
      sleep "$every" &
      child=$!
      wait "$child"
      logs_command
      (run_logs_uploader) &
      child=$!
      wait "$child" || echo "biffix-coordinator: periodic log upload exited with status $?; retrying next interval" >&2
      child=""
    done
  ) 1>&"${ORIG_STDOUT_FD:-1}" 2>&"${ORIG_STDERR_FD:-2}" &
  LOGS_PID=$!
}

# A trapped signal interrupts wait even when its handler returns. Do not let an older upload
# outlive stopping its loop and race the final snapshot. A completed/signalled child is reaped.
wait_logs_child() {
  local status
  while :; do
    if wait "$1" 2>/dev/null; then
      return 0
    else
      status=$?
    fi
    [ "$status" -gt 128 ] && kill -0 "$1" 2>/dev/null || return 0
  done
}

stop_logs_sync() {
  [ -n "${LOGS_PID:-}" ] || return 0
  kill "$LOGS_PID" 2>/dev/null || true
  wait_logs_child "$LOGS_PID"
  LOGS_PID=""
}

# Both the normal epilogue and EXIT use this. Latch before attempting the upload so an upload
# failure cannot spend the shutdown budget twice. Replay still needs the work directory afterward.
archive_coordinator_logs() {
  [ "${COORDINATOR_LOGS_ARCHIVED:-0}" = 0 ] || return 0
  COORDINATOR_LOGS_ARCHIVED=1
  stop_logs_sync || true
  finish_coordinator_capture || true
  upload_run_logs final || true
  return 0
}
