#!/usr/bin/env bash
# biffix coordinator entrypoint.
#
# Contract (env the backend injects at launch, crates/biffix-backend/src/aws/coordinator_env.rs):
#   BIFFIX_RUN_ID           - the run this coordinator drives (also read by the logger plugin)
#   BIFFIX_INGEST_URL       - ingest route base; the logger plugin POSTs {url}/events
#   BIFFIX_INGEST_TOKEN     - per-run capability token (logger plugin auth)
#   BIFFIX_WORKFLOW_SOURCE  - JSON: {"kind":"git","repo":..,"git_ref":..} | {"kind":"s3","uri":..}
#                             (an s3 tarball also carries the "sha256" registration recorded for it,
#                             which the staged tarball must match)
#   BIFFIX_GIT_SHA          - exact commit to check out (reproducibility)
#   BIFFIX_ENGINE           - "snakemake" (default) | "nextflow"
#   BIFFIX_ENGINE_VERSION   - the exact engine release the version pins; this coordinator variant's
#                             runtime layer (/opt/biffix/sm or /opt/biffix/nf) must be that release
#   BIFFIX_RUNTIME_KEY      - the runtime the variant was built from (logged, for provenance)
#   BIFFIX_ENTRYPOINT       - engine entrypoint relative to the source root (e.g. workflow/Snakefile)
#   BIFFIX_TARGET           - optional target to build (Snakemake target rule/output, or Nextflow `-entry`);
#                             a relative path is the source root's, both engines' working directory
#   BIFFIX_OUTPUT_PREFIX    - optional workflow default-storage prefix (s3://…, within the project
#                             bucket); defaults to s3://$BIFFIX_PROJECT_BUCKET/$BIFFIX_RUN_PREFIX
#   BIFFIX_PARAMS           - JSON object materialized into the engine config
#   BIFFIX_PROJECT_BUCKET   - project bucket for run I/O
#   BIFFIX_PROJECT          - the run's project id; every child job must be tagged
#                             `biffix:project=<it>` (the coordinator role may submit, tag and
#                             terminate only jobs of its own project)
#   BIFFIX_RUN_PREFIX       - key prefix under the bucket (biffix/runs/{run_id}/)
#   BIFFIX_ENGINE_SESSION_ID - Nextflow only: the session uuid biffix minted for this run (exported
#                             as NXF_UUID), or, with BIFFIX_RESUME=1, the PRIOR run's session to
#                             `-resume`. Its cache lives at $STORE.nextflow/cache/<id>/.
#   BIFFIX_RESUME           - "1" => resume the prior run in the same STORE (forced backend-side).
#                             Nextflow: `-resume $BIFFIX_ENGINE_SESSION_ID`. Snakemake: rerun only
#                             what is missing, stale or incomplete, judged by the .snakemake metadata
#                             kept at $STORE.snakemake/ (restored for every run into a STORE that
#                             has it, so a stable output prefix keeps one merged record).
#   BIFFIX_SNAKEMAKE_CACHE  - Snakemake only: an s3:// prefix for the between-workflow output cache
#                             (the version opted in with `cache: true`); set => `--cache` for the
#                             workflow's `cache: True` rules.
#   BIFFIX_WORKER_IMAGE     - image the engine runs child jobs with (Snakemake --container-image,
#                             Nextflow process.container): the version's default image, else (a
#                             Snakemake version with none) this coordinator variant itself, except
#                             in a single-job run (unset: such tasks run in this job itself)
#   BIFFIX_IMAGE_MAP_URI    - optional s3:// uri of the run's images.json (declared image -> the
#                             tenant copy to run; crates/biffix-backend/src/images/map.rs). Set =>
#                             every task image resolves through it and an image not in it fails the
#                             task (Snakemake: the executor's container image map; Nextflow:
#                             nf-biffix's container resolver). A runtime whose engine cannot
#                             resolve through it fails the run (78) rather than run unmapped.
#   BIFFIX_JOB_DEFINITION_MAP_URI - s3:// uri of the run's job-definitions.json (tenant image -> the
#                             task job definition the backend registered for it;
#                             crates/biffix-backend/src/aws/task_definitions.rs), sent with the
#                             image map and required with it (78 without). Every task runs on the
#                             definition of its image and the engines register none (Snakemake:
#                             the executor's job definition map; Nextflow: nf-biffix's handler);
#                             see "Project isolation" below. Not sent, and not read, for a
#                             single-job run.
#   BIFFIX_EXECUTION        - "tasks" (the default: every task is a Batch job of its own) |
#                             "single_job": the whole run in this one job, on a host of its own,
#                             every task in its image through Apptainer nested in this container
#                             (lib/single_job.sh); anything else fails the run (78). The job runs as
#                             the project's run role, with the host's instance store at /scratch
#                             and /dev/fuse; it needs no worker queue, no job definition map and no
#                             SNAKEMAKE_AWS_BATCH_JOB_ROLE. Then also, required (78 without):
#   BIFFIX_SINGLE_JOB_CPUS  - the vCPUs the job was given (its whole host's): the engine runs
#                             its tasks within them
#   BIFFIX_SINGLE_JOB_MEMORY_MIB - the memory the job was given, MiB: the engine's tasks get it less
#                             a reserve for the engine itself, and must get at least 1 GiB (78)
#                             A single-job run never resumes (BIFFIX_RESUME=1 is refused, 78): a
#                             later Batch attempt of it starts afresh.
# Baked into the job definition (infra/biffix_infra/constructs/coordinator.py):
#   BIFFIX_QUEUE_CLASS_MAP  - JSON {queue_class_name: job_queue_arn}, the WORKER queue-classes only
#                             (the coordinator's is not in it), in deploy-config order
#   BIFFIX_REGION           - required: the deploy's region (Snakemake: --aws-batch-region, as
#                             the executor reads no env var for it; Nextflow: aws.region)
#   BIFFIX_TASK_LOG_GROUP   - required: the project's own CloudWatch log group for task jobs, which
#                             the backend's task job definitions name (Nextflow reads a failed
#                             task's log from it, aws.batch.logsGroup; without a job definition map
#                             the engines name it in their own: Snakemake --aws-batch-log-group; see
#                             "Project isolation" below)
#   BIFFIX_SCRATCH_ROOT     - the single-job template only: the host's instance store, a single-job
#                             run's work root (/scratch; lib/single_job.sh)
#   BIFFIX_INSTANCE_STORE_MARKER - the single-job template only: the file the host's boot writes
#                             once it has mounted the instance store at the scratch root, outside
#                             it and mounted read-only (/var/lib/biffix/instance-store)
# Optional, from the backend: BIFFIX_QUEUE_CLASS (worker queue-class to submit child jobs to;
#   default: the map's first), BIFFIX_QUEUE_ASSIGNMENT (JSON {"by_label": {label: queue_class},
#   "by_name": {name: queue_class}}: the tasks the version sends to another worker queue-class,
#   from its biffix.yaml `queues:`; Nextflow: withLabel/withName `queue` selectors in the -c
#   config; Snakemake: the executor's --aws-batch-rule-queues map; lib/tasks.sh),
#   SNAKEMAKE_AWS_BATCH_JOB_ROLE (the project run base role, which child
#   jobs assume; required, and handed to the engine, only without a job definition map, where the
#   engine registers its own job definitions: the backend's carry it; the Nextflow path reads it
#   too, as it does the worker queue resolved into the Snakemake executor's variable: the
#   executor's names serve both engines),
#   SNAKEMAKE_AWS_BATCH_JOB_TAGS (`biffix:run=<run id>`: the Snakemake
#   executor tags every child job with it, and this script adds the run and project tags below
#   when missing; the Nextflow path tags them via process.resourceLabels).
#   The backend's POST /events accepts a task_submitted claim on a Batch job only when that tag
#   names the claiming run, so a child job without it is untracked.
# Optional retry policy for application failures: BIFFIX_RETRIES (worker re-submits on failure;
#   default 3) and BIFFIX_KEEP_GOING (finish the rest of the DAG on a failure; true/1/yes/on or
#   false/0/no/off, any case; default true).
#   Snakemake → --retries/--keep-going; Nextflow → process.errorStrategy 'retry'/maxRetries.
#   Both are DEFAULTS: a workflow's own retry settings win (a Snakemake rule's `retries:`, a Nextflow
#   pipeline's errorStrategy/maxRetries in its config or process definitions). Spot reclaims are the
#   other layer (see "Spot reclaims" below).
# Optional, for validation: BIFFIX_DRY_RUN=1 runs the engine in --dry-run mode, which exercises
#   source staging, config materialization, and queue resolution WITHOUT submitting child jobs,
#   and writes nothing to S3 (no metadata sync, no events.jsonl upload; Nextflow previews with the
#   local executor and a local work dir, no cloud cache or remote bin dir); it still reads the
#   source, the image map, the job definition map (tasks mode) and any metadata to restore.
#   This is the fast local check: `docker run` this image with the real env and BIFFIX_DRY_RUN=1.
#
# The process exit code is the run's outcome: AWS Batch reports it via EventBridge, which the
# ingest path folds into the run's terminal state.
# shellcheck source-path=SCRIPTDIR
set -euo pipefail

