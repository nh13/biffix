#!/usr/bin/env bash
# Build one engine runtime layer, and one coordinator variant: the base coordinator image + the layer.
#
# Runs in the `biffix-<tenant>-runtime-build` CodeBuild project (x86_64, unprivileged, no source:
# infra/biffix_infra/stacks/build_stack.py inlines this file into the buildspec). The backend starts
# one build per (runtime, base image) and names it by BIFFIX_BUILD_SUBJECT; the build's state and the
# outputs below come back through EventBridge into the build projection.
#
# Inputs (environment):
#   BIFFIX_BUILD_SUBJECT          coordinator:<runtime_key>@<base digest> (the build's join key)
#   BIFFIX_BASE_IMAGE             <registry>/<repo>@sha256:… — the base coordinator image index
#   BIFFIX_COORDINATOR_REPO       <registry>/<repo> the layer and variant are pushed to (project env)
#   BIFFIX_RUNTIME_ARTIFACT_URI   s3://<bucket>/runtimes/ — where resolved locks are kept (project env)
#   BIFFIX_ARCHIVE_URI            s3://<archive bucket>/ — where the layer images are archived (project env)
#   BIFFIX_RT_*                   the runtime recipe (crates/biffix-workflow/src/runtime/mod.rs build_env)
#   BIFFIX_OUTPUTS                file the KEY=VALUE outputs are written to (the buildspec exports them)
#
# The layer is stored once per runtime as a layer-only image per architecture,
# <repo>:runtime-<key12>-<arch> (keep-tagged and archived), and everything else is appended from it:
# this build's variant, a later base image's variant (no reinstall), and the derived task images an
# image import builds (codebuild/image/import.sh derive).
#
# Outputs: VARIANT_IMAGE (<repo>@<index digest>), VARIANT_DIGEST, ARCHS, RUNTIME_IMAGE_AMD64/ARM64
# (<repo>@<layer image digest>), RUNTIME_LAYER_AMD64/ARM64 (layer blob digests), RUNTIME_ARCHIVED;
# Snakemake: LOCK_SHA256_X86_64/AARCH64; Nextflow: NF_JAR_SHA256, NF_AMAZON_VERSION,
# NF_CLOUDCACHE_VERSION (the last two groups only from the build that installed the layer).
#
# Everything the layer installs is pinned by the recipe or checked against the base image:
#   - Snakemake: a python-build-standalone CPython (sha256), and a hash-locked package set resolved
#     by uv (sha256) as of the recipe's cutoff date, for each architecture. The biffix-owned wheels
#     come from the base image's /opt/biffix/wheels (their manifest must match the recipe), so no
#     package is built from git here.
#   - Nextflow: the release's self-contained `-dist` launcher and the nf-amazon / nf-cloudcache
#     releases it defaults to, each checked against the recipe's sha256, nf-biffix from the base
#     image, and the plugins the workflow itself declares (BIFFIX_RT_NF_WORKFLOW_PLUGINS:
#     space-separated `id@version@sha256`, pinned at registration; absent when it declares none),
#     each checked against its sha256: the coordinator runs Nextflow offline. A layer with workflow
#     plugins lists them in /opt/biffix/nf/workflow-plugins, which tells the coordinator how to
#     enable biffix's own plugins (see run_nextflow in docker/coordinator/lib/nextflow.sh).
# The x86_64 build host installs the arm64 packages without running them, and smoke-tests the amd64
# layer, which is native here.
#
# Test hook: BIFFIX_CRANE_INSECURE=1 lets crane talk to a plain-http registry.
set -euo pipefail
export BIFFIX_LOG_NAME="biffix-runtime-build"
# shellcheck source-path=SCRIPTDIR source=../lib/common.sh
[ -n "${BIFFIX_COMMON_LOADED:-}" ] || . "$(dirname "${BASH_SOURCE[0]}")/../lib/common.sh"

: "${BIFFIX_BUILD_SUBJECT:?}" "${BIFFIX_BASE_IMAGE:?}" "${BIFFIX_COORDINATOR_REPO:?}"
: "${BIFFIX_RUNTIME_ARTIFACT_URI:?}" "${BIFFIX_ARCHIVE_URI:?}" "${BIFFIX_OUTPUTS:?}"
: "${BIFFIX_RT_KEY:?}" "${BIFFIX_RT_ENGINE:?}" "${BIFFIX_RT_ENGINE_VERSION:?}"

