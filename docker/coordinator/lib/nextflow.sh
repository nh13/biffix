# shellcheck shell=bash
# biffix coordinator: the Nextflow engine. Sourced by entrypoint.sh; defines functions only.
#   Reads: WORK, SRC, STORE, PARAMS_JSON, RUNTIME_ROOT, NXF_HOME, NXF_PLUGINS_DIR, the run's BIFFIX_*
#          env, the resolved SNAKEMAKE_AWS_BATCH_JOB_QUEUE / _ROLE, BIFFIX_JOB_DEFINITION_MAP_FILE
#          and QUEUE_SELECTORS_FILE; for a single-job run (lib/single_job.sh) SCRATCH, SJ_CPUS,
#          SJ_TASK_MEMORY_MIB, SJ_JVM_HEAP_MIB, SJ_IMAGES and SJ_BIN.
#   Sets:  rc (Nextflow's exit status); NXF_PLUGINS_DEFAULT, and NXF_UUID or
#          NXF_IGNORE_RESUME_HISTORY; for a single-job run NXF_OPTS, NXF_APPTAINER_NEW_PID_NAMESPACE,
#          PATH, LOCAL_WORK_DIR, and BIFFIX_SINGLE_JOB_CPUS and BIFFIX_SINGLE_JOB_TASK_MEMORY_MIB
#          (exported for nf-biffix's local-executor budget).

