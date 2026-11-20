# biffix BASE coordinator image: the control-plane glue, with no workflow engine in it.
#
# A workflow version pins its engine to one exact release; biffix builds that release into a
# runtime layer under /opt/biffix/{sm,nf} (codebuild/runtime/build.sh, which build_stack.py inlines
# into the runtime build's buildspec) and appends it to THIS image by digest, giving one
# coordinator variant (and Batch job definition) per runtime. So this
# image carries only what every variant shares:
#   - biffix-coordinator (entrypoint.sh) and its helpers; python3 + boto3 for their S3 calls
#   - git (git-sourced workflows), a JRE (Nextflow head node), unzip/procps
#   - /opt/biffix/wheels/: the biffix-owned engine plugins the runtime build installs (the executor
#     fork, the Snakemake logger, nf-biffix) + manifest.json, which also records the coordinator
#     contract this image implements (wheels.env COORDINATOR_CONTRACT). Built by build-wheels.sh;
#     the runtime build reads them out of this image's digest, so they are never fetched from git
#     at build time, and refuses an image whose contract is below what a runtime needs.
# No aws CLI: Nextflow tasks stage S3 I/O with the one every Batch host installs
# (infra/biffix_infra/host/), and the entrypoint uses boto3.
#
# Build context is the repo root, after `docker/coordinator/build-wheels.sh build/coordinator-wheels`.
# CI builds a linux/amd64 + linux/arm64 manifest (Batch runs both amd64 and Graviton queue-classes).
# The base is pinned by digest (the python:3.12-slim index, every platform), so two builds of one
# commit start from the same bytes. To bump it, take the index digest the tag names now (e.g.
# `crane digest python:3.12-slim` or `docker buildx imagetools inspect python:3.12-slim`) and
# replace the one below; the Debian packages installed next are the base release's current ones.
FROM python:3.12-slim@sha256:5e2bdb1b0d542d5039cffb8181c65930093949194a65c365dfb1b0fe13a4fa5b

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        git ca-certificates default-jre-headless curl unzip procps \
    && rm -rf /var/lib/apt/lists/*

# The entrypoint's own S3 calls (source staging, events.jsonl upload, Snakemake metadata sync) run on
# this python3 for a Nextflow variant; a Snakemake variant puts the runtime's Python (which has boto3
# through the executor plugin) first on PATH instead.
RUN pip install --no-cache-dir "boto3==1.43.103"

COPY build/coordinator-wheels/ /opt/biffix/wheels/

# The runtime layer's bin directories come first, for every process in a variant: the coordinator,
# and the Snakemake child jobs that run on the variant, whose `python -m snakemake` command the
# executor leaves to PATH. The base has neither directory, so here they resolve to nothing.
ENV PATH=/opt/biffix/sm/bin:/opt/biffix/nf/bin:$PATH

COPY docker/coordinator/entrypoint.sh /usr/local/bin/biffix-coordinator
# Its libraries (sourced) and helpers (run with python3), by name: never a stray file of the tree
# (tests/test_image_layout.py checks this list against lib/).
COPY docker/coordinator/lib/common.sh \
     docker/coordinator/lib/runtime.sh \
     docker/coordinator/lib/signals.sh \
     docker/coordinator/lib/source.sh \
     docker/coordinator/lib/tasks.sh \
     docker/coordinator/lib/events.sh \
     docker/coordinator/lib/snakemake.sh \
     docker/coordinator/lib/nextflow.sh \
     docker/coordinator/lib/json_field.py \
     docker/coordinator/lib/stage_source.py \
     docker/coordinator/lib/image_map.py \
     docker/coordinator/lib/worker_queue.py \
     docker/coordinator/lib/queue_selectors.py \
     docker/coordinator/lib/events_jsonl.py \
     docker/coordinator/lib/sm_metadata.py \
     docker/coordinator/lib/snakemake_cache_rules.py \
     /usr/local/lib/biffix/
RUN chmod +x /usr/local/bin/biffix-coordinator

WORKDIR /work

# Deliberately NO ENTRYPOINT. A Snakemake variant also runs every child job that declares no image
# of its own. Its tasks run on the task job definitions the backend registers, which take each
# job's command (`python -m snakemake …`) from that job's SubmitJob container overrides. A
# coordinator ENTRYPOINT would be prepended to those commands and hijack them (the child would
# re-run the coordinator script and die on its BIFFIX_RUN_ID guard).
# Being the coordinator is therefore expressed by the COORDINATOR JOB DEFINITION's command
# (`biffix-coordinator`, set in infra/biffix_infra/constructs/coordinator.py), not baked into the
# image.
