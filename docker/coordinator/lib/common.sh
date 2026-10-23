# shellcheck shell=bash
# biffix coordinator: what every step shares. Sourced by entrypoint.sh; defines functions only.
#   Reads: LIB_DIR (the helpers' directory).

# runtime_error <message…>: fail the run as misconfigured, with EX_CONFIG (78).
runtime_error() {
  echo "biffix-coordinator: $*" >&2
  exit 78
}

# require_env <name> [<why>]: fail the run as misconfigured unless the env var <name> is set and
# not empty.
require_env() {
  [ -n "${!1:-}" ] || runtime_error "$1 required${2:+ ($2)}"
}

# require_count <name> <value>: fail the run as misconfigured unless <value> (the env var <name>'s,
# or its default) is a non-negative integer. Both engines check every count knob this way, rather
# than leave a bad one to an engine that may read it as "none".
require_count() {
  [[ "$2" =~ ^[0-9]+$ ]] || runtime_error "$1 is not a number: $2"
}

# jget <json> <key>: one string field of a JSON object (json_field.py). Malformed input, a missing
# key or a non-string value yields an empty value instead of a traceback.
jget() {
  printf '%s' "$1" | python3 "$LIB_DIR/json_field.py" "$2"
}

# A URL as it may be logged: without any user:password@ it carries.
redact_url() {
  printf '%s' "$1" | sed -E 's#^([A-Za-z][A-Za-z0-9+.-]*://)[^/@]*@#\1#'
}