# Set by Batch: AWS_BATCH_JOB_ATTEMPT (a later attempt of a Nextflow run resumes the session its
#   first attempt started, unless it is a single-job run; lib/nextflow.sh).
# Debug overrides: nothing biffix deploys sets them (neither the backend nor the job definition),
# so the defaults are the deployed behaviour; the tests and a manual `docker run` may change them.
#   BIFFIX_SPOT_ATTEMPTS      Batch attempts per task job on host terminations (default 5)
#   BIFFIX_METADATA_SYNC_SECS Snakemake: how often the .snakemake metadata is synced (default 300)
#   BIFFIX_MAX_JOBS           the engine's --jobs (Snakemake; default 100)
#   BIFFIX_CORES              Snakemake's --cores: workflow.cores, and the most threads (so Batch
#                             vCPUs) any one job gets (default 64; lib/snakemake.sh)
#   BIFFIX_STOP_GRACE_SECS    how long a cancelled engine has to stop before it is killed, leaving
#                             the rest of Batch's 30 s to the post-engine steps (default 20;
#                             lib/signals.sh)
#   BIFFIX_EVENT_FILE         the engine plugin's events.jsonl, relative to the engine's launch
#                             dir or absolute (default .biffix_events.jsonl; lib/events.sh)
#   BIFFIX_LOGS_SYNC_SECS     how often the run's logs are uploaded while the engine runs
#                             (default 120; lib/logs.sh)
#   BIFFIX_LOGS_FINAL_SECS    the most the logs upload at exit may take (default 8, less in a run
#                             a signal stopped; lib/logs.sh)
#   BIFFIX_SIF_PARALLEL       single-job: how many task images are converted at once (default 4,
#                             at least 1; lib/single_job.py)
# Test hooks: BIFFIX_RUNTIME_ROOT (/opt/biffix), BIFFIX_WORK_ROOT (the job's scratch, /work; a
#   single-job run's work root is $BIFFIX_SCRATCH_ROOT/run instead, lib/single_job.sh),
#   BIFFIX_LIB_DIR (this script's libraries and helpers, /usr/local/lib/biffix) and
#   BIFFIX_OOM_SCORE_ADJ_FILE (where a single-job task writes its oom score,
#   /proc/self/oom_score_adj).
#
# Logs (both modes): the engine's log, a Nextflow trace the pipeline itself turned on, the engine
# config biffix generated, failed local tasks' files and, for a failed run, failure.txt, at
# s3://$BIFFIX_PROJECT_BUCKET/${BIFFIX_RUN_PREFIX}logs/, while the engine runs and at exit
# (lib/logs.sh).

