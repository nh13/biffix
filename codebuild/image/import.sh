#!/usr/bin/env bash
# Import one workflow image into the tenant's workflow repository, derive a Snakemake task image
# from an imported or built one, or join a built image's architectures into one image in the
# tenant's builds repository.
#
# Runs in the `biffix-<tenant>-image-import` CodeBuild project (unprivileged, no source:
# infra/biffix_infra/stacks/build_stack.py inlines this file into the buildspec). The backend starts
# one build per distinct import or derive, named by BIFFIX_BUILD_SUBJECT; the outputs below come
# back through EventBridge into the build projection. Nothing here runs the image.
#
# Inputs (environment):
#   BIFFIX_BUILD_SUBJECT    image:<content key> | image:derived:<derived key> | image:build:<key>
#                           (the build's join key)
#   BIFFIX_IMAGE_STEP       import (default) | derive | index
#   BIFFIX_IMAGE_REPO       <registry>/<repo> images are imported into: the workflow repository
#                           (project environment)
#   BIFFIX_BUILDS_REPO      <registry>/<repo> biffix builds images into: the builds repository,
#                           which only biffix's build and import projects can push to (project env)
#   BIFFIX_ARCHIVE_URI      s3://<archive bucket>/ — where their exact bytes are archived (project env)
#   BIFFIX_OUTPUTS          file the KEY=VALUE outputs are written to (the buildspec exports them)
#  import:
#   BIFFIX_IMAGE_SOURCE     <registry>/<repo>@sha256:… — the image to import, pinned
#   BIFFIX_CONTENT_KEY      the import's content key (names the import tag)
#  derive:
#   BIFFIX_IMAGE_BASE       <repo>@sha256:… — the imported (workflow repository) or built (builds
#                           repository) image; the derive is pushed to the same repository
#   BIFFIX_RUNTIME_LAYER_AMD64, BIFFIX_RUNTIME_LAYER_ARM64
#                           <coordinator repo>@sha256:… — the runtime's layer images
#                           (codebuild/runtime/build.sh)
#   BIFFIX_DERIVED_KEY      the derive's key (names the derived tag)
#   BIFFIX_SNAKEMAKE_VERSION the Snakemake the runtime layer carries (checked in the derived image)
#  index:
#   BIFFIX_INDEX_MANIFESTS  <builds repo>@sha256:…,… — each architecture's built image
#                           (codebuild/image/build.sh)
#   BIFFIX_CONTENT_KEY      the build's key (names the tag: build-<key12>)
#
# Import:
#   1. copy: `crane copy` by digest (an index with every platform) to <repo>:import-<key12>; the
#      digest must not change.
# Derive (for each linux/amd64 and linux/arm64 image of the base):
#   1. append the runtime layer's stored blob, and end the image's entrypoint with the layer's
#      /opt/biffix/shim-exec (`<the image's entrypoint…> /opt/biffix/shim-exec`), which puts the python
#      shim first on PATH once the image's own entrypoint has set up its environment; then index the
#      results as <repo>:derived-<key12>.
# Index:
#   1. join the architectures' images as <repo>:build-<key12> (one image is tagged as it is).
# A tag already in the repository (a retried or repeated build) is reused only when what it names
# is what the step would produce: for an import, the source's digest; for a derive, each base image
# with the runtime layer on top; for an index, exactly the images it joins.
# Then, all:
#   2. keep-tag: biffix-keep-<digest12> on the image, and -<n> on each image an index lists, so the
#      repository's untagged-expiry lifecycle never removes what a version pins.
#   3. probe: list each platform's filesystem (`crane export | tar -t`) for a bash, the C library
#      (glibc or musl), and the Snakemake and plugin versions installed (derive: the runtime's, which
#      must be BIFFIX_SNAKEMAKE_VERSION, the shim, and the entrypoint); `probe` in
#      codebuild/lib/common.sh.
#   4. archive: the image as an OCI layout under the archive bucket (codebuild/lib/common.sh).
#
# Outputs: IMAGE (<repo>@<digest>), IMAGE_DIGEST, ARCHS, CAPABILITIES
# (bash=1,libc=glibc,snakemake=9.3.0,storage_s3=0.3.6,logger=,executor=), KEEP_TAG, ARCHIVE_BYTES
# (bytes newly archived), ARCHIVED; import: SOURCE_DIGEST; derive: BASE_DIGEST.
#
# Repositories. An import writes the workflow repository, an index the builds repository, and a
# derive the repository its base is in (one of the two). Anyone holding push on the workflow
# repository (the registration role, which third-party workflow CI assumes) can write a tag there
# first, so no tag found there is reused unchecked; the builds repository only biffix can push to.
#
# Tags. Both repositories keep the tags biffix writes immutable (the workflow repository its
# biffix-keep-*, import-*, derived-* and restore-*; the builds repository every tag but scratch-*),
# so each is written exactly once, by tag_once, which accepts a tag that already names the same
# digest and fails on one that names another. An image made in steps (append, then mutate; an
# index) is made under a mutable scratch-* tag and only its final digest gets the immutable tag.
#
# Test hooks: BIFFIX_CRANE_INSECURE=1 (plain-http registry), BIFFIX_SKIP_ECR_LOGIN=1. Sourced
# rather than run, the script only defines its functions (the tests drive them with a stub crane).
set -euo pipefail
export BIFFIX_LOG_NAME="biffix-image-import"
# shellcheck source-path=SCRIPTDIR source=../lib/common.sh
[ -n "${BIFFIX_COMMON_LOADED:-}" ] || . "$(dirname "${BASH_SOURCE[0]}")/../lib/common.sh"

