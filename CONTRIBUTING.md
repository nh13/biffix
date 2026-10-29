# Contributing to biffix

Thanks for considering a contribution. Bug reports, documentation fixes and code changes are all
welcome. For anything larger than a small fix, open an issue first to agree on the approach; it
saves rework on both sides.

Read [docs/architecture.md](docs/architecture.md) before changing how biffix works. Its design
principles are the rules a change is reviewed against: biffix does not schedule tasks itself,
manages AWS resources through CDK rather than its own screens, treats the run event log as the
source of truth, and relies on IAM for isolation.

## Building and testing

[docs/developing.md](docs/developing.md) has the toolchains and the commands for every part of
the repository: the Rust workspace, the Postgres-backed tests, the UI and its mock API, the CDK
app, the engine plugins and the coordinator image. The short version for Rust:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
```

CI (`.github/workflows/ci.yml`) runs every suite on each pull request, including the drift checks
for generated code; `.github/workflows/audit.yml` checks the Rust dependencies with cargo-deny.
A pull request needs a green CI run to be merged.

A pre-push hook (`.githooks/pre-push`) runs the fast subset of CI before you push;
[docs/developing.md](docs/developing.md#pre-push-hook) says how to enable it.

## Changing a contract

The `RunEvent` schema, the per-source wire formats, FormSchema, `biffix.yaml` and the HTTP API are
public contracts. Engine plugins built into an older runtime keep sending the old shapes, so a
contract change must be additive (new optional fields, new kinds) unless it comes with a version
bump and a deprecation path. The checklist in
[docs/developing.md](docs/developing.md#changing-the-contract) lists every file to update; in
brief:

1. Change the types in `biffix-core` (`biffix-workflow` for FormSchema and `biffix.yaml`,
   `biffix-api` for the HTTP API) and fix every struct literal it breaks.
2. Regenerate `schemas/` with `cargo xtask emit-schemas`.
3. For a projection change, add a numbered migration in `crates/biffix-db/migrations/`, append its
   version and checksum to `APPLIED` in `crates/biffix-db/src/migrate.rs` (the failing
   `every_migration_keeps_its_version_and_checksum` test prints it), add any new column to the
   fold's upsert in `crates/biffix-db/src/projection/`, and change the reducer. Never edit a
   migration that has been applied, and never patch projection rows by hand.
4. For an API change, run `npm run generate:api` in `ui/` and update the mock API in `ui/mock/`.
5. For a plugin change, update the plugin and its conformance tests, and its version pin in
   `crates/biffix-workflow/src/runtime/pins.rs`.

## Pull requests

- **Keep each pull request focused** on one change. Separate a refactor from the behavior change
  it enables, and a file move from edits to the moved file.
- **Include tests.** A bug fix needs a test that fails without it; a feature needs tests for its
  behavior. Documentation-only changes are the exception.
- **Update the docs** in `docs/` or `infra/README.md` when behavior, configuration or a command
  changes.
- **Use [Conventional Commits](https://www.conventionalcommits.org/)** for commit messages and the
  pull request title: `feat:`, `fix:`, `docs:`, `refactor:`, `test:`, `chore:`, `ci:`, with an
  optional scope such as `fix(ui):`.
- **Rebase, do not merge,** to bring a branch up to date.
- **Sign your commits** if you can (`git commit -S`). It is encouraged, not required.
- **Pin GitHub Actions by commit hash**, with the version in a comment.

When a change belongs in Snakemake, Nextflow or one of their AWS Batch executors, prefer fixing it
there; biffix carries a workaround only while the upstream change is in review.

## Releases

Releases are cut by the maintainer by merging the release PR that release-plz keeps open: it bumps
the Cargo workspace version and updates `CHANGELOG.md`. release-plz proposes one only when a crate
published to crates.io changes; for a release of anything else, the maintainer runs `publish.yml` by
hand, which opens the same kind of PR. On merge, the crates are published to
crates.io and the commit is tagged `v<version>`, and the release workflow builds and publishes the
Lambda bundles, the UI and the coordinator image. The "Releases" section of [infra/README.md](infra/README.md#releases)
describes what a release contains and how it is cut and deployed.

The engine plugins are versioned on their own (`plugins/*/pyproject.toml`, `build.gradle`): a
version bump merged to `main` publishes the Snakemake logger to PyPI and nf-biffix to the Nextflow
plugin registry. Publishing nf-biffix needs the `NPR_API_KEY` repository secret; without it that job
fails and the version stays unpublished until a later push to `main` with the secret set.

## Reporting security issues

Do not open a public issue for a vulnerability. Follow [SECURITY.md](SECURITY.md).

## Code of conduct

This project follows the [Contributor Covenant](CODE_OF_CONDUCT.md). By participating, you agree to
uphold it.

## License

By contributing, you agree that your contributions are licensed under the project's
[MIT License](LICENSE).
