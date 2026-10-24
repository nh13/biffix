#!/usr/bin/env bash
# Deploy the biffix control plane end-to-end: guard the target account, build the frontend and the
# four Rust Lambdas (backend, sweep, ingest, reconcile), stage the pinned host AWS CLI in the
# reference bucket, run `cdk deploy`, protect the deployed coordinator image and prune old build
# tags (image retention), and verify the result with `biffix health`.
#
# This automates the manual steps in infra/README.md ("Deploying the control plane"). Built from
# source, it does NOT build/push the coordinator image or bump the image tag in the deploy config
# (coordinator_image_tag) — that is the CI path (build-coordinator-image
# workflow), by design (immutable tags, reviewed commit). See the README.
#
# With --release vX.Y.Z it builds nothing: it downloads that published release (the UI build, the
# four Lambda zips and a manifest naming the coordinator image by digest), checks every file against
# the release's SHA256SUMS (biffix_infra.release), deploys those artifacts, and copies the
# coordinator image by digest into the tenant's coordinator repository under the tag vX.Y.Z, which
# is coordinator_image_tag's default. The release must be this checkout's version (check out the
# tag), so the infra code deploying it is the release's own.
#
# Usage:
#   infra/deploy.sh --config <file> [options]
#
# Options:
#   -c, --config <file>     REQUIRED. The deploy config (start one from
#                           infra/biffix-deploy.example.yaml). A relative path is looked up in
#                           infra/ first, then in the current directory. Read through
#                           biffix_infra.config (never parsed here) and passed to cdk as
#                           `-c configFile=…`.
#   -a, --account <id>      The AWS account to deploy into. REQUIRED unless the config sets
#                           `account:`; when both are given they must agree. The deploy aborts
#                           unless the (post-assume) caller account is exactly this one, and it
#                           always refuses the AWS Organizations management account.
#       --assume-role <arn> Act as this role (e.g. an org member account's
#                           OrganizationAccountAccessRole), then apply the guard. Its session is
#                           renewed as it nears expiry, so a deploy may outlast the hour.
#   -s, --stack <name>      Deploy only this stack (cdk --exclusively), and skip image retention.
#                           Default: --all.
#       --url <api-base>    Verify against this API base instead of deriving it from the deploy.
#       --skip-ui           Do not rebuild the frontend (reuse ui/build, which must be a real
#                           SvelteKit build with its _app/ assets).
#       --skip-lambdas      Do not rebuild the Rust Lambdas.
#       --no-verify         Skip the post-deploy `biffix health` check.
#       --release <tag>     Deploy the published release <tag> (vX.Y.Z) instead of building the UI
#                           and Lambdas, and import its coordinator image (see above). Needs crane
#                           (https://github.com/google/go-containerregistry) for the import, but no
#                           Rust, cargo-lambda or Node toolchain for the build. Downloads from
#                           $BIFFIX_RELEASE_BASE_URL/<tag>/ when set (a fork's or a mirror's
#                           releases, or a file:// directory), else from the workspace repository's
#                           GitHub releases; into infra/.release/<tag>/.
#       --dry-run           `cdk synth` instead of deploy (still builds the artifacts, or fetches the
#                           release with --release). Changes nothing in AWS: the Google secret and
#                           the ACM cert are only looked up, never created, so a custom-domain dry
#                           run without an issued cert synthesizes the alias-less template.
#   -h, --help              Show this help.
#
# Google federation: if the config has a `google_oidc:` block and the client secret is not yet in
# Secrets Manager (name `biffix/<tenant>/google-oidc`) in the target account, this prompts for it
# once (input hidden) and stores it there. Set BIFFIX_GOOGLE_CLIENT_SECRET to supply it without a
# prompt (CI). The (non-secret) client_id stays in config; the secret never touches config or git.
#
# Custom domain: if the config sets `app_domain` but no `app_certificate_arn`, this requests a
# DNS-validated ACM cert in us-east-1 (or reuses an existing ISSUED/pending one), prints the DNS
# validation record to publish, waits for issuance, and attaches the CloudFront alias in the same
# run. You still add two records at your DNS provider (any provider where you can add a CNAME): the
# ACM validation CNAME, and the alias `app_domain` -> the CloudFront domain. NOTE: CloudFront
# refuses the alias while that name still points at a DIFFERENT distribution, so repoint the alias
# record first.
#
# BIFFIX_ALLOW_UNBUILT_LAMBDA (the synth-only escape hatch that swaps in a placeholder Lambda) is
# refused for a real deploy: it would ship a Lambda that cannot run.
#
# Prereqs: node deps installed in infra/ (`npm ci`), uv, cargo-lambda (crane instead with
# --release), and AWS credentials for the target account already available (or reachable via
# --assume-role).
set -euo pipefail

