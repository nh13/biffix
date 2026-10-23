# shellcheck shell=bash
# Shared by deploy.sh and teardown.sh (sourced, not run). The caller defines log/warn/die and sets
# INFRA_DIR before sourcing. Written for bash 3.2 (macOS /bin/bash) as well as newer bash.

# The value of option $1, which must be present and not look like another option.
optarg() {
  { [ $# -ge 2 ] && [ -n "$2" ] && [ "${2#-}" = "$2" ]; } || die "$1 requires a value (see --help)"
  printf '%s' "$2"
}

# Resolve a --config value to an absolute path, the way the CDK app does (app.py resolves a
# relative configFile against infra/), falling back to the current directory. Dies if neither has it.
# There is no default config: a deploy or teardown always names the tenant it acts on.
resolve_config() {
  local c="$1"
  [ -n "$c" ] \
    || die "no deploy config: pass --config <file> (copy infra/biffix-deploy.example.yaml to start one)"
  case "$c" in
    /*) [ -f "$c" ] || die "config not found: $c"; printf '%s' "$c"; return ;;
  esac
  if [ -f "$INFRA_DIR/$c" ]; then
    printf '%s/%s' "$INFRA_DIR" "$c"
  elif [ -f "$c" ]; then
    printf '%s/%s' "$(cd "$(dirname "$c")" && pwd)" "$(basename "$c")"
  else
    die "config not found: $c (looked in $INFRA_DIR and $(pwd))"
  fi
}

# Read config values through biffix_infra.config (biffix_infra.script_config), never by parsing
# the YAML here, so the scripts see exactly what the CDK app sees. `load_config <config> <key>...`
# loads them; `cfg <key>` prints one of them.
CFG_OUT=""
CFG_KEYS=""
load_config() {
  local config="$1"
  shift
  CFG_KEYS="$*"
  CFG_OUT="$(cd "$INFRA_DIR" && uv run --quiet python -m biffix_infra.script_config "$config" "$@")" \
    || die "could not read $config through biffix_infra.config (see the error above)"
}
# The value of key $1, which load_config must have loaded (by name, not by position).
cfg() {
  local key n=0
  for key in $CFG_KEYS; do
    n=$((n + 1))
    if [ "$key" = "$1" ]; then
      printf '%s\n' "$CFG_OUT" | sed -n "${n}p"
      return 0
    fi
  done
  die "cfg: $1 was not loaded"
}

# The tools deploy.sh and teardown.sh both run: the CDK CLI infra/'s `npm ci` installs (sets CDK),
# the aws CLI and uv.
require_deploy_tools() {
  CDK="$INFRA_DIR/node_modules/.bin/cdk"
  [ -x "$CDK" ] || die "cdk not found at $CDK — run 'npm ci' in infra/ first"
  command -v aws >/dev/null || die "aws CLI not found"
  command -v uv >/dev/null || die "uv not found"
}

# Assume role $1 (session name $2, at least 2 characters) for every later aws, cdk and boto3 call.
# A role session lasts an hour, and a teardown or a from-source deploy can outlast it, so rather
# than export one session's keys this points the SDKs at a profile that assumes the role itself and
# assumes it again whenever the session nears expiry. The profile lives in a private copy of the
# caller's config file (so the caller's own profiles still resolve; the shared credentials file
# stays where it is), named by ROLE_CONFIG, which the caller's EXIT trap removes. The renewal
# lasts only as long as the caller's own credentials do.
ASSUME_PROFILE=biffix-assume-role
ROLE_CONFIG=""
assume_role() {
  local arn="$1" session="$2" caller_config="${AWS_CONFIG_FILE:-$HOME/.aws/config}"
  local profile="${AWS_PROFILE:-${AWS_DEFAULT_PROFILE:-}}" src caller
  log "assuming role: $arn (profile $ASSUME_PROFILE, renewed as each session nears expiry)"
  if [ -f "$caller_config" ] && grep -q "^\[profile $ASSUME_PROFILE" "$caller_config"; then
    die "$caller_config already defines a profile $ASSUME_PROFILE*; --assume-role needs that name"
  fi
  ROLE_CONFIG="$(mktemp "${TMPDIR:-/tmp}/biffix-aws-config.XXXXXX")" \
    || die "could not create a temp file"
  chmod 600 "$ROLE_CONFIG"
  {
    [ ! -f "$caller_config" ] || { cat "$caller_config"; printf '\n'; }
    # The role's source is whatever the caller's credentials are, in the aws CLI's order. Exported
    # keys outrank any profile for the aws CLI and boto3 (though not for cdk, whose SDK ignores
    # them once AWS_PROFILE is set), so they cannot stay exported beside the profile, as
    # `credential_source = Environment` would need: they move into a source profile in this
    # (0600) file instead, and are unset below.
    if [ -n "${AWS_ACCESS_KEY_ID:-}" ]; then
      src="source_profile = $ASSUME_PROFILE-source"
      printf '[profile %s-source]\n' "$ASSUME_PROFILE"
      printf 'aws_access_key_id = %s\n' "$AWS_ACCESS_KEY_ID"
      printf 'aws_secret_access_key = %s\n' "${AWS_SECRET_ACCESS_KEY:-}"
      [ -z "${AWS_SESSION_TOKEN:-}" ] || printf 'aws_session_token = %s\n' "$AWS_SESSION_TOKEN"
      printf '\n'
    # A named profile, or the default one when it is configured.
    elif [ -n "$profile" ] || aws configure list-profiles 2>/dev/null | grep -qx default; then
      src="source_profile = ${profile:-default}"
    # No keys and no profile: the container's or the instance's role.
    elif [ -n "${AWS_CONTAINER_CREDENTIALS_RELATIVE_URI:-}${AWS_CONTAINER_CREDENTIALS_FULL_URI:-}" ]; then
      src="credential_source = EcsContainer"
    else
      src="credential_source = Ec2InstanceMetadata"
    fi
    # No duration_seconds: a chained role is capped at an hour anyway; renewal is what lasts.
    printf '[profile %s]\nrole_arn = %s\n%s\nrole_session_name = %s\n' \
      "$ASSUME_PROFILE" "$arn" "$src" "$session"
  } > "$ROLE_CONFIG"
  unset AWS_ACCESS_KEY_ID AWS_SECRET_ACCESS_KEY AWS_SESSION_TOKEN AWS_SECURITY_TOKEN \
    AWS_DEFAULT_PROFILE
  export AWS_CONFIG_FILE="$ROLE_CONFIG" AWS_PROFILE="$ASSUME_PROFILE"
  # Assume it now, so a role that cannot be assumed fails here, and check the calls act as it.
  caller="$(aws sts get-caller-identity --query Arn --output text)" || die "assume-role failed"
  case "$caller" in
    *":assumed-role/${arn##*/}/$session") ;;
    *) die "assume-role failed: the caller is $caller, not role $arn" ;;
  esac
}

