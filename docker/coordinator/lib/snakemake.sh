# shellcheck shell=bash
# biffix coordinator: the Snakemake engine. Sourced by entrypoint.sh; defines functions only.
#   Reads: WORK, SRC, CONFIG, STORE, LIB_DIR, SM_META_URI, SM_META_FINGERPRINT, the run's BIFFIX_*
#          env, the resolved SNAKEMAKE_AWS_BATCH_JOB_QUEUE / _ROLE, BIFFIX_JOB_DEFINITION_MAP_FILE
#          and QUEUE_SELECTORS_FILE; for a single-job run (lib/single_job.sh) SCRATCH, SJ_CPUS,
#          SJ_TASK_MEMORY_MIB, SJ_IMAGES, SJ_BIN and BIFFIX_DRY_RUN; ORIG_STDOUT_FD and
#          ORIG_STDERR_FD (background diagnostics).
#   Sets:  rc (Snakemake's exit status), SNAKEMAKE_AWS_BATCH_JOB_TAGS, SNAKEMAKE_OUTPUT_CACHE,
#          SM_META_RESTORE_FAILED; for a single-job run PATH and XDG_CACHE_HOME.

# Snakemake's run metadata (.snakemake/metadata + incomplete markers) is what lets a run see that a
# rule's code, params, inputs or software changed; without it Snakemake keeps any existing output.
# It lives in this ephemeral container, so it is kept at the run's STORE (SM_META_URI): restored
# before the engine starts whenever the STORE has it (a resumed run, and any run into a stable
# output prefix, so the record there stays the union of the runs into it rather than the last run's
# jobs alone), synced while the engine runs, and once more at the end. Best-effort: a sync failure
# never changes the run's outcome.

# sync_snakemake_metadata [final]: upload the metadata (sm_metadata.py). A periodic sync skips the
# upload when nothing under it changed since the last one (the file list, sizes and mtimes); the
# final one always uploads. A dry run writes nothing to S3, so it never syncs.
sync_snakemake_metadata() {
  [ -z "${BIFFIX_DRY_RUN:-}" ] && [ -d "$SRC/.snakemake" ] && [ "${SM_META_RESTORE_FAILED:-0}" = 0 ] \
    || return 0
  python3 "$LIB_DIR/sm_metadata.py" sync "$SM_META_URI" "$SRC/.snakemake" "$SM_META_FINGERPRINT" \
    ${1:+final} || true
}

# restore_snakemake_metadata: restore the STORE's metadata. When the STORE's object exists but
# cannot be read (anything but "no such key"), every sync is skipped (SM_META_RESTORE_FAILED, set
# before the periodic sync starts so its subshell sees it): an upload would replace the STORE's
# record with this run's jobs alone.
restore_snakemake_metadata() {
  SM_META_RESTORE_FAILED=0
  python3 "$LIB_DIR/sm_metadata.py" restore "$SM_META_URI" "$SRC/.snakemake" \
    || SM_META_RESTORE_FAILED=1
}

# The periodic sync, in the background while the engine runs, so a coordinator that is killed
# outright still leaves a resumable run. It stops cooperatively: its sleep runs in the background
# and is waited on, so a TERM ends it at once between syncs, and a TERM that lands during a sync
# lets that sync finish first. stop_metadata_sync waits for it, so no upload of an older snapshot
# can land after the final sync.
start_metadata_sync() {
  (
    sleeper=""
    trap '[ -z "$sleeper" ] || kill "$sleeper" 2>/dev/null; exit 0' TERM
    while :; do
      sleep "${BIFFIX_METADATA_SYNC_SECS:-300}" &
      sleeper=$!
      wait "$sleeper"
      sleeper=""
      sync_snakemake_metadata
    done
  ) 1>&"${ORIG_STDOUT_FD:-1}" 2>&"${ORIG_STDERR_FD:-2}" &
  SYNC_PID=$!
}
stop_metadata_sync() {
  kill "$SYNC_PID" 2>/dev/null || true
  wait "$SYNC_PID" 2>/dev/null || true
}

