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
#                             Snakemake version with none) this coordinator variant itself
#   BIFFIX_IMAGE_MAP_URI    - optional s3:// uri of the run's images.json (declared image -> the
#                             tenant copy to run; crates/biffix-backend/src/images/map.rs). Set =>
#                             every task image resolves through it and an image not in it fails the
#                             task (Snakemake: the executor's container image map; Nextflow:
#                             nf-biffix's container resolver). A runtime whose engine cannot
#                             resolve through it fails the run (78) rather than run unmapped.
# Baked into the job definition (infra/biffix_infra/constructs/coordinator.py):
#   BIFFIX_QUEUE_CLASS_MAP  - JSON {queue_class_name: job_queue_arn}, the WORKER queue-classes only
#                             (the coordinator's is not in it), in deploy-config order
#   BIFFIX_REGION           - required: the deploy's region (Snakemake: --aws-batch-region, as
#                             the executor reads no env var for it; Nextflow: aws.region)
#   BIFFIX_TASK_LOG_GROUP   - required: the project's own CloudWatch log group for task jobs
#                             (Nextflow: aws.batch.logsGroup; Snakemake: --aws-batch-log-group; see
#                             "Project isolation" below)
# Optional, from the backend: BIFFIX_QUEUE_CLASS (worker queue-class to submit child jobs to;
#   default: the map's first), SNAKEMAKE_AWS_BATCH_JOB_ROLE (the job role child jobs assume; the
#   project run base role; the Nextflow path reads it too, as it does the worker queue resolved
#   into the Snakemake executor's variable: the executor's names serve both engines),
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
#   and writes nothing to S3 (no metadata sync, no events.jsonl upload, no Nextflow cloud cache or
#   remote bin dir); it still reads the source, the image map and any metadata to restore.
#   This is the fast local check: `docker run` this image with the real env and BIFFIX_DRY_RUN=1.
#
# The process exit code is the run's outcome: AWS Batch reports it via EventBridge, which the
# ingest path folds into the run's terminal state.
# shellcheck source-path=SCRIPTDIR
set -euo pipefail

# Set by Batch: AWS_BATCH_JOB_ATTEMPT (a later attempt of a Nextflow run resumes the session its
#   first attempt started; lib/nextflow.sh).
# Debug overrides: nothing biffix deploys sets them (neither the backend nor the job definition),
# so the defaults are the deployed behaviour; the tests and a manual `docker run` may change them.
#   BIFFIX_SPOT_ATTEMPTS      Batch attempts per task job on host terminations (default 5)
#   BIFFIX_METADATA_SYNC_SECS Snakemake: how often the .snakemake metadata is synced (default 300)
#   BIFFIX_MAX_JOBS           the engine's --jobs (Snakemake; default 100)
#   BIFFIX_STOP_GRACE_SECS    how long a cancelled engine has to stop before it is killed, leaving
#                             the rest of Batch's 30 s to the post-engine steps (default 20;
#                             lib/signals.sh)
#   BIFFIX_EVENT_FILE         the engine plugin's events.jsonl, relative to the engine's launch
#                             dir or absolute (default .biffix_events.jsonl; lib/events.sh)
# Test hooks: BIFFIX_RUNTIME_ROOT (/opt/biffix), BIFFIX_WORK_ROOT (the job's scratch, /work) and
#   BIFFIX_LIB_DIR (this script's libraries and helpers, /usr/local/lib/biffix).

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
# run role, and submits only child job definitions tagged with its own project
# (infra/biffix_infra/stacks/compute_stack.py grant_coordinator):
#   - Job definitions. Snakemake's executor registers each child job definition with the job's tags
#     (biffix:run, biffix:project). Nextflow's are registered by nf-biffix's `awsbatch` executor
#     (nf-amazon's, under the same name), which tags each `nf-…` definition
#     biffix:project=$BIFFIX_PROJECT next to nf-amazon's own nextflow.io/* tags, hashes the project
#     into its nf-token, and reuses only a definition tagged with the project. The project only,
#     never biffix:run: a definition is reused by every later run of the project, so a run tag
#     would name whichever run first registered it. IAM requires that tag to register
#     an `nf-*` definition, and to submit either engine's (a `name:revision` tagged with this
#     project; both engines submit by revision), so a coordinator cannot run as another
#     project's run role through its definitions.
#   - Task logs. Task jobs log to the project's own group (BIFFIX_TASK_LOG_GROUP): Nextflow's as
#     aws.batch.logsGroup, Snakemake's as --aws-batch-log-group (upstream PR #61, in the pinned
#     fork). Only that project's coordinator (and the backend) may read it, and a coordinator may
#     register a job definition that logs only there or to /aws/batch/job, which no coordinator may
#     read.

RUNTIME_ROOT="${BIFFIX_RUNTIME_ROOT:-/opt/biffix}"
select_runtime
install_signal_traps

SRC="$WORK/src"
mkdir -p "$SRC"
cd "$WORK"

echo "biffix-coordinator: run=$BIFFIX_RUN_ID engine=$ENGINE" >&2

stage_workflow_source
materialize_params
fetch_image_map

# The workflow's default storage prefix. Default: the isolated per-run path, so two runs never
# collide. A submit may override it (BIFFIX_OUTPUT_PREFIX, validated backend-side to be within the
# project bucket) to a caller-chosen stable prefix — e.g. a SHA-keyed benchmark root, so a re-run
# skips outputs that already exist and its own tooling finds them where it expects. biffix's own run
# artifacts (events.jsonl, task records) stay under BIFFIX_RUN_PREFIX regardless.
STORE="${BIFFIX_OUTPUT_PREFIX:-s3://$BIFFIX_PROJECT_BUCKET/${BIFFIX_RUN_PREFIX}}"

track_staged_tree
resolve_worker_queue

echo "biffix-coordinator: worker queue=$SNAKEMAKE_AWS_BATCH_JOB_QUEUE image=${BIFFIX_WORKER_IMAGE:-<default>} store=$STORE" >&2

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
case "$ENGINE" in
  snakemake) run_snakemake ;;
  nextflow)  run_nextflow ;;
esac

# Both are best-effort and MUST NOT change the run's outcome: guard each call with `|| true` so a
# non-zero from the archival upload or the replay backstop (even outside their own try/except)
# can never abort the script under `set -e` and overwrite the engine's real exit code.
upload_events_jsonl || true
replay_events_jsonl || true
exit "$rc"
