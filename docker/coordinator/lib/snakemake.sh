# shellcheck shell=bash
# biffix coordinator: the Snakemake engine. Sourced by entrypoint.sh; defines functions only.
#   Reads: WORK, SRC, CONFIG, STORE, LIB_DIR, SM_META_URI, SM_META_FINGERPRINT, the run's BIFFIX_*
#          env and the resolved SNAKEMAKE_AWS_BATCH_JOB_QUEUE / _ROLE.
#   Sets:  rc (Snakemake's exit status), SNAKEMAKE_AWS_BATCH_JOB_TAGS, SNAKEMAKE_OUTPUT_CACHE,
#          SM_META_RESTORE_FAILED.

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

# Run Snakemake with the AWS Batch executor + the biffix logger plugin.
run_snakemake() {
  local SNAKEFILE="$SRC/$BIFFIX_ENTRYPOINT"
  # `--retries` re-submits a fresh Batch job on a worker failure (a reclaim included; see "Spot
  # reclaims" in entrypoint.sh), and `--keep-going` lets the rest of the DAG finish rather than
  # aborting on the first failure. Default to 3 retries + keep-going (the value spot-backed
  # benchmark fleets use); BIFFIX_RETRIES / BIFFIX_KEEP_GOING override (set 0 / false to opt out).
  local RETRIES="${BIFFIX_RETRIES:-3}" MAX_JOBS="${BIFFIX_MAX_JOBS:-100}"
  require_count BIFFIX_RETRIES "$RETRIES"
  require_count BIFFIX_MAX_JOBS "$MAX_JOBS"
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
  local -a ARGS=(
    --snakefile "$SNAKEFILE"
    --configfile "$CONFIG"
    "${CACHE_ARGS[@]}"
    --executor aws-batch
    --aws-batch-region "$BIFFIX_REGION"
    --aws-batch-job-queue "$SNAKEMAKE_AWS_BATCH_JOB_QUEUE"
    --aws-batch-job-role "$SNAKEMAKE_AWS_BATCH_JOB_ROLE"
  )
  [ -z "${BIFFIX_WORKER_IMAGE:-}" ] || ARGS+=(--container-image "$BIFFIX_WORKER_IMAGE")
  # Spot reclaims and task logs (see "Spot reclaims" and "Project isolation" in entrypoint.sh).
  local SPOT_ATTEMPTS="${BIFFIX_SPOT_ATTEMPTS:-5}"
  require_count BIFFIX_SPOT_ATTEMPTS "$SPOT_ATTEMPTS"
  ARGS+=(
    --aws-batch-spot-attempts "$SPOT_ATTEMPTS"
    --aws-batch-log-group "$BIFFIX_TASK_LOG_GROUP"
    --default-storage-provider s3
    --default-storage-prefix "$STORE"
    --logger biffix
    --jobs "$MAX_JOBS"
    --retries "$RETRIES"
    "${KEEP_GOING_ARGS[@]}"
    --verbose
    --rerun-incomplete
  )
  [ -z "${BIFFIX_DRY_RUN:-}" ] || ARGS+=(--dry-run)
  [ -z "${BIFFIX_TARGET:-}" ] || ARGS+=("$BIFFIX_TARGET")
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
