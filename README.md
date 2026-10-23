# biffix

biffix (say "BFX", short for bioinformatics) is a self-hostable control plane for running and
tracking Snakemake and Nextflow workflows on AWS. You deploy it into your own AWS account. Workflow
authors register a pipeline once, lab users launch it from a generated form, and everyone watches
runs, reads results and sees what they cost. biffix does not schedule tasks itself: each run is one
AWS Batch job that runs the engine, and the engine's own AWS Batch executor runs the pipeline's
tasks. biffix is the layer around that: registration, launching, tracking, provenance, and the
infrastructure to host it.

## Features

- **Registration.** A workflow (from a git repository or a staged source tarball) is registered as
  an immutable version: an exact commit, an exact engine release, and container images pinned by
  digest and copied into your own ECR. Images can also be built from a Dockerfile or a pixi
  environment in the workflow.
- **Launch forms.** The form is generated from the workflow's own parameter schema (nf-schema for
  Nextflow, JSON Schema for a Snakemake config) and curated in a `biffix.yaml` manifest. The
  browser checks the form with a port of the server's validator, tested against the same cases,
  and the server's verdict is authoritative.
- **Two views.** A Lab view for people who launch pipelines and read results (pipeline catalog,
  step-by-step launch, per-sample progress, result cards), and an Expert view with every Batch job,
  log, the task graph, the coordinator log, parameters and cost.
- **Run tracking.** Live progress, per-task attempts and logs, spot reclaims shown as retries, and
  failure summaries that name a cause and suggest what to do next.
- **Results.** Output browsing with viewers for tables, reports, images and PDFs, FastQC and
  MultiQC results, and a genome browser for BAM and CRAM files.
- **Resume and re-run.** Resume a Nextflow or Snakemake run from its finished work, or re-run it
  with the same parameters. A Nextflow run's intermediate files are deleted after a retention
  period.
- **Experiments and reference bundles.** Group runs into an experiment and compare their results;
  register reference genomes once and pick them from a dropdown.
- **Costs.** Per-job and per-run estimates from instance time and spot or on-demand prices, and a
  usage page that sums them by month, workflow, submitter, instance type or project.
- **Provenance.** Every run exports a Workflow Run RO-Crate built from the same events the UI shows.
- **Isolation through IAM.** Each project has its own bucket, KMS key and roles. Tasks run on spot
  by default; no AWS credentials are passed into a run.
- **A CLI.** `biffix` registers workflows, submits, follows and cancels runs, and checks a
  deployment's health.

## Quick start

1. **Deploy.** Bootstrap CDK in an AWS account, write a deploy config from
   `infra/biffix-deploy.example.yaml`, check out the release tag `vX.Y.Z`, and deploy it with
   `infra/deploy.sh --config <your-config>.yaml --release vX.Y.Z`.
   [docs/deploying.md](docs/deploying.md) walks through it, including the first admin and a custom
   domain.
2. **Register a workflow.** Install the CLI, add a `biffix.yaml` to your workflow, and run
   `biffix workflows register`. See [docs/user-guide.md](docs/user-guide.md#authoring-a-workflow).
3. **Run it.** Launch from the web UI or with `biffix runs submit`, and follow it on the run page.
   See [docs/user-guide.md](docs/user-guide.md#launching-runs).

## How it works

Everything biffix knows about a run is appended to one immutable log of `RunEvent`s in Postgres
(Aurora in a deployment). The engine plugins inside the coordinator post engine events, AWS Batch
and EC2 state changes arrive through EventBridge and SQS, and the backend records its own actions.
Everything the UI, the CLI and the provenance export show is a deterministic fold of that log, so
a view can always be rebuilt from it. One deployment is one tenant in one AWS account, defined by a
CDK app. [docs/architecture.md](docs/architecture.md) covers the components, the design
principles and the event contract.

## Repository layout

| Path | What it holds |
|---|---|
| `crates/` | Rust: the event contract and reducer (`biffix-core`), workflow authoring and registration (`biffix-workflow`), Postgres access (`biffix-db`), API types (`biffix-api`), the HTTP API (`biffix-backend`), the ingest and reconcile Lambdas (`biffix-ingest`), and the CLI (`biffix-cli`) |
| `plugins/` | The engine plugins: a Snakemake logger plugin and a Nextflow plugin |
| `docker/coordinator/` | The coordinator image and its entrypoint |
| `codebuild/` | Scripts the deployment's CodeBuild projects run: engine runtimes, image import and build |
| `ui/` | The SvelteKit web UI and its mock API server |
| `infra/` | The CDK app (Python), `deploy.sh` and `teardown.sh` |
| `schemas/` | JSON Schemas generated from the Rust types |
| `xtask/` | Repository tooling (`cargo xtask emit-schemas`) |
| `dev/` | Local Postgres for the integration tests and the local API server |
| `docs/` | The documentation |

## Documentation

- [Architecture](docs/architecture.md): the event log, the components and the design principles.
- [Deploying](docs/deploying.md): from an empty AWS account to a working deployment, upgrades and
  teardown.
- [User guide](docs/user-guide.md): authoring and registering workflows, launching and monitoring
  runs.
- [Developing](docs/developing.md): building and testing biffix itself.
- [infra/README.md](infra/README.md): the full operational reference for the CDK app and the deploy
  scripts.

## Status

biffix is early. It runs real Snakemake and Nextflow pipelines, but it has one maintainer, and the
HTTP API, the CLI, the `biffix.yaml` manifest and the deploy config may change in incompatible ways
before 1.0. [docs/deploying.md](docs/deploying.md#upgrading) describes how to upgrade a
deployment. Running it on regulated data needs your own review; see the "Ready for
regulated data" principle in [docs/architecture.md](docs/architecture.md#ready-for-regulated-data).

## Contributing, security and license

- [CONTRIBUTING.md](CONTRIBUTING.md) describes how to build, test and submit a change.
- Report vulnerabilities privately as described in [SECURITY.md](SECURITY.md), not in a public
  issue.
- Participation is governed by the [Code of Conduct](CODE_OF_CONDUCT.md).
- biffix is released under the [MIT License](LICENSE).