# snakemake_job_tags <tags>: <tags> (comma-separated key=value) with the run and project tags added
# when missing. Every child job carries both: without the run's, its task is untracked; without the
# project's, IAM refuses to submit it. The backend sends the run tag.
snakemake_job_tags() {
  local tags=$1 tag
  for tag in "biffix:run=$BIFFIX_RUN_ID" "biffix:project=$BIFFIX_PROJECT"; do
    case ",$tags," in
      *",${tag%%=*}="*) ;;
      *) tags="${tags:+$tags,}$tag" ;;
    esac
  done
  printf '%s' "$tags"
}

# snakemake_settings: the run's Snakemake settings from its BIFFIX_* env, checked (exit 78 for a
# bad one): RETRIES, MAX_JOBS, CORES, SPOT_ATTEMPTS and KEEP_GOING_ARGS. entrypoint.sh calls it, in
# a subshell, right after the runtime check, only so that a bad setting is refused before anything
# is staged or converted; run_snakemake calls it for its own (local) copies.
snakemake_settings() {
  # `--retries` re-submits a fresh Batch job on a worker failure (a reclaim included; see "Spot
  # reclaims" in entrypoint.sh), and `--keep-going` lets the rest of the DAG finish rather than
  # aborting on the first failure. Default to 3 retries + keep-going (the value spot-backed
  # benchmark fleets use); BIFFIX_RETRIES / BIFFIX_KEEP_GOING override (set 0 / false to opt out).
  RETRIES="${BIFFIX_RETRIES:-3}" MAX_JOBS="${BIFFIX_MAX_JOBS:-100}"
  require_count BIFFIX_RETRIES "$RETRIES"
  require_count BIFFIX_MAX_JOBS "$MAX_JOBS"
  # --cores is what a workflow reads as workflow.cores (`threads: workflow.cores`, say), which
  # Snakemake refuses to parse without it. With a remote executor it does not limit how many jobs
  # run at once, local rules' included: once --jobs is set, the scheduler lifts its cores limit
  # (Snakemake 9.27, job_scheduler.py). But it caps every job's threads, remote ones included, and
  # the executor sizes a Batch job's vCPUs from its threads (rules.py expand_resources; the
  # executor's batch_job_builder). So it is not the coordinator's own vCPUs, which would shrink
  # every task job to the coordinator's size, but a ceiling for one job: a large instance's worth.
  # Each remote job's inner Snakemake gets the same value, so workflow.cores reads the same on the
  # coordinator and the workers.
  CORES="${BIFFIX_CORES:-64}"
  require_count BIFFIX_CORES "$CORES"
  [ "$CORES" -ge 1 ] || runtime_error "BIFFIX_CORES must be at least 1: $CORES"
  # Spot reclaims (see "Spot reclaims" in entrypoint.sh): an AWS Batch run's only, but checked for
  # every run, as Nextflow's is.
  SPOT_ATTEMPTS="${BIFFIX_SPOT_ATTEMPTS:-5}"
  require_count BIFFIX_SPOT_ATTEMPTS "$SPOT_ATTEMPTS"
  KEEP_GOING_ARGS=(--keep-going)
  local keep_going="${BIFFIX_KEEP_GOING:-true}"
  case "${keep_going,,}" in
    true | 1 | yes | on) ;;
    false | 0 | no | off) KEEP_GOING_ARGS=() ;;
    *) runtime_error "BIFFIX_KEEP_GOING is not a boolean (true/1/yes/on or false/0/no/off): $keep_going" ;;
  esac
}

