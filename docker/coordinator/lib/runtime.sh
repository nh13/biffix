# shellcheck shell=bash
# biffix coordinator: the engine runtime. Sourced by entrypoint.sh; defines functions only.
#   Reads: ENGINE, RUNTIME_ROOT, WORK, BIFFIX_ENGINE_VERSION, BIFFIX_RUNTIME_KEY.
#   Sets:  PATH (the runtime's bin first), RUNTIME_VERSION; for Nextflow NXF_HOME,
#          NXF_PLUGINS_DIR and NXF_OFFLINE.

# select_runtime: put the engine runtime on PATH and check it is the pinned release, before staging
# anything. The runtime layer is appended to the base image per pinned release
# (crates/biffix-workflow/src/runtime/pins.rs), so a mismatch means the backend launched the wrong
# variant: fail with EX_CONFIG (78) rather than run the workflow on an engine it was not registered
# with.
select_runtime() {
  case "$ENGINE" in
    snakemake)
      [ -x "$RUNTIME_ROOT/sm/bin/snakemake" ] \
        || runtime_error "this image has no Snakemake runtime ($RUNTIME_ROOT/sm); re-register the workflow"
      # The runtime's Python also runs this script's own S3 helpers (it has boto3 via the executor).
      export PATH="$RUNTIME_ROOT/sm/bin:$PATH"
      RUNTIME_VERSION=$(snakemake --version)
      ;;
    nextflow)
      [ -x "$RUNTIME_ROOT/nf/bin/nextflow" ] \
        || runtime_error "this image has no Nextflow runtime ($RUNTIME_ROOT/nf); re-register the workflow"
      export PATH="$RUNTIME_ROOT/nf/bin:$PATH"
      # The layer is read-only in spirit: plugins come from it, while NXF_HOME (which Nextflow
      # writes: secrets/, history, the defaults config) lives in the job's scratch. Offline, so a
      # run never resolves a plugin from the registry: everything it needs is in the layer.
      export NXF_HOME="$WORK/.nextflow-home" NXF_PLUGINS_DIR="$RUNTIME_ROOT/nf/plugins" NXF_OFFLINE=true
      # `nextflow -v` prints `nextflow version 25.10.4.11177`: keep MAJOR.MINOR.PATCH.
      RUNTIME_VERSION=$(nextflow -v | sed -n 's/^nextflow version \([0-9]*\.[0-9]*\.[0-9]*\).*$/\1/p')
      ;;
    *)
      runtime_error "unknown engine '$ENGINE'"
      ;;
  esac
  [ "$RUNTIME_VERSION" = "$BIFFIX_ENGINE_VERSION" ] \
    || runtime_error "runtime is $ENGINE ${RUNTIME_VERSION:-<unknown>}, but the version pins $BIFFIX_ENGINE_VERSION"
  echo "biffix-coordinator: runtime $ENGINE $RUNTIME_VERSION (key ${BIFFIX_RUNTIME_KEY:-<unset>})" >&2
}