# Refuse to act unless the caller is in exactly the intended account, and never in the AWS
# Organizations management account. `$1` is --account, `$2` the config's `account:`; either may be
# empty, but not both, and when both are set they must agree. Sets CALLER_ACCOUNT and CALLER_ARN.
guard_account() {
  local flag="$1" configured="$2" want mgmt errf
  if [ -n "$flag" ] && [ -n "$configured" ] && [ "$flag" != "$configured" ]; then
    die "--account $flag disagrees with the config's account $configured — aborting before any change"
  fi
  want="${flag:-$configured}"
  [ -n "$want" ] || die "no target account: pass --account <id> or set 'account:' in the config (refusing to guess)"
  CALLER_ACCOUNT="$(aws sts get-caller-identity --query Account --output text)" \
    || die "could not resolve the AWS account (no valid credentials?)"
  CALLER_ARN="$(aws sts get-caller-identity --query Arn --output text)" \
    || die "could not resolve the AWS caller identity"
  log "aws account: $CALLER_ACCOUNT ($CALLER_ARN)"
  [ "$CALLER_ACCOUNT" = "$want" ] \
    || die "WRONG ACCOUNT: caller is $CALLER_ACCOUNT, expected $want — aborting before any change"
  # A member account may describe its organization; a standalone account gets
  # AWSOrganizationsNotInUseException. Anything else cannot rule the management account out.
  errf="$(mktemp)"
  if mgmt="$(aws organizations describe-organization --query Organization.MasterAccountId \
      --output text 2>"$errf")"; then
    rm -f "$errf"
    [ "$mgmt" != "$CALLER_ACCOUNT" ] \
      || die "REFUSING: $CALLER_ACCOUNT is the AWS Organizations management account; deploy tenants into member accounts"
  else
    if ! grep -q AWSOrganizationsNotInUseException "$errf"; then
      warn "could not check whether $CALLER_ACCOUNT is an Organizations management account: $(cat "$errf")"
    fi
    rm -f "$errf"
  fi
}

# Print the physical id of the one resource of type $2 whose logical id starts with $3 in stack
# $1 (region $REGION). Prints nothing when the stack or the resource does not exist. Returns 1,
# with the reason on stderr, when the lookup fails or more than one resource matches.
stack_resource() {
  local stack="$1" type="$2" logical="$3" out errf n
  errf="$(mktemp)"
  if ! out="$(aws cloudformation list-stack-resources --region "$REGION" --stack-name "$stack" \
      --query "StackResourceSummaries[?ResourceType=='${type}' && starts_with(LogicalResourceId, '${logical}')].PhysicalResourceId" \
      --output text 2>"$errf")"; then
    if grep -q "does not exist" "$errf"; then
      rm -f "$errf"
      return 0
    fi
    printf 'could not list the resources of %s: %s\n' "$stack" "$(cat "$errf")" >&2
    rm -f "$errf"
    return 1
  fi
  rm -f "$errf"
  [ "$out" = "None" ] && out=""
  # shellcheck disable=SC2086 # word-split the tab-separated ids on purpose
  set -- $out
  n=$#
  if [ "$n" -gt 1 ]; then
    printf 'expected at most one %s %s* in %s, found %s: %s\n' "$type" "$logical" "$stack" "$n" "$out" >&2
    return 1
  fi
  printf '%s' "${1:-}"
}
