# Local Postgres

A Postgres 16 container on `:5433` for the Postgres-backed integration tests and for running the
API server locally (Aurora stands in for it in a deploy). No AWS emulation: the ingest tests feed
synthetic SQS events straight to the drain.

## Run the Postgres-backed tests

```sh
docker compose -f dev/docker-compose.yml up -d postgres      # :5433
BIFFIX_TEST_PG=postgres://biffix:biffix@localhost:5433/biffix \
  cargo test -p biffix-backend --all-features --test it -- --include-ignored pg_store:: event_sink_pg::
BIFFIX_TEST_PG=postgres://biffix:biffix@localhost:5433/biffix \
  cargo test -p biffix-ingest --all-features --test it -- --include-ignored store_pg:: reconcile_pg:: drain_pg::
docker compose -f dev/docker-compose.yml down
```

Each test runs in its own schema, so the suites run in parallel against one database. The tests
that need Postgres are marked `#[ignore]` so the default `cargo test` stays daemon-free; CI's
`rust-pg` job runs them against a Postgres service container with the same image and credentials.

## The API server locally

The server (the `biffix-backend` bin, `src/bin/server.rs`) reads its projects from a JSON file
instead of a deploy's SSM parameters: `BIFFIX_PROJECTS_FILE` names a file holding one project
context, or an array of them, each in the shape the deploy writes to SSM (`crates/biffix-backend/tests/fixtures/project_context.json` is one).
`BIFFIX_DEFAULT_PROJECT` picks the default; unset, it is the file's first project.

```sh
BIFFIX_PROJECTS_FILE=crates/biffix-backend/tests/fixtures/project_context.json \
DATABASE_URL=postgres://biffix:biffix@localhost:5433/biffix \
BIFFIX_AUTH_DISABLED=true \
BIFFIX_WORKER_IMAGE_REPO= BIFFIX_INGEST_URL= BIFFIX_COORDINATOR_QUEUE= \
BIFFIX_BUILDS_IMAGE_REPO=111122223333.dkr.ecr.us-east-1.amazonaws.com/biffix-builds \
AWS_REGION=us-east-1 \
  cargo run -p biffix-backend --features aws --bin biffix-backend
curl -s localhost:8080/healthz
```

The deploy's variables are a contract with the CDK, so the server reads them as the Lambda does:
`BIFFIX_WORKER_IMAGE_REPO`, `BIFFIX_INGEST_URL` and `BIFFIX_COORDINATOR_QUEUE` must be set, though
blank is accepted here, and `BIFFIX_BUILDS_IMAGE_REPO` must name a repository (a made-up one is
enough until a build runs). With authentication off it listens on `127.0.0.1:8080` only. Without
`BIFFIX_CAPABILITY_SECRET_ARN` no run launches (a submit answers 503) and engine ingest is off;
`RUST_LOG` sets the log filter.
