#!/usr/bin/env bash
# Build one architecture of a workflow image from the recipe the workflow ships (a Dockerfile, or
# the Dockerfile biffix generates for a pixi environment), and push it to the tenant's builds
# repository.
#
# Runs in the `biffix-<tenant>-image-build-amd64` / `-arm64` CodeBuild projects (privileged, for
# BuildKit; native to their architecture, so nothing is emulated; no source:
# infra/biffix_infra/stacks/build_stack.py inlines this file into the buildspec). The backend starts
# one build per architecture, named by BIFFIX_BUILD_SUBJECT; the import project's `index` step then
# joins them into one image and keep-tags, probes and archives it (codebuild/image/import.sh).
#
# Inputs (environment):
#   BIFFIX_BUILD_SUBJECT    image:build:<key>:<arch> (the build's join key)
#   BIFFIX_BUILD_ARCH       amd64 | arm64 — what this project builds (project environment)
#   BIFFIX_BUILDS_REPO      <registry>/<repo> the image is pushed to: the builds repository
#                           (project environment)
#   BIFFIX_CONTENT_KEY      the build's key (names the tag: build-<key12>-<arch>)
#   BIFFIX_CONTEXT_URI      s3://…/contexts/<tree_hash>.tar.gz — the context's git-tracked files
#   BIFFIX_CONTEXT_SHA256   the tarball's sha256 (checked before it is used; the key is of it, so
#                           an image under the key is only ever built from this tarball)
#   BIFFIX_DOCKERFILE       the Dockerfile's path within the context, or
#   BIFFIX_DOCKERFILE_B64   the Dockerfile's text, base64 (one biffix generated)
#   BIFFIX_BUILD_TARGET     the Dockerfile stage to build (`--target`; optional)
#   BIFFIX_BUILD_ARGS_B64   the build args, base64 of one KEY=VALUE line each (`--build-arg`;
#                           optional). The target and build args are part of the key.
#   BIFFIX_OUTPUTS          file the KEY=VALUE outputs are written to (the buildspec exports them)
#
# Outputs: IMAGE (<repo>@<digest>), IMAGE_DIGEST, ARCH, and for a build with a target or build
# args BUILD_OPTIONS: their digest, sha256 of "$BIFFIX_BUILD_TARGET\n$BIFFIX_BUILD_ARGS_B64".
#
# A build with options labels its image with that digest (org.biffix.build-options) and reuses a tag
# only when it carries the same label. A script that predates options built them away (no
# --target/--build-arg) under the key that names them; its tag is refused rather than reused, and
# the backend does not use a build that reports no BUILD_OPTIONS
# (crates/biffix-backend/src/images/build.rs). A build without options is as before: no label,
# no BUILD_OPTIONS, no label check.
#
# The image is pushed as a plain manifest (no attestations, which would make each architecture an
# index of its own), and reused when the tag already names one: the key covers everything it is
# built from. The repository keeps build-* tags immutable, so the tag is written once (a concurrent
# build of the same key fails its push rather than move it). A build is not reproducible, so unlike
# the import project's reuses (codebuild/image/import.sh) this one cannot check what the tag names
# against anything. It is sound because the builds repository is biffix's own: only the image-build
# projects and the import project can push to it (infra/biffix_infra/stacks/build_stack.py), never
# the registration role third-party workflow CI assumes, which pushes to the workflow repository.
# The backend trusts a built image only from the builds repository
# (crates/biffix-backend/src/images/build.rs).
set -euo pipefail
export BIFFIX_LOG_NAME="biffix-image-build"
# shellcheck source-path=SCRIPTDIR source=../lib/common.sh
[ -n "${BIFFIX_COMMON_LOADED:-}" ] || . "$(dirname "${BASH_SOURCE[0]}")/../lib/common.sh"

: "${BIFFIX_BUILD_SUBJECT:?}" "${BIFFIX_BUILD_ARCH:?}" "${BIFFIX_BUILDS_REPO:?}" "${BIFFIX_OUTPUTS:?}"
: "${BIFFIX_CONTENT_KEY:?}" "${BIFFIX_CONTEXT_URI:?}" "${BIFFIX_CONTEXT_SHA256:?}"
: >"$BIFFIX_OUTPUTS"
ARCH=$BIFFIX_BUILD_ARCH
REPO=$BIFFIX_BUILDS_REPO
# biffix-workflow images::BUILD_TAG_PREFIX.
TARGET="$REPO:build-${BIFFIX_CONTENT_KEY:0:12}-$ARCH"
WORK=$(mktemp -d)

case "$(uname -m)" in
  x86_64) native=amd64 ;;
  aarch64 | arm64) native=arm64 ;;
  *) die "unsupported build host $(uname -m)" ;;
esac
[ "$native" = "$ARCH" ] || die "this project builds $ARCH but runs on $native"
log "subject=$BIFFIX_BUILD_SUBJECT arch=$ARCH target=$TARGET context=$BIFFIX_CONTEXT_URI"

if [ "${BIFFIX_SKIP_ECR_LOGIN:-}" != 1 ]; then
  aws ecr get-login-password | docker login -u AWS --password-stdin "${REPO%%/*}" >/dev/null
fi

