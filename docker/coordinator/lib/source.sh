# shellcheck shell=bash
# biffix coordinator: the workflow source and its params, staged under the work root. Sourced by
# entrypoint.sh; defines functions only.
#   Reads: WORK, SRC, LIB_DIR, BIFFIX_WORKFLOW_SOURCE, BIFFIX_GIT_SHA, BIFFIX_PARAMS.
#   Sets:  PARAMS_JSON (the params object) and CONFIG (the file it is written to; track_staged_tree
#          reads it).

# stage_workflow_source: fetch the immutable workflow source into $SRC (exit 78 for a source this
# script cannot run).
stage_workflow_source() {
  # Each field is read in an assignment of its own, so a failing read stops the run (errexit). An
  # unset source reads as an empty kind (jget), which is refused below.
  local source_json="${BIFFIX_WORKFLOW_SOURCE:-}" kind repo ref uri sha256
  kind=$(jget "$source_json" kind)
  case "$kind" in
    git)
      repo=$(jget "$source_json" repo)
      ref=$(jget "$source_json" git_ref)
      [ -n "$repo" ] || runtime_error "the workflow source (kind=git) names no repo"
      stage_git_source "$repo" "$ref"
      ;;
    s3)
      uri=$(jget "$source_json" uri)
      sha256=$(jget "$source_json" sha256)
      [ -n "$uri" ] || runtime_error "the workflow source (kind=s3) names no uri"
      stage_s3_source "$uri" "$sha256"
      ;;
    *)
      runtime_error "unsupported/empty workflow source (kind='$kind')"
      ;;
  esac
}

# git_https_url <repo>: the anonymous-HTTPS URL to clone <repo> from. `register` records whatever
# `git remote get-url origin` returns, which is often an SSH URL (git@github.com:owner/repo or
# ssh://git@github.com/owner/repo). This image is python:3.12-slim with no SSH client or keys, so an
# SSH clone can never succeed. Rewrite SSH origins to anonymous HTTPS, which works for public repos.
# (A private repo needs credentials or S3-staged source instead; anonymous HTTPS will fail with a
# clear auth error rather than a confusing missing-ssh one.)
git_https_url() {
  case "$1" in
    git@*)       printf '%s' "$1" | sed -E 's#^git@([^:]+):#https://\1/#' ;;
    ssh://git@*) printf '%s' "$1" | sed -E 's#^ssh://git@([^/]+)/#https://\1/#' ;;
    http://*|https://*) printf '%s' "$1" ;;
    *) printf '%s' "https://$1" ;;
  esac
}

# stage_git_source <repo> <ref>: clone <repo> into $SRC at BIFFIX_GIT_SHA (else <ref>, else the
# default branch), without its .git.
stage_git_source() {
  local repo=$1 ref=$2 url
  url=$(git_https_url "$repo")
  case "$repo" in
    git@*|ssh://git@*) echo "biffix-coordinator: rewrote SSH origin to HTTPS: $repo -> $url" >&2 ;;
  esac
  # An origin recorded with credentials in it (https://user:token@host/…) is never logged.
  echo "biffix-coordinator: git clone $(redact_url "$url") (ref=${ref:-default} sha=${BIFFIX_GIT_SHA:-none})" >&2
  git clone --quiet "$url" "$SRC"
  if [ -n "${BIFFIX_GIT_SHA:-}" ]; then
    git -C "$SRC" checkout --quiet "$BIFFIX_GIT_SHA"
  elif [ -n "$ref" ]; then
    git -C "$SRC" checkout --quiet "$ref"
  fi
  # Drop the clone's own .git: the pinned SHA is now checked out, so the working tree IS the
  # immutable source and biffix never needs git history at runtime. Leaving .git here would defeat
  # the full-tree worker-deployment step (track_staged_tree) — that step git-inits a throwaway repo
  # over the work root so `git ls-files` from the engine's cwd enumerates the WHOLE staged tree
  # (config/, helper packages, the params configfile) for no-shared-fs source deployment. A nested
  # $SRC/.git would (a) make that step skip (its guard), leaving `git ls-files` to the clone's own
  # index, which lacks the params configfile, so workers would run without it, or (b) if
  # force-init'd, be recorded as an embedded-repo gitlink whose contents never ship. Removing it
  # makes a git clone behave exactly like an S3-staged tree.
  rm -rf "$SRC/.git"
}

