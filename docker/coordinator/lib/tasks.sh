# shellcheck shell=bash
# biffix coordinator: what the engine's child (task) jobs run with: their images, queue and role.
# Sourced by entrypoint.sh; defines functions only.
#   Reads: WORK, LIB_DIR, ENGINE, BIFFIX_IMAGE_MAP_URI, BIFFIX_JOB_DEFINITION_MAP_URI,
#          BIFFIX_QUEUE_CLASS, BIFFIX_QUEUE_CLASS_MAP, BIFFIX_QUEUE_ASSIGNMENT,
#          SNAKEMAKE_AWS_BATCH_JOB_QUEUE, SNAKEMAKE_AWS_BATCH_JOB_ROLE.
#   Sets:  SNAKEMAKE_AWS_BATCH_JOB_QUEUE, QUEUE_SELECTORS_FILE; with an image map,
#          SNAKEMAKE_AWS_BATCH_CONTAINER_IMAGE_MAP and BIFFIX_IMAGE_MAP_FILE, and
#          SNAKEMAKE_AWS_BATCH_JOB_DEFINITION_MAP and BIFFIX_JOB_DEFINITION_MAP_FILE.

# fetch_image_map: the run's image map, when it has one; every task image resolves through it
# (strict). Exit 78 when it cannot be read.
fetch_image_map() {
  [ -n "${BIFFIX_IMAGE_MAP_URI:-}" ] || return 0
  local image_map="$WORK/images.json"
  python3 "$LIB_DIR/image_map.py" "$BIFFIX_IMAGE_MAP_URI" "$image_map" \
    || runtime_error "could not read the image map $BIFFIX_IMAGE_MAP_URI"
  echo "biffix-coordinator: task images resolve through $BIFFIX_IMAGE_MAP_URI" >&2
  # Snakemake: the AWS Batch executor's container_image_map setting (read from its env var).
  # Nextflow: nf-biffix's container resolver.
  export SNAKEMAKE_AWS_BATCH_CONTAINER_IMAGE_MAP="$image_map" BIFFIX_IMAGE_MAP_FILE="$image_map"
}

# fetch_job_definition_map: the run's map of image to task job definition, which the backend writes
# beside the image map (crates/biffix-backend/src/aws/task_definitions.rs). Every task runs on the
# job definition biffix registered for its image; the engines register none (IAM denies the
# coordinator every job-definition write). The backend writes the two together, so it is read only
# with an image map (without one, a local composition, the engine registers its own), and is
# required with one: exit 78 without it, or when it cannot be read.
fetch_job_definition_map() {
  [ -n "${BIFFIX_IMAGE_MAP_URI:-}" ] || return 0
  [ -n "${BIFFIX_JOB_DEFINITION_MAP_URI:-}" ] \
    || runtime_error "BIFFIX_IMAGE_MAP_URI is set without BIFFIX_JOB_DEFINITION_MAP_URI: tasks would have no job definitions (deploy the backend at the same release as this coordinator image)"
  local definitions="$WORK/job-definitions.json"
  python3 "$LIB_DIR/image_map.py" "$BIFFIX_JOB_DEFINITION_MAP_URI" "$definitions" "job definition map" \
    || runtime_error "could not read the job definition map $BIFFIX_JOB_DEFINITION_MAP_URI"
  echo "biffix-coordinator: tasks run on the job definitions in $BIFFIX_JOB_DEFINITION_MAP_URI" >&2
  # Snakemake: the AWS Batch executor's job_definition_map setting (read from its env var).
  # Nextflow: nf-biffix's task handler.
  export SNAKEMAKE_AWS_BATCH_JOB_DEFINITION_MAP="$definitions" BIFFIX_JOB_DEFINITION_MAP_FILE="$definitions"
}

# resolve_worker_queue: the worker queue ARN the engine submits child jobs to, and, for an engine
# that registers its own task job definitions (a run without a job definition map), the role they
# assume (exit 78 without either). The engine's AWS Batch executor wants a queue ARN;
# BIFFIX_QUEUE_CLASS names which queue-class from the deploy menu to use, and defaults to the FIRST
# entry in the map (map insertion order == the deploy config order, which lists the intended default
# first). The map holds only worker queue-classes: the coordinator's queue is not one a child job
# may use, and it is left out of the map rather than guessed by name.
resolve_worker_queue() {
  if [ -z "${SNAKEMAKE_AWS_BATCH_JOB_QUEUE:-}" ]; then
    SNAKEMAKE_AWS_BATCH_JOB_QUEUE=$(python3 "$LIB_DIR/worker_queue.py" "${BIFFIX_QUEUE_CLASS:-}")
    export SNAKEMAKE_AWS_BATCH_JOB_QUEUE
  fi
  [ -n "${SNAKEMAKE_AWS_BATCH_JOB_QUEUE:-}" ] \
    || runtime_error "no worker queue resolved (BIFFIX_QUEUE_CLASS='${BIFFIX_QUEUE_CLASS:-}')"
  [ -n "${BIFFIX_JOB_DEFINITION_MAP_FILE:-}" ] || require_env SNAKEMAKE_AWS_BATCH_JOB_ROLE "child-job role"
}

# resolve_queue_selectors: the tasks the version sends to another worker queue than the default
# (BIFFIX_QUEUE_ASSIGNMENT, by label or name), in the engine's own terms, in QUEUE_SELECTORS_FILE
# (empty without an assignment; queue_selectors.py): Nextflow withLabel / withName `queue`
# selectors for the -c config, one per line, or the Snakemake executor's rule queue map. Each
# queue-class resolves through the same map as the default; exit 78 when one does not. Resolved
# before the engine starts, so a dry run checks it too.
resolve_queue_selectors() {
  QUEUE_SELECTORS_FILE="$WORK/.biffix-queue-selectors"
  : >"$QUEUE_SELECTORS_FILE"
  [ -n "${BIFFIX_QUEUE_ASSIGNMENT:-}" ] || return 0
  python3 "$LIB_DIR/queue_selectors.py" "$ENGINE" >"$QUEUE_SELECTORS_FILE" \
    || runtime_error "could not resolve the run's queue assignment (BIFFIX_QUEUE_ASSIGNMENT)"
}