WORK="${BIFFIX_WORK_ROOT:-/work}"
LIB_DIR="${BIFFIX_LIB_DIR:-/usr/local/lib/biffix}"

# The script's steps, one library per concern (lib/ in the repo, /usr/local/lib/biffix in the image):
# each defines functions only, which the steps below call in order. A library reads the globals set
# here and by the steps before it (its header names them).
# shellcheck source=lib/common.sh
. "$LIB_DIR/common.sh"
# shellcheck source=lib/runtime.sh
. "$LIB_DIR/runtime.sh"
# shellcheck source=lib/signals.sh
. "$LIB_DIR/signals.sh"
# shellcheck source=lib/source.sh
. "$LIB_DIR/source.sh"
# shellcheck source=lib/tasks.sh
. "$LIB_DIR/tasks.sh"
# shellcheck source=lib/events.sh
. "$LIB_DIR/events.sh"
# shellcheck source=lib/logs.sh
. "$LIB_DIR/logs.sh"
# shellcheck source=lib/single_job.sh
. "$LIB_DIR/single_job.sh"
# shellcheck source=lib/snakemake.sh
. "$LIB_DIR/snakemake.sh"
# shellcheck source=lib/nextflow.sh
. "$LIB_DIR/nextflow.sh"

# The contract's required variables: a run without one is misconfigured (EX_CONFIG, 78).
require_env BIFFIX_RUN_ID
require_env BIFFIX_ENTRYPOINT
require_env BIFFIX_PROJECT_BUCKET
require_env BIFFIX_PROJECT "every child job is tagged with the run's project"
require_env BIFFIX_TASK_LOG_GROUP
require_env BIFFIX_REGION
require_env BIFFIX_RUN_PREFIX
require_env BIFFIX_ENGINE_VERSION
ENGINE="${BIFFIX_ENGINE:-snakemake}"

