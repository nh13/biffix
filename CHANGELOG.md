# Changelog

All notable changes to this project will be documented in this file.

## [0.4.0] - 2026-11-27

### Bug Fixes

- Harden single-job retries and image checks, and pin the S3 storage plugin ([#89](https://github.com/nh13/biffix/pull/89))
- Sanitize terminal output without changing JSON values ([#98](https://github.com/nh13/biffix/pull/98))
- Bump plugin and runtime pin to 0.8.1 ([#108](https://github.com/nh13/biffix/pull/108))

### Features

- Reconcile backfills a Batch job's start, for running and finished jobs ([#96](https://github.com/nh13/biffix/pull/96))
- Record each AWS Batch execution as its own event ([#99](https://github.com/nh13/biffix/pull/99))
- Run a run's tasks in nested Apptainer on one host (engines, coordinator, events) ([#86](https://github.com/nh13/biffix/pull/86))
- Launch a run as one Batch job on a whole host (backend, infra) ([#87](https://github.com/nh13/biffix/pull/87))
- Run a single-job Snakemake run's jobs through a biffix executor plugin ([#91](https://github.com/nh13/biffix/pull/91))
- Show why a run failed in runs show, runs follow and the run page ([#97](https://github.com/nh13/biffix/pull/97))
- Show AWS Batch executions and interruptions in the API, CLI and provenance ([#102](https://github.com/nh13/biffix/pull/102))
- Read and follow coordinator and task logs ([#106](https://github.com/nh13/biffix/pull/106))


## [0.3.0] - 2026-11-19

### Features

- Route Nextflow and Snakemake processes to queue-classes from biffix.yaml ([#79](https://github.com/nh13/biffix/pull/79))


## [0.2.1] - 2026-11-19

### Bug Fixes

- Register task job definitions in the backend, not the coordinator ([#74](https://github.com/nh13/biffix/pull/74))


## [0.2.0] - 2026-11-17

### Features

- Add image target and build_args, and register --manifest ([#60](https://github.com/nh13/biffix/pull/60))
- Support Nextflow 26 (26.04.9, 26.10.5) alongside 25.10.4 ([#63](https://github.com/nh13/biffix/pull/63))
- Add `workflows rebuild` for failed runtime and image builds ([#64](https://github.com/nh13/biffix/pull/64))