# stage_s3_source <uri> <sha256>: stage an S3 tarball (which must have the recorded <sha256>) or
# every object under an S3 prefix into $SRC (stage_source.py).
stage_s3_source() {
  local uri=$1 sha256=$2
  echo "biffix-coordinator: staging workflow source from $uri" >&2
  case "$uri" in
    *.tar.gz|*.tgz)
      # The tarball must be the one registration recorded: its key is only a name, and what is
      # under it could have been replaced since.
      if [ -z "$sha256" ]; then
        runtime_error "the version records no sha256 for its source tarball $uri; refusing to run it"
      fi
      if ! [[ "$sha256" =~ ^[0-9a-f]{64}$ ]]; then
        runtime_error "the workflow source's recorded sha256 is not a sha256: $sha256"
      fi
      python3 "$LIB_DIR/stage_source.py" tarball "$uri" "$SRC" "$sha256"
      ;;
    *)
      # A prefix has no one digest to check (the CLI always stages a tarball); it stages as it is
      # listed.
      [ -z "$sha256" ] \
        || echo "biffix-coordinator: a prefix source has no tarball to check the recorded sha256 against" >&2
      python3 "$LIB_DIR/stage_source.py" prefix "$uri" "$SRC"
      ;;
  esac
}

# materialize_params: write the params into an engine config file (JSON is valid Snakemake config).
# It goes in the source root, which Snakemake runs from: a remote job gets only the files below its
# launch dir, the --configfile included (track_staged_tree). A file of that name in the workflow's
# own source is replaced.
materialize_params() {
  CONFIG="$SRC/.biffix-config.json"
  PARAMS_JSON="${BIFFIX_PARAMS:-}"
  if [ -z "$PARAMS_JSON" ]; then
    PARAMS_JSON='{}'
  fi
  printf '%s' "$PARAMS_JSON" > "$CONFIG"
}

# track_staged_tree: make the whole staged tree deployable to worker jobs. Snakemake's
# no-shared-filesystem source deployment ships the files returned by `git ls-files` (dag.py:
# get_sources) PLUS the Snakefile, its `include:`s, and the --configfile. A workflow whose rules
# read *sibling* files the parser never `include:`d — a `config/` directory, a helper Python package
# imported at parse time — therefore has those files present on the coordinator (we staged the
# whole prefix) but ABSENT on the worker, which re-parses the Snakefile and dies with
# FileNotFoundError. Staging via S3 sync leaves no git repo, so `git ls-files` finds nothing and
# only the Snakefile subtree deploys. Initialize a throwaway repo over the WORK ROOT so every staged
# file — and the params configfile, which materialize_params writes into the source root — is
# git-tracked and ships to workers. Snakemake runs from the source root (lib/snakemake.sh), so its
# `git ls-files .` finds this repo above it and lists exactly that tree, and every path it ships
# (the --configfile's too) is relative to it and below it; Snakemake drops a source that is not
# ("Only files relative to the working directory are allowed"). The repo itself stays out of the
# source root, where Nextflow would take it for the pipeline's own. `git add` (no commit) is
# enough — `git ls-files` lists staged files. No-op for a self-contained single-Snakefile workflow.
# Git-clone sources reach here too: the clone's own .git was removed after checkout
# (stage_git_source), so they stage via this same throwaway repo — one deployment path for both
# source kinds. A failure here is not fatal (a single-file workflow needs none of it), but it is
# logged: without it, a worker that needs a sibling file dies later with FileNotFoundError.
#
# Exactly the staged source (the params file with it) is added, and forced: the source is what
# registration recorded (the CLI's tarball is built from `git ls-files`, a clone has only tracked
# files), so a file the workflow's repo tracks despite one of its own .gitignore patterns must ship
# too, and nothing else the work root holds (the image map, say) is the workflow's.
track_staged_tree() {
  if command -v git >/dev/null 2>&1 && [ ! -e "$SRC/.git" ] && [ ! -e "$WORK/.git" ]; then
    if ! git init -q "$WORK" \
      || ! git -C "$WORK" add --all --force -- "${SRC#"$WORK"/}"; then
      echo "biffix-coordinator: WARNING: could not git-track the staged tree; worker jobs get only the Snakefile and what it includes" >&2
    fi
  fi
}