WORK=$(mktemp -d)
: >"$BIFFIX_OUTPUTS"
KEY12=${BIFFIX_RT_KEY:0:12}
BASE_DIGEST=${BIFFIX_BASE_IMAGE#*@}
BASE12=${BASE_DIGEST#sha256:}
BASE12=${BASE12:0:12}
REPO=$BIFFIX_COORDINATOR_REPO
VARIANT_TAG="coordinator-${KEY12}-${BASE12}"
ARCHS=(amd64 arm64)
declare -A LAYER_IMAGE
log "subject=$BIFFIX_BUILD_SUBJECT runtime=$BIFFIX_RT_ENGINE $BIFFIX_RT_ENGINE_VERSION variant=$REPO:$VARIANT_TAG"

# ── tools ──
install_crane "$WORK"
ecr_login "$REPO"

# The layer tag; its prefix is biffix-workflow runtime::RUNTIME_LAYER_TAG_PREFIX (a test checks it).
layer_tag() { echo "runtime-${KEY12}-$1"; }
layers_pushed() {
  local arch
  for arch in "${ARCHS[@]}"; do
    crane digest "$REPO:$(layer_tag "$arch")" >/dev/null 2>&1 || return 1
  done
}

# ── the biffix-owned plugins, from the base image ──
WHEELS="$WORK/base/opt/biffix/wheels"
verify_base_wheels() {
  mkdir -p "$WORK/base"
  # The wheels, and the coordinator's Snakemake helper (smoke-tested against the runtime below).
  crane export --platform linux/amd64 "$BIFFIX_BASE_IMAGE" - |
    tar -x -C "$WORK/base" opt/biffix/wheels usr/local/lib/biffix
  python3 - "$WHEELS" <<'PY'
import hashlib, json, os, pathlib, sys

wheels = pathlib.Path(sys.argv[1])
manifest = json.loads((wheels / "manifest.json").read_text())
for name, sha in manifest["files"].items():
    got = hashlib.sha256((wheels / name).read_bytes()).hexdigest()
    if got != sha:
        sys.exit(f"base image {name}: sha256 {got}, manifest says {sha}")
pinned = {
    "snakemake": {"executor_sha": "BIFFIX_RT_EXECUTOR_SHA", "logger_version": "BIFFIX_RT_LOGGER_VERSION"},
    "nextflow": {"nf_biffix_version": "BIFFIX_RT_NF_BIFFIX_VERSION"},
}[os.environ["BIFFIX_RT_ENGINE"]]
for field, var in pinned.items():
    if manifest[field] != os.environ[var]:
        sys.exit(f"base image ships {field}={manifest[field]}, but the recipe pins {os.environ[var]}")
PY
}

# Every shared object in <dir> must be built for <arch> (x86_64|aarch64): an sdist compiled on this
# host would otherwise slip an x86_64 extension into the arm64 layer.
check_elf_arch() {
  python3 - "$1" "$2" <<'PY'
import pathlib, struct, sys

want = {"x86_64": 0x3E, "aarch64": 0xB7}[sys.argv[2]]
bad = []
for path in pathlib.Path(sys.argv[1]).rglob("*.so*"):
    if not path.is_file() or path.is_symlink():
        continue
    head = path.read_bytes()[:20]
    if head[:4] == b"\x7fELF" and struct.unpack("<H", head[18:20])[0] != want:
        bad.append(str(path))
if bad:
    sys.exit(f"shared objects not built for {sys.argv[2]}: {bad[:5]}")
PY
}

# The `python`/`python3` a derived task image puts first on PATH (biffix-workflow
# images::SHIM_DIR): the executor's `python -m snakemake …` runs the pinned runtime; anything
# else — the rule's shell, a tool's `#!/usr/bin/env python` — runs the image's own python, with
# the shim dropped from PATH.
# A derived image's entrypoint ends with /opt/biffix/shim-exec (images::SHIM_EXEC), which puts the
# shim first on PATH only after the image's own entrypoint has set up its environment (BioContainers'
# env-execute, for one, resets PATH).
# One thing does NOT reach the image's python: a rule's `script: foo.py`. Snakemake runs a Python
# script with its own interpreter (`sys.executable`, snakemake/script/__init__.py PythonScript
# .execute_script) unless the job has a software env, and a worker job has none (the image IS the
# job's environment), so the script runs on the runtime's python3 and sees only the runtime's
# packages, not the image's. That is also what the script's preamble needs (it unpickles Snakemake
# objects, so the interpreter must have Snakemake). A rule that needs the image's Python packages
# uses `shell: "python foo.py"` (or `#!/usr/bin/env python`), which the shim hands to the image.
write_python_shim() {
  cat >"$1/opt/biffix/shim-exec" <<'SHIM'
#!/bin/sh
# biffix: run the task's command with the python shim first on PATH.
PATH=/opt/biffix/shim:$PATH
export PATH
exec "$@"
SHIM
  chmod 755 "$1/opt/biffix/shim-exec"
  mkdir -p "$1/opt/biffix/shim"
  cat >"$1/opt/biffix/shim/python" <<'SHIM'
#!/bin/sh
# biffix: `python -m snakemake …` runs the pinned runtime; anything else, the image's own python.
if [ "$1" = -m ] && [ "$2" = snakemake ]; then exec /opt/biffix/sm/bin/python3 "$@"; fi
p=
set -f
IFS=:
for d in $PATH; do [ "$d" = /opt/biffix/shim ] || p=${p:+$p:}$d; done
unset IFS
PATH=$p
export PATH
exec "$(basename "$0")" "$@"
SHIM
  chmod 755 "$1/opt/biffix/shim/python"
  ln -s python "$1/opt/biffix/shim/python3"
}

# ── Snakemake ──
build_snakemake_layers() {
  : "${BIFFIX_RT_PYTHON_RELEASE:?}" "${BIFFIX_RT_PYTHON_VERSION:?}" "${BIFFIX_RT_UV_VERSION:?}"
  : "${BIFFIX_RT_STORAGE_S3:?}" "${BIFFIX_RT_EXCLUDE_NEWER:?}"
  fetch "https://github.com/astral-sh/uv/releases/download/${BIFFIX_RT_UV_VERSION}/uv-x86_64-unknown-linux-gnu.tar.gz" \
    "$WORK/uv.tgz" "$BIFFIX_RT_UV_SHA256_X86_64"
  tar -xzf "$WORK/uv.tgz" -C "$WORK" --strip-components=1 uv-x86_64-unknown-linux-gnu/uv
  local uv="$WORK/uv" pyver=${BIFFIX_RT_PYTHON_VERSION%.*}
  local executor logger
  executor=$(find "$WHEELS" -name 'snakemake_executor_plugin_aws_batch-*.whl' -print -quit)
  logger=$(find "$WHEELS" -name "snakemake_logger_plugin_biffix-${BIFFIX_RT_LOGGER_VERSION}-*.whl" -print -quit)
  [ -n "$executor" ] && [ -n "$logger" ] || die "base image lacks the executor or logger wheel"
  # The biffix-owned plugins by file, so the resolver can never substitute a PyPI release of the
  # same name; Snakemake is exact and exempt from the cutoff (a newer 9.x must still resolve).
  cat >"$WORK/requirements.in" <<EOF
snakemake==${BIFFIX_RT_ENGINE_VERSION}
snakemake-storage-plugin-s3==${BIFFIX_RT_STORAGE_S3}
snakemake-executor-plugin-aws-batch @ file://${executor}
snakemake-logger-plugin-biffix @ file://${logger}
EOF
  local arch pyarch sha stage sm lock
  for arch in "${ARCHS[@]}"; do
    pyarch=$([ "$arch" = amd64 ] && echo x86_64 || echo aarch64)
    sha=BIFFIX_RT_PYTHON_SHA256_${pyarch^^}
    stage="$WORK/layer-$arch"
    sm="$stage/opt/biffix/sm"
    mkdir -p "$sm"
    fetch "https://github.com/astral-sh/python-build-standalone/releases/download/${BIFFIX_RT_PYTHON_RELEASE}/cpython-${BIFFIX_RT_PYTHON_VERSION}%2B${BIFFIX_RT_PYTHON_RELEASE}-${pyarch}-unknown-linux-gnu-install_only.tar.gz" \
      "$WORK/python-$pyarch.tgz" "${!sha:?}"
    tar -xzf "$WORK/python-$pyarch.tgz" -C "$sm" --strip-components=1

    lock="$WORK/requirements-$pyarch.lock"
    "$uv" pip compile "$WORK/requirements.in" --quiet --generate-hashes --no-header \
      --python-version "$pyver" --python-platform "$pyarch-manylinux_2_28" \
      --exclude-newer "$BIFFIX_RT_EXCLUDE_NEWER" \
      --exclude-newer-package "snakemake=2100-01-01T00:00:00Z" -o "$lock"
    "$uv" pip install --quiet --require-hashes --no-deps -r "$lock" \
      --python-version "$pyver" --python-platform "$pyarch-manylinux_2_28" \
      --target "$sm/lib/python$pyver/site-packages"
    check_elf_arch "$sm" "$pyarch"
    # --target leaves console scripts whose shebangs name this host's interpreter; the runtime is
    # invoked as `python -m snakemake`, plus one relocatable `snakemake` for the coordinator.
    rm -rf "$sm/lib/python$pyver/site-packages/bin"
    printf '#!/bin/sh\nexec /opt/biffix/sm/bin/python3 -m snakemake "$@"\n' >"$sm/bin/snakemake"
    chmod 755 "$sm/bin/snakemake"
    write_python_shim "$stage"

    aws s3 cp --quiet "$lock" "${BIFFIX_RUNTIME_ARTIFACT_URI}${BIFFIX_RT_KEY}/requirements-$pyarch.lock"
    emit "LOCK_SHA256_${pyarch^^}" "$(sha256sum "$lock" | cut -d' ' -f1)"
  done

  local got
  got=$("$WORK/layer-amd64/opt/biffix/sm/bin/python3" -m snakemake --version)
  [ "$got" = "$BIFFIX_RT_ENGINE_VERSION" ] || die "smoke: snakemake --version is $got"
  "$WORK/layer-amd64/opt/biffix/sm/bin/python3" -c \
    'import snakemake_executor_plugin_aws_batch, snakemake_storage_plugin_s3, snakemake_logger_plugin_biffix, boto3'
  log "smoke: snakemake $got and its plugins import (amd64)"
  # The coordinator's cache-rule helper reads a workflow through Snakemake's internals, which move
  # between releases: it must name exactly the `cache: True` rule of this workflow, on this one.
  mkdir -p "$WORK/smoke-sm"
  printf '%s\n' 'rule a:' '    output: "a"' '    cache: True' '    shell: "touch {output}"' '' \
    'rule b:' '    output: "b"' '    shell: "touch {output}"' >"$WORK/smoke-sm/Snakefile"
  echo '{}' >"$WORK/smoke-sm/config.json"
  got=$(cd "$WORK/smoke-sm" && "$WORK/layer-amd64/opt/biffix/sm/bin/python3" \
    "$WORK/base/usr/local/lib/biffix/snakemake_cache_rules.py" Snakefile config.json)
  [ "$got" = "BIFFIX_CACHE_RULE=a" ] || die "smoke: the cache-rule helper printed '$got', not rule a"
  log "smoke: the cache-rule helper reads the workflow's cache: rules"
  # Anything but `-m snakemake` falls through to the next python on PATH, shim dropped.
  got=$(PATH="/usr/bin:/opt/biffix/shim:$PATH" "$WORK/layer-amd64/opt/biffix/shim/python3" -c 'import os; print(os.environ["PATH"])')
  [[ ":$got:" != *:/opt/biffix/shim:* ]] || die "smoke: the python3 shim kept itself on PATH ($got)"
  log "smoke: the python shim falls through to the next python3"
}

# ── Nextflow ──

# Smoke: in the layer, `awsbatch` is nf-biffix's executor (which tags every job definition it
# registers with the run's project; the coordinator IAM refuses an untagged one), not stock
# nf-amazon's. That proves offline that nf-biffix's Plugin-Dependencies resolve to the layer's
# nf-amazon, that nf-amazon's classes load through them, and that nf-biffix's executor wins the
# name. A local workDir makes the AWS Batch executor abort in register(), before any AWS call, so
# the run fails by design: the verdict is the log. ExecutorFactory logs the name going to nf-biffix
# (`has priority` or `Replacing executor`, depending on the plugins' discovery order), and the
# abort's stack names nf-biffix's executor. Args: the layer's nf dir, a scratch dir, the ids of
# nf-amazon and nf-biffix, and the workflow's own `id@version`s, if any.
#
# It runs in the mode the coordinator will use this layer in (see run_nextflow in
# docker/coordinator/lib/nextflow.sh): with workflow plugins, biffix's two are NXF_PLUGINS_DEFAULT and
# the workflow's are declared in the pipeline's own nextflow.config, so the priority is checked with
# the merged list; without, both are a `plugins {}` block in the -c config.
check_nf_awsbatch_executor() {
  local nf=$1 dir=$2 amazon=$3 biffix=$4 nflog p
  shift 4
  mkdir -p "$dir"
  nflog="$dir/.nextflow.log"
  printf 'process A {\n  container "ubuntu:24.04"\n  script: "true"\n}\nworkflow { A() }\n' >"$dir/main.nf"
  local -a vars=(NXF_HOME="$dir/nxf-home" NXF_PLUGINS_DIR="$nf/plugins" NXF_OFFLINE=true BIFFIX_PROJECT=smoke)
  if [ "$#" -gt 0 ]; then
    printf "process.executor = 'awsbatch'\n" >"$dir/awsbatch.config"
    {
      echo "plugins {"
      for p in "$@"; do echo "  id '$p'"; done
      echo "}"
    } >"$dir/nextflow.config"
    vars+=(NXF_PLUGINS_DEFAULT="$amazon,$biffix")
  else
    printf "process.executor = 'awsbatch'\nplugins { id '%s'; id '%s' }\n" "$amazon" "$biffix" >"$dir/awsbatch.config"
  fi
  (cd "$dir" && env "${vars[@]}" "$nf/bin/nextflow" -log "$nflog" run main.nf -c awsbatch.config) \
    >"$dir/run.out" 2>&1 || true
  if ! grep -qE 'Executor BiffixAwsBatchExecutor has priority|Replacing executor AwsBatchExecutor with class io\.github\.nh13\.biffix\.BiffixAwsBatchExecutor' "$nflog" ||
    ! grep -qF 'at io.github.nh13.biffix.BiffixAwsBatchExecutor.register(' "$nflog"; then
    tail -n 40 "$dir/run.out" "$nflog" >&2 2>/dev/null || true
    die "smoke: awsbatch is not $biffix's executor in the layer (output above)"
  fi
  local with=""
  [ "$#" = 0 ] || with=", with the workflow's plugins ($*)"
  log "smoke: awsbatch is $biffix's executor (over $amazon)$with"
}

# unzip_to <zip> <dir>: unpack <zip> into <dir>.
unzip_to() {
  python3 -c 'import sys, zipfile; zipfile.ZipFile(sys.argv[1]).extractall(sys.argv[2])' "$1" "$2"
}

# install_nf_plugin <id> <version> <sha256> <plugins dir>: a plugin's registry zip, checked against
# its sha256 and unpacked as `nextflow plugin install` lays it out (<plugins dir>/<id>-<version>).
# The URL is biffix-workflow runtime::plugin_zip_url's (a test checks the spelling).
install_nf_plugin() {
  local id=$1 pv=$2 sha=$3 dir=$4
  fetch "https://registry.nextflow.io/api/v1/plugins/$id/$pv/download/$id-$pv.zip" "$WORK/$id-$pv.zip" "$sha"
  unzip_to "$WORK/$id-$pv.zip" "$dir/$id-$pv"
}

# install_nf_workflow_plugins <nf dir>: the workflow's own plugins (its nextflow.config `plugins {}`,
# e.g. nf-core's nf-schema; BIFFIX_RT_NF_WORKFLOW_PLUGINS, `id@version@sha256` each, pinned at
# registration) into <nf dir>/plugins, and their `id@version`s, one per line, into the marker
# <nf dir>/workflow-plugins. The marker is written only when there are some: the coordinator enables
# biffix's own plugins through NXF_PLUGINS_DEFAULT only in a layer that has it, and through a
# `plugins {}` block in its -c config otherwise. The recipe's validation keeps `@` and whitespace out of every field.
install_nf_workflow_plugins() {
  local nf=$1 entry id pv sha
  local -a entries
  read -r -a entries <<<"${BIFFIX_RT_NF_WORKFLOW_PLUGINS:-}"
  for entry in "${entries[@]}"; do
    IFS=@ read -r id pv sha <<<"$entry"
    [ -n "$id" ] && [ -n "$pv" ] && [ -n "$sha" ] || die "bad BIFFIX_RT_NF_WORKFLOW_PLUGINS entry '$entry'"
    install_nf_plugin "$id" "$pv" "$sha" "$nf/plugins"
    echo "$id@$pv" >>"$nf/workflow-plugins"
  done
}

# Smoke: the workflow's own plugins load offline from the layer, enabled as the coordinator enables
# them (the pipeline's `plugins {}`, with biffix's as NXF_PLUGINS_DEFAULT). A plugin that needs a
# newer Nextflow, or another plugin the layer lacks, fails here rather than in every run. Args: the
# layer's nf dir, a scratch dir, biffix's NXF_PLUGINS_DEFAULT, and the workflow's `id@version`s.
check_nf_workflow_plugins() {
  local nf=$1 dir=$2 defaults=$3 p
  shift 3
  mkdir -p "$dir"
  printf 'workflow { Channel.of("ok") | view }\n' >"$dir/main.nf"
  {
    echo "plugins {"
    for p in "$@"; do echo "  id '$p'"; done
    echo "}"
  } >"$dir/nextflow.config"
  if ! (cd "$dir" && NXF_HOME="$dir/nxf-home" NXF_PLUGINS_DIR="$nf/plugins" NXF_OFFLINE=true \
    NXF_PLUGINS_DEFAULT="$defaults" "$nf/bin/nextflow" -log "$dir/.nextflow.log" run main.nf) \
    >"$dir/run.out" 2>&1; then
    tail -n 40 "$dir/run.out" "$dir/.nextflow.log" >&2 2>/dev/null || true
    die "smoke: nextflow $BIFFIX_RT_ENGINE_VERSION cannot load the workflow's plugins ($*) offline from the layer: a plugin may need a newer Nextflow or a plugin the layer lacks (output above)"
  fi
  log "smoke: the workflow's plugins load offline ($*)"
}

build_nextflow_layers() {
  : "${BIFFIX_RT_NF_BIFFIX_VERSION:?}" "${BIFFIX_RT_NF_SHA256:?}"
  : "${BIFFIX_RT_NF_AMAZON_VERSION:?}" "${BIFFIX_RT_NF_AMAZON_SHA256:?}"
  : "${BIFFIX_RT_NF_CLOUDCACHE_VERSION:?}" "${BIFFIX_RT_NF_CLOUDCACHE_SHA256:?}"
  local v=$BIFFIX_RT_ENGINE_VERSION stage="$WORK/layer-amd64"
  local nf="$stage/opt/biffix/nf"
  mkdir -p "$nf/bin" "$nf/plugins"
  fetch "https://github.com/nextflow-io/nextflow/releases/download/v${v}/nextflow-${v}-dist" \
    "$nf/bin/nextflow" "$BIFFIX_RT_NF_SHA256"
  chmod 755 "$nf/bin/nextflow"
  emit NF_JAR_SHA256 "$BIFFIX_RT_NF_SHA256"

  # nf-amazon and nf-cloudcache: the versions this release defaults to (what `nextflow run` would
  # pick online), which the recipe pins with each zip's sha256; fetched from the plugin registry.
  local defaults plugin id pv sha
  defaults=$(python3 -c 'import sys, zipfile; print(zipfile.ZipFile(sys.argv[1]).read("META-INF/plugins-info.txt").decode())' "$nf/bin/nextflow")
  for plugin in "nf-amazon@$BIFFIX_RT_NF_AMAZON_VERSION@$BIFFIX_RT_NF_AMAZON_SHA256" \
    "nf-cloudcache@$BIFFIX_RT_NF_CLOUDCACHE_VERSION@$BIFFIX_RT_NF_CLOUDCACHE_SHA256"; do
    IFS=@ read -r id pv sha <<<"$plugin"
    grep -qxF "$id@$pv" <<<"$defaults" || die "nextflow $v does not default to $id@$pv (the recipe is wrong)"
    install_nf_plugin "$id" "$pv" "$sha" "$nf/plugins"
  done
  local amazon="nf-amazon@$BIFFIX_RT_NF_AMAZON_VERSION" cloudcache="nf-cloudcache@$BIFFIX_RT_NF_CLOUDCACHE_VERSION"
  emit NF_AMAZON_VERSION "$BIFFIX_RT_NF_AMAZON_VERSION"
  emit NF_CLOUDCACHE_VERSION "$BIFFIX_RT_NF_CLOUDCACHE_VERSION"

  local biffix="nf-biffix-${BIFFIX_RT_NF_BIFFIX_VERSION}"
  [ -f "$WHEELS/$biffix.zip" ] || die "base image lacks $biffix.zip"
  unzip_to "$WHEELS/$biffix.zip" "$nf/plugins/$biffix"

  install_nf_workflow_plugins "$nf"
  local -a workflow_plugins=()
  [ ! -f "$nf/workflow-plugins" ] || mapfile -t workflow_plugins <"$nf/workflow-plugins"

  # Smoke: the release, and its plugins loading offline from the layer. nf-cloudcache only accepts
  # an object-store path, so it is checked as installed rather than run.
  local got
  got=$(NXF_HOME="$WORK/nxf-smoke" "$nf/bin/nextflow" -v | sed -n 's/^nextflow version \([0-9]*\.[0-9]*\.[0-9]*\).*$/\1/p')
  [ "$got" = "$v" ] || die "smoke: nextflow -v is $got"
  mkdir -p "$WORK/smoke"
  printf 'workflow { Channel.of("ok") | view }\n' >"$WORK/smoke/main.nf"
  [ -d "$nf/plugins/${cloudcache/@/-}" ] || die "smoke: $cloudcache is not installed"
  printf "plugins { id '%s'; id '%s' }\n" "$amazon" "nf-biffix@${BIFFIX_RT_NF_BIFFIX_VERSION}" >"$WORK/smoke/plugins.config"
  (cd "$WORK/smoke" && NXF_HOME="$WORK/nxf-smoke" NXF_PLUGINS_DIR="$nf/plugins" NXF_OFFLINE=true \
    "$nf/bin/nextflow" -log "$WORK/smoke/.nextflow.log" run main.nf -c plugins.config) ||
    die "smoke: nextflow could not run offline with the layer's plugins"
  check_nf_awsbatch_executor "$nf" "$WORK/smoke-awsbatch" "$amazon" "nf-biffix@${BIFFIX_RT_NF_BIFFIX_VERSION}" "${workflow_plugins[@]}"
  [ "${#workflow_plugins[@]}" = 0 ] ||
    check_nf_workflow_plugins "$nf" "$WORK/smoke-workflow-plugins" "$amazon,nf-biffix@${BIFFIX_RT_NF_BIFFIX_VERSION}" "${workflow_plugins[@]}"
  log "smoke: nextflow $got runs offline with $amazon $cloudcache nf-biffix@${BIFFIX_RT_NF_BIFFIX_VERSION}"
  # The layer is architecture-independent (a JVM launcher and plugin jars).
  cp -a "$stage" "$WORK/layer-arm64"
}

# ── the layer images: installed once per runtime ──
if layers_pushed; then
  log "runtime layer images already pushed; reusing them"
else
  verify_base_wheels
  case "$BIFFIX_RT_ENGINE" in
    snakemake) build_snakemake_layers ;;
    nextflow) build_nextflow_layers ;;
    *) die "unknown engine $BIFFIX_RT_ENGINE" ;;
  esac
  for arch in "${ARCHS[@]}"; do
    tag="$REPO:$(layer_tag "$arch")"
    crane digest "$tag" >/dev/null 2>&1 && continue
    # A reproducible tar: the same files give the same layer.
    tar --sort=name --owner=0 --group=0 --numeric-owner --mtime=@0 \
      -C "$WORK/layer-$arch" -cf "$WORK/layer-$arch.tar" opt
    crane append -f "$WORK/layer-$arch.tar" -t "$tag" >/dev/null
    log "pushed $tag"
  done
