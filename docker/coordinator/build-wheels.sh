#!/usr/bin/env bash
# Build the biffix-owned engine plugins into one directory for the base coordinator image's
# /opt/biffix/wheels/:
#   - the Snakemake AWS Batch executor fork at EXECUTOR_SHA (wheels.env), as a wheel
#   - the biffix Snakemake logger plugin, as a wheel
#   - the nf-biffix Nextflow plugin zip (build it first: plugins/nf-biffix `./gradlew assemble`)
#   - manifest.json: the versions the runtime build checks against its recipe, and each file's sha256
#
# The runtime build installs these rather than fetching them from git, so a runtime's inputs are
# fixed by the base image digest.
#
# Usage: docker/coordinator/build-wheels.sh <out_dir>
set -euo pipefail

OUT="${1:?usage: build-wheels.sh <out_dir>}"
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
# shellcheck source=SCRIPTDIR/wheels.env
. "$ROOT/docker/coordinator/wheels.env"

LOGGER_DIR="$ROOT/plugins/snakemake-logger-plugin-biffix"
NF_DIR="$ROOT/plugins/nf-biffix"
LOGGER_VERSION=$(sed -n 's/^version = "\(.*\)"$/\1/p' "$LOGGER_DIR/pyproject.toml" | head -1)
NF_BIFFIX_VERSION=$(sed -n "s/^version = '\(.*\)'$/\1/p" "$NF_DIR/build.gradle" | head -1)
NF_ZIP="$NF_DIR/build/distributions/nf-biffix-${NF_BIFFIX_VERSION}.zip"
[ -n "$LOGGER_VERSION" ] || { echo "build-wheels: no logger version in pyproject.toml" >&2; exit 1; }
[ -f "$NF_ZIP" ] || { echo "build-wheels: $NF_ZIP missing; run ./gradlew assemble in plugins/nf-biffix" >&2; exit 1; }

rm -rf "$OUT" && mkdir -p "$OUT"
python3 -m pip wheel --quiet --no-deps --wheel-dir "$OUT" \
  "git+${EXECUTOR_REPO}@${EXECUTOR_SHA}" "$LOGGER_DIR"
cp "$NF_ZIP" "$OUT/"

EXECUTOR_SHA="$EXECUTOR_SHA" LOGGER_VERSION="$LOGGER_VERSION" NF_BIFFIX_VERSION="$NF_BIFFIX_VERSION" \
  python3 - "$OUT" <<'PY'
import hashlib, json, os, pathlib, sys

out = pathlib.Path(sys.argv[1])
files = {
    p.name: hashlib.sha256(p.read_bytes()).hexdigest()
    for p in sorted(out.iterdir())
    if p.is_file()
}
manifest = {
    "executor_sha": os.environ["EXECUTOR_SHA"],
    "logger_version": os.environ["LOGGER_VERSION"],
    "nf_biffix_version": os.environ["NF_BIFFIX_VERSION"],
    "files": files,
}
(out / "manifest.json").write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n")
print(json.dumps(manifest, indent=2, sort_keys=True))
PY
