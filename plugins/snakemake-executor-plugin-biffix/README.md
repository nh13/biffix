# snakemake-executor-plugin-biffix

A [Snakemake](https://snakemake.github.io) (9.0 or later) executor plugin that runs every job of a
workflow on the host Snakemake itself runs on, each in the container image its job on AWS Batch
would run. It is how [biffix](https://github.com/nh13/biffix), the AWS-native control plane for
running and tracking workflows, runs a Snakemake workflow as one AWS Batch job on one host (a
single-job run). Its companion is the logger plugin in `../snakemake-logger-plugin-biffix`.

## What it does

The plugin is a remote executor that submits nothing. Snakemake treats its jobs as it treats jobs
for a cluster or a cloud: it forms group jobs, merges their resources, and hands the executor each
job, or group job, as a Snakemake command of its own (`python -m snakemake …`). The plugin runs
that command as a process on this host:

- **In the job's image.** The image is chosen as the AWS Batch executor plugin chooses one: the
  job's `aws_batch_container_image` resource, else the default image
  (`--biffix-default-image`), then its entry in the image map (`--biffix-image-map`) when there is
  one. For a group job Snakemake has already merged that resource across the group's jobs, and
  refused a group job whose jobs do not agree on it, as it does for AWS Batch. A job whose image
  the map does not name fails, like any failed job, and so does one whose resource is `""`, which
  the AWS Batch executor refuses too. A rule's `container:` directive plays no part.
- **Through Apptainer, from an image file.** Nothing is pulled: each image is run from the file
  `--biffix-images-dir` holds for it, named as the biffix coordinator names the files it converts
  before the run. The command is `apptainer run --cleanenv --no-eval --home <working directory>
  --pwd <working directory> [--bind <path>]… <image file> /bin/bash -c '<the job's command>'`,
  so the image's entrypoint runs, with exactly these arguments, as it does for an AWS Batch job.
  `apptainer` is found on `PATH`. The working directory is the one Snakemake started in: the
  job's Snakemake applies a workflow's `workdir:` itself, as the main process did.
- **Without a container, when there is no image.** A job that names no image when no default
  image is set runs on the host, in Snakemake's own environment.
- **With a clean environment.** A job in a container gets none of Snakemake's environment but the
  variables Snakemake passes to jobs (a workflow's `envvars:`, `--envvars`, and the storage
  plugins' own). No job, in a container or not, gets `BIFFIX_INGEST_TOKEN`. A variable the
  workflow registers (`envvars:`, or `workflow.register_envvars`) reaches the jobs on every
  Snakemake 9 release: before 9.6 Snakemake passes a remote executor's jobs only `--envvars`, so
  the plugin adds the registered ones itself.

Snakemake's scheduler decides what runs when, within `--cores` and `--resources`. The plugin has no
scheduler: it starts what it is handed, polls its processes twice a second, and reports each as
finished or failed. Every job's own Snakemake, a group job's included, gets the caller's
`--cores`, which a workflow reads as `workflow.cores`, so a DAG, threads or params built from it
read the same in each job. So a group job's Snakemake may run more threads at once than the width
the scheduler reserved for the group job (its threads), beside the other jobs: on AWS Batch its
container's vCPUs bound it, and here nothing does, since no option bounds a Snakemake's threads
but `--cores`. A job's output goes to Snakemake's own output. When Snakemake is interrupted
(SIGINT), the plugin stops every job: SIGTERM to each job's process group, then SIGKILL to each
group with a process still in it after five seconds (a job's tool that ignores SIGTERM included). A
job the interrupt stops is not reported as failed.

Before Snakemake announces a job, the plugin reports the job's image on a log record, in the
payload shape the AWS Batch executor plugin uses for a Batch job's state (the `remote_job_state`
key, with no external job id). The biffix logger plugin records it as the task's image.

The plugin uses only `snakemake-interface-executor-plugins`, and no part of Snakemake itself.

## What the caller must arrange

A remote executor's jobs are scheduled by `--jobs`, not `--cores`, and a memory or disk limit is
reckoned for each job on its own, since each would have a node of its own. For jobs that share one
host:

- pass `--set-resource-scopes mem_mb=global disk_mb=global`, so that those limits bound the jobs
  running at once. On the command line it replaces a profile's `set-resource-scopes:` whole, so the
  biffix coordinator adds those two scopes to the profile's from its wrapper Snakefile instead;
- note that Snakemake stops limiting the threads of the jobs running at once to `--cores` as soon
  as `--jobs` is set. The biffix coordinator restores that limit from a wrapper Snakefile
  (`docker/coordinator/lib/single_job.py`), which clears the scheduler's nodes limit; without
  that, set `--jobs` to the number of jobs the host can run at once.
- pass one directory as both `--local-storage-prefix` and `--remote-job-local-storage-prefix`
  (and bind it), so that Snakemake and its jobs share the local copies of storage files.

Every image must carry the same Snakemake as the caller, with its storage plugin, and `bash`: a
job's Snakemake runs inside it. biffix adds its pinned Snakemake runtime to each image it runs.

## Options

Enable it with `snakemake --executor biffix`.

| Option | Meaning |
| --- | --- |
| `--biffix-images-dir` | Directory of the converted image files (required). |
| `--biffix-image-map` | Path of a JSON object from image reference to the image to run instead. With it, every job's image is looked up in it, as written and then in its normalized spelling (`ubuntu` is `docker.io/library/ubuntu:latest`), and a job whose image it does not name fails. |
| `--biffix-default-image` | The image of a job that names none. Without it, such a job runs without a container. |
| `--biffix-bind` | Paths to bind into every job's container, besides the working directory (one or more Apptainer `--bind` specs). |

## How biffix uses it

biffix builds the plugin into a wheel for the coordinator base image
(`docker/coordinator/build-wheels.sh`) and installs it into each Snakemake runtime layer
(`codebuild/runtime/build.sh`). The coordinator (`docker/coordinator/lib/snakemake.sh`) runs a
single-job run with `--executor biffix`, after converting every image of the run's image map to an
image file, and puts its own `apptainer` wrapper first on `PATH`. The version in `pyproject.toml`
must match `single_job_executor_version` in `crates/biffix-workflow/src/runtime/pins.rs`; a test
in `runtime/tests.rs` checks it.

## Development

```sh
uv sync --dev
uv run ruff check . && uv run ruff format --check .
uv run validate-pyproject pyproject.toml && uv run mypy
uv run pytest
# The oldest Snakemake 9, with the oldest executor interface the plugin accepts and the storage
# plugin release for its storage interface (CI's matrix entry, .github/workflows/ci.yml). It is
# `python -m pytest`: the project's own `pytest` script would run the locked Snakemake instead.
uv run --with snakemake==9.0.0 --with snakemake-interface-executor-plugins==9.3.2 \
  --with snakemake-interface-common==1.17.4 --with snakemake-storage-plugin-fs==1.0.6 \
  python -m pytest
```

The tests run workflows through the plugin with a real Snakemake and a stand-in for Apptainer.
The biffix coordinator's own tests (`docker/coordinator/tests/test_snakemake_single_job.py`) run
workflows through the command line a single-job run builds.

## License

MIT; see [LICENSE](https://github.com/nh13/biffix/blob/main/plugins/snakemake-executor-plugin-biffix/LICENSE).

## NOTICE

The image map in `images.py` (reference normalization and lookup) is adapted from the
[AWS Batch executor plugin](https://github.com/snakemake/snakemake-executor-plugin-aws-batch) (its
pull request 60). This package does not depend on it. It is distributed under the following
license:

```text
MIT License

Copyright (c) 2023 Snakemake

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
