# shellcheck shell=bash
# biffix coordinator: what the engine's child (task) jobs run with: their images, queue and role.
# Sourced by entrypoint.sh; defines functions only.
#   Reads: WORK, LIB_DIR, BIFFIX_IMAGE_MAP_URI, BIFFIX_QUEUE_CLASS, BIFFIX_QUEUE_CLASS_MAP,
#          SNAKEMAKE_AWS_BATCH_JOB_QUEUE, SNAKEMAKE_AWS_BATCH_JOB_ROLE.
#   Sets:  SNAKEMAKE_AWS_BATCH_JOB_QUEUE; with an image map, SNAKEMAKE_AWS_BATCH_CONTAINER_IMAGE_MAP
#          and BIFFIX_IMAGE_MAP_FILE.

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

# resolve_worker_queue: the worker queue ARN the engine submits child jobs to, and the role they
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
  require_env SNAKEMAKE_AWS_BATCH_JOB_ROLE "child-job role"
}
