# Developing biffix

This page covers working on biffix itself: the repository layout, the toolchains, running each
test suite, and the generated code you must keep in step. Read [architecture.md](architecture.md)
first; most of the rules below follow from the event-log design it describes.

## Repository layout

```text
crates/
  biffix-core/     the RunEvent contract: wire formats, translators, the reducer (no I/O)
  biffix-workflow/ workflow authoring: manifest, FormSchema, validation, images, runtimes
                   (no network, database or AWS)
  biffix-db/       Postgres: migrations, the event log, projection writers
  biffix-api/      HTTP API request/response types (shared by the backend and the CLI)
  biffix-backend/  the HTTP API, registration, submit, RBAC, the AWS provider, the sweep
  biffix-ingest/   the SQS drain Lambda and the scheduled reconcile Lambda
  biffix-cli/      the `biffix` CLI
xtask/             repository tooling (`cargo xtask emit-schemas`)
schemas/           JSON Schemas and test cases generated from the Rust types
plugins/
  snakemake-logger-plugin-biffix/   Snakemake logger plugin (Python)
  nf-biffix/                        Nextflow plugin (Groovy, Gradle)
docker/coordinator/  the coordinator image and its entrypoint
codebuild/         scripts the deployment's CodeBuild projects run (runtimes, image import/build)
ui/                the SvelteKit web UI and its mock API server
infra/             the CDK app (Python), deploy.sh and teardown.sh
dev/               local Postgres for the integration tests and the local API server
docs/              these docs
```

## Toolchains

