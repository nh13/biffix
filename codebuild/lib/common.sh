# shellcheck shell=bash
# Shared by the biffix CodeBuild scripts (codebuild/runtime/build.sh, codebuild/image/import.sh,
# codebuild/image/build.sh).
#
# infra/biffix_infra/stacks/build_stack.py prepends this file to each script it inlines into a
# buildspec; run anywhere else, a script sources it from its own checkout. A script sets
# BIFFIX_LOG_NAME (its log prefix) and BIFFIX_OUTPUTS before calling these.
#
# Test hook: BIFFIX_CRANE_INSECURE=1 lets crane talk to a plain-http registry.
# shellcheck disable=SC2034 # read by the scripts that source this file
BIFFIX_COMMON_LOADED=1

CRANE_VERSION=v0.22.1
CRANE_SHA256_X86_64=0ab7a1d6932a213aed964ce97666c3077fe691c8606413674a8b3e0b9ec4cda0
# biffix-workflow images::KEEP_TAG_PREFIX: what a version pins is never expired by the lifecycle.
KEEP_TAG_PREFIX=biffix-keep-

log() { echo "${BIFFIX_LOG_NAME:-biffix-build}: $*" >&2; }
die() {
  log "$*"
  exit 1
}
# emit <KEY> <value>: one KEY=VALUE output line. The buildspec reads the file literally (never as
# shell) and exports only the keys it declares; a value is also held to the characters an output
# legitimately has (image references, digests, versions, comma-separated lists), since some are
# read out of the image a build handles, and a line break in one would forge another output.
emit() {
  [[ "$1" =~ ^[A-Z][A-Z0-9_]*$ ]] || die "invalid output name: $1"
  [[ "$2" =~ ^[A-Za-z0-9@:/_.,=+!-]*$ ]] || die "output $1 has an unexpected character: $2"
  printf '%s=%s\n' "$1" "$2" >>"$BIFFIX_OUTPUTS"
}
# fetch <url> <dest> <sha256>
fetch() {
  curl -fsSL --retry 3 -o "$2" "$1"
  echo "$3  $2" | sha256sum -c --quiet - || die "sha256 mismatch for $1"
}

# install_crane <dir>: the pinned crane, into <dir>; `crane` then runs it.
install_crane() {
  fetch "https://github.com/google/go-containerregistry/releases/download/${CRANE_VERSION}/go-containerregistry_Linux_x86_64.tar.gz" \
    "$1/crane.tgz" "$CRANE_SHA256_X86_64"
  tar -xzf "$1/crane.tgz" -C "$1" crane
  CRANE=("$1/crane")
  if [ "${BIFFIX_CRANE_INSECURE:-}" = 1 ]; then CRANE+=(--insecure); fi
}
crane() { "${CRANE[@]}" "$@"; }

# layers <image@digest>: the image's layer digests, bottom first, space-separated.
layers() { crane manifest "$1" | python3 -c 'import json, sys; print(" ".join(l["digest"] for l in json.load(sys.stdin)["layers"]))'; }

# last_layer <image@digest>: the digest of the image's top layer.
last_layer() { crane manifest "$1" | python3 -c 'import json, sys; print(json.load(sys.stdin)["layers"][-1]["digest"])'; }

# image_arch <image>: the architecture a single-platform image's config names.
image_arch() { crane config "$1" | python3 -c 'import json, sys; print(json.load(sys.stdin).get("architecture", ""))'; }

# ecr_login <image-or-repo>…: log crane in to each distinct private ECR registry named. One ECR token
# works for every registry the build's role may read (the tenant's, and another account's whose
# repository policy grants it). Skipped with BIFFIX_SKIP_ECR_LOGIN=1 or BIFFIX_CRANE_INSECURE=1.
ecr_login() {
  if [ "${BIFFIX_SKIP_ECR_LOGIN:-}" = 1 ] || [ "${BIFFIX_CRANE_INSECURE:-}" = 1 ]; then return 0; fi
  local token host seen=" "
  token=$(aws ecr get-login-password)
  for host in "${@%%/*}"; do
    [[ "$host" == *.dkr.ecr.*.amazonaws.com && "$seen" != *" $host "* ]] || continue
    crane auth login "$host" -u AWS -p "$token" >/dev/null
    seen+="$host "
  done
}

# tag_once <repo> <digest> <tag>: tag <repo>@<digest> unless the tag already names it (repositories
# with immutable tags refuse a re-tag); a tag naming another digest is an error.
tag_once() {
  local existing
  if existing=$(crane digest "$1:$3" 2>/dev/null); then
    [ "$existing" = "$2" ] || die "$1:$3 already names $existing, not $2"
  else
    crane tag "$1@$2" "$3"
  fi
}