# Run Snakemake with the AWS Batch executor + the biffix logger plugin; a single-job run with the
# biffix executor, its jobs on this host instead (snakemake_single_job_args).
run_snakemake() {
  local SNAKEFILE="$SRC/$BIFFIX_ENTRYPOINT" RETRIES MAX_JOBS CORES SPOT_ATTEMPTS
  local -a KEEP_GOING_ARGS
  snakemake_settings
  SNAKEMAKE_AWS_BATCH_JOB_TAGS=$(snakemake_job_tags "${SNAKEMAKE_AWS_BATCH_JOB_TAGS:-}")
  export SNAKEMAKE_AWS_BATCH_JOB_TAGS
  # The between-workflow cache: the inner Snakemake on each worker fetches/stores it, so the
  # location is forwarded to jobs (--envvars for AWS Batch jobs, the wrapper's register_envvars for
  # a single-job run) along with --cache. The rules are named explicitly: a bare `--cache` ("every
  # eligible rule") is an empty list, which Snakemake drops when it builds a remote job's command
  # line, so the jobs would never cache. Only the rules marked `cache:` may be named (Snakemake
  # rejects any other), so they are read from the workflow itself. The helper prints each as
  # `BIFFIX_CACHE_RULE=<name>` (loading the workflow runs its own code, which may print too); its
  # errors go to this log.
  # ENVVARS: the variables every job gets (snakemake_mode_args passes them).
  local -a CACHE_ARGS=() ENVVARS=()
  if [ -n "${BIFFIX_SNAKEMAKE_CACHE:-}" ]; then
    local -a CACHE_RULES=()
    local rules_out="$WORK/.biffix-cache-rules"
    # From the source root, as the run itself, so the workflow's own relative configfile loads.
    if (cd "$SRC" && python3 "$LIB_DIR/snakemake_cache_rules.py" "$SNAKEFILE" "$CONFIG") >"$rules_out"; then
      mapfile -t CACHE_RULES < <(sed -n 's/^BIFFIX_CACHE_RULE=//p' "$rules_out")
    else
      echo "biffix-coordinator: WARNING: could not read the workflow's cache: rules (above); running without the between-workflow cache" >&2
    fi
    if [ "${#CACHE_RULES[@]}" -gt 0 ]; then
      export SNAKEMAKE_OUTPUT_CACHE="$BIFFIX_SNAKEMAKE_CACHE"
      ENVVARS+=(SNAKEMAKE_OUTPUT_CACHE)
      CACHE_ARGS=(--cache "${CACHE_RULES[@]}")
    else
      echo "biffix-coordinator: no rule is marked cache:; running without the cache" >&2
    fi
  fi
  local -a ARGS=(--configfile "$CONFIG" "${CACHE_ARGS[@]}")
  snakemake_mode_args
  ARGS+=(
    # What makes Snakemake re-run a job whose output exists: not a change of its software
    # environment. In neither mode is a job's image one Snakemake knows (no --sdm: the executor
    # runs the job's own Snakemake in it), so both record the same constant, and either mode's
    # metadata serves a resume or a later run into the same output prefix. A job is not re-run
    # only because its image changed.
    --rerun-triggers mtime params input code
    --default-storage-provider s3
    --default-storage-prefix "$STORE"
    --logger biffix
    --retries "$RETRIES"
    "${KEEP_GOING_ARGS[@]}"
    --verbose
    --rerun-incomplete
  )
  [ -z "${BIFFIX_DRY_RUN:-}" ] || ARGS+=(--dry-run)
  [ -z "${BIFFIX_TARGET:-}" ] || ARGS+=("$BIFFIX_TARGET")
  # The command line, for the run's logs (lib/logs.sh): it names files, not their contents.
  printf '%s\n' "${ARGS[@]}" > "$WORK/snakemake.args"
  init_engine_output
  start_logs_sync
  restore_snakemake_metadata
  start_metadata_sync
  # From the source root, as `snakemake` from a checkout: Snakemake reads a workflow's `configfile:`
  # (the standard layout's config/config.yaml), and every other relative path (a target included),
  # against its working directory. From anywhere else that configfile is not found and, since
  # --configfile is given, skipped without a word, so the run went on with biffix's params alone.
  # The params still override it: a --configfile outranks the workflow's own. Its .snakemake (the
  # metadata above) and the logger's events.jsonl are kept there too.
  run_engine INT run_in "$SRC" snakemake "${ARGS[@]}"
  stop_metadata_sync
  sync_snakemake_metadata final
}

# snakemake_mode_args: run_snakemake's ARGS for the run's execution mode, with the variables
# every job gets (ENVVARS): for AWS Batch jobs on the command line (snakemake_batch_args), for a
# single-job run from its wrapper Snakefile (snakemake_single_job_args).
snakemake_mode_args() {
  if is_single_job; then
    snakemake_single_job_args
  else
    snakemake_batch_args
  fi
}