# Spot reclaims. Infrastructure interruptions belong in AWS Batch's own retryStrategy
# (transparent to the engine, same batch_job_id; ingest folds a retried job's attempts) and
# application failures in the engine's retries (a new Batch job).
#   - Nextflow: nf-amazon gives every task job a retryStrategy of BIFFIX_SPOT_ATTEMPTS attempts that
#     retries only a host termination (statusReason `Host EC2*`) and exits on anything else
#     (aws.batch.maxSpotAttempts), so a reclaim does not spend the errorStrategy's maxRetries.
#   - Snakemake: the AWS Batch executor submits every task job with the same retryStrategy, of
#     BIFFIX_SPOT_ATTEMPTS attempts (--aws-batch-spot-attempts; upstream since its PR #62),
#     so a reclaim does not spend one of the rule's --retries. A rule's aws_batch_spot_attempts
#     resource overrides it.

# Project isolation. Each project's coordinator role reaches only its own project's jobs, bucket and
# run role, and submits child jobs only on the task job definitions biffix registered for its own
# project (infra/biffix_infra/constructs/batch_access.py):
#   - Job definitions. The coordinator runs the workflow's own code, so it never registers a job
#     definition (IAM denies it every job-definition write but tagging its own task definitions
#     with its own project, as a tagged submit requires): a definition decides whether a
#     container is privileged and which host paths it sees, on hosts every project shares. The
#     backend registers one per image the run may use, at launch (never privileged; no volumes but
#     the Nextflow tasks' read-only host AWS CLI; the project's run role and task log group), tagged
#     biffix:project, and hands the coordinator their map (BIFFIX_JOB_DEFINITION_MAP_URI). The
#     Snakemake executor runs each job on its image's definition (--aws-batch-job-definition-map, in
#     the pinned fork) and so does nf-biffix's `awsbatch` executor (nf-amazon's, under the same
#     name); an image with no definition fails its task. IAM lets the coordinator submit only a
#     `name:revision` of those definitions tagged with its own project, so it cannot run as
#     another project's run role, nor on a definition of anyone else's making.
#   - Task logs. Task jobs log to the project's own group (BIFFIX_TASK_LOG_GROUP), which their job
#     definitions name. Only that project's coordinator (and the backend) may read it; no
#     coordinator may read /aws/batch/job, which holds every project's coordinator logs.

