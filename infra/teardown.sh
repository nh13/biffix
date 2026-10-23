#!/usr/bin/env bash
# Tear a biffix deployment down — the inverse of deploy.sh, for one tenant in one account.
#
# Why this exists as a script: a clean teardown is not just `cdk destroy`. Three things bite you,
# and this encodes the fix for each:
#   1. Stateful stacks carry termination protection; CloudFormation refuses to delete them until it
#      is cleared.
#   2. The Aurora event store is `RemovalPolicy.RETAIN` + deletion-protection (PersistenceStack), so
#      `cdk destroy` deliberately leaves the cluster behind — and the *retained* cluster still holds
#      the persistence stack's security group, so deleting that stack then fails with a
#      DependencyViolation on the SG. The cluster must be deleted FIRST (when data is disposable).
#   3. The data/reference/UI buckets are retained too, and the reference bucket is versioned, so a
#      plain `aws s3 rm` cannot empty it and the bucket lingers.
#
# What it deletes is found EXACTLY, never by a name prefix (tenant `acme` must not touch tenant
# `acme-dev`): the stacks the CDK app names for this config, the buckets and Aurora clusters those
# stacks own, and buckets/clusters tagged `biffix:tenant=<tenant>` (which finds the ones an earlier,
# partial teardown left behind). A project's imported bucket (`bucket_name:`) is never touched.
#
# Usage:
#   infra/teardown.sh --config <file> --account <id> [options]
#
# Options:
#   -c, --config <file>     REQUIRED. The deploy config of the tenant to tear down (it selects the
#                           tenant and its stacks). A relative path is looked up in infra/ first,
#                           then in the current directory. Read through biffix_infra.config (never
#                           parsed here).
#   -a, --account <id>      REQUIRED. Abort unless the (post-assume) caller account equals this id
#                           (and the config's `account:`, when set). Destruction must never guess
#                           the account; the AWS Organizations management account is always refused.
#       --assume-role <arn> Act as this role (e.g. an org member account's
#                           OrganizationAccountAccessRole), then apply the guard. Its session is
#                           renewed as it nears expiry, so a teardown may outlast the hour.
#       --keep-data         Destroy the stacks but KEEP the retained Aurora cluster and S3 buckets,
#                           and the persistence and network stacks a kept cluster needs. (Default
#                           deletes them — a full teardown, since they are otherwise orphans.)
#       --delete-image-archive
#                           Also purge and delete the image archive bucket. By default it is kept:
#                           it is the byte-for-byte copy of every pinned workflow image, meant to
#                           outlive an account rebuild (and it may be Object-Locked).
#       --yes               Do not prompt for confirmation (non-interactive / CI).
#   -h, --help              Show this help.
#
# Not deleted (retained by design; the script lists them at the end): the ECR repositories and
# their images, the Cognito user pool, KMS keys, and the capability and database secrets.
#
# Prereqs: node deps installed in infra/ (`npm ci`), uv, aws CLI, and credentials for the target
# account already available (or reachable via --assume-role). Runs on bash 3.2 (macOS) or newer.
set -euo pipefail

# ── Resolve paths ────────────────────────────────────────────────────────────────
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
INFRA_DIR="$SCRIPT_DIR"

# ── Defaults ─────────────────────────────────────────────────────────────────────
CONFIG=""
ACCOUNT=""
ASSUME_ROLE=""
KEEP_DATA=0
DELETE_ARCHIVE=0
ASSUME_YES=0

log()  { printf '\033[1;36m[teardown]\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33m[teardown]\033[0m %s\n' "$*" >&2; }
die()  { printf '\033[1;31m[teardown] error:\033[0m %s\n' "$*" >&2; exit 1; }

usage() { sed -n '2,/^set -euo/p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//; /^set -euo/d'; }

# shellcheck source=SCRIPTDIR/script_lib.sh
. "$INFRA_DIR/script_lib.sh"