# require_single_job_executor: refuse (exit 78) a Snakemake runtime that cannot run a single-job
# run's jobs: one built before the biffix executor plugin (plugins/snakemake-executor-plugin-biffix)
# was part of the runtime layer, or one whose plugin is there but does not import. Called by
# entrypoint.sh right after the runtime check, before anything is staged or converted.
require_single_job_executor() {
  # One interpreter for both checks (each loads the plugin's imports): 3 when there is no plugin,
  # 4 when it is there but does not import (its traceback on stderr), else 0, or whatever else
  # python3 itself exits with (anything but 0 refuses the runtime).
  local status=0
  python3 -c 'import importlib.util, sys
if importlib.util.find_spec("snakemake_executor_plugin_biffix") is None:
    sys.exit(3)
try:
    import snakemake_executor_plugin_biffix
except Exception:
    import traceback
    traceback.print_exc()
    sys.exit(4)' || status=$?
  case "$status" in
    0) ;;
    4) runtime_error "this Snakemake runtime's biffix executor plugin (snakemake-executor-plugin-biffix) is present but could not be imported (its error is above)" ;;
    3) runtime_error "this Snakemake runtime has no biffix executor plugin (snakemake-executor-plugin-biffix), which a single-job run needs: re-register the workflow once the backend is at a release that pins it (an older backend records a runtime without it)" ;;
    *) runtime_error "this Snakemake runtime's python3 could not check for the biffix executor plugin (snakemake-executor-plugin-biffix) (exit $status)" ;;
  esac
}

# snakemake_single_job_args: run_snakemake's ARGS for a single-job run (lib/single_job.sh): every
# job runs on this host, in the image its job on AWS Batch would run, from the image files the
# coordinator converted before the run. Reads run_snakemake's SNAKEFILE and ENVVARS.
snakemake_single_job_args() {
  # The AWS credentials and region a job's own Snakemake needs for the run's storage, as a task on
  # AWS Batch has its job role's: those of this job (its project's run role), passed by name to
  # each job's container, which otherwise starts with none of this environment. Only the container
  # credential provider's (the role's, fetched as needed), never inline keys this process may
  # carry: the executor would hand their values to every task.
  local var
  for var in AWS_CONTAINER_CREDENTIALS_RELATIVE_URI AWS_CONTAINER_CREDENTIALS_FULL_URI \
    AWS_CONTAINER_AUTHORIZATION_TOKEN AWS_CONTAINER_AUTHORIZATION_TOKEN_FILE \
    AWS_REGION AWS_DEFAULT_REGION; do
    [ -z "${!var:-}" ] || ENVVARS+=("$var")
  done
  # A wrapper Snakefile beside the workflow's (so its relative includes and scripts resolve as
  # before) includes it and, in Snakemake's main process, sets the host's memory budget, scopes
  # memory and disk to the whole host, registers ENVVARS with Snakemake, and checks every rule's
  # image, module rules' included, before any job starts: an image the map does not name, or one
  # whose image file was not converted, fails the run now, so no task pulls one (single_job.py
  # check_rule_images). The wrapper adds the scopes and the variables to a workflow profile's own
  # (`set-resource-scopes:`, `envvars:`), which the same options on the command line would replace.
  local snakefile check=check
  [ -z "${BIFFIX_DRY_RUN:-}" ] || check=no-check
  run_setup_step "Writing the Snakefile that checks each rule's image failed" \
    python3 "$LIB_DIR/single_job.py" snakefile-wrapper "$SNAKEFILE" "" \
    "${BIFFIX_IMAGE_MAP_FILE:-}" "$SJ_IMAGES" "${BIFFIX_WORKER_IMAGE:-}" \
    "$check" "$SJ_TASK_MEMORY_MIB" "${ENVVARS[@]}" > "$WORK/snakefile-wrapper-path"
  IFS= read -r snakefile < "$WORK/snakefile-wrapper-path"
  cp "$snakefile" "$WORK/Snakefile.biffix"
  ARGS+=(
    --snakefile "$snakefile"
    # The biffix executor: a remote executor, so Snakemake forms group jobs and merges their
    # resources as it does for AWS Batch, that runs each job, or group job, on this host: the
    # job's own Snakemake (`python -m snakemake …`, as on AWS Batch) in the image the AWS Batch
    # executor would pick (the job's aws_batch_container_image through the image map, else the
    # version's default image; none: in this container, the image its Batch job would have run),
    # through biffix's apptainer wrapper (lib/single_job.sh), from the converted image files. A
    # rule's own `container:` is ignored, as on AWS Batch. A job's container starts with a clean
    # environment plus what Snakemake passes jobs (a workflow's `envvars:` and ENVVARS); the
    # logger also removes the ingest token from the engine's environment once read, and the
    # executor never passes it.
    --executor biffix
    --biffix-images-dir "$SJ_IMAGES"
    # Bound into each job's container: the work root (the source root, Snakemake's own
    # .snakemake, the config, the local storage below and the source cache).
    --biffix-bind "$SCRATCH"
    # The host's CPUs bound the threads of the jobs running at once. A remote executor needs
    # --jobs, which would lift that bound (Snakemake schedules by nodes then); the wrapper clears
    # it again and merges the host memory cap with the profile's limits (single_job.py
    # configure_local_resources), preserving the profile's other settings. Each job's own
    # Snakemake gets the same --cores, a group job's included, so workflow.cores reads the same
    # in every job; a group job's may so run more threads than its reserved width (the executor).
    --cores "$SJ_CPUS"
    --jobs "$SJ_CPUS"
    # Inputs fetched from S3, and local copies of outputs (each still published to the run's
    # output prefix), on the instance store; one directory for the engine and its jobs, which
    # share it.
    --local-storage-prefix "$SCRATCH/storage"
    --remote-job-local-storage-prefix "$SCRATCH/storage"
  )
  [ -z "${BIFFIX_IMAGE_MAP_FILE:-}" ] || ARGS+=(--biffix-image-map "$BIFFIX_IMAGE_MAP_FILE")
  [ -z "${BIFFIX_WORKER_IMAGE:-}" ] || ARGS+=(--biffix-default-image "$BIFFIX_WORKER_IMAGE")
  # Snakemake's cache of remote sources, which the engine hands its jobs by path, on the work
  # root, where the jobs' containers see it.
  export XDG_CACHE_HOME="$SCRATCH/cache"
  export PATH="$SJ_BIN:$PATH"
}

