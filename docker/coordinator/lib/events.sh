# shellcheck shell=bash
# biffix coordinator: the engine's durable events.jsonl, after the engine exits. Sourced by
# entrypoint.sh; defines functions only. Both steps are best-effort and never change the run's
# outcome.
#   Reads: SRC, LIB_DIR, BIFFIX_PROJECT_BUCKET, BIFFIX_RUN_PREFIX, BIFFIX_EVENT_FILE,
#          BIFFIX_INGEST_URL, BIFFIX_INGEST_TOKEN.

# events_file: the events.jsonl, when the engine wrote one. The engine plugin (the Snakemake logger,
# or nf-biffix's EventWriter) writes it to BIFFIX_EVENT_FILE (default .biffix_events.jsonl),
# relative to the directory the engine starts in (the source root, $SRC, for both engines). Read
# from exactly there, never searched for: a same-named file elsewhere (one committed in the
# workflow's source, say) is not this run's.
events_file() {
  local file="${BIFFIX_EVENT_FILE:-.biffix_events.jsonl}"
  case "$file" in
    /*) ;;
    *) file="$SRC/$file" ;;
  esac
  [ ! -s "$file" ] || printf '%s' "$file"
}

# Upload the engine's durable events.jsonl (the archival replay copy) under the run's
# own prefix, so a finished run's event stream survives the container. Best-effort: an upload
# failure never changes the run's outcome. A dry run writes nothing to S3, so it keeps its stream.
upload_events_jsonl() {
  if [ -n "${BIFFIX_DRY_RUN:-}" ]; then
    echo "biffix-coordinator: dry run: not uploading events.jsonl" >&2
    return 0
  fi
  local src
  src=$(events_file)
  if [ -z "$src" ]; then
    echo "biffix-coordinator: no events.jsonl to upload" >&2
    return 0
  fi
  # Under the run's OWN prefix, not STORE: a shared --output-prefix (or a --resume-from, which
  # re-uses the prior run's STORE) would otherwise overwrite another run's archived stream.
  local dest="s3://$BIFFIX_PROJECT_BUCKET/${BIFFIX_RUN_PREFIX}events.jsonl"
  echo "biffix-coordinator: uploading $src -> $dest" >&2
  python3 "$LIB_DIR/events_jsonl.py" upload "$src" "$dest"
}

# Replay the durable events.jsonl to the ingest route as an at-least-once backstop. The logger's
# live POSTs are best-effort (a failed POST is swallowed), so a dropped event — e.g. a local rule's
# engine terminal — would strand a task at "running". Re-POSTing every recorded event on exit closes
# that gap generally; the backend dedupes on dedup_key, so repeats are no-ops. Best-effort itself.
replay_events_jsonl() {
  local src
  src=$(events_file)
  [ -n "$src" ] || return 0
  [ -n "${BIFFIX_INGEST_URL:-}" ] && [ -n "${BIFFIX_INGEST_TOKEN:-}" ] || return 0
  echo "biffix-coordinator: replaying events.jsonl -> ${BIFFIX_INGEST_URL%/}/events (at-least-once backstop)" >&2
  python3 "$LIB_DIR/events_jsonl.py" replay "$src" || true
}
