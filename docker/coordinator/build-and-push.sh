#!/usr/bin/env bash
# Build the biffix base coordinator image and push it to the tenant's coordinator ECR repo.
#
# CI (.github/workflows/build-coordinator-image.yml) is the supported path: it builds both fleet
# architectures natively from a known commit. This script is the manual fallback for a scratch
# tenant. Like CI, it pushes one multi-architecture image (linux/amd64 + linux/arm64) under the
# commit SHA, because the engine-runtime build derives a variant for every architecture from this
# base. The non-native half is built under emulation, so expect it to be slow; it needs a buildx
# builder that can build and push multi-platform images (Docker Desktop's default can).
#
# Prereqs: docker (with buildx), python3 + pip, Java 17 for plugins/nf-biffix, uv (to read the
# deploy config), and AWS credentials for the target account already exported.
#
# Usage:
#   BIFFIX_DEPLOY_CONFIG=biffix-deploy.yaml ./docker/coordinator/build-and-push.sh
# The config (a path relative to infra/ or the current directory) names the tenant and region, and
# the repo is the tenant data stack's own CoordinatorRepo. Without a config, set AWS_REGION and
# either BIFFIX_TENANT or COORDINATOR_REPO.
#
# Optional overrides:
#   COORDINATOR_REPO  the repository name (skips the lookup).
#   IMAGE_TAG         default: the 40-character commit SHA of HEAD, which requires a clean tree
#                     (the repo's tags are immutable, and the tag must name exactly what was built).
#                     Deploy it by setting `coordinator_image_tag` to this tag.
#   PLATFORMS         default: linux/amd64,linux/arm64. Dropping one breaks the runtime builds
#                     for that architecture.
set -euo pipefail

log()  { printf '[build-and-push] %s\n' "$*"; }
die()  { printf '[build-and-push] error: %s\n' "$*" >&2; exit 1; }

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
INFRA_DIR="$REPO_ROOT/infra"
# shellcheck source=SCRIPTDIR/../../infra/script_lib.sh
. "$INFRA_DIR/script_lib.sh"

TENANT="${BIFFIX_TENANT:-}"
REGION="${AWS_REGION:-}"
if [ -n "${BIFFIX_DEPLOY_CONFIG:-}" ]; then
  config="$(resolve_config "$BIFFIX_DEPLOY_CONFIG")"
  load_config "$config" tenant region data_stack
  TENANT="$(cfg tenant)"
  REGION="$(cfg region)"
  DATA_STACK="$(cfg data_stack)"
else
  [ -n "$REGION" ] || die "set BIFFIX_DEPLOY_CONFIG, or AWS_REGION (with BIFFIX_TENANT or COORDINATOR_REPO)"
  DATA_STACK="biffix-${TENANT}-data"
fi

ACCOUNT="$(aws sts get-caller-identity --query Account --output text)" \
  || die "could not resolve the AWS account (no valid credentials?)"
REGISTRY="${ACCOUNT}.dkr.ecr.${REGION}.amazonaws.com"

if [ -z "${IMAGE_TAG:-}" ]; then
  [ -z "$(git -C "$REPO_ROOT" status --porcelain)" ] \
    || die "the working tree has uncommitted changes; commit them, or set IMAGE_TAG explicitly"
  IMAGE_TAG="$(git -C "$REPO_ROOT" rev-parse HEAD)"
fi
PLATFORMS="${PLATFORMS:-linux/amd64,linux/arm64}"

# The repo is CFN-named, so resolve it from the tenant's data stack (exactly one CoordinatorRepo),
# never by a substring that a retained repo from an earlier deploy, or another tenant's, also has.
if [ -z "${COORDINATOR_REPO:-}" ]; then
  [ -n "$TENANT" ] || die "set BIFFIX_DEPLOY_CONFIG, BIFFIX_TENANT or COORDINATOR_REPO"
  COORDINATOR_REPO="$(stack_resource "$DATA_STACK" AWS::ECR::Repository CoordinatorRepo)" \
    || die "could not resolve the coordinator repo from $DATA_STACK"
  [ -n "$COORDINATOR_REPO" ] || die "no CoordinatorRepo in stack $DATA_STACK ($REGION); deploy the data stack first"
fi

log "registry:    $REGISTRY"
log "coordinator: $COORDINATOR_REPO:$IMAGE_TAG ($PLATFORMS)"

(cd "$REPO_ROOT/plugins/nf-biffix" && ./gradlew --no-daemon assemble)
"$REPO_ROOT/docker/coordinator/build-wheels.sh" "$REPO_ROOT/build/coordinator-wheels"

aws ecr get-login-password --region "$REGION" \
  | docker login --username AWS --password-stdin "$REGISTRY"

# A multi-platform image cannot be loaded into the local image store, so build and push in one step.
docker buildx build --platform "$PLATFORMS" --push \
  -f "$REPO_ROOT/docker/coordinator/Dockerfile" \
  -t "$REGISTRY/$COORDINATOR_REPO:$IMAGE_TAG" \
  "$REPO_ROOT"

log "pushed: $REGISTRY/$COORDINATOR_REPO:$IMAGE_TAG"
log "deploy it with: coordinator_image_tag: \"$IMAGE_TAG\""