# build_flags: set BUILD_FLAGS to the `docker buildx build` flags for BIFFIX_BUILD_TARGET and
# BIFFIX_BUILD_ARGS_B64, each value one argument (never word-split or globbed), and, when there
# are any, BUILD_OPTIONS to their digest and a flag labelling the image with it.
build_flags() {
  BUILD_FLAGS=()
  BUILD_OPTIONS=
  if [ -n "${BIFFIX_BUILD_TARGET:-}" ]; then
    BUILD_FLAGS+=(--target "$BIFFIX_BUILD_TARGET")
  fi
  if [ -n "${BIFFIX_BUILD_ARGS_B64:-}" ]; then
    local args line
    args=$(base64 -d <<<"$BIFFIX_BUILD_ARGS_B64") || die "BIFFIX_BUILD_ARGS_B64 is not base64"
    while IFS= read -r line; do
      [ -n "$line" ] || continue
      [[ "$line" =~ ^[A-Za-z_][A-Za-z0-9_]*= ]] || die "malformed build arg: $line"
      BUILD_FLAGS+=(--build-arg "$line")
    done <<<"$args"
  fi
  if [ -n "${BIFFIX_BUILD_TARGET:-}${BIFFIX_BUILD_ARGS_B64:-}" ]; then
    BUILD_OPTIONS=$(printf '%s\n%s' "${BIFFIX_BUILD_TARGET:-}" "${BIFFIX_BUILD_ARGS_B64:-}" |
      sha256sum | cut -d' ' -f1)
    BUILD_FLAGS+=(--label "org.biffix.build-options=$BUILD_OPTIONS")
  fi
}

# image_label <image> <label>: the value of <label> on <image>'s config (empty when it has none).
image_label() {
  local json
  json=$(docker buildx imagetools inspect "$1" --format '{{json .Image}}') || return 1
  python3 -c '
import json, sys
image = json.load(sys.stdin)
if "config" not in image and len(image) == 1:  # one platform, keyed by it
    image = next(iter(image.values()))
print(((image.get("config") or {}).get("Labels") or {}).get(sys.argv[1], ""))
' "$2" <<<"$json"
}

# check_reused <image>: for a build with options, refuse to reuse <image> unless its label says it
# was built with them.
check_reused() {
  [ -n "$BUILD_OPTIONS" ] || return 0
  local label
  label=$(image_label "$1" org.biffix.build-options) || die "cannot read the labels of $1"
  [ "$label" = "$BUILD_OPTIONS" ] ||
    die "tag $1 was built without its build options; delete it and retry"
}

# manifest <image>: "<digest> <media type>" of what <image> names, or nothing when it does not exist.
manifest() {
  local json
  json=$(docker buildx imagetools inspect "$1" --format '{{json .Manifest}}' 2>/dev/null) || return 0
  python3 -c 'import json, sys; m = json.load(sys.stdin); print(m["digest"], m["mediaType"])' <<<"$json"
}

build_flags
existing=$(manifest "$TARGET")
if [ -z "$existing" ]; then
  aws s3 cp --quiet "$BIFFIX_CONTEXT_URI" "$WORK/context.tar.gz"
  [[ "$BIFFIX_CONTEXT_SHA256" =~ ^[0-9a-f]{64}$ ]] || die "BIFFIX_CONTEXT_SHA256 is not a sha256"
  [ "$(sha256sum "$WORK/context.tar.gz" | cut -d' ' -f1)" = "$BIFFIX_CONTEXT_SHA256" ] ||
    die "the context $BIFFIX_CONTEXT_URI does not match its sha256"
  mkdir "$WORK/context"
  tar -xzf "$WORK/context.tar.gz" -C "$WORK/context"
  if [ -n "${BIFFIX_DOCKERFILE_B64:-}" ]; then
    dockerfile="$WORK/Dockerfile.biffix"
    base64 -d <<<"$BIFFIX_DOCKERFILE_B64" >"$dockerfile"
  else
    : "${BIFFIX_DOCKERFILE:?BIFFIX_DOCKERFILE or BIFFIX_DOCKERFILE_B64 is required}"
    dockerfile="$WORK/context/$BIFFIX_DOCKERFILE"
    [ -f "$dockerfile" ] || die "the context has no $BIFFIX_DOCKERFILE (only git-tracked files are sent)"
  fi
  docker buildx build \
    --platform "linux/$ARCH" \
    ${BUILD_FLAGS[@]+"${BUILD_FLAGS[@]}"} \
    --provenance=false --sbom=false \
    --file "$dockerfile" \
    --tag "$TARGET" \
    --push \
    "$WORK/context"
  existing=$(manifest "$TARGET")
else
  check_reused "$TARGET"
  log "already built ($existing); reusing it"
fi

read -r DIGEST media_type <<<"$existing"
case "$media_type" in
  application/vnd.docker.distribution.manifest.v2+json | application/vnd.oci.image.manifest.v1+json) ;;
  *) die "$TARGET is a $media_type, not an image manifest" ;;
esac

emit IMAGE "$REPO@$DIGEST"
emit IMAGE_DIGEST "$DIGEST"
emit ARCH "$ARCH"
if [ -n "$BUILD_OPTIONS" ]; then
  emit BUILD_OPTIONS "$BUILD_OPTIONS"
fi
log "built $ARCH: $REPO@$DIGEST"