# ── Parse args ───────────────────────────────────────────────────────────────────
while [ $# -gt 0 ]; do
  case "$1" in
    -c|--config)   CONFIG="$(optarg "$@")"; shift 2 ;;
    -a|--account)  ACCOUNT="$(optarg "$@")"; shift 2 ;;
    --assume-role) ASSUME_ROLE="$(optarg "$@")"; shift 2 ;;
    --keep-data)   KEEP_DATA=1; shift ;;
    --delete-image-archive) DELETE_ARCHIVE=1; shift ;;
    --yes)         ASSUME_YES=1; shift ;;
    -h|--help)     usage; exit 0 ;;
    *)             die "unknown option: $1 (see --help)" ;;
  esac
done

require_deploy_tools
[ -n "$ACCOUNT" ] || die "--account <id> is required for teardown (refusing to guess the target account)"

# ── Read the config (through biffix_infra.config, exactly as the CDK app reads it) ─
CONFIG="$(resolve_config "$CONFIG")"
load_config "$CONFIG" tenant region account stack_names data_stack imported_buckets \
  network_stack persistence_stack
TENANT="$(cfg tenant)"
REGION="$(cfg region)"
CONFIG_ACCOUNT="$(cfg account)"
ALL_STACKS="$(cfg stack_names)"
DATA_STACK="$(cfg data_stack)"
IMPORTED_BUCKETS="$(cfg imported_buckets)"
NETWORK_STACK="$(cfg network_stack)"
PERSISTENCE_STACK="$(cfg persistence_stack)"
[ -n "$TENANT" ] && [ -n "$REGION" ] || die "$CONFIG: tenant and region must be set"
export AWS_REGION="$REGION" AWS_DEFAULT_REGION="$REGION"

# The temp files this run makes (--assume-role's AWS config, the placeholder UI build), removed
# however it exits.
UI_STUB_DIR=""
trap '[ -z "$ROLE_CONFIG" ] || rm -f "$ROLE_CONFIG"; [ -z "$UI_STUB_DIR" ] || rm -rf "$UI_STUB_DIR"' EXIT

# ── Assume role (optional) ───────────────────────────────────────────────────────
[ -z "$ASSUME_ROLE" ] || assume_role "$ASSUME_ROLE" biffix-teardown

# ── Account guard (REQUIRED — destruction never guesses the account) ──────────────
guard_account "$ACCOUNT" "$CONFIG_ACCOUNT"

# ── Helpers ────────────────────────────────────────────────────────────────────────
# Space-separated sets (names carry no spaces); plain strings keep this bash 3.2 compatible.
contains() { case " $1 " in *" $2 "*) return 0 ;; esac; return 1; }
add() { contains "$1" "$2" && printf '%s' "$1" || printf '%s' "${1:+$1 }$2"; }

# Every live (not DELETE_COMPLETE) stack in the region, space-separated.
all_live_stacks() {
  local out
  out="$(aws cloudformation list-stacks --region "$REGION" \
    --query "StackSummaries[?StackStatus!='DELETE_COMPLETE'].StackName" --output text)" \
    || die "could not list CloudFormation stacks"
  printf '%s' "$out" | tr '\t\n' '  '
}

# The stacks among $1 that are this config's (by exact name).
ours() {
  local s live=""
  for s in $1; do
    if contains "$ALL_STACKS" "$s"; then live="$(add "$live" "$s")"; fi
  done
  printf '%s' "$live"
}

# ── Discover what we would destroy ────────────────────────────────────────────────
LIVE_STACKS="$(all_live_stacks)"
STACKS="$(ours "$LIVE_STACKS")"
CLUSTERS=""
BUCKETS=""
ARCHIVE_BUCKETS=""
RETAINED=""
for s in $STACKS; do
  out="$(aws cloudformation list-stack-resources --region "$REGION" --stack-name "$s" \
    --query "StackResourceSummaries[].[ResourceType,LogicalResourceId,PhysicalResourceId]" --output text)" \
    || die "could not list the resources of $s"
  while IFS=$'\t' read -r type logical phys; do
    [ -n "${phys:-}" ] && [ "$phys" != "None" ] || continue
    case "$type" in
      AWS::RDS::DBCluster) CLUSTERS="$(add "$CLUSTERS" "$phys")" ;;
      AWS::S3::Bucket)
        BUCKETS="$(add "$BUCKETS" "$phys")"
        case "$s:$logical" in
          "$DATA_STACK:ImageArchiveBucket"*) ARCHIVE_BUCKETS="$(add "$ARCHIVE_BUCKETS" "$phys")" ;;
        esac ;;
      AWS::ECR::Repository|AWS::Cognito::UserPool|AWS::KMS::Key|AWS::SecretsManager::Secret)
        RETAINED="${RETAINED:+$RETAINED
}    $type $phys" ;;
    esac
  done <<< "$out"