- **Rust**, at the version pinned in `rust-toolchain.toml` (rustup installs it on first use).
- **Node.js 24** for the UI and the CDK CLI.
- **[uv](https://docs.astral.sh/uv/)** for the Python projects: `infra/`, `docker/coordinator/`
  and the Snakemake plugin.
- **Java 17** for the Nextflow plugin.
- **Docker** (or a compatible runtime) for the local Postgres.

## Rust

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features   # with the unit tests behind `aws`
cargo test -p biffix-core <name_filter> # one crate, one filter
```

Code that talks to AWS, and the backend's Postgres stores, is behind the `aws` feature, so a
plain `cargo test` needs neither an AWS account nor a database.

`cargo deny --locked check` checks the dependency tree against the RustSec advisories and the
license, ban and source rules in `deny.toml`; CI runs it daily and on every dependency change
(`.github/workflows/audit.yml`).

### Postgres-backed tests

The tests that apply migrations and exercise the Postgres stores and the ingest path are marked
`#[ignore]`, so the default test run needs no database. Run them against the local Postgres in
`dev/` (port 5433):

```sh
docker compose -f dev/docker-compose.yml up -d postgres
BIFFIX_TEST_PG=postgres://biffix:biffix@localhost:5433/biffix \
  cargo test -p biffix-backend --all-features --test it -- --include-ignored pg_store:: event_sink_pg::
BIFFIX_TEST_PG=postgres://biffix:biffix@localhost:5433/biffix \
  cargo test -p biffix-ingest --all-features --test it -- --include-ignored store_pg:: reconcile_pg:: drain_pg::
docker compose -f dev/docker-compose.yml down
```

Each test runs in its own schema, so the suites can run in parallel against one database.
[dev/README.md](../dev/README.md) also shows how to run the API server locally against it.

### Migrations

Schema changes are numbered SQL files in `crates/biffix-db/migrations/`, applied by every biffix
process when it starts (the API, sweep, ingest and reconcile Lambdas, and the local server). Never
edit a migration that has been applied anywhere; add a new one, and append its version and
checksum to `APPLIED` in `crates/biffix-db/src/migrate.rs`. The unit test
`every_migration_keeps_its_version_and_checksum` (run by `cargo test --workspace`) fails on an
edited migration and on an unpinned new one, and prints the checksum to pin. A new projection
column needs a migration and an entry in the fold's upsert (`crates/biffix-db/src/projection/`),
and the reducer change that fills it needs a replay of affected runs, not a backfill by hand.

## UI

```sh
cd ui
npm ci
npx svelte-kit sync && npm run check   # svelte-check
npm run lint                           # prettier --check
npm run test                           # vitest
npx vitest run src/lib/runs/status.test.ts   # one test file
npm run e2e                            # Playwright, against the mock API
npm run build
```

### The mock API server

`ui/mock/` is a hand-maintained mock of the backend API with fixture data (two projects, runs,
workflows, data, uploads), used for local UI work and by the Playwright suite. It needs no AWS or
Postgres:

```sh
cd ui
npm run mock                                          # :8080 (MOCK_PORT overrides it)
PUBLIC_BIFFIX_API=http://localhost:8080 npm run dev   # in another shell
```

Its responses are checked against the generated OpenAPI document (`src/mock-contract.test.ts`), and
`src/mock-coverage.test.ts` checks that every API operation has a mock route. When the API changes,
the mock must follow, or the UI tests fail.

## Infrastructure

```sh
cd infra
uv sync && npm ci
uv run ruff check . && uv run ruff format --check .
uv run validate-pyproject pyproject.toml && uv run mypy
uv run pytest -q
```

The synth tests use placeholder Lambda bundles but need a UI build to exist. Without one, create a
placeholder first: `mkdir -p ui/build && printf '<!doctype html>\n' > ui/build/index.html` from the
repository root.

## Plugins and the coordinator

```sh
cd plugins/snakemake-logger-plugin-biffix && uv sync --dev \
  && uv run ruff check . && uv run ruff format --check . \
  && uv run validate-pyproject pyproject.toml && uv run mypy && uv run pytest
cd plugins/nf-biffix && ./gradlew --no-daemon test
cd docker/coordinator && uv sync \
  && uv run ruff check . && uv run ruff format --check . \
  && uv run validate-pyproject pyproject.toml && uv run mypy \
  && MYPYPATH=tests/fakes/py uv run mypy --no-warn-unused-configs -p boto3 -p botocore \
  && uv run pytest
```

Every Python project type-checks its code and its tests with mypy's strict checks. The
coordinator's second mypy run checks its stand-in boto3 and botocore (`tests/fakes/py`) on their
own, since in the first run they would shadow the stubs the helpers are checked against.

The plugin tests read files from `schemas/`, so they run from a checkout of this repository. Each
plugin's version must match its pin in `crates/biffix-workflow/src/runtime/pins.rs`; a test checks
it. The coordinator image itself is built in CI (`.github/workflows/build-coordinator-image.yml`),
not on a laptop, because the fleet runs both x86 and Graviton.

## Generated code

Two kinds of generated files are committed, and CI fails when either drifts from its source:

- **`schemas/*.json`**, from the Rust types in `biffix-core` and `biffix-workflow`. Regenerate
  with `cargo xtask emit-schemas`; check with `cargo xtask emit-schemas --check`.
- **`ui/src/lib/generated/`**, the UI's API types, from the backend's OpenAPI document. Regenerate
  with `npm run generate:api` in `ui/` (it runs the backend's `biffix-openapi` binary through
  cargo, so it waits on the cargo lock while another cargo command runs). `check`, `test` and
  `build` use the committed files and never regenerate them.

## Changing the contract

The `RunEvent` schema, the wire formats, FormSchema and the API are public contracts (see
[architecture.md](architecture.md#the-runevent-contract)). A change to any of them:

1. Change the types in `biffix-core` (`biffix-workflow` for FormSchema and `biffix.yaml`,
   `biffix-api` for the HTTP API). Keep it additive: new optional fields, new kinds. A breaking
   change needs a contract version bump and a deprecation path, because older plugins keep
   sending the old shape.
2. Fix every struct literal the new field breaks across the workspace, then run clippy.
3. Run `cargo xtask emit-schemas` and commit `schemas/`.
4. If it changes a projection, add a migration, pin its checksum in `APPLIED`
   (`crates/biffix-db/src/migrate.rs`), add any new column to the fold's upsert, and teach the
   reducer; never patch rows.
5. If it reaches the HTTP API, run `npm run generate:api` and update `ui/mock/` to match.
6. If a plugin emits it, update the plugin and its schema conformance tests, and bump the plugin's
   pin in `crates/biffix-workflow/src/runtime/pins.rs` when its version changes.

## Pre-push hook

`.githooks/pre-push` runs the fast subset of CI: `cargo fmt`, clippy, ruff, prettier,
svelte-check, actionlint and shellcheck, each only when its toolchain is present. Enable it once
per clone:

```sh
git config core.hooksPath .githooks
```

`BIFFIX_PREPUSH_QUICK=1` skips clippy and svelte-check, which are slow on a cold build; the other
checks still run. CI runs everything else (tests, drift checks, Playwright, synth).

## Conventions

- Commit messages follow [Conventional Commits](https://www.conventionalcommits.org/): `feat:`,
  `fix:`, `docs:`, `refactor:`, `test:`, `chore:`, `ci:`.
- Signed commits are recommended.
- Rebase rather than merge to keep history linear.
- GitHub Actions are pinned by commit hash, with the version in a comment.
- Gaps that belong in Snakemake, Nextflow or their AWS executors are fixed upstream where
  possible; see [architecture.md](architecture.md#fix-it-upstream-not-in-a-fork).
- In Svelte, do not name a variable `state`: it collides with the `$state` rune.
