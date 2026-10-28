# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

biffix (say "BFX", short for bioinformatics) is an AWS-native, self-hostable control plane for
running and tracking Snakemake and Nextflow workflows. It launches **one coordinator AWS Batch job
per run** and delegates the DAG to the engine's native AWS Batch executor; biffix builds only the
thin control plane around it.

Read the design principles in `docs/architecture.md` before designing anything. They are the
load-bearing invariants: don't build a scheduler, manage AWS through AWS/CDK, one event-sourced
log, IAM is the authorization engine, schema → form, spot plus two-layer retries, ready for
regulated data, and fix upstream rather than in a fork. `docs/` also has the deployment, user and
developer guides.

## Architecture

**Event flow (the core idea).** Every producer appends normalized `RunEvent`s to one immutable
log in Postgres (Aurora in prod); everything the UI shows is a **projection** = a deterministic
fold of that log. Invariant: `projection == fold(events in ts order)`, so projection bugs are fixed
in the reducer + replay, never by patching rows. The RunEvent contract is a versioned public API:
additive-only, unknown-field tolerant, open enums.

- Producers: the engine plugins inside the coordinator (`plugins/snakemake-logger-plugin-biffix`,
  `plugins/nf-biffix`) POST engine events with a per-run capability token; AWS Batch / EC2 state
  changes arrive via EventBridge → SQS; the backend emits control-plane events (submit, cancel…).
- `crates/biffix-ingest`: the `biffix-ingest-lambda` bin (`src/bin/lambda.rs`) drains SQS
  (decode with `biffix_core::eventbridge`, then append and advance projections through
  `biffix_db::log`, dedup on `dedup_key`); the `biffix-reconcile-lambda` bin
  (`src/bin/reconcile.rs`) runs `src/tick/` on a schedule, one module per step (reconcile stuck state,
  status probe/alarms, host lifecycles, pricing, retention and cache sweeps); their SQL is in
  `src/store/`, one module per table.
- Join keys: `batch_job_id` joins the engine stream to the AWS stream; `task_key`
  (`rule:sorted(wildcards)` / NF process+tag) is stable across retries; `run_id` is nullable on
  events (late attribution). Known, accepted limitation: Snakemake wildcard values are joined
  without escaping, so a value containing `,` or `=` can give two tasks one key (see
  `events.task_key` in the SM plugin). Revisit, as a contract change for every producer and
  consumer, only if a real workflow hits it.

**Crates** (`crates/*`, one workspace):
- `biffix-core` — the RunEvent contract and its folds: `event/` (RunEvent), `state.rs` (lifecycle
  states), `reducer/` (the fold), `wire/` (per-source wire formats), `translate.rs`,
  `eventbridge/` (raw EventBridge JSON and ingest-queue messages → RunEvents), `failure.rs`, and
  small shared helpers (`env`, `ids`, `clock`, `s3_layout`). Pure: no database or AWS
  dependencies, and no other workspace crate.
- `biffix-workflow` — workflow versions: `manifest.rs` (`biffix.yaml`), `formschema/` (schema →
  form), `validate.rs`, `samples/`, `scan/`, `images/` (references, registries, pinning keys),
  `runtime/` (engine runtimes; the pin tables in `runtime/pins.rs`) and `engines.rs`. Depends on
  `biffix-core` only. With core, it holds the Rust types `schemas/*.json` are generated from.
- `biffix-db` — the Postgres layer the backend and ingest share: SQL migrations
  (`migrations/NNNN_*.sql`, `migrate`), the connection URL and pool (`url`, `pool`, and `secrets`
  behind `aws`), the event-log writer (`log`: the one append path) and the projection fold it
  advances (`projection`), SQL rules both crates' queries apply (`rules`), and the schema-isolated
  pg test pool (`testing`, behind `test-support`).
- `biffix-api` — shared API request/response types (optional `openapi` feature).
- `biffix-backend` — axum HTTP API (`api/*.rs`), registration (`register/`: image resolve/pin,
  schema → FormSchema), submit pipeline, RBAC, stores (`store/`: one trait per concern, and
  `Store`, all of them; the Postgres impls in its sibling `pg/`, the in-memory doubles in
  `testing/`), AWS provider (`aws/`). Bins (Cargo names): `biffix-backend-lambda` (prod),
  `biffix-backend-sweep` (the sweep Lambda), `biffix-backend` (the local server,
  `src/bin/server.rs`; needs `--features aws`; reads its projects from the JSON file
  `BIFFIX_PROJECTS_FILE` names, see `dev/README.md`), `biffix-openapi` (emits the OpenAPI doc the
  UI types are generated from). Code behind AWS/Postgres is feature-gated (`aws`).
