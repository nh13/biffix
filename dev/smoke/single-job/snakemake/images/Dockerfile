# One of the acceptance fixture's three task images. All three build this file from the same base
# and differ only in ACCEPTANCE_IMAGE (biffix.yaml `build_args`), so they share every base layer:
# what the single-job image conversion measurement needs (dev/smoke/single-job/README.md).
# The base is Ubuntu 24.04's multi-architecture index, pinned by digest (linux/amd64 and
# linux/arm64 each resolve to their own native image).
FROM public.ecr.aws/docker/library/ubuntu:24.04@sha256:562111b88caa79c71391c0c4d3c2114a7d5c35915c20561fd893cb1e07929e6b
ARG ACCEPTANCE_IMAGE
RUN test -n "${ACCEPTANCE_IMAGE}" \
    && printf '%s\n' "${ACCEPTANCE_IMAGE}" > /opt/biffix-acceptance-image