done
# Retained buckets and clusters whose stack is already gone carry the tenant tag (app_graph.py).
# The image archive is told apart by its `biffix:role` tag (DataStack), never by its generated name,
# which CloudFormation truncates for a longer tenant; one from before that tag is told apart by the
# stack and logical id CloudFormation tagged it with.
# shellcheck disable=SC2016 # the backticks are JMESPath literals, not shell expansions
tagged="$(aws resourcegroupstaggingapi get-resources --region "$REGION" \
  --tag-filters "Key=biffix:tenant,Values=$TENANT" --resource-type-filters s3 rds:cluster \
  --query 'ResourceTagMappingList[].[ResourceARN, Tags[?Key==`biffix:role`].Value | [0], Tags[?Key==`aws:cloudformation:stack-name`].Value | [0], Tags[?Key==`aws:cloudformation:logical-id`].Value | [0]]' \
  --output text)" \
  || die "could not list resources tagged biffix:tenant=$TENANT"
while IFS=$'\t' read -r arn role stack logical; do
  # A live stack this config does not name (a project since removed from the config) still owns
  # its resources: leave them to that stack.
  if [ -n "$arn" ] && [ "$stack" != "None" ] && ! contains "$ALL_STACKS" "$stack" \
      && contains "$LIVE_STACKS" "$stack"; then
    warn "leaving ${arn##*[:/]}: its stack $stack is live but not in this config (a project removed from it?); delete that stack first"
    continue
  fi
  case "$arn" in
    arn:*:s3:::*)
      b="${arn#*:::}"
      BUCKETS="$(add "$BUCKETS" "$b")"
      case "$role:$stack:$logical" in
        image-archive:*|"None:$DATA_STACK:ImageArchiveBucket"*)
          ARCHIVE_BUCKETS="$(add "$ARCHIVE_BUCKETS" "$b")" ;;
      esac ;;
    arn:*:rds:*:cluster:*) CLUSTERS="$(add "$CLUSTERS" "${arn##*:cluster:}")" ;;
  esac
done <<< "$tagged"
# Belt and braces: an imported project bucket is not biffix's to delete.
kept=""
for b in $BUCKETS; do
  contains "$IMPORTED_BUCKETS" "$b" || kept="$(add "$kept" "$b")"
done
BUCKETS="$kept"

if [ -z "$STACKS$CLUSTERS$BUCKETS" ]; then
  log "no stacks, buckets or Aurora clusters of tenant '${TENANT}' in ${CALLER_ACCOUNT}/${REGION} — nothing to tear down"
  exit 0
fi

# A kept Aurora cluster holds the persistence stack's security group and sits in the network stack's
# subnets, so CloudFormation can delete neither while it lives: --keep-data keeps both stacks too
# (a later full teardown deletes the cluster, then them).
KEPT_STACKS=""
DESTROY_STACKS="$STACKS"
if [ "$KEEP_DATA" -eq 1 ] && [ -n "$CLUSTERS" ]; then
  DESTROY_STACKS=""
  for s in $STACKS; do
    case "$s" in
      "$NETWORK_STACK"|"$PERSISTENCE_STACK") KEPT_STACKS="$(add "$KEPT_STACKS" "$s")" ;;
      *) DESTROY_STACKS="$(add "$DESTROY_STACKS" "$s")" ;;
    esac
  done