- `biffix` (`crates/biffix-cli`) — the `biffix` CLI (OAuth public client): `login`, `workflows`
  (register/form/…), `runs`.
- `xtask` — repo tooling: `cargo xtask emit-schemas [--check]` writes (or drift-checks)
  `schemas/`; its tests, run by `cargo test`, also check the crate layering (core depends on no
  workspace crate; ingest and db never link workflow).

`biffix`, `biffix-core`, `biffix-workflow` and `biffix-api` are published to crates.io, all at the
workspace version; the rest are `publish = false`. Releases: release-plz (`release-plz.toml`) keeps a
release PR open; merging it lets `.github/workflows/publish.yml` publish the crates and push
`v<version>`, which runs `release.yml`. The four declare their own, older
`rust-version` (CI's `rust-msrv` job builds them with it); the workspace's matches the toolchain
`rust-toolchain.toml` pins, since the AWS SDK and sqlx need a newer rustc.

**Coordinator image** (`docker/coordinator`): one image serves as both coordinator and generic
worker (no ENTRYPOINT; the Batch job definition picks the role). `entrypoint.sh` documents the
env contract the backend injects (`BIFFIX_RUN_ID`, `BIFFIX_ENGINE`, `BIFFIX_PARAMS`, …) and runs
the steps its `lib/` defines, one file per concern (`lib/*.sh` sourced, `lib/*.py` helpers; the
Dockerfile copies each by name). Built in CI, not on a laptop: `.github/workflows/release.yml`
publishes it to GHCR with each release, and `build-coordinator-image.yml` builds it into a
deployer's own ECR, both through the two-architecture build in `coordinator-image.yml`
(`build-and-push.sh` is only the manual fallback for a tenant without the image-build role). Its Snakemake AWS Batch executor is pinned to a fork
(`docker/coordinator/wheels.env` holds the repo and commit; the branch, `biffix-pin/<date>`, never
force-pushed, is named in `crates/biffix-workflow/src/runtime/mod.rs`): upstream plus three open
upstream PRs, #55 (remote job-state events), #60 (strict container image map) and #61 (task log
group), plus small log-noise fixes to upstream's own code. Per-rule queues (#40) and spot
reclaim retries (#62) are already upstream. Re-pin to
upstream once they merge. `lib/snakemake.sh` always passes the spot-attempts, log-group and
image-map settings, so a re-pin must keep an executor that has all three. `plugins/nf-biffix` also provides
the `awsbatch` executor (a subclass of nf-amazon's) that tags every `nf-*` job definition with
`biffix:project`, which the coordinator IAM requires; bumping nf-biffix's version means bumping
`nf_biffix_version` in `crates/biffix-workflow/src/runtime/pins.rs` too.

**UI** (`ui/`): SvelteKit (Svelte 5 runes) + static adapter, served from S3/CloudFront. API types in
`src/lib/generated/` are **generated** from the backend's OpenAPI doc (`npm run generate:api` runs
the Rust `biffix-openapi` bin) and CI fails if they drift. `mock/` is a hand-maintained mock of the
API used for local dev and by Playwright (`mock/server.mjs`; routes in `mock/routes.mjs`, one
`handlers/` and `fixtures/` module per resource) — keep it in step with contract changes:
`src/mock-coverage.test.ts` fails on an operation it has no route for, `src/mock-contract.test.ts`
on an answer the contract does not admit.

**Infra** (`infra/`): CDK (Python, uv). One deploy = one tenant = one AWS account. Stacks: network,
data (buckets, ECR), identity (Cognito, IAM), compute (Batch CEs/queues), build (CodeBuild
projects for engine runtimes, image import and image builds), project-<id>
(bucket/KMS/run role per project), persistence (Aurora), ingest, api, edge. `infra/README.md` is
the operational deploy reference. `deploy.sh --config <file>` (the config is required; start from
`infra/biffix-deploy.example.yaml`) builds artifacts from source, or with `--release vX.Y.Z` fetches
and verifies a published release instead; then it deploys, health-checks, and runs the ECR
image-retention step (`biffix_infra/image_retention.py`). The deploy config's schema is
`biffix_infra/config/`.

## Commands

Rust (toolchain pinned in `rust-toolchain.toml`):
```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo test -p biffix-backend <name_filter>   # one crate / one filter (only one filter allowed)
cargo test -p biffix-backend --test it submit_api::   # one integration-test module
cargo xtask emit-schemas --check   # schemas/*.json drift gate; drop --check to regenerate
```

Each crate's integration tests are one binary, `tests/it/main.rs` (`--test it`), with a module per
suite. The Postgres-backed suites are `#[ignore]`d modules of it and need the local
harness:
```sh
docker compose -f dev/docker-compose.yml up -d postgres      # :5433
BIFFIX_TEST_PG=postgres://biffix:biffix@localhost:5433/biffix \
  cargo test -p biffix-backend --all-features --test it -- --include-ignored pg_store:: event_sink_pg::
BIFFIX_TEST_PG=... cargo test -p biffix-ingest --all-features --test it -- --include-ignored store_pg:: reconcile_pg:: drain_pg::
```

UI (`cd ui`):
```sh
npm run generate:api      # regenerate src/lib/generated/ after any backend API change
npx svelte-kit sync && npm run check    # svelte-check
npm run lint              # prettier --check
npm run test              # vitest
npm run e2e               # playwright (starts the mock :8080 + vite :5174)
npx vitest run src/lib/util/format.test.ts   # single test file
npm run mock & PUBLIC_BIFFIX_API=http://localhost:8080 npm run dev   # local dev against the mock
```

Infra (`cd infra`):
```sh
uv run ruff check . && uv run ruff format --check .
uv run validate-pyproject pyproject.toml && uv run mypy
uv run pytest -q    # synth tests; needs ui/build/index.html to exist
```

Coordinator entrypoint (`cd docker/coordinator`; its own uv project, not part of the image):
```sh
uv run ruff check . && uv run ruff format --check .
uv run validate-pyproject pyproject.toml && uv run mypy && uv run pytest -q
MYPYPATH=tests/fakes/py uv run mypy --no-warn-unused-configs -p boto3 -p botocore   # the stand-ins
```

Plugins: `cd plugins/snakemake-logger-plugin-biffix && uv sync --dev`, then the same ruff,
validate-pyproject, mypy and `uv run pytest`;
`cd plugins/nf-biffix && ./gradlew --no-daemon test`.

Each Python project resolves only releases at least a week old (`[tool.uv] exclude-newer`), to
match Dependabot's cooldown, and CI syncs with `uv sync --locked`.

Enable the fast pre-push fmt/lint hook with `git config core.hooksPath .githooks`.

## Gotchas

- Changing a RunEvent / wire / FormSchema type means: update `biffix-core` or `biffix-workflow`, regenerate
  `schemas/` (`cargo xtask emit-schemas`), and if it reaches the API, `npm run generate:api` + update
  the mock (`ui/mock/`). Adding a field to a widely-constructed struct breaks struct literals across
  crates (E0063) — fix them all before clippy passes. Tests build events and anchors with
  `biffix_core::testing` (`event`, `anchor`, `RunEventExt`, feature `test-support`) rather than
  literals, so a new `RunEvent` field is defaulted there once.
- New projection columns need a new numbered migration in `crates/biffix-db/migrations/`; never
  edit an applied one (sqlx checksums each applied file; `migrate.rs`'s test pins them, so a new
  migration's version and checksum must also be appended to `APPLIED` there), and add the column
  to the fold's upsert in `biffix_db::projection`.
- `check`/`test`/`build` use the committed `src/lib/generated/` types and never regenerate them:
  after a backend API change run `npm run generate:api` yourself (it needs cargo, and blocks on the
  cargo lock while another cargo command runs). CI regenerates and fails on drift.
- Don't edit `crates/` or `ui/` while `infra/deploy.sh` is running (it builds from the working tree).
- Registering a workflow version against an older deployed backend silently drops newer FormSchema
  fields — re-register after deploying.
- Svelte: don't name a variable `state` (collides with the `$state` rune).
- The backend sweep Lambda (`bin/sweep.rs`) launches runs parked in `preparing_images`
  (`runs::launch::sweep`), so it needs the capability secret (`BIFFIX_CAPABILITY_SECRET_ARN`)
  and the launch grants (SubmitJob/TerminateJob, run-inputs writes); without the secret it
  refuses to launch and the runs time out after 6 h.