# platforms <image@digest>: one line per platform image, "<digest> <os>/<arch>[/<variant>]"; a
# single image lists itself with an empty platform. Attestations (os `unknown`) are skipped.
platforms() {
  crane manifest "$1" | python3 -c '
import json, sys

manifest = json.load(sys.stdin)
children = manifest.get("manifests")
if children is None:
    print(sys.argv[1], "")
else:
    for m in children:
        p = m.get("platform") or {}
        if p.get("os") in (None, "unknown"):
            continue
        plat = "/".join(x for x in (p.get("os"), p.get("architecture"), p.get("variant")) if x)
        print(m["digest"], plat)
' "${1##*@}"
}

# keep_tag <repo> <digest> <platforms file>: keep-tag the image `biffix-keep-<digest12>` and each
# image an index lists `-<n>`, so the repository's untagged-expiry lifecycle never removes it.
# Prints the keep tag.
keep_tag() {
  local repo=$1 digest=$2 tag child n=0
  tag="${KEEP_TAG_PREFIX}$(cut -c8-19 <<<"$digest")"
  tag_once "$repo" "$digest" "$tag"
  while read -r child _; do
    [ "$child" = "$digest" ] && continue
    n=$((n + 1))
    tag_once "$repo" "$child" "$tag-$n"
  done <"$3"
  echo "$tag"
}

# archive <image@digest> <archive uri> <work dir>: store the image as an OCI layout under the
# archive — blobs/sha256/<digest> once per blob (content-addressed, so shared layers are stored
# once) and images/<digest>/{oci-layout,index.json}, unless the archive already holds the image.
# index.json goes last: it is what marks the image archived, so an interrupted archive is redone.
# Prints the bytes newly archived.
archive() {
  local image=$1 archive=${2%/} layout="$3/layout-${1##*:}" blob name bytes=0
  if aws s3 ls "$archive/images/${image##*@}/index.json" >/dev/null 2>&1; then
    echo 0
    return 0
  fi
  crane pull --format=oci "$image" "$layout"
  # The blobs already archived, listed once (nothing listed => every blob is uploaded, which is
  # only redundant: the keys are content-addressed).
  { aws s3 ls "$archive/blobs/sha256/" 2>/dev/null || true; } | awk '{ print $4 }' >"$layout.archived"
  for blob in "$layout"/blobs/sha256/*; do
    name=$(basename "$blob")
    if ! grep -qxF "$name" "$layout.archived"; then
      aws s3 cp --quiet "$blob" "$archive/blobs/sha256/$name"
      bytes=$((bytes + $(wc -c <"$blob")))
    fi
  done
  aws s3 cp --quiet "$layout/oci-layout" "$archive/images/${image##*@}/oci-layout"
  aws s3 cp --quiet "$layout/index.json" "$archive/images/${image##*@}/index.json"
  rm -rf "$layout" "$layout.archived"
  echo "$bytes"
}

# probe [<prefix>]: capabilities from a `tar -t` listing on stdin (nothing is executed): a bash, the
# C library (glibc or musl), and the Snakemake and plugin versions installed — the newest
# `<name>-<version>.dist-info` found (under <prefix> only, when given), in PEP 440 order as far as
# the stdlib allows (a dev release before a pre-release before the final, then a post release). A
# version is matched only in the characters a normalized PEP 440 version has: the names come from
# the image.
probe() {
  python3 -c '
import re, sys
prefix = sys.argv[1] if len(sys.argv) > 1 else ""
names = {line.rstrip("\n").lstrip("./").rstrip("/") for line in sys.stdin}
bash = any(n in ("bin/bash", "usr/bin/bash", "usr/local/bin/bash") for n in names)
if any(re.search(r"(^|/)lib(64)?/([^/]*-linux-gnu[^/]*/)?libc\.so\.6$", n) for n in names):
    libc = "glibc"
elif any(re.search(r"(^|/)lib/ld-musl-.*\.so\.1$", n) for n in names):
    libc = "musl"
else:
    libc = "none"
def order(v):
    release, rest = re.match(r"([0-9]+(?:\.[0-9]+)*)(.*)", v).groups()
    numbers = [int(x) for x in release.split(".")]
    while numbers and numbers[-1] == 0:
        numbers.pop()
    rest = rest.split("+")[0].lower()
    pre = re.search(r"rc|a|b", rest)
    phase = 3 if "post" in rest else 0 if "dev" in rest and not pre else 1 if rest else 2
    rank = {"a": 0, "b": 1, "rc": 2}[pre.group(0)] if pre else 0
    return numbers, phase, rank, [int(x) for x in re.findall(r"[0-9]+", rest)]
def version(dist):
    pattern = re.compile(r"/" + dist.replace("_", "[-_]") + r"-([0-9][0-9A-Za-z.+!_]*)\.dist-info(/|$)")
    found = {m.group(1) for n in names if n.startswith(prefix) for m in [pattern.search(n)] if m}
    return max(found, key=order, default="")
caps = [f"bash={int(bash)}", f"libc={libc}", "snakemake=" + version("snakemake")]
caps += [f"{k}={version(d)}" for k, d in (
    ("storage_s3", "snakemake_storage_plugin_s3"),
    ("logger", "snakemake_logger_plugin_biffix"),
    ("executor", "snakemake_executor_plugin_aws_batch"),
)]
print(",".join(caps))
' "$@"
}