fi

log "tenant '${TENANT}' in ${CALLER_ACCOUNT}/${REGION} — will destroy:"
for s in $DESTROY_STACKS; do printf '    stack   %s\n' "$s"; done
for s in $KEPT_STACKS; do printf '    stack   %s (KEPT: it holds the kept Aurora cluster)\n' "$s"; done
if [ "$KEEP_DATA" -eq 0 ]; then
  for cl in $CLUSTERS; do printf '    AURORA  %s (DATA DELETED)\n' "$cl"; done
  for b in $BUCKETS; do
    if contains "$ARCHIVE_BUCKETS" "$b" && [ "$DELETE_ARCHIVE" -eq 0 ]; then
      printf '    bucket  %s (image archive: KEPT; pass --delete-image-archive to delete it)\n' "$b"
    else
      printf '    bucket  %s (DATA DELETED)\n' "$b"
    fi
  done
else
  warn "--keep-data: the Aurora cluster and S3 buckets will be KEPT (stacks only)"
fi

if [ "$ASSUME_YES" -eq 0 ]; then
  printf '\033[1;31m[teardown]\033[0m Type the tenant name (%s) to confirm destruction: ' "$TENANT"
  read -r reply || die "no confirmation could be read from stdin; pass --yes to tear down non-interactively"
  [ "$reply" = "$TENANT" ] || die "confirmation did not match — aborting"
fi

# ── Clear termination protection on every stack to destroy ─────────────────────────
log "clearing termination protection"
for s in $DESTROY_STACKS; do
  aws cloudformation update-termination-protection --region "$REGION" --stack-name "$s" \
    --no-enable-termination-protection >/dev/null 2>&1 || true
done

# ── Delete the RETAIN'd Aurora FIRST (unless --keep-data) ───────────────────────────
# Must precede `cdk destroy`: the retained cluster holds the persistence stack's security group, and
# CloudFormation cannot delete that SG (DependencyViolation) while the cluster lives, failing the
# stack. Delete instances then the cluster, skipping the final snapshot (teardown = data disposable).
# The protection toggles are idempotent and may fail benignly; a delete that does not end with the
# resource gone stops the teardown with the AWS error.
delete_cluster() {
  local cl="$1" members m errf
  errf="$(mktemp)"
  if ! members="$(aws rds describe-db-clusters --region "$REGION" --db-cluster-identifier "$cl" \
      --query 'DBClusters[0].DBClusterMembers[].DBInstanceIdentifier' --output text 2>"$errf")"; then
    if grep -q DBClusterNotFound "$errf"; then
      rm -f "$errf"; log "  Aurora cluster $cl already gone"; return 0
    fi
    die "could not describe Aurora cluster $cl: $(cat "$errf")"
  fi
  [ "$members" = "None" ] && members=""
  log "deleting Aurora cluster $cl (instances first)"
  aws rds modify-db-cluster --region "$REGION" --db-cluster-identifier "$cl" \
    --no-deletion-protection --apply-immediately >/dev/null 2>&1 || true
  for m in $members; do
    aws rds modify-db-instance --region "$REGION" --db-instance-identifier "$m" \
      --no-deletion-protection --apply-immediately >/dev/null 2>&1 || true
    # A failure here may just be an instance already being deleted; the wait decides.
    aws rds delete-db-instance --region "$REGION" --db-instance-identifier "$m" \
      --skip-final-snapshot --delete-automated-backups >/dev/null 2>"$errf" \
      || warn "  delete-db-instance $m: $(cat "$errf")"
  done
  for m in $members; do
    aws rds wait db-instance-deleted --region "$REGION" --db-instance-identifier "$m" \
      || die "Aurora instance $m was not deleted (see the errors above)"
  done
  aws rds delete-db-cluster --region "$REGION" --db-cluster-identifier "$cl" --skip-final-snapshot \
    >/dev/null 2>"$errf" || warn "  delete-db-cluster $cl: $(cat "$errf")"
  rm -f "$errf"
  aws rds wait db-cluster-deleted --region "$REGION" --db-cluster-identifier "$cl" \
    || die "Aurora cluster $cl was not deleted (see the errors above)"
  log "  Aurora cluster $cl deleted"
}
if [ "$KEEP_DATA" -eq 0 ]; then
  for cl in $CLUSTERS; do delete_cluster "$cl"; done