# snakemake_batch_args: run_snakemake's ARGS for a run whose jobs are AWS Batch jobs. Reads
# run_snakemake's SNAKEFILE, MAX_JOBS, CORES, SPOT_ATTEMPTS and ENVVARS.
snakemake_batch_args() {
  ARGS+=(
    --snakefile "$SNAKEFILE"
    --executor aws-batch
    --aws-batch-region "$BIFFIX_REGION"
    --aws-batch-job-queue "$SNAKEMAKE_AWS_BATCH_JOB_QUEUE"
  )
  # Task job definitions (see "Project isolation" in entrypoint.sh). With the run's job definition
  # map (SNAKEMAKE_AWS_BATCH_JOB_DEFINITION_MAP, from fetch_job_definition_map), every job runs on
  # the definition biffix registered for its image, which carries the job role and the project's
  # task log group, and is never privileged: the executor registers none, and refuses a job role,
  # log group or --aws-batch-privileged beside the map (and fails a job whose rule sets the
  # aws_batch_job_definition or aws_batch_privileged resource), so none is passed, and the job
  # role's env var (which the executor also reads) is cleared. Without a map (a local run), the
  # executor registers its own, with the role and log group, never privileged.
  if [ -n "${BIFFIX_JOB_DEFINITION_MAP_FILE:-}" ]; then
    unset SNAKEMAKE_AWS_BATCH_JOB_ROLE
  else
    ARGS+=(
      --aws-batch-job-role "$SNAKEMAKE_AWS_BATCH_JOB_ROLE"
      --aws-batch-log-group "$BIFFIX_TASK_LOG_GROUP"
    )
  fi
  # The rules the version sends to other queues (lib/tasks.sh): the executor's rule queue map, a
  # JSON object of rule to queue in QUEUE_SELECTORS_FILE, passed by its path. It outranks a rule's
  # own `batch_queue` resource and, unlike --set-resources on the command line, leaves the
  # workflow profile's set-resources in place (Snakemake would drop the profile's whole list).
  [ ! -s "$QUEUE_SELECTORS_FILE" ] || ARGS+=(--aws-batch-rule-queues "$QUEUE_SELECTORS_FILE")
  [ -z "${BIFFIX_WORKER_IMAGE:-}" ] || ARGS+=(--container-image "$BIFFIX_WORKER_IMAGE")
  ARGS+=(
    --aws-batch-spot-attempts "$SPOT_ATTEMPTS"
    --jobs "$MAX_JOBS"
    --cores "$CORES"
  )
  # The variables every job gets, given once: a second --envvars would replace the first.
  [ "${#ENVVARS[@]}" = 0 ] || ARGS+=(--envvars "${ENVVARS[@]}")
}