fi
for arch in "${ARCHS[@]}"; do
  image=$(crane digest "$REPO:$(layer_tag "$arch")")
  keep_tag "$REPO" "$image" /dev/null >/dev/null
  bytes=$(archive "$REPO@$image" "$BIFFIX_ARCHIVE_URI" "$WORK")
  log "layer image $arch $image (archived $bytes new bytes)"
  emit "RUNTIME_IMAGE_${arch^^}" "$REPO@$image"
  LAYER_IMAGE[$arch]=$image
done
emit RUNTIME_ARCHIVED 1

# ── the variant: base + layer, per architecture, then one index ──
# variant_image_matches <image@digest> <arch>: whether the image is the base's <arch> image with
# the runtime layer on top, layer for layer.
variant_image_matches() {
  local base_arch
  base_arch=$(crane digest --platform "linux/$2" "$BIFFIX_BASE_IMAGE") || return 1
  [ "$(layers "$1")" = "$(layers "${BIFFIX_BASE_IMAGE%@*}@$base_arch") $(last_layer "$REPO@${LAYER_IMAGE[$2]}")" ]
}

# variant_matches <digest>: whether the index <digest> joins exactly one such image per
# architecture. A tag in the repository proves nothing by itself (whoever may push to it could have
# put it there, and its name is predictable), so a variant found under its tag is reused only when
# it checks out, as codebuild/image/import.sh does for a derive.
variant_matches() {
  local child plat arch seen=()
  while read -r child plat; do
    arch=${plat#*/}
    arch=${arch%%/*}
    [ -n "$arch" ] && [ -n "${LAYER_IMAGE[$arch]:-}" ] || return 1
    variant_image_matches "$REPO@$child" "$arch" || return 1
    seen+=("$arch")
  done < <(platforms "$REPO@$1")
  [ "$(printf '%s\n' "${seen[@]}" | sort)" = "$(printf '%s\n' "${ARCHS[@]}" | sort)" ]
}

# A variant is immutable once pushed: a retried or repeated build reuses it, once it checks out.
if existing=$(crane digest "$REPO:$VARIANT_TAG" 2>/dev/null); then
  variant_matches "$existing" ||
    die "$REPO:$VARIANT_TAG names $existing, which is not the base with the runtime layer on top"
  log "variant already pushed ($existing); reusing it"
  for arch in "${ARCHS[@]}"; do
    emit "RUNTIME_LAYER_${arch^^}" "$(last_layer "$REPO@${LAYER_IMAGE[$arch]}")"
  done
  emit VARIANT_IMAGE "$REPO@$existing"
  emit VARIANT_DIGEST "$existing"
  emit ARCHS "amd64,arm64"
  exit 0
fi
MANIFESTS=()
for arch in "${ARCHS[@]}"; do
  layer=$(last_layer "$REPO@${LAYER_IMAGE[$arch]}")
  base_arch=$(crane digest --platform "linux/$arch" "$BIFFIX_BASE_IMAGE")
  tag="$REPO:$VARIANT_TAG-$arch"
  if ! digest=$(crane digest "$tag" 2>/dev/null); then
    # The stored (compressed) layer blob, appended as-is: the variant's layer is the layer image's.
    crane blob "$REPO@$layer" >"$WORK/layer-$arch.tar.gz"
    crane append -b "${BIFFIX_BASE_IMAGE%@*}@$base_arch" -f "$WORK/layer-$arch.tar.gz" -t "$tag" >/dev/null
    digest=$(crane digest "$tag")
  fi
  variant_image_matches "$REPO@$digest" "$arch" ||
    die "variant $tag ($digest) is not the base's $arch image with layer $layer on top"
  emit "RUNTIME_LAYER_${arch^^}" "$layer"
  MANIFESTS+=(-m "$REPO@$digest")
done
crane index append "${MANIFESTS[@]}" -t "$REPO:$VARIANT_TAG" >/dev/null
VARIANT_DIGEST=$(crane digest "$REPO:$VARIANT_TAG")
emit VARIANT_IMAGE "$REPO@$VARIANT_DIGEST"
emit VARIANT_DIGEST "$VARIANT_DIGEST"
emit ARCHS "$(
  IFS=,
  echo "${ARCHS[*]}"
)"
log "pushed $REPO:$VARIANT_TAG ($VARIANT_DIGEST)"