# groovy_str <value> <what>: <value> as a Groovy single-quoted string literal for nextflow.config
# (`\` and `'` escaped). A control character (a line break) has no business in any value written
# there, so it is refused rather than escaped.
groovy_str() {
  case "$1" in
    *[[:cntrl:]]*) runtime_error "$2 has a control character; it cannot go into nextflow.config" ;;
  esac
  local v=$1 bs="\\" q="'"
  v=${v//"$bs"/"$bs$bs"}
  v=${v//"$q"/"$bs$q"}
  printf "'%s'" "$v"
}

# queue_selector_config: the -c config lines for the tasks the version sends to other queues, from
# queue_selectors.py's lines on stdin (lib/tasks.sh QUEUE_SELECTORS_FILE): each
# `withLabel|withName <TAB> <selector> <TAB> <queue>`, the selector an exact label or name (the
# script checked it), becomes `process { withLabel: '<selector>' { queue = '<queue>' } }`, the
# queue quoted by groovy_str.
queue_selector_config() {
  local kind selector queue
  while IFS=$'\t' read -r kind selector queue; do
    queue=$(groovy_str "$queue" "a worker queue")
    echo "process { ${kind}: '${selector}' { queue = ${queue} } }"
  done
}

# layer_plugin_version <id>: the version of the one <id> plugin the runtime layer carries
# ($NXF_PLUGINS_DIR/<id>-<version>). It goes into NXF_PLUGINS_DEFAULT, a comma-separated list, so
# only a plain version is accepted. The layer also carries the workflow's own plugins, so only a
# version-shaped suffix is matched; registration also refuses a workflow plugin id that starts
# with one of biffix's and `-` (biffix-workflow runtime/mod.rs validate_plugin_id_version), which a
# version# glob alone cannot tell apart (`nf-amazon-2fa-1.0.0`).
layer_plugin_version() {
  local -a dirs=("$NXF_PLUGINS_DIR/$1"-[0-9]*)
  { [ "${#dirs[@]}" = 1 ] && [ -d "${dirs[0]}" ]; } \
    || runtime_error "the runtime layer must carry exactly one $1 plugin (found: ${dirs[*]})"
  local version=${dirs[0]##*/"$1"-}
  [[ "$version" =~ ^[0-9A-Za-z][0-9A-Za-z.+_-]*$ ]] \
    || runtime_error "the runtime layer's $1 plugin has an unusable version: $version"
  printf '%s' "$version"
}

# local_cache_plugins: for a run whose work dir is local (a single-job run, a dry run), a -c
# `plugins {}` keeping the workflow's own plugins but nf-cloudcache, when its layer lists that one.
# On Nextflow 25.10 and 26.04 a loaded nf-cloudcache takes the cache over whatever
# `cloudcache.enabled` says, then refuses the run for having no cloud path (26.10 serves only a
# run that enables it). A -c `plugins {}` replaces the pipeline's list, so it names every other
# workflow plugin, and nf-biffix so it is never empty; NXF_PLUGINS_DEFAULT still adds biffix's.
# Nothing for any other layer: its -c config's `plugins {}` already replaces the pipeline's list.
local_cache_plugins() {
  local list="$RUNTIME_ROOT/nf/workflow-plugins" plugins line id
  [ "$NF_PLUGIN_MODE" = defaults ] || return 0
  # Read once, whole, and refuse a list that cannot be read: it might name nf-cloudcache, and a
  # read cut short would write a `plugins {}` that drops a plugin the pipeline needs. Both the
  # check and the block below come from this one copy.
  plugins=$(cat -- "$list") \
    || runtime_error "cannot read the runtime layer's workflow plugin list ($list)"
  grep -q '^nf-cloudcache@' <<< "$plugins" || return 0
  # Each id is quoted into a variable before it is written: groovy_str's refusal exits only the
  # command substitution, so `echo "  id $(groovy_str …)"` would still write a bare `id` line and
  # carry on. The `|| exit` takes its status (78) to the run.
  echo "plugins {"
  while IFS= read -r line || [ -n "$line" ]; do
    [ -n "$line" ] && [[ "$line" != nf-cloudcache@* ]] || continue
    id=$(groovy_str "$line" "the workflow plugin $line") || exit $?
    echo "  id ${id}"
  done <<< "$plugins"
  id=$(groovy_str "nf-biffix@${NF_BIFFIX_VERSION}" "the nf-biffix version") || exit $?
  echo "  id ${id}"
  echo "}"
}

# Run Nextflow with the AWS Batch executor (nf-amazon's, as nf-biffix's `awsbatch`, which runs each
# task on the job definition biffix registered for its image; see "Project isolation" in
# entrypoint.sh) + the nf-biffix TraceObserver plugin, both from the runtime layer.
# nf-amazon stages each task's I/O to/from the S3 workDir with the host's AWS CLI, bind-mounted into
# the task container (aws.batch.cliPath below), so task images need no `aws` of their own. The
# plugin reports the same RunEvents to the same ingest route as the Snakemake logger;
# queue/role/region come from the same resolved BIFFIX_* env. A single-job run runs every task on
# this host instead (nextflow_single_job_config).
run_nextflow() {
  local ENTRY="$SRC/$BIFFIX_ENTRYPOINT"
  local NF_CONFIG="$WORK/nextflow.config"
  local PARAMS_FILE="$WORK/params.json"
  # Nextflow's analog of Snakemake's --retries: retry a failed task up to N times. Same default (3)
  # and env knob (BIFFIX_RETRIES) as the Snakemake path. Nextflow has no exact --keep-going.
  local RETRIES="${BIFFIX_RETRIES:-3}" SPOT_ATTEMPTS="${BIFFIX_SPOT_ATTEMPTS:-5}"
  require_count BIFFIX_RETRIES "$RETRIES"
  require_count BIFFIX_SPOT_ATTEMPTS "$SPOT_ATTEMPTS"
  # The plugins biffix itself needs, at the versions the runtime layer carries (the runtime build
  # installs exactly one of each, the versions its recipe pins), are enabled one of two ways,
  # chosen by the layer:
  #   - A layer built with the workflow's own plugins (registration pins them into the runtime
  #     recipe) lists them in nf/workflow-plugins. biffix's are then Nextflow's DEFAULT plugins
  #     (NXF_PLUGINS_DEFAULT), not a `plugins {}` block in the -c config below (nor `-plugins`):
  #     either REPLACES the pipeline's own plugins list, so e.g. nf-core's nf-schema would never
  #     load. NXF_PLUGINS_DEFAULT is added to the declared list instead, and replaces Nextflow's
  #     executor-based default (so nf-amazon is named too) (verified on Nextflow 25.10 and 26.x).
  #   - Any other layer (for a workflow that declares none) runs nf-biffix in a `plugins {}` block
  #     in the -c config, which replaces the pipeline's own list: a pipeline that declares plugins
  #     its layer lacks still runs, where the pipeline's list would fail it offline before any task.
  #     Registering it with `plugins:` builds them into its layer.
  local NF_PLUGIN_MODE=config NF_AMAZON_VERSION="" NF_BIFFIX_VERSION
  [ ! -f "$RUNTIME_ROOT/nf/workflow-plugins" ] || NF_PLUGIN_MODE=defaults
  NF_BIFFIX_VERSION=$(layer_plugin_version nf-biffix)
  [ "$NF_PLUGIN_MODE" = config ] || NF_AMAZON_VERSION=$(layer_plugin_version nf-amazon)
  # BIFFIX_PARAMS is a JSON object; Nextflow reads it directly as a -params-file.
  printf '%s' "$PARAMS_JSON" > "$PARAMS_FILE"
  # The retry policy is only a DEFAULT, so it goes in $NXF_HOME/config — the LOWEST-priority config
  # Nextflow reads — not the `-c` file below. A `-c` file outranks the pipeline's own config, so a
  # generic `process.errorStrategy` there silently replaced e.g. nf-core's conf/base.config retry
  # policy; from here the pipeline's config and process directives win, and this applies only where
  # the pipeline sets nothing (verified on Nextflow 25.10 and 26.x).
  local NF_DEFAULTS="${NXF_HOME:-$HOME/.nextflow}/config"
  mkdir -p "$(dirname "$NF_DEFAULTS")"
  # A single-job run's tasks get its size less the engine's reserve, whose JVM heap is capped
  # (single_job_memory).
  local task_memory=""
  if is_single_job; then
    task_memory=$SJ_TASK_MEMORY_MIB
    # nf-biffix bounds the effective local pool after workflow config merge, including $local
    # overrides. Pass the computed task budget, not the allocation before the JVM reserve.
    export BIFFIX_SINGLE_JOB_CPUS="$SJ_CPUS"
    export BIFFIX_SINGLE_JOB_TASK_MEMORY_MIB="$task_memory"
    export NXF_OPTS="${NXF_OPTS:+$NXF_OPTS }-Xmx${SJ_JVM_HEAP_MIB}m"
    # Nextflow runs `apptainer exec --pid` by default, which cannot mount a new /proc under the
    # paths Docker masks (and Batch cannot unmask them).
    export NXF_APPTAINER_NEW_PID_NAMESPACE=false
  fi
  {
    if [ "$RETRIES" -gt 0 ]; then
      echo "process.errorStrategy = 'retry'"
      echo "process.maxRetries = ${RETRIES}"
    fi
    # No trace.enabled: Nextflow's task metrics (which the trace turns on) run `ps`, `awk` and
    # more inside every task's container (.command.run), which a slim task image may lack.
    if [ -n "$task_memory" ]; then
      # What the local executor may run at once, and the most one task may ask for: the job's
      # size, as defaults, so a pipeline's own settings win. An older nf-core template caps its
      # requests with params.max_cpus/max_memory, which it declares in its own config, so a value
      # here could never reach it (and a newer pipeline's schema would reject the unknown param);
      # resourceLimits caps those requests all the same.
      echo "executor.cpus = ${SJ_CPUS}"
      echo "executor.memory = '${task_memory} MB'"
      echo "process.resourceLimits = [cpus: ${SJ_CPUS}, memory: '${task_memory} MB']"
      # nf-core modules name a different image (an image file URL on a Galaxy or Seqera depot) when
      # the engine is Apptainer, unless this is set: the image the version registered is the
      # Docker one, which the image map and the converted image files hold.
      echo "process.ext.singularity_pull_docker_container = true"
    fi
  } > "$NF_DEFAULTS"
  local plugin="" image=""
  [ "$NF_PLUGIN_MODE" = defaults ] || plugin=$(groovy_str "nf-biffix@${NF_BIFFIX_VERSION}" "the nf-biffix version")
  [ -z "${BIFFIX_WORKER_IMAGE:-}" ] || image=$(groovy_str "$BIFFIX_WORKER_IMAGE" "BIFFIX_WORKER_IMAGE")
  if is_single_job; then
    nextflow_single_job_config > "$NF_CONFIG"
  else
    nextflow_batch_config > "$NF_CONFIG"
  fi
  if [ "$NF_PLUGIN_MODE" = defaults ]; then
    # The runtime layer's nf-biffix reports to biffix; enabling it by default means the workflow's
    # own .nf and config need not know about biffix.
    export NXF_PLUGINS_DEFAULT="nf-amazon@${NF_AMAZON_VERSION},nf-biffix@${NF_BIFFIX_VERSION}"
    echo "biffix-coordinator: NXF_PLUGINS_DEFAULT=${NXF_PLUGINS_DEFAULT}" >&2
  fi
  echo "biffix-coordinator: nextflow.config:" >&2
  cat "$NF_CONFIG" >&2
  echo "biffix-coordinator: default config ($NF_DEFAULTS):" >&2
  cat "$NF_DEFAULTS" >&2
  # Name the Nextflow run after the biffix run: the resume cache's index is written per run name
  # (`<cache>/<session>/index.<name>`), so each biffix run's index is traceable. A resume needs it
  # anyway: with history off (below) Nextflow cannot generate a name ("Missing workflow run name").
  local -a SESSION_ARGS=(-name "$BIFFIX_RUN_ID")
  # Batch retries the coordinator job (a lost host, a failed image pull) under the same env, so a
  # later attempt resumes the session the first one started (NXF_UUID below) from the S3 cache, as
  # a Snakemake attempt restores the metadata its first one synced: a task that attempt finished is
  # not run, and paid for, again. A single-job run has no such cache (its work dir was on the lost
  # host): every attempt starts afresh, under the session biffix chose.
  local attempt="${AWS_BATCH_JOB_ATTEMPT:-1}"
  [[ "$attempt" =~ ^[0-9]+$ ]] || attempt=1
  if [ -n "${BIFFIX_ENGINE_SESSION_ID:-}" ]; then
    if ! is_single_job && { [ "${BIFFIX_RESUME:-}" = 1 ] || [ "$attempt" -gt 1 ]; }; then
      # Explicit id: a bare -resume reads the LOCAL .nextflow/history, which this container never
      # has, and Nextflow then silently runs from scratch (ConfigBuilder.normalizeResumeId).
      SESSION_ARGS+=(-resume "$BIFFIX_ENGINE_SESSION_ID")
      # Even an explicit id is checked against that local history ("Can't find a run with the
      # specified id", ScriptRunner) unless history tracking is off; the cache itself is in S3.
      export NXF_IGNORE_RESUME_HISTORY=true
    else
      # Pre-choose the fresh session's uuid so the cache dir is known to biffix before the engine
      # starts (Session reads NXF_UUID when no -resume is given).
      export NXF_UUID="$BIFFIX_ENGINE_SESSION_ID"
    fi
  fi
  echo "biffix-coordinator: nextflow session=${BIFFIX_ENGINE_SESSION_ID:-<engine-chosen>} resume=${BIFFIX_RESUME:-0} attempt=$attempt" >&2
  # nf-biffix's own package is outside `nextflow.*`, which is the only package Nextflow logs at DEBUG
  # by default; -debug keeps the plugin's diagnostics in the job's .nextflow.log.
  local -a ARGS=(-log "$WORK/.nextflow.log" -debug io.github.nh13.biffix run "$ENTRY"
    -c "$NF_CONFIG"
    -params-file "$PARAMS_FILE"
    -ansi-log false
    "${SESSION_ARGS[@]}")
  [ -z "${BIFFIX_DRY_RUN:-}" ] || ARGS+=(-preview)
  [ -z "${BIFFIX_TARGET:-}" ] || ARGS+=(-entry "$BIFFIX_TARGET")
  # A single-job run's tasks start their containers through biffix's apptainer wrapper
  # (lib/single_job.sh); the work dir is this host's, so its failed tasks' files are in the logs.
  if is_single_job; then
    export PATH="$SJ_BIN:$PATH"
    # shellcheck disable=SC2034 # read by lib/logs.sh
    LOCAL_WORK_DIR="$SCRATCH/work"
  fi
  start_logs_sync
  # From the source root, as `nextflow run <entrypoint>` from a checkout: Nextflow reads the
  # nextflow.config of the launch dir and the entrypoint's own dir only, so a pipeline whose config
  # sits at the repo root above its entrypoint (e.g. refprep/main.nf) would otherwise run without
  # it: its plugins {}, params and profiles all silently ignored. Every path above is absolute.
  run_engine TERM run_in "$SRC" nextflow "${ARGS[@]}"
}

# nextflow_single_job_config: the -c config of a single-job run (lib/single_job.sh): every task
# runs on this host, with the local executor, in its image through Apptainer, from the image files
# the coordinator converted before the run. Reads run_nextflow's image and plugin.
nextflow_single_job_config() {
  local region images cache workdir
  region=$(groovy_str "$BIFFIX_REGION" "the region")
  images=$(groovy_str "$SJ_IMAGES" "the scratch root")
  cache=$(groovy_str "$SCRATCH/apptainer/nextflow-cache" "the scratch root")
  workdir=$(groovy_str "$SCRATCH/work" "the scratch root")
  echo "process.executor = 'local'"
  # Selectors outrank process directives. nf-biffix also pins every existing selector to local
  # at session creation: merging this match-all alone does not move an existing `.*` key last.
  echo "process { withName: '.*' { executor = 'local' } }"
  [ -z "$image" ] || echo "process.container = ${image}"
  # S3 inputs and published outputs, which Nextflow itself reads and writes.
  echo "aws.region = ${region}"
  # Every task in its pinned image, through Apptainer, and no other engine: Nextflow looks for a
  # tenant image's file in libraryDir first (the files single_job.py converted, named as Nextflow
  # names them), and pulls one it lacks to cacheDir.
  echo "apptainer.enabled = true"
  # Never a pull at task launch, which a pipeline's own `ociAutoPull = true` would otherwise make:
  # it bypasses libraryDir and would pull a private image without the conversion's ECR login.
  echo "apptainer.ociAutoPull = false"
  echo "apptainer.libraryDir = ${images}"
  echo "apptainer.cacheDir = ${cache}"
  local engine
  for engine in docker podman singularity charliecloud shifter sarus conda spack wave fusion; do
    echo "${engine}.enabled = false"
  done
  # On the instance store: a task's inputs and outputs never leave the host until published.
  echo "workDir = ${workdir}"
  # No cloud cache (the work dir it would index dies with the host), and off rather than unset: a
  # pipeline's own `cloudcache.enabled = true` with no path would take this local work dir as its
  # path, which Nextflow refuses before any task starts. No aws.batch settings and no queue:
  # nothing is submitted to Batch.
  echo "cloudcache.enabled = false"
  local_cache_plugins
  [ -z "$plugin" ] || echo "plugins { id ${plugin} }"
}

# nextflow_batch_config: the -c config of a run whose tasks are AWS Batch jobs (or, for a dry run,
# previewed locally). Reads run_nextflow's image, plugin and SPOT_ATTEMPTS.
nextflow_batch_config() {
  # Every value from the environment is written as an escaped Groovy string (groovy_str): the
  # output prefix, for one, is caller-chosen, and a `'` in it would otherwise end the string.
  local executor="'awsbatch'" queue run_id project region role workdir cache logs_group
  queue=$(groovy_str "$SNAKEMAKE_AWS_BATCH_JOB_QUEUE" "the worker queue")
  run_id=$(groovy_str "$BIFFIX_RUN_ID" "BIFFIX_RUN_ID")
  project=$(groovy_str "$BIFFIX_PROJECT" "BIFFIX_PROJECT")
  region=$(groovy_str "$BIFFIX_REGION" "the region")
  # The job role goes into the job definitions nf-amazon registers, which it does only without
  # the run's job definition map (a local run): biffix's own definitions carry the role.
  role=""
  [ -n "${BIFFIX_JOB_DEFINITION_MAP_FILE:-}" ] || role=$(groovy_str "$SNAKEMAKE_AWS_BATCH_JOB_ROLE" "the job role")
  workdir=$(groovy_str "${STORE}work" "the output prefix")
  # A dry run writes nothing to S3: -preview still opens the work dir as the session starts (on S3,
  # a PutObject of its directory marker), and the AWS Batch executor requires an S3 one. So it
  # previews against a local work dir with the local executor, which submits nothing either way.
  if [ -n "${BIFFIX_DRY_RUN:-}" ]; then
    executor="'local'"
    workdir=$(groovy_str "$WORK/nf-work" "the work root")
  fi
  cache=$(groovy_str "${STORE}.nextflow/cache" "the output prefix")
  logs_group=$(groovy_str "$BIFFIX_TASK_LOG_GROUP" "BIFFIX_TASK_LOG_GROUP")
  {
    echo "process.executor = ${executor}"
    # Preview still constructs executors. nf-biffix pins existing selectors to local too, so
    # an earlier `.*` key cannot let a later pipeline selector re-enable a remote executor.
    [ -z "${BIFFIX_DRY_RUN:-}" ] || echo "process { withName: '.*' { executor = 'local' } }"
    echo "process.queue = ${queue}"
    # The tasks the version sends to other queues (lib/tasks.sh): withLabel/withName selectors,
    # which outrank process.queue and a process's own `queue` directive, and, read after the
    # pipeline's config, its `queue` under the same selector; a pipeline selector that is more
    # specific (withName over withLabel) still wins, as Nextflow ranks them.
    queue_selector_config <"$QUEUE_SELECTORS_FILE"
    [ -z "$image" ] || echo "process.container = ${image}"
    # Tag every task's Batch job with the run and project it belongs to (nf-amazon submits
    # resourceLabels as job tags): biffix accepts the plugin's task_submitted claim on a job only
    # when the run tag names the claiming run, and IAM lets the coordinator submit only jobs that
    # carry its project's tag. The Snakemake executor gets the same tags from
    # SNAKEMAKE_AWS_BATCH_JOB_TAGS.
    echo "process.resourceLabels = ['biffix:run': ${run_id}, 'biffix:project': ${project}]"
    echo "aws.region = ${region}"
    [ -z "$role" ] || echo "aws.batch.jobRole = ${role}"
    # Spot reclaims retry in Batch, on the same job id (see "Spot reclaims" in entrypoint.sh); 0
    # turns that off and leaves a reclaim to the errorStrategy.
    echo "aws.batch.maxSpotAttempts = ${SPOT_ATTEMPTS}"
    # Task jobs log to the project's own group, not the shared /aws/batch/job (see "Project
    # isolation" in entrypoint.sh): biffix's job definitions name it, and nf-amazon reads a failed
    # task's log from it (and, without a job definition map, writes it into the definitions it
    # registers).
    echo "aws.batch.logsGroup = ${logs_group}"
    # Every Batch host installs the pinned AWS CLI here at boot (the compute stack's launch template;
    # infra/biffix_infra/host/install_aws_cli.sh). Each task's job definition bind-mounts
    # /opt/biffix/aws-cli read-only (crates/biffix-backend/src/aws/task_definitions.rs), and
    # Nextflow stages the task's S3 I/O with it, so task images need no AWS CLI of their own. A path
    # the image also has would be shadowed by the mount, hence a biffix-only path.
    echo "aws.batch.cliPath = '/opt/biffix/aws-cli/bin/aws'"
    echo "workDir = ${workdir}"
    # The resume cache, next to the work dir it indexes. Written per task as the run goes (so a
    # cancelled or killed coordinator still leaves a resumable cache), read back by a later run
    # submitted with --resume-from. Session-keyed: the plugin stores under <path>/<session uuid>/.
    # A dry run writes nothing to S3: it keeps Nextflow's local cache (the cloud cache writes its
    # index on open), and does not upload the pipeline's bin/ to the work dir, which the AWS Batch
    # executor does as it is set up (Nextflow reads the switch from executor.disableRemoteBinDir).
    # Off for a dry run, not just unset: a pipeline's own cloud cache with no path would take its
    # local work dir as the path, which Nextflow refuses.
    if [ -z "${BIFFIX_DRY_RUN:-}" ]; then
      echo "cloudcache { enabled = true; path = ${cache} }"
    else
      echo "cloudcache.enabled = false"
      echo "executor.disableRemoteBinDir = true"
      local_cache_plugins
    fi
    # The runtime layer's plugin reports to biffix; enabling it in config means the workflow's own
    # .nf need not know about biffix (a layer without workflow plugins; see above).
    [ -z "$plugin" ] || echo "plugins { id ${plugin} }"
  }
}