fi

# ── cdk destroy (dependency order handled by CDK) ──────────────────────────────────
# Synth needs the same two escape hatches deploy.sh's non-lambda paths use: the stale-Lambda guard
# (crates may be unbuilt) and the edge stack's required UI build artifact. Neither is shipped by a
# destroy; they only let synth succeed. The placeholder UI lives in a temp directory (the app's
# `uiAssetPath`), never in the source tree, so a later deploy can never mistake it for a real build.
if [ -n "$DESTROY_STACKS" ]; then
  UI_STUB_DIR="$(mktemp -d)"
  printf '<!doctype html>\n' > "$UI_STUB_DIR/index.html"
  # Every stack, or (keeping some) exactly the others: cdk also destroys the stacks that depend on
  # a named one, and none depends only on the kept ones.
  if [ -z "$KEPT_STACKS" ]; then
    set -- --all
  else
    # shellcheck disable=SC2086 # word-split the space-separated stack names on purpose
    set -- $DESTROY_STACKS
  fi
  log "cdk destroy $*"
  ( cd "$INFRA_DIR" && JSII_SILENCE_WARNING_UNTESTED_NODE_VERSION=1 BIFFIX_ALLOW_UNBUILT_LAMBDA=1 \
      "$CDK" destroy "$@" --force -c "configFile=$CONFIG" -c "uiAssetPath=$UI_STUB_DIR" )
fi

# ── Sweep the retained buckets (unless --keep-data) ────────────────────────────────
# Version-aware: the reference bucket is versioned, so `aws s3 rm` leaves noncurrent versions and
# delete-markers behind and the bucket cannot be deleted until they are purged. Each round deletes
# at most 1000 versions (the delete-objects limit). A round that reports per-key errors (e.g. an
# Object Lock retention) or makes no progress stops that bucket; the verify step then reports it.
purge_bucket() {
  local b="$1" errf batch objs errs head prev=""
  errf="$(mktemp)"
  if ! aws s3api head-bucket --bucket "$b" >/dev/null 2>"$errf"; then
    if grep -qE "Not Found|404|NoSuchBucket" "$errf"; then
      rm -f "$errf"; log "bucket already gone: $b"; return 0
    fi
    warn "  cannot access bucket $b: $(cat "$errf")"; rm -f "$errf"; return 1
  fi
  log "purging + deleting bucket $b"
  # Each batch goes to delete-objects through a file: 1000 versions as one argument can exceed
  # Linux's 128 KiB limit on a single argv string. `--max-items 1000` alone caps a batch at
  # delete-objects' 1000 (the CLI pages through the listing until it has them); adding the API's own
  # `--max-keys` makes AWS CLI v2.35+ treat the call as manually paged and refuse --max-items.
  batch="$(mktemp)"
  while :; do
    # shellcheck disable=SC2016 # the backticks are a JMESPath literal, not a shell expansion
    if ! aws s3api list-object-versions --bucket "$b" --max-items 1000 \
        --query '{Objects: [Versions, DeleteMarkers][][].{Key:Key,VersionId:VersionId} | [:1000], Quiet: `true`}' \
        --output json >"$batch" 2>"$errf"; then
      warn "  could not list the versions in $b: $(cat "$errf")"; rm -f "$errf" "$batch"; return 1
    fi
    objs="$(tr -d '[:space:]' < "$batch")"
    case "$objs" in
      '{"Objects":[]'*|'{"Objects":null'*|'') break ;;
    esac
    head="$(printf '%s' "$objs" | cut -c1-512)"
    if [ "$head" = "$prev" ]; then
      warn "  no progress purging $b (the same versions came back); leaving it"
      rm -f "$errf" "$batch"; return 1
    fi
    prev="$head"
    if ! errs="$(aws s3api delete-objects --bucket "$b" --delete "file://$batch" \
        --query 'Errors[].[Code,Key,Message]' --output text 2>"$errf")"; then
      warn "  delete-objects failed on $b: $(cat "$errf")"; rm -f "$errf" "$batch"; return 1
    fi
    if [ -n "$errs" ] && [ "$errs" != "None" ]; then
      warn "  some versions in $b cannot be deleted (e.g. Object Lock retention); leaving it:"
      printf '%s\n' "$errs" | head -5 >&2
      rm -f "$errf" "$batch"; return 1
    fi
  done
  rm -f "$errf" "$batch"
  if aws s3api delete-bucket --bucket "$b" --region "$REGION" >/dev/null; then
    log "  deleted bucket: $b"
  else
    warn "  could not delete bucket: $b"; return 1
  fi
}
SWEPT=""
if [ "$KEEP_DATA" -eq 0 ]; then
  for b in $BUCKETS; do
    if contains "$ARCHIVE_BUCKETS" "$b" && [ "$DELETE_ARCHIVE" -eq 0 ]; then
      log "keeping the image archive bucket $b (pass --delete-image-archive to delete it)"
      continue
    fi
    SWEPT="$(add "$SWEPT" "$b")"
    purge_bucket "$b" || true
  done
