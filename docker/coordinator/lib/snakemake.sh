# shellcheck shell=bash
# biffix coordinator: the Snakemake engine. Sourced by entrypoint.sh; defines functions only.
#   Reads: WORK, SRC, CONFIG, STORE, LIB_DIR, SM_META_URI, SM_META_FINGERPRINT, the run's BIFFIX_*
#          env, the resolved SNAKEMAKE_AWS_BATCH_JOB_QUEUE / _ROLE, BIFFIX_JOB_DEFINITION_MAP_FILE
#          and QUEUE_SELECTORS_FILE; for a single-job run (lib/single_job.sh) SCRATCH, SJ_CPUS,
#          SJ_TASK_MEMORY_MIB, SJ_IMAGES, SJ_BIN, BIFFIX_ENGINE_VERSION and BIFFIX_DRY_RUN.
#   Sets:  rc (Snakemake's exit status), SNAKEMAKE_AWS_BATCH_JOB_TAGS, SNAKEMAKE_OUTPUT_CACHE,
#          SM_META_RESTORE_FAILED; for a single-job run PATH and BIFFIX_TASK_IMAGES_FILE.

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
  ) &
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

# Run Snakemake with the AWS Batch executor + the biffix logger plugin; a single-job run with its
# jobs on this host instead (snakemake_single_job_args).
run_snakemake() {
  local SNAKEFILE="$SRC/$BIFFIX_ENTRYPOINT"
  # `--retries` re-submits a fresh Batch job on a worker failure (a reclaim included; see "Spot
  # reclaims" in entrypoint.sh), and `--keep-going` lets the rest of the DAG finish rather than
  # aborting on the first failure. Default to 3 retries + keep-going (the value spot-backed
  # benchmark fleets use); BIFFIX_RETRIES / BIFFIX_KEEP_GOING override (set 0 / false to opt out).
  local RETRIES="${BIFFIX_RETRIES:-3}" MAX_JOBS="${BIFFIX_MAX_JOBS:-100}"
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
  local CORES="${BIFFIX_CORES:-64}"
  require_count BIFFIX_CORES "$CORES"
  [ "$CORES" -ge 1 ] || runtime_error "BIFFIX_CORES must be at least 1: $CORES"
  local -a KEEP_GOING_ARGS=(--keep-going)
  local keep_going="${BIFFIX_KEEP_GOING:-true}"
  case "${keep_going,,}" in
    true | 1 | yes | on) ;;
    false | 0 | no | off) KEEP_GOING_ARGS=() ;;
    *) runtime_error "BIFFIX_KEEP_GOING is not a boolean (true/1/yes/on or false/0/no/off): $keep_going" ;;
  esac
  SNAKEMAKE_AWS_BATCH_JOB_TAGS=$(snakemake_job_tags "${SNAKEMAKE_AWS_BATCH_JOB_TAGS:-}")
  export SNAKEMAKE_AWS_BATCH_JOB_TAGS
  # The between-workflow cache: the inner Snakemake on each worker fetches/stores it, so the
  # location is forwarded to jobs (--envvars) along with --cache. The rules are named explicitly: a
  # bare `--cache` ("every eligible rule") is an empty list, which Snakemake drops when it builds a
  # remote job's command line, so the jobs would never cache. Only the rules marked `cache:` may be
  # named (Snakemake rejects any other), so they are read from the workflow itself. The helper
  # prints each as `BIFFIX_CACHE_RULE=<name>` (loading the workflow runs its own code, which may
  # print too); its errors go to this log.
  local -a CACHE_ARGS=()
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
      CACHE_ARGS=(--envvars SNAKEMAKE_OUTPUT_CACHE --cache "${CACHE_RULES[@]}")
    else
      echo "biffix-coordinator: no rule is marked cache:; running without the cache" >&2
    fi
  fi
  local -a ARGS=(--configfile "$CONFIG" "${CACHE_ARGS[@]}")
  if is_single_job; then
    snakemake_single_job_args
  else
    snakemake_batch_args
  fi
  ARGS+=(
    # What makes Snakemake re-run a job whose output exists: not a change of its software
    # environment, whose recorded hash differs between the two modes (tasks mode, which passes no
    # --sdm, records a constant one; a single-job run, with --sdm apptainer, hashes each job's
    # image), so either mode's metadata serves a resume or a later run into the same output prefix.
    # The cost: a single-job run does not re-run a job only because its image changed.
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

# snakemake_single_job_args: run_snakemake's ARGS for a single-job run (lib/single_job.sh): every
# job runs on this host, in the image its job on AWS Batch would run, from the image files the
# coordinator converted before the run. Reads run_snakemake's SNAKEFILE.
snakemake_single_job_args() {
  # A wrapper Snakefile beside the workflow's (so its relative includes and scripts resolve as
  # before) includes it, then sets every rule's container, module rules' included, as the AWS Batch
  # executor picks a job's image: the rule's aws_batch_container_image through the image map, else
  # the version's default image (none: the rule runs in this container, the image its Batch job
  # would have run). A rule's own `container:` is ignored, as on AWS Batch. An image the map does
  # not name fails the run before any job starts, so no task pulls one (single_job.py
  # set_rule_containers). It also writes each rule's image to BIFFIX_TASK_IMAGES_FILE, for the
  # logger plugin to record each task's image.
  local snakefile check=check
  [ -z "${BIFFIX_DRY_RUN:-}" ] || check=no-check
  export BIFFIX_TASK_IMAGES_FILE="$WORK/task-images.json"
  run_setup_step "Writing the Snakefile that sets each rule's image failed" \
    python3 "$LIB_DIR/single_job.py" snakefile-wrapper "$SNAKEFILE" "" \
    "${BIFFIX_IMAGE_MAP_FILE:-}" "$SJ_IMAGES" "${BIFFIX_WORKER_IMAGE:-}" \
    "$BIFFIX_TASK_IMAGES_FILE" "$check" > "$WORK/snakefile-wrapper-path"
  IFS= read -r snakefile < "$WORK/snakefile-wrapper-path"
  cp "$snakefile" "$WORK/Snakefile.biffix"
  # Snakemake schedules its jobs within the job's size less its own reserve (single_job_memory), by
  # their memory resource.
  local memory_values
  memory_values=$(snakemake_memory_resources "$BIFFIX_ENGINE_VERSION" "$SJ_TASK_MEMORY_MIB") \
    || runtime_error "could not determine Snakemake's single-job memory resources"
  local -a memory
  mapfile -t memory <<< "$memory_values"
  ARGS+=(
    --snakefile "$snakefile"
    --executor local
    --cores "$SJ_CPUS"
    --resources "${memory[@]}"
    # Every job in its image, through biffix's apptainer wrapper (lib/single_job.sh), from the
    # converted image files. --cleanenv covers Apptainer-wrapped jobs only. The logger also removes
    # the ingest token from the engine's environment after capturing it for authentication, so
    # unwrapped local shells do not inherit it (not a /proc sandbox, lib/single_job.sh).
    # The wrapper passes a job's thread counts (OMP_NUM_THREADS
    # and the like) through it, but no workflow `envvars:`. Its working directory (the source root,
    # its home in the container) and the local storage are bound by Snakemake itself.
    --sdm apptainer
    --apptainer-prefix "$SCRATCH/apptainer/snakemake"
    --apptainer-args=--cleanenv
    # Inputs fetched from S3, and local copies of outputs (each still published to the run's
    # output prefix), on the instance store.
    --local-storage-prefix "$SCRATCH/storage"
  )
  export PATH="$SJ_BIN:$PATH"
}

# snakemake_memory_resources <snakemake version> <MiB>: the --resources values that cap the memory
# of the jobs running at once at <MiB>, one per line. From 9.16.3, Snakemake reads mem_mb, mem_mib
# and mem as one resource (a rule asking for any is capped by any) and refuses more than one of
# them on the command line; before it, mem_mb and mem_mib are separate (a rule's mem_mb implies its
# mem_mib, not the other way round), so both are given.
snakemake_memory_resources() {
  local mib=$2 major minor patch
  IFS=. read -r major minor patch <<<"$1"
  printf 'mem_mb=%s\n' "$((mib * 1048576 / 1000000))"
  major=${major//[!0-9]/} minor=${minor//[!0-9]/} patch=${patch%%[!0-9]*}
  if [ $((10#${major:-0} * 1000000 + 10#${minor:-0} * 1000 + 10#${patch:-0})) -lt 9016003 ]; then
    printf 'mem_mib=%s\n' "$mib"
  fi
}

# snakemake_batch_args: run_snakemake's ARGS for a run whose jobs are AWS Batch jobs. Reads
# run_snakemake's SNAKEFILE, MAX_JOBS and CORES.
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
  # Spot reclaims (see "Spot reclaims" in entrypoint.sh).
  local SPOT_ATTEMPTS="${BIFFIX_SPOT_ATTEMPTS:-5}"
  require_count BIFFIX_SPOT_ATTEMPTS "$SPOT_ATTEMPTS"
  ARGS+=(
    --aws-batch-spot-attempts "$SPOT_ATTEMPTS"
    --jobs "$MAX_JOBS"
    --cores "$CORES"
  )
}