# ── Resolve paths ────────────────────────────────────────────────────────────────
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
INFRA_DIR="$SCRIPT_DIR"
# Cargo's target directory: CARGO_TARGET_DIR when set, else the workspace's target/. Cargo resolves
# a relative one against the directory it runs in, and every cargo call below runs in $REPO_ROOT.
# The CDK app finds the Lambda artifacts the same way (biffix_infra/paths.py).
TARGET_DIR="${CARGO_TARGET_DIR:-target}"
case "$TARGET_DIR" in /*) ;; *) TARGET_DIR="$REPO_ROOT/$TARGET_DIR" ;; esac

# ── Defaults ─────────────────────────────────────────────────────────────────────
CONFIG=""
ACCOUNT=""
ASSUME_ROLE=""
STACK=""
VERIFY_URL=""
SKIP_UI=0
SKIP_LAMBDAS=0
NO_VERIFY=0
DRY_RUN=0
RELEASE=""

log()  { printf '\033[1;36m[deploy]\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33m[deploy]\033[0m %s\n' "$*" >&2; }
die()  { printf '\033[1;31m[deploy] error:\033[0m %s\n' "$*" >&2; exit 1; }

usage() { sed -n '2,/^set -euo/p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//; /^set -euo/d'; }

# shellcheck source=SCRIPTDIR/script_lib.sh
. "$INFRA_DIR/script_lib.sh"

# The temp files this run makes (the Google client secret, the cdk log, the host AWS CLI zips, and
# --assume-role's AWS config), removed however it exits: success, die, or an interrupt.
# `new_temp <var> [mktemp args]` makes one and sets <var> to its path.
TEMP_FILES=""
remove_temp_files() {
  local f
  for f in $TEMP_FILES; do rm -f "$f"; done
  [ -z "$ROLE_CONFIG" ] || rm -f "$ROLE_CONFIG"
}
trap remove_temp_files EXIT
new_temp() {
  local name="$1" path
  shift
  path="$(mktemp "$@")" || die "could not create a temp file"
  TEMP_FILES="$TEMP_FILES $path"
  printf -v "$name" '%s' "$path"
}

# ── Parse args ───────────────────────────────────────────────────────────────────
while [ $# -gt 0 ]; do
  case "$1" in
    -c|--config)      CONFIG="$(optarg "$@")"; shift 2 ;;
    -a|--account)     ACCOUNT="$(optarg "$@")"; shift 2 ;;
    --assume-role)    ASSUME_ROLE="$(optarg "$@")"; shift 2 ;;
    -s|--stack)       STACK="$(optarg "$@")"; shift 2 ;;
    --url)            VERIFY_URL="$(optarg "$@")"; shift 2 ;;
    --skip-ui)        SKIP_UI=1; shift ;;
    --skip-lambdas)   SKIP_LAMBDAS=1; shift ;;
    --no-verify)      NO_VERIFY=1; shift ;;
    --dry-run)        DRY_RUN=1; shift ;;
    --release)        RELEASE="$(optarg "$@")"; shift 2 ;;
    -h|--help)        usage; exit 0 ;;
    *)                die "unknown option: $1 (see --help)" ;;
  esac
done

require_deploy_tools

# A release brings its own UI and Lambdas, so the flags that reuse local builds do not apply.
if [ -n "$RELEASE" ]; then
  [[ "$RELEASE" =~ ^v[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.]+)?$ ]] \
    || die "--release takes a release tag, vMAJOR.MINOR.PATCH (got '$RELEASE')"
  { [ "$SKIP_UI" -eq 0 ] && [ "$SKIP_LAMBDAS" -eq 0 ]; } \
    || die "--release deploys the release's UI and Lambdas; drop --skip-ui/--skip-lambdas"
  [ "$DRY_RUN" -eq 1 ] || command -v crane >/dev/null \
    || die "crane not found — --release copies the coordinator image with it (e.g. 'brew install crane')"
fi

# The synth-only placeholder-Lambda escape hatch must never reach a real deploy.
if [ -n "${BIFFIX_ALLOW_UNBUILT_LAMBDA:-}" ]; then
  [ "$DRY_RUN" -eq 1 ] \
    || die "BIFFIX_ALLOW_UNBUILT_LAMBDA is set: it deploys placeholder Lambdas that cannot run; unset it"
  warn "BIFFIX_ALLOW_UNBUILT_LAMBDA is set (allowed for --dry-run only)"
fi

# ── Read the config (through biffix_infra.config, the single source of truth) ──────
# Every value this script uses comes from DeployConfig.load, exactly as the CDK app reads it, so the
# deploy gates can never drift from the app (quoting, defaults, absolute paths).
CONFIG="$(resolve_config "$CONFIG")"
load_config "$CONFIG" tenant region account google_secret_name app_domain app_certificate_arn \
  coordinator_image_tag stack_names data_stack edge_stack host_aws_cli_version host_aws_cli_zips
TENANT="$(cfg tenant)"
REGION="$(cfg region)"
CONFIG_ACCOUNT="$(cfg account)"
SECRET_NAME="$(cfg google_secret_name)"
APP_DOMAIN="$(cfg app_domain)"
APP_CERT_CFG="$(cfg app_certificate_arn)"
COORD_TAG="$(cfg coordinator_image_tag)"
ALL_STACKS="$(cfg stack_names)"
DATA_STACK="$(cfg data_stack)"
EDGE_STACK="$(cfg edge_stack)"
HOST_AWS_CLI_VERSION="$(cfg host_aws_cli_version)"
HOST_AWS_CLI_ZIPS="$(cfg host_aws_cli_zips)"
{ [ -n "$TENANT" ] && [ -n "$REGION" ]; } || die "$CONFIG: tenant and region must be set"
# Every region-scoped call below (and the aws subprocesses of image_retention) targets the config's
# region, whatever the shell's default is. The ACM calls pass us-east-1 explicitly (CloudFront).
export AWS_REGION="$REGION" AWS_DEFAULT_REGION="$REGION"
log "config: $CONFIG (tenant $TENANT, region $REGION)"
# The release's image is imported under its own tag, so that is the tag the deploy must run.
if [ -n "$RELEASE" ] && [ "$COORD_TAG" != "$RELEASE" ]; then
  die "--release $RELEASE imports the coordinator image as $RELEASE, but the config's coordinator_image_tag is $COORD_TAG: remove coordinator_image_tag from the config (it defaults to this checkout's release tag) or set it to $RELEASE"
fi

# ── Assume role (optional) ───────────────────────────────────────────────────────
[ -z "$ASSUME_ROLE" ] || assume_role "$ASSUME_ROLE" biffix-deploy

# ── Account guard (REQUIRED) ─────────────────────────────────────────────────────
# One deploy = one account. Deploying into the wrong (usually management) account creates RETAIN'd
# buckets, KMS keys and an Aurora cluster that are painful to remove, so the target account must be
# named (--account or the config's `account:`) and the caller must be exactly there.
guard_account "$ACCOUNT" "$CONFIG_ACCOUNT"

# ── Google OIDC client secret (provisioned here, never in config/git) ──────────────
# When the config enables Google federation, only the (non-secret) client_id is in config; the
# client SECRET is provisioned into Secrets Manager at the derived name `biffix/<tenant>/google-oidc`
# in THIS (post-assume-role) account, which is exactly where IdentityStack reads it. If the secret
# is already there, reuse it; otherwise prompt once (input hidden, never in argv/history/git), or
# take $BIFFIX_GOOGLE_CLIENT_SECRET for non-interactive/CI use. A dry run only looks.
if [ -n "$SECRET_NAME" ]; then
  if aws secretsmanager describe-secret --secret-id "$SECRET_NAME" --region "$REGION" >/dev/null 2>&1; then
    log "Google client secret present ($SECRET_NAME in $CALLER_ACCOUNT) — reusing"
  elif [ "$DRY_RUN" -eq 1 ]; then
    log "dry run: Google client secret $SECRET_NAME is missing; a real deploy would create it"
  else
    secret_value="${BIFFIX_GOOGLE_CLIENT_SECRET:-}"
    if [ -z "$secret_value" ]; then
      [ -t 0 ] || die "Google federation enabled but $SECRET_NAME is missing and stdin is not a TTY; set BIFFIX_GOOGLE_CLIENT_SECRET"
      printf '\033[1;36m[deploy]\033[0m Google OAuth client secret for %s (input hidden): ' "$SECRET_NAME"
      read -rs secret_value; printf '\n'
      [ -n "$secret_value" ] || die "empty secret — aborting"
    fi
    # Pass via a 0600 temp file so the value never appears in argv (visible in `ps`).
    new_temp tmp; chmod 600 "$tmp"; printf '%s' "$secret_value" > "$tmp"; unset secret_value
    aws secretsmanager create-secret --name "$SECRET_NAME" --region "$REGION" \
      --description "Google OAuth client secret ($SECRET_NAME)" \
      --secret-string "file://$tmp" >/dev/null || { rm -f "$tmp"; die "failed to create $SECRET_NAME"; }
    rm -f "$tmp"
    log "created Google client secret $SECRET_NAME in $CALLER_ACCOUNT"
  fi
fi

# ── ACM certificate for the custom app domain (provisioned here, DNS-validated) ────
# When app_domain is set but app_certificate_arn is NOT, provision the cert the CloudFront alias
# needs. CloudFront certs MUST live in us-east-1 regardless of the deploy region, and we do NOT
# create it in-stack: an in-stack Certificate makes CloudFormation block opaquely on DNS validation.
# So the script owns it — reuse an ISSUED cert, else request one, surface the DNS validation record,
# and wait for issuance (you publish that record at your DNS provider). The resolved
# ARN is injected into the deploy via context; a cert ARN already set in config wins (skips this).
# A dry run reuses an ISSUED cert but never requests one or waits.
APP_CERT_ARN=""
# The ARN of the app domain's cert in status $1, or nothing. A failed lookup stops the deploy: it
# is not "no certificate" (which would request a second one).
acm_cert() {
  local arn
  arn="$(aws acm list-certificates --region us-east-1 --certificate-statuses "$1" \
    --query "CertificateSummaryList[?DomainName=='$APP_DOMAIN'].CertificateArn | [0]" --output text)" \
    || die "could not list the $1 ACM certificates in us-east-1 (see the AWS error above)"
  [ "$arn" = "None" ] || printf '%s' "$arn"
}
if [ -n "$APP_DOMAIN" ] && [ -z "$APP_CERT_CFG" ]; then
  acm_region="us-east-1"
  issued="$(acm_cert ISSUED)"
  if [ -n "$issued" ]; then
    APP_CERT_ARN="$issued"
    log "reusing ISSUED ACM cert for $APP_DOMAIN: $APP_CERT_ARN"
  elif [ "$DRY_RUN" -eq 1 ]; then
    log "dry run: no ISSUED ACM cert for $APP_DOMAIN; synthesizing without the CloudFront alias"
  else
    APP_CERT_ARN="$(acm_cert PENDING_VALIDATION)"
    if [ -z "$APP_CERT_ARN" ]; then
      APP_CERT_ARN="$(aws acm request-certificate --region "$acm_region" --domain-name "$APP_DOMAIN" \
        --validation-method DNS --query CertificateArn --output text)" || die "acm request-certificate failed"
      log "requested ACM cert for $APP_DOMAIN: $APP_CERT_ARN"
    else
      log "resuming pending ACM cert for $APP_DOMAIN: $APP_CERT_ARN"
    fi
    # ACM fills the validation record in shortly after the request: wait for it (~1 min).
    rec=""
    for _ in $(seq 1 12); do
      rec="$(aws acm describe-certificate --region "$acm_region" --certificate-arn "$APP_CERT_ARN" \
        --query 'Certificate.DomainValidationOptions[0].ResourceRecord.[Name,Type,Value]' --output text)" \
        || die "could not describe ACM cert $APP_CERT_ARN (see the AWS error above)"
      [ -n "$rec" ] && [ "$rec" != "None" ] && break
      rec=""
      sleep 5
    done
    [ -n "$rec" ] || die "ACM has not published the DNS validation record of $APP_CERT_ARN yet — re-run shortly"
    warn "ACM DNS validation record — add this CNAME at your DNS provider (not proxied):"
    warn "    $rec"
    log "waiting for the cert to validate (polling up to ~30 min; add the record above)…"
    st=""
    for _ in $(seq 1 120); do
      st="$(aws acm describe-certificate --region "$acm_region" --certificate-arn "$APP_CERT_ARN" \
        --query 'Certificate.Status' --output text)" \
        || die "could not read the status of ACM cert $APP_CERT_ARN (see the AWS error above)"
      case "$st" in
        ISSUED) break ;;
        FAILED|VALIDATION_TIMED_OUT|REVOKED)
          die "ACM cert $APP_CERT_ARN is $st; it will not be issued (delete it, then re-run)" ;;
      esac
      sleep 15
    done
    [ "$st" = "ISSUED" ] || die "cert $APP_CERT_ARN not ISSUED (status=$st) — add the DNS record and re-run"
    log "ACM cert ISSUED: $APP_CERT_ARN"
  fi
fi

# ── Fetch the release (--release) ────────────────────────────────────────────────
# Downloaded and verified against its SHA256SUMS by biffix_infra.release, then handed to the CDK app
# as BIFFIX_RELEASE_DIR, which makes it read the Lambda zips and the UI from there (paths.py).
RELEASE_DIR=""
if [ -n "$RELEASE" ]; then
  RELEASE_DIR="$INFRA_DIR/.release/$RELEASE"
  log "fetching release $RELEASE"
  ( cd "$INFRA_DIR" && uv run --quiet python -m biffix_infra.release fetch --tag "$RELEASE" \
      --dest "$RELEASE_DIR" ) || die "could not fetch release $RELEASE (see the error above)"
  export BIFFIX_RELEASE_DIR="$RELEASE_DIR"
fi

# ── Build the frontend ───────────────────────────────────────────────────────────
# A real SvelteKit static build has its hashed assets under _app/; a bare index.html (e.g. a
# synth placeholder) would deploy a blank site, so it is not accepted as a build.
ui_build_ok() { [ -f "$REPO_ROOT/ui/build/index.html" ] && [ -d "$REPO_ROOT/ui/build/_app" ]; }
if [ -n "$RELEASE" ]; then
  log "using the release's UI build"
elif [ "$SKIP_UI" -eq 0 ]; then
  log "building the frontend (ui/)"
  ( cd "$REPO_ROOT/ui"
    npm ci
    # The UI ships with the backend built from this same tree, so its API types come from it too.
    npm run generate:api
    npm run build )
  ui_build_ok || die "ui build produced no index.html + _app/ assets"
else
  log "skipping UI build (--skip-ui)"
  ui_build_ok || die "--skip-ui set but ui/build is not a real UI build (needs index.html and _app/)"
fi

# ── Build the Rust Lambdas ───────────────────────────────────────────────────────
# All of them, always: every binary links biffix-core, so the stale-artifact guard (lambda_artifacts
# .py) rejects the deploy if any bootstrap predates a core change. Cheaper to always rebuild.
if [ -n "$RELEASE" ]; then
  log "using the release's Lambdas"
elif [ "$SKIP_LAMBDAS" -eq 0 ]; then
  command -v cargo-lambda >/dev/null || die "cargo-lambda not found — 'cargo install cargo-lambda'"
  log "building the Rust Lambdas (backend, sweep, ingest, reconcile)"
  ( cd "$REPO_ROOT"
    cargo lambda build --release --features aws --bin biffix-backend-lambda
    cargo lambda build --release --features aws --bin biffix-backend-sweep
    cargo lambda build --release --features aws --bin biffix-ingest-lambda
    cargo lambda build --release --features aws --bin biffix-reconcile-lambda
    # Cargo leaves a binary whose inputs did not change as it was, older than a Cargo.lock that
    # changed only for another binary; the guard compares times, so mark the ones just checked.
    touch "$TARGET_DIR"/lambda/biffix-{backend-lambda,backend-sweep,ingest-lambda,reconcile-lambda}/bootstrap )
else
  log "skipping Lambda build (--skip-lambdas)"
fi

# ── Host AWS CLI ─────────────────────────────────────────────────────────────────
# Every Batch host installs the AWS CLI pinned in config (host_aws_cli) from the reference bucket at
# boot (compute stack launch template), so stage the per-arch zips there: downloaded from AWS once,
# checked against the pinned sha256, uploaded under host/aws-cli/<version>/. Objects already there
# are left alone (hosts verify the checksum themselves). Returns 1 when the data stack (and so the
# reference bucket) does not exist yet; dies when the lookup itself fails.
stage_host_aws_cli() {
  local bucket zip arch sha key tmp actual
  bucket="$(stack_resource "$DATA_STACK" AWS::S3::Bucket ReferenceBucket)" \
    || die "could not find the reference bucket in $DATA_STACK"
  [ -n "$bucket" ] || return 1
  for zip in $HOST_AWS_CLI_ZIPS; do
    arch="${zip%%:*}"; sha="${zip#*:}"; key="${sha#*:}"; sha="${sha%%:*}"
    if aws s3api head-object --bucket "$bucket" --key "$key" >/dev/null 2>&1; then
      continue
    fi
    new_temp tmp -t biffix-awscli.XXXXXX
    curl -fsSL "https://awscli.amazonaws.com/awscli-exe-linux-${arch}-${HOST_AWS_CLI_VERSION}.zip" \
      -o "$tmp" || { rm -f "$tmp"; die "could not download AWS CLI ${HOST_AWS_CLI_VERSION} (${arch})"; }
    actual="$(shasum -a 256 "$tmp" | cut -d' ' -f1)"
    [ "$actual" = "$sha" ] \
      || { rm -f "$tmp"; die "AWS CLI ${HOST_AWS_CLI_VERSION} (${arch}) sha256 is $actual, config pins $sha"; }
    aws s3 cp "$tmp" "s3://${bucket}/${key}" --only-show-errors \
      || { rm -f "$tmp"; die "could not upload s3://${bucket}/${key}"; }
    rm -f "$tmp"
    log "staged host AWS CLI s3://${bucket}/${key}"
  done
}

# ── Deploy (or synth) ────────────────────────────────────────────────────────────
export JSII_SILENCE_WARNING_UNTESTED_NODE_VERSION=1
CDK_ARGS=(-c "configFile=$CONFIG")
# The cert ARN deploy.sh just provisioned for a custom-domain deploy (app.py applies it over config).
[ -n "${APP_CERT_ARN:-}" ] && CDK_ARGS+=(-c "appCertificateArn=$APP_CERT_ARN")
if [ -n "$STACK" ]; then
  CDK_TARGET=("$STACK" --exclusively)
  TARGET_STACKS="$STACK"
else
  CDK_TARGET=(--all)
  TARGET_STACKS="$ALL_STACKS"
fi

# ── Import the release's coordinator image (--release) ─────────────────────────────
# Copied by digest into the data stack's own CoordinatorRepo (never a name match) under the release
# tag: the backend resolves the deployed base there, the runtime builds layer onto it there, and
# image retention protects it there. biffix_infra.release checks the tag then names exactly the
# digest the release recorded. Returns 1 when the data stack (and so the repository) does not
# exist yet.
import_coordinator() {
  local repo registry digest
  repo="$(stack_resource "$DATA_STACK" AWS::ECR::Repository CoordinatorRepo)" \
    || die "could not find the coordinator repo in $DATA_STACK"
  [ -n "$repo" ] || return 1
  registry="${CALLER_ACCOUNT}.dkr.ecr.${REGION}.amazonaws.com"
  aws ecr get-login-password --region "$REGION" \
    | crane auth login "$registry" -u AWS --password-stdin >/dev/null \
    || die "could not log crane in to $registry"
  digest="$(cd "$INFRA_DIR" && uv run --quiet python -m biffix_infra.release import-image \
    --release-dir "$RELEASE_DIR" --target "$registry/$repo:$RELEASE")" \
    || die "could not import the release's coordinator image (see the error above)"
  log "coordinator image: $registry/$repo:$RELEASE = $digest"
}

if [ "$DRY_RUN" -eq 1 ]; then
  if [ -n "$RELEASE" ]; then
    image="$(cd "$INFRA_DIR" && uv run --quiet python -m biffix_infra.release describe \
      --release-dir "$RELEASE_DIR")" || die "could not read the release's coordinator image"
    log "dry run: a real deploy would import $image as the coordinator image $RELEASE"
  fi
  log "dry run: cdk synth ${CDK_TARGET[*]}"
  ( cd "$INFRA_DIR" && "$CDK" synth "${CDK_TARGET[@]}" "${CDK_ARGS[@]}" >/dev/null )
  log "synth ok (dry run: nothing in AWS was changed)"
  exit 0
fi

# Before the deploy when the reference bucket already exists, so no host booted from the new launch
# template looks for a zip that is not there yet; a first deploy stages right after `cdk deploy`.
stage_host_aws_cli || log "no reference bucket yet (first deploy); staging the host AWS CLI after the deploy"
# Before the deploy too when the repository exists, so the backend the deploy updates finds the
# image its new tag names; a first deploy imports right after `cdk deploy` creates the repository.
IMPORTED=0
if [ -n "$RELEASE" ]; then
  if import_coordinator; then
    IMPORTED=1
  else
    log "no coordinator repository yet (first deploy); importing the image after the deploy"
  fi
fi

log "deploying: cdk deploy ${CDK_TARGET[*]}"
deploy_rc=0
cdk_log=""
new_temp cdk_log -t biffix-cdk-deploy.XXXXXX
( cd "$INFRA_DIR" && "$CDK" deploy "${CDK_TARGET[@]}" "${CDK_ARGS[@]}" --require-approval never ) 2>&1 \
  | tee "$cdk_log" || deploy_rc=${PIPESTATUS[0]}
if [ "$deploy_rc" -ne 0 ]; then
  # A non-zero cdk exit is a failure unless there is POSITIVE evidence that every targeted stack
  # deployed: the CDK CLI has exited non-zero on advisory warnings after a fully successful deploy,
  # but "every stack is still *_COMPLETE" is also what a deploy that failed before touching
  # CloudFormation (synth, asset publishing, bootstrap, change-set creation) looks like. So require
  # cdk's own per-stack success line for every target, no failure marker, and a clean stack state.
  # Searched from a here-string, never `printf | grep -q`: under pipefail, grep quitting at an
  # early match kills the printf of a long log (SIGPIPE) and fails the pipeline.
  plain="$(sed $'s/\x1b\\[[0-9;]*[A-Za-z]//g' "$cdk_log")"
  if grep -q "Subprocess exited with error" <<< "$plain"; then
    die "cdk deploy failed: the CDK app did not synthesize (nothing was deployed; see the error above)"
  fi
  if grep -q "❌" <<< "$plain"; then
    die "cdk deploy failed (exit $deploy_rc; see the ❌ lines above)"
  fi
  for s in $TARGET_STACKS; do
    grep -Eq "✅ +${s}( \\(no changes\\))?[[:space:]]*\$" <<< "$plain" \
      || die "cdk deploy exited $deploy_rc without reporting $s as deployed — treating it as a failure"
    status="$(aws cloudformation describe-stacks --region "$REGION" --stack-name "$s" \
      --query 'Stacks[0].StackStatus' --output text)" \
      || die "cdk deploy exited $deploy_rc and the state of $s could not be read"
    case "$status" in
      CREATE_COMPLETE|UPDATE_COMPLETE) ;;
      *) die "cdk deploy failed — $s is $status" ;;
    esac
  done
  warn "cdk deploy exited $deploy_rc but reported every targeted stack deployed and each is CREATE/UPDATE_COMPLETE — treating the exit as the CDK-CLI advisory-warning quirk"
fi

stage_host_aws_cli \
  || die "no reference bucket in $DATA_STACK: hosts cannot install the AWS CLI (re-run once the data stack exists)"
if [ -n "$RELEASE" ] && [ "$IMPORTED" -eq 0 ]; then
  import_coordinator \
    || die "no coordinator repository in $DATA_STACK: runs cannot launch (re-run once the data stack exists)"
fi

# ── Image retention ──────────────────────────────────────────────────────────────
# Tag the deployed base coordinator image (and what its index lists) so it is never untagged, and
# remove old build tags (biffix_infra.image_retention). The repo's lifecycle only expires untagged
# images, so this is what keeps it from growing; coordinator variants (`coordinator-*`, built by the
# backend) and import keep-tags are other tags, never pruned here. Best-effort: a failure here
# leaves the deploy itself good, so it warns rather than fails. The repo is the data stack's own
# CoordinatorRepo, never a name match (a retained repo from an earlier deploy shares the prefix).
retain_images() {
  local repo
  if ! repo="$(stack_resource "$DATA_STACK" AWS::ECR::Repository CoordinatorRepo)"; then
    warn "image retention: could not resolve the coordinator repo; skipped"
    return 0
  fi
  if [ -z "$repo" ]; then
    warn "image retention: no coordinator repo in $DATA_STACK; skipped"
    return 0
  fi
  ( cd "$INFRA_DIR" && uv run python -m biffix_infra.image_retention --repo "$repo" \
      --deployed-tag "$COORD_TAG" \
      --protect-prefix "deployed-${TENANT}-" ) \
    || warn "image retention for $repo failed (the deploy itself is fine)"
}
if [ -z "$STACK" ]; then
  log "image retention: protecting deployed images, pruning old build tags"
  retain_images
else
  log "image retention: skipped (--stack deploys one stack; a full deploy runs it)"
fi

# ── Verify ───────────────────────────────────────────────────────────────────────
if [ "$NO_VERIFY" -eq 1 ]; then
  log "skipping verify (--no-verify)"
  exit 0
fi

# Resolve the API base: explicit --url wins; else the edge stack's UiUrl output; else the config's
# app_domain. All three yield "<origin>/api". Returns 1 when the stack lookup itself fails.
resolve_origin() {
  if [ -n "$VERIFY_URL" ]; then printf '%s' "$VERIFY_URL"; return; fi
  local origin errf
  # The UiUrl output is the `edge` stack's.
  errf="$(mktemp)"
  if origin="$(aws cloudformation describe-stacks --region "$REGION" --stack-name "$EDGE_STACK" \
      --query "Stacks[0].Outputs[?OutputKey=='UiUrl'].OutputValue | [0]" --output text 2>"$errf")"; then
    rm -f "$errf"
    if [ -n "$origin" ] && [ "$origin" != "None" ]; then printf '%s/api' "$origin"; return; fi
  elif grep -q "does not exist" "$errf"; then
    rm -f "$errf"
  else
    printf 'could not read the %s outputs: %s\n' "$EDGE_STACK" "$(cat "$errf")" >&2
    rm -f "$errf"
    return 1
  fi
  [ -z "$APP_DOMAIN" ] || printf 'https://%s/api' "$APP_DOMAIN"
}

API_BASE="$(resolve_origin)" || die "could not resolve the API URL to verify"
if [ -z "$API_BASE" ]; then
  warn "could not resolve an API URL to verify; pass --url to enable the health check"
  exit 0
fi

log "verifying: biffix health --url $API_BASE"
# Prefer an installed biffix; else build the release binary from this tree. A release deploy may
# have no Rust toolchain: then it skips the check rather than failing a good deploy.
if command -v biffix >/dev/null; then
  BIFFIX=biffix
elif [ -n "$RELEASE" ] && ! command -v cargo >/dev/null; then
  warn "no biffix CLI on PATH and no cargo to build one: skipping the health check (run 'biffix health --url $API_BASE')"
  exit 0
else
  log "building the biffix CLI for the health check"
  ( cd "$REPO_ROOT" && cargo build --release --bin biffix ) >&2 \
    || die "could not build the biffix CLI for the health check (see the cargo output above)"
  BIFFIX="$TARGET_DIR/release/biffix"
fi
# A cold Lambda can time out the first request: retry for ~2 min before failing a good deploy.
attempts=6
for attempt in $(seq 1 "$attempts"); do
  "$BIFFIX" health --url "$API_BASE" && break
  [ "$attempt" -lt "$attempts" ] || die "post-deploy health check failed after $attempts attempts"
  warn "health check attempt $attempt/$attempts failed; retrying in 20 s"
  sleep 20
done
log "deploy complete and healthy"