fi

# ── Verify ─────────────────────────────────────────────────────────────────────────
# Only what this run set out to delete: every lookup is by exact name, and a failing lookup is an
# error, never "gone".
log "verifying the tenant is gone"
left_stacks=""
live="$(all_live_stacks)"
for s in $(ours "$live"); do
  contains "$KEPT_STACKS" "$s" || left_stacks="$(add "$left_stacks" "$s")"
done
left_rds=""
left_buckets=""
if [ "$KEEP_DATA" -eq 0 ]; then
  errf="$(mktemp)"
  for cl in $CLUSTERS; do
    if aws rds describe-db-clusters --region "$REGION" --db-cluster-identifier "$cl" >/dev/null 2>"$errf"; then
      left_rds="$(add "$left_rds" "$cl")"
    elif ! grep -q DBClusterNotFound "$errf"; then
      die "could not check Aurora cluster $cl: $(cat "$errf")"
    fi
  done
  for b in $SWEPT; do
    if aws s3api head-bucket --bucket "$b" >/dev/null 2>"$errf"; then
      left_buckets="$(add "$left_buckets" "$b")"
    elif ! grep -qE "Not Found|404|NoSuchBucket" "$errf"; then
      die "could not check bucket $b: $(cat "$errf")"
    fi
  done
  rm -f "$errf"
fi
if [ -n "$left_stacks$left_rds$left_buckets" ]; then
  warn "teardown incomplete — still present:"
  [ -z "$left_stacks" ]  || warn "  stacks:  $left_stacks"
  [ -z "$left_rds" ]     || warn "  aurora:  $left_rds"
  [ -z "$left_buckets" ] || warn "  buckets: $left_buckets"
  die "re-run, or clean the above up by hand"
fi
if [ -n "$RETAINED" ]; then
  log "retained by design (not deleted by teardown; remove by hand if the tenant is gone for good):"
  printf '%s\n' "$RETAINED"
fi
if [ -n "$KEPT_STACKS" ]; then
  log "stacks holding the kept Aurora cluster, kept: $KEPT_STACKS"
fi
if [ "$KEEP_DATA" -eq 1 ]; then
  log "tenant '${TENANT}' stacks torn down in ${CALLER_ACCOUNT}/${REGION} (--keep-data: Aurora and buckets kept)"
elif [ "$DELETE_ARCHIVE" -eq 0 ] && [ -n "$ARCHIVE_BUCKETS" ]; then
  log "tenant '${TENANT}' torn down in ${CALLER_ACCOUNT}/${REGION} (image archive kept: $ARCHIVE_BUCKETS)"
else
  log "tenant '${TENANT}' torn down in ${CALLER_ACCOUNT}/${REGION}"
fi
