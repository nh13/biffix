# shellcheck shell=bash
# biffix coordinator: the Nextflow engine. Sourced by entrypoint.sh; defines functions only.
#   Reads: WORK, SRC, STORE, PARAMS_JSON, RUNTIME_ROOT, NXF_HOME, NXF_PLUGINS_DIR, the run's BIFFIX_*
#          env and the resolved SNAKEMAKE_AWS_BATCH_JOB_QUEUE / _ROLE.
#   Sets:  rc (Nextflow's exit status); NXF_PLUGINS_DEFAULT, and NXF_UUID or
#          NXF_IGNORE_RESUME_HISTORY.

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

# Run Nextflow with the AWS Batch executor (nf-amazon's, as nf-biffix's project-tagging `awsbatch`;
# see "Project isolation" in entrypoint.sh) + the nf-biffix TraceObserver plugin, both from the
# runtime layer.
# nf-amazon stages each task's I/O to/from the S3 workDir with the host's AWS CLI, bind-mounted into
# the task container (aws.batch.cliPath below), so task images need no `aws` of their own. The
# plugin reports the same RunEvents to the same ingest route as the Snakemake logger;
# queue/role/region come from the same resolved BIFFIX_* env.
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
  #     executor-based default (so nf-amazon is named too) (verified on Nextflow 25.10).
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
  # the pipeline sets nothing (verified on Nextflow 25.10).
  local NF_DEFAULTS="${NXF_HOME:-$HOME/.nextflow}/config"
  mkdir -p "$(dirname "$NF_DEFAULTS")"
  {
    if [ "$RETRIES" -gt 0 ]; then
      echo "process.errorStrategy = 'retry'"
      echo "process.maxRetries = ${RETRIES}"
    fi
  } > "$NF_DEFAULTS"
  # Every value from the environment is written as an escaped Groovy string (groovy_str): the
  # output prefix, for one, is caller-chosen, and a `'` in it would otherwise end the string.
  local queue image run_id project region role workdir cache logs_group plugin
  queue=$(groovy_str "$SNAKEMAKE_AWS_BATCH_JOB_QUEUE" "the worker queue")
  run_id=$(groovy_str "$BIFFIX_RUN_ID" "BIFFIX_RUN_ID")
  project=$(groovy_str "$BIFFIX_PROJECT" "BIFFIX_PROJECT")
  region=$(groovy_str "$BIFFIX_REGION" "the region")
  role=$(groovy_str "$SNAKEMAKE_AWS_BATCH_JOB_ROLE" "the job role")
  workdir=$(groovy_str "${STORE}work" "the output prefix")
  cache=$(groovy_str "${STORE}.nextflow/cache" "the output prefix")
  plugin=""
  [ "$NF_PLUGIN_MODE" = defaults ] || plugin=$(groovy_str "nf-biffix@${NF_BIFFIX_VERSION}" "the nf-biffix version")
  image=""
  [ -z "${BIFFIX_WORKER_IMAGE:-}" ] || image=$(groovy_str "$BIFFIX_WORKER_IMAGE" "BIFFIX_WORKER_IMAGE")
  logs_group=$(groovy_str "$BIFFIX_TASK_LOG_GROUP" "BIFFIX_TASK_LOG_GROUP")
  {
    echo "process.executor = 'awsbatch'"
    echo "process.queue = ${queue}"
    [ -z "$image" ] || echo "process.container = ${image}"
    # Tag every task's Batch job with the run and project it belongs to (nf-amazon submits
    # resourceLabels as job tags): biffix accepts the plugin's task_submitted claim on a job only
    # when the run tag names the claiming run, and IAM lets the coordinator submit only jobs that
    # carry its project's tag. The Snakemake executor gets the same tags from
    # SNAKEMAKE_AWS_BATCH_JOB_TAGS.
    echo "process.resourceLabels = ['biffix:run': ${run_id}, 'biffix:project': ${project}]"
    echo "aws.region = ${region}"
    echo "aws.batch.jobRole = ${role}"
    # Spot reclaims retry in Batch, on the same job id (see "Spot reclaims" in entrypoint.sh); 0
    # turns that off and leaves a reclaim to the errorStrategy.
    echo "aws.batch.maxSpotAttempts = ${SPOT_ATTEMPTS}"
    # Task jobs log to the project's own group, not the shared /aws/batch/job (see "Project
    # isolation" in entrypoint.sh). nf-amazon writes it into each job definition's awslogs-group.
    echo "aws.batch.logsGroup = ${logs_group}"
    # Every Batch host installs the pinned AWS CLI here at boot (the compute stack's launch template;
    # infra/biffix_infra/host/install_aws_cli.sh). Nextflow bind-mounts /opt/biffix/aws-cli read-only
    # into each task and stages the task's S3 I/O with it, so task images need no AWS CLI of their
    # own. A path the image also has would be shadowed by the mount, hence a biffix-only path.
    echo "aws.batch.cliPath = '/opt/biffix/aws-cli/bin/aws'"
    echo "workDir = ${workdir}"
    # The resume cache, next to the work dir it indexes. Written per task as the run goes (so a
    # cancelled or killed coordinator still leaves a resumable cache), read back by a later run
    # submitted with --resume-from. Session-keyed: the plugin stores under <path>/<session uuid>/.
    # A dry run writes nothing to S3: it keeps Nextflow's local cache (the cloud cache writes its
    # index on open), and does not upload the pipeline's bin/ to the work dir, which the AWS Batch
    # executor does as it is set up (Nextflow reads the switch from executor.disableRemoteBinDir).
    if [ -z "${BIFFIX_DRY_RUN:-}" ]; then
      echo "cloudcache { enabled = true; path = ${cache} }"
    else
      echo "executor.disableRemoteBinDir = true"
    fi
    # The runtime layer's plugin reports to biffix; enabling it in config means the workflow's own
    # .nf need not know about biffix (a layer without workflow plugins; see above).
    [ -z "$plugin" ] || echo "plugins { id ${plugin} }"
  } > "$NF_CONFIG"
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
  # not run, and paid for, again.
  local attempt="${AWS_BATCH_JOB_ATTEMPT:-1}"
  [[ "$attempt" =~ ^[0-9]+$ ]] || attempt=1
  if [ -n "${BIFFIX_ENGINE_SESSION_ID:-}" ]; then
    if [ "${BIFFIX_RESUME:-}" = 1 ] || [ "$attempt" -gt 1 ]; then
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
  # From the source root, as `nextflow run <entrypoint>` from a checkout: Nextflow reads the
  # nextflow.config of the launch dir and the entrypoint's own dir only, so a pipeline whose config
  # sits at the repo root above its entrypoint (e.g. refprep/main.nf) would otherwise run without
  # it: its plugins {}, params and profiles all silently ignored. Every path above is absolute.
  run_engine TERM run_in "$SRC" nextflow "${ARGS[@]}"
}