SHIM_DIR=/opt/biffix/shim
SHIM_EXEC=/opt/biffix/shim-exec

# ── 1. import: copy ──
import_image() {
  : "${BIFFIX_IMAGE_SOURCE:?}" "${BIFFIX_CONTENT_KEY:?}"
  local source_digest=${BIFFIX_IMAGE_SOURCE##*@} target existing
  [[ "$source_digest" =~ ^sha256:[0-9a-f]{64}$ ]] || die "BIFFIX_IMAGE_SOURCE is not pinned: $BIFFIX_IMAGE_SOURCE"
  target="$REPO:import-${BIFFIX_CONTENT_KEY:0:12}"
  log "subject=$BIFFIX_BUILD_SUBJECT source=$BIFFIX_IMAGE_SOURCE target=$target"
  ecr_login "$REPO" "$BIFFIX_IMAGE_SOURCE"
  if ! existing=$(crane digest "$target" 2>/dev/null); then
    crane copy "$BIFFIX_IMAGE_SOURCE" "$target"
    existing=$(crane digest "$target")
  fi
  [ "$existing" = "$source_digest" ] || die "imported digest $existing differs from the source's $source_digest"
  emit SOURCE_DIGEST "$source_digest"
  DIGEST=$source_digest
}

# ── 1. derive: base + runtime layer + shim on PATH, per platform ──
# derived_config_matches <derived image> <base image>: whether the derived image's config is the
# base's with shim-exec ending its entrypoint (the command kept), and nothing else changed.
derived_config_matches() {
  python3 -c '
import json, sys
derived, base = (json.loads(c).get("config") or {} for c in sys.argv[1:3])
want = dict(base, Entrypoint=(base.get("Entrypoint") or []) + [sys.argv[3]], Cmd=base.get("Cmd") or [])
got = dict(derived, Entrypoint=derived.get("Entrypoint") or [], Cmd=derived.get("Cmd") or [])
sys.exit(0 if got == want else 1)
' "$(crane config "$1")" "$(crane config "$2")" "$SHIM_EXEC"
}

# derived_matches <digest> <single>: whether <digest> is exactly the derive the plan describes — one
# image per planned architecture, each the base's image with the runtime layer on top and shim-exec
# ending its entrypoint. A tag in the repository proves nothing by itself (whoever may push to the
# repository could have put it there, under a derived or a scratch tag), so a derived image found
# under the tag, or read back from the scratch tag after the mutate, is used only when it checks
# out.
derived_matches() {
  local digest=$1 single=$2 child arch layer dchild dplat d
  if [ "$single" = 1 ]; then
    echo "$digest $(image_arch "$REPO@$digest")" >"$WORK/derived-archs.txt"
  else
    platforms "$REPO@$digest" | while read -r dchild dplat; do
      d=${dplat#*/}
      echo "$dchild ${d%%/*}"
    done >"$WORK/derived-archs.txt"
  fi
  [ "$(($(wc -l <"$WORK/derived-archs.txt")))" = "$(($(wc -l <"$WORK/derive-plan.txt")))" ] || return 1
  while read -r child arch layer _; do
    dchild=$(awk -v a="$arch" '$2 == a { print $1 }' "$WORK/derived-archs.txt")
    [ -n "$dchild" ] || return 1
    [ "$(layers "$REPO@$dchild")" = "$(layers "$REPO@$child") $layer" ] || return 1
    derived_config_matches "$REPO@$dchild" "$REPO@$child" || return 1
  done <"$WORK/derive-plan.txt"
}

derive_image() {
  : "${BIFFIX_IMAGE_BASE:?}" "${BIFFIX_DERIVED_KEY:?}" "${BIFFIX_SNAKEMAKE_VERSION:?}"
  local base=$BIFFIX_IMAGE_BASE target existing child plat arch layer_image layer entrypoint scratch digest
  target="$REPO:derived-${BIFFIX_DERIVED_KEY:0:12}"
  log "subject=$BIFFIX_BUILD_SUBJECT base=$base target=$target snakemake=$BIFFIX_SNAKEMAKE_VERSION"
  ecr_login "$REPO" "${BIFFIX_RUNTIME_LAYER_AMD64:-}" "${BIFFIX_RUNTIME_LAYER_ARM64:-}"
  emit BASE_DIGEST "${base##*@}"
  # The plan, "<base image> <arch> <runtime layer digest> <layer image>" per platform the Batch
  # fleet runs (x86_64 and Graviton): only those get the runtime.
  local single=0
  platforms "$base" >"$WORK/base-platforms.txt"
  : >"$WORK/derive-plan.txt"
  while read -r child plat; do
    arch=${plat#*/}
    if [ -z "$plat" ]; then
      single=1
      arch=$(image_arch "$base")
    elif [[ "$plat" != linux/* ]]; then
      continue
    fi
    case "$arch" in
      amd64 | amd64/*) layer_image=${BIFFIX_RUNTIME_LAYER_AMD64:-} ;;
      arm64 | arm64/*) layer_image=${BIFFIX_RUNTIME_LAYER_ARM64:-} ;;
      *) continue ;;
    esac
    arch=${arch%%/*}
    [ -n "$layer_image" ] || die "no runtime layer image for $arch"
    echo "$child $arch $(last_layer "$layer_image") $layer_image" >>"$WORK/derive-plan.txt"
  done <"$WORK/base-platforms.txt"
  [ -s "$WORK/derive-plan.txt" ] || die "the image has no linux/amd64 or linux/arm64 platform"
  if existing=$(crane digest "$target" 2>/dev/null); then
    derived_matches "$existing" "$single" ||
      die "$target names $existing, which is not the base with the runtime layer on top"
    log "derived image already pushed ($existing); reusing it"
    DIGEST=$existing
    return 0
  fi
  local manifests=()
  while read -r child arch layer layer_image; do
    [ -f "$WORK/layer-$arch.tar.gz" ] || crane blob "${layer_image%@*}@$layer" >"$WORK/layer-$arch.tar.gz"
    # The image's own entrypoint, then shim-exec (a wrapper entrypoint execs its arguments; an image
    # whose entrypoint does not could not run the executor's command either). Setting an
    # entrypoint clears the command, so the image's own is set again. `crane mutate` reads each
    # list as one CSV record, so the elements are CSV-encoded (quotes and commas survive).
    local config=()
    mapfile -t config < <(crane config "$REPO@$child" | python3 -c '
import csv, io, json, sys
config = json.load(sys.stdin).get("config") or {}
entrypoint, cmd = config.get("Entrypoint") or [], config.get("Cmd") or []
if any("\n" in e or "\r" in e for e in entrypoint + cmd):
    sys.exit(f"cannot rewrite an entrypoint or command with a line break in it: {entrypoint} {cmd}")
def record(items):
    out = io.StringIO()
    csv.writer(out, lineterminator="").writerow(items)
    return out.getvalue()
print(record(entrypoint + [sys.argv[1]]))
print(record(cmd))
' "$SHIM_EXEC")
    entrypoint=${config[0]:?}
    local mutate=(--entrypoint "$entrypoint")
    [ -n "${config[1]:-}" ] && mutate+=(--cmd "${config[1]}")
    # Appended and mutated under the scratch tag; the result is then tagged derived-…-<arch> once,
    # and the scratch tag left on it (so the pre-mutate image is untagged, and expires).
    scratch="$REPO:scratch-${target##*:}-$arch"
    crane append -b "$REPO@$child" -f "$WORK/layer-$arch.tar.gz" -t "$scratch" >/dev/null
    crane mutate "$scratch" "${mutate[@]}" -t "$scratch" >/dev/null
    digest=$(crane digest "$scratch")
    tag_once "$REPO" "$digest" "${target##*:}-$arch"
    log "derived $arch ${child:0:19} -> ${digest:0:19} (layer ${layer:0:19}, entrypoint $entrypoint)"
    manifests+=("$digest")
  done <"$WORK/derive-plan.txt"
  if [ "$single" = 1 ]; then
    # A single-platform image derives a single image.
    DIGEST=${manifests[0]}
  else
    local args=() d
    for d in "${manifests[@]}"; do args+=(-m "$REPO@$d"); done
    crane index append "${args[@]}" -t "$REPO:scratch-${target##*:}" >/dev/null
    DIGEST=$(crane digest "$REPO:scratch-${target##*:}")
  fi
  derived_matches "$DIGEST" "$single" || die "the derived image $DIGEST is not the base with the runtime layer on top"
  tag_once "$REPO" "$DIGEST" "${target##*:}"
}

# ── 1. index: a built image's architectures, as one image ──
# index_matches <digest> <image@digest>…: whether <digest> joins exactly those images (for one
# image: is it). As for a derive, a tag found in the repository is reused only when it checks out.
index_matches() {
  local digest=$1
  shift
  if [ "$#" = 1 ]; then
    [ "$digest" = "${1##*@}" ]
    return
  fi
  [ "$(platforms "$REPO@$digest" | awk '{ print $1 }' | sort)" = "$(printf '%s\n' "${@##*@}" | sort)" ]
}

index_image() {
  : "${BIFFIX_INDEX_MANIFESTS:?}" "${BIFFIX_CONTENT_KEY:?}"
  local target="$REPO:build-${BIFFIX_CONTENT_KEY:0:12}" existing manifests=() args=() m
  IFS=, read -ra manifests <<<"$BIFFIX_INDEX_MANIFESTS"
  log "subject=$BIFFIX_BUILD_SUBJECT target=$target manifests=${#manifests[@]}"
  ecr_login "$REPO"
  for m in "${manifests[@]}"; do
    [[ "$m" == "$REPO@sha256:"* && "${m##*@sha256:}" =~ ^[0-9a-f]{64}$ ]] || die "not an image in $REPO: $m"
    args+=(-m "$m")
  done
  if existing=$(crane digest "$target" 2>/dev/null); then
    index_matches "$existing" "${manifests[@]}" ||
      die "$target names $existing, which does not join exactly $BIFFIX_INDEX_MANIFESTS"
    log "index already pushed ($existing); reusing it"
    DIGEST=$existing
    return 0
  fi
  if [ "${#manifests[@]}" = 1 ]; then
    DIGEST=${manifests[0]##*@}
  else
    crane index append "${args[@]}" -t "$REPO:scratch-${target##*:}" >/dev/null
    DIGEST=$(crane digest "$REPO:scratch-${target##*:}")
  fi
  index_matches "$DIGEST" "${manifests[@]}" || die "the index $DIGEST does not join exactly $BIFFIX_INDEX_MANIFESTS"
  tag_once "$REPO" "$DIGEST" "${target##*:}"
}

# step_repo <step>: the repository <step> writes (see "Repositories" above).
step_repo() {
  case "$1" in
    import) echo "$BIFFIX_IMAGE_REPO" ;;
    index) echo "$BIFFIX_BUILDS_REPO" ;;
    derive)
      local base_repo=${BIFFIX_IMAGE_BASE:?}
      base_repo=${base_repo%@*}
      [ "$base_repo" = "$BIFFIX_IMAGE_REPO" ] || [ "$base_repo" = "$BIFFIX_BUILDS_REPO" ] ||
        die "the base $BIFFIX_IMAGE_BASE is in neither $BIFFIX_IMAGE_REPO nor $BIFFIX_BUILDS_REPO"
      echo "$base_repo"
      ;;
    *) die "unknown BIFFIX_IMAGE_STEP $1" ;;
  esac
}

main() {
  : "${BIFFIX_BUILD_SUBJECT:?}" "${BIFFIX_IMAGE_REPO:?}" "${BIFFIX_BUILDS_REPO:?}"
  : "${BIFFIX_ARCHIVE_URI:?}" "${BIFFIX_OUTPUTS:?}"
  STEP=${BIFFIX_IMAGE_STEP:-import}
  WORK=$(mktemp -d)
  : >"$BIFFIX_OUTPUTS"
  REPO=$(step_repo "$STEP")
  install_crane "$WORK"

  case "$STEP" in
    import) import_image ;;
    derive) derive_image ;;
    index) index_image ;;
    *) die "unknown BIFFIX_IMAGE_STEP $STEP" ;;
  esac
  IMAGE="$REPO@$DIGEST"

  # ── 2. keep-tag (the image, and each image an index lists) ──
  platforms "$IMAGE" >"$WORK/platforms.txt"
  KEEP_TAG=$(keep_tag "$REPO" "$DIGEST" "$WORK/platforms.txt")
  ARCHS=$(awk '{ split($2, p, "/"); if (p[2] != "") print p[2] }' "$WORK/platforms.txt" | sort -u | paste -sd, -)
  if [ -z "$ARCHS" ]; then
    # A single-platform image names its architecture in its config.
    ARCHS=$(image_arch "$IMAGE")
  fi

  # ── 3. probe (no execution: the file listing of each platform's filesystem) ──
  # probe (codebuild/lib/common.sh) reads each platform's file listing.
  CAPABILITIES=""
  while read -r child plat; do
    case "$plat" in "" | linux/amd64* | linux/arm64*) ;; *) continue ;; esac
    platform=()
    [ -n "$plat" ] && platform=(--platform "$plat")
    crane export "${platform[@]}" "$IMAGE" - | tar -t 2>/dev/null >"$WORK/listing.txt"
    if [ "$STEP" = derive ]; then
      caps=$(probe opt/biffix/sm/ <"$WORK/listing.txt")
      [[ ",$caps," == *",snakemake=$BIFFIX_SNAKEMAKE_VERSION,"* ]] ||
        die "derived ${plat:-image} lacks the runtime's Snakemake $BIFFIX_SNAKEMAKE_VERSION ($caps)"
      grep -Eq "^(\./)?${SHIM_DIR#/}/python$" "$WORK/listing.txt" ||
        die "derived ${plat:-image} lacks $SHIM_DIR/python"
      crane config "${platform[@]}" "$IMAGE" | python3 -c '
import json, sys
entrypoint = (json.load(sys.stdin).get("config") or {}).get("Entrypoint") or []
sys.exit(0 if entrypoint[-1:] == [sys.argv[1]] else f"the entrypoint does not end with shim-exec: {entrypoint}")
' "$SHIM_EXEC"
    else
      caps=$(probe <"$WORK/listing.txt")
    fi
    log "probe ${plat:-image} ${child:0:19}: $caps"
    # The first platform with a libc (amd64 before arm64 in most indexes) describes the image.
    if [ -z "$CAPABILITIES" ] || [[ "$CAPABILITIES" == *libc=none* && "$caps" != *libc=none* ]]; then
      CAPABILITIES=$caps
    fi
  done <"$WORK/platforms.txt"

  # ── 4. archive ──
  ARCHIVE_BYTES=$(archive "$IMAGE" "$BIFFIX_ARCHIVE_URI" "$WORK")

  emit IMAGE "$IMAGE"
  emit IMAGE_DIGEST "$DIGEST"
  emit ARCHS "$ARCHS"
  emit CAPABILITIES "$CAPABILITIES"
  emit KEEP_TAG "$KEEP_TAG"
  emit ARCHIVE_BYTES "$ARCHIVE_BYTES"
  emit ARCHIVED 1
  log "$STEP: $IMAGE (archs ${ARCHS:-?}; $CAPABILITIES; archived $ARCHIVE_BYTES new bytes)"
}

# Run, unless sourced (the tests source it for its functions).
if [ "${BASH_SOURCE[0]}" = "$0" ]; then
  main
fi