RUNTIME_ROOT="${BIFFIX_RUNTIME_ROOT:-/opt/biffix}"
# A single-job run's work root is elsewhere (lib/single_job.sh), and is checked and cleared
# before the runtime check writes anything under it (Nextflow's NXF_HOME).
select_execution
install_signal_traps
prepare_scratch
select_runtime
# The engine's settings, and a runtime layer that cannot run this run (one without biffix's
# Nextflow plugins, a Snakemake one without the single-job executor), are refused now (exit 78),
# before the source is staged and, for a single-job run, every task image converted. The settings
# are only checked here, in a subshell, so none is left set: run_snakemake and run_nextflow read
# their own.
case "$ENGINE" in
  snakemake) (snakemake_settings); ! is_single_job || require_single_job_executor ;;
  nextflow) (nextflow_settings) ;;
esac

SRC="$WORK/src"
mkdir -p "$SRC"
cd "$WORK"

echo "biffix-coordinator: run=$BIFFIX_RUN_ID engine=$ENGINE execution=$EXECUTION" >&2

stage_workflow_source
materialize_params
fetch_image_map
fetch_job_definition_map

# The workflow's default storage prefix. Default: the isolated per-run path, so two runs never
# collide. A submit may override it (BIFFIX_OUTPUT_PREFIX, validated backend-side to be within the
# project bucket) to a caller-chosen stable prefix — e.g. a SHA-keyed benchmark root, so a re-run
# skips outputs that already exist and its own tooling finds them where it expects. biffix's own run
# artifacts (events.jsonl, task records) stay under BIFFIX_RUN_PREFIX regardless.
STORE="${BIFFIX_OUTPUT_PREFIX:-s3://$BIFFIX_PROJECT_BUCKET/${BIFFIX_RUN_PREFIX}}"

if is_single_job; then
  echo "biffix-coordinator: tasks run on this host; image=${BIFFIX_WORKER_IMAGE:-<this one>} store=$STORE" >&2
  # Every task image as an image file before the engine starts (exit 1 when one cannot be had).
  prepare_task_images
else
  track_staged_tree
  resolve_worker_queue
  resolve_queue_selectors
  echo "biffix-coordinator: worker queue=$SNAKEMAKE_AWS_BATCH_JOB_QUEUE image=${BIFFIX_WORKER_IMAGE:-<default>} store=$STORE" >&2
fi

# Where Snakemake's run metadata is kept (see lib/snakemake.sh), and the record of its last sync.
SM_META_URI="${STORE}.snakemake/biffix-metadata.tar.gz"
SM_META_FINGERPRINT="$WORK/.biffix-metadata-synced"

# Not `exec`: run the engine, then upload the durable events.jsonl, then exit with the engine's
# EXACT code (AWS Batch reports that via EventBridge, which the ingest path folds into the run's
# terminal state), or 143 when a cancel's SIGTERM stopped it (lib/signals.sh). errexit is lifted
# only around the engine so a failing run still uploads and still exits with the real code. Nothing
# here is traced (no `set -x`): the job log would carry the run's parameters; run_engine logs the
# engine's command line, which names only files.
rc=0
# Each engine starts the periodic logs upload (start_logs_sync) just before it runs the engine,
# once nothing can exit the script before stop_logs_sync below.
case "$ENGINE" in
  snakemake) run_snakemake ;;
  nextflow)  run_nextflow ;;
esac
stop_logs_sync

# All are best-effort and MUST NOT change the run's outcome: guard each call with `|| true` so a
# non-zero from the archival upload, the logs upload or the replay backstop (even outside their
# own try/except) can never abort the script under `set -e` and overwrite the engine's real exit
# code. The logs come before the replay, which may take the rest of a cancel's grace.
upload_events_jsonl || true
upload_run_logs final || true
replay_events_jsonl || true
exit "$rc"
