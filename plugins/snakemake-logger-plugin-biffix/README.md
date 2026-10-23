# snakemake-logger-plugin-biffix

A [Snakemake](https://snakemake.github.io) (9.0 or later) logger plugin that reports a workflow's
progress to [biffix](https://github.com/nh13/biffix), the AWS-native control plane for running and
tracking workflows. It is the Snakemake counterpart of the `nf-biffix` Nextflow plugin in
`../nf-biffix`, and both report the same wire contract (`schemas/wire.engine.schema.json`).

## What it does

The plugin maps Snakemake's log records to biffix engine events (`run_started`, `run_progress`,
`run_dag`, `task_created`, `task_submitted`, `task_retrying`, `task_succeeded`, `task_failed`,
`task_outputs` and `engine_task_verdict`) and sends each one to two places:

- **The ingest route, live.** Events are buffered and POSTed in batches to
  `{ingest_url}/events`, bearer-authenticated with the run's capability token, by a background
  thread, so Snakemake's logging never waits on the network. Failed requests are retried or
  dropped; a lost POST never fails the workflow.
- **A durable `events.jsonl`.** Every event is also appended to a local file, which the biffix
  coordinator uploads and replays through ingest when the run exits, so an event the live POST
  dropped still arrives (ingest dedupes the repeats).

For a job on AWS Batch, the plugin emits `task_submitted` carrying the Batch job id, read from the
payload the AWS Batch executor plugin attaches to its log records, and AWS EventBridge supplies the
job's terminal state. For a local rule, which runs on the coordinator and never reaches Batch, the
plugin emits the terminal event itself.

The plugin has no dependencies beyond `snakemake-interface-logger-plugins`; it uses only the
standard library for HTTP.

## How biffix uses it

biffix builds the plugin into a wheel for the coordinator base image
(`docker/coordinator/build-wheels.sh`) and installs it into each Snakemake runtime layer
(`codebuild/runtime/build.sh`). The coordinator (`docker/coordinator/lib/snakemake.sh`) runs
Snakemake with `--logger biffix` and sets the variables below. The version in `pyproject.toml` must match
`logger_version` in `crates/biffix-workflow/src/runtime/pins.rs`; a test in `runtime/tests.rs`
checks it.

## Configuration

Enable it with `snakemake --logger biffix`. Each setting comes from an environment variable or a
command-line option; a non-empty option wins.

| Environment variable | Option | Meaning |
| --- | --- | --- |
| `BIFFIX_RUN_ID` | `--logger-biffix-run-id` | The biffix run the events belong to. |
| `BIFFIX_INGEST_URL` | `--logger-biffix-ingest-url` | Base URL of the ingest route. |
| `BIFFIX_INGEST_TOKEN` | `--logger-biffix-ingest-token` | The run's capability token. It is a secret, so prefer the environment variable. |
| `BIFFIX_EVENT_FILE` | `--logger-biffix-event-file` | Name of the durable events file, relative to the directory Snakemake starts in (default `.biffix_events.jsonl`). |

Snakemake's own derived variables (`SNAKEMAKE_LOGGER_BIFFIX_*`) are not read; the `BIFFIX_*` names
are the only environment channel.

## Development

```sh
uv sync --dev
uv run ruff check . && uv run ruff format --check .
uv run validate-pyproject pyproject.toml && uv run mypy
uv run pytest
```

The tests read files from the biffix repository's `schemas/` directory, so they run from a biffix
checkout: `test_schema_conformance.py` checks every emitted event against the engine wire schema,
and `test_ingest_limits.py` checks that the poster's limits (largest batch, largest body, buffer
size, retries and timeouts) equal `schemas/ingest-limits.json`, which biffix-core writes from the
constants the ingest route enforces.

## License

MIT; see [LICENSE](https://github.com/nh13/biffix/blob/main/plugins/snakemake-logger-plugin-biffix/LICENSE).

## NOTICE

Parts of `handler.py`, `remote_adapter.py` and `writer.py` (Snakemake log record parsing, the
remote executor payload adapter and the JSONL event writer) are adapted from the
`snakemake-logger-plugin-snakesee` package of [snakesee](https://github.com/fg-labs/snakesee).
This package does not depend on snakesee. snakesee is distributed under the following license:

```text
MIT License

Copyright (c) 2024 Fulcrum Genomics LLC

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```
