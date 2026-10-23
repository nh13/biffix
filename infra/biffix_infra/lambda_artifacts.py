"""Shared loader for the prebuilt Rust Lambda artifacts.

Rust Lambdas are built out-of-band by `cargo lambda build` into `target/lambda/<bin>`, or
`$CARGO_TARGET_DIR/lambda/<bin>` when that is set (see infra/README). Referencing a prebuilt
artifact keeps `cdk synth` fast and Docker-free. This module is the ONE place that logic lives —
every stack with a Rust Lambda uses it — so the two guards below hold for every Lambda.

Two hard failures:

- A MISSING artifact is a hard failure, never a placeholder bootstrap. A placeholder exits non-zero
  on every invocation, and nothing looks broken: for the ingest writer, the API is healthy and runs
  execute, but every EventBridge event fails, retries, and lands in the DLQ, so no run is ever
  visible in the UI. Silent, and only findable by reading Lambda logs.

- A STALE artifact is just as silent: a binary built before a change (e.g. by `cargo build` instead
  of `cargo lambda build`) ships without it, and a new API field is simply not there. mtime is
  a deliberate heuristic — it costs nothing and an edit always bumps mtime; false positives are
  cheap (rebuild) as long as the comparison is scoped to the crates the binary is built from. It
  compares EVERY file under those crates (not just `.rs`/`.toml`: `sqlx::migrate!` embeds
  `migrations/*.sql` and `include_str!` embeds data files), the workspace files every build reads
  (`Cargo.lock`, the root `Cargo.toml`, `rust-toolchain.toml`). Integration tests
  (`<crate>/tests/`) are never linked into a binary and are skipped.

Tests and pure-synth runs opt in explicitly with `BIFFIX_ALLOW_UNBUILT_LAMBDA=1`.

A release deploy (`deploy.sh --release`, `BIFFIX_RELEASE_DIR`) takes each Lambda from the
release instead: the zip the release workflow built, verified against the release's SHA256SUMS by
`biffix_infra.release`. The mtime check does not apply to it (the zip was not built from this
tree's files); what stands in for it is that the release is this checkout's own version, which
`release.fetch` requires. A missing zip, or a directory `fetch` did not finish, is a hard failure.
"""

from __future__ import annotations

import os
from pathlib import Path

from aws_cdk import aws_lambda as lambda_

from biffix_infra import paths, release

# The repository the artifacts are built from (a module attribute so the tests can point it at a
# throwaway tree).
_REPO_ROOT = paths.REPO_ROOT

# Which crates each Lambda binary is actually built from. Scoped per binary on purpose: comparing
# against every crate in the workspace flagged the ingest artifact as stale because a file in
# `biffix-backend` had changed, a false positive that trains people to ignore the check. Every
# binary depends on `biffix-core` and `biffix-db` (the migrations, embedded at compile time, and the
# event-log writer), so a change there correctly invalidates all of them. The backend binaries
# additionally link `biffix-api` (the whole API surface, non-optional) and `biffix-workflow` (form
# schema, manifest, validation, images, runtimes); both MUST be listed, or the exact "new API field
# missing" staleness this guard exists to catch slips through for the backend Lambdas. The ingest
# binaries do not link `biffix-workflow`, so a change there leaves them fresh. (These are the
# binaries' first-party PATH deps; if that tree ever deepens, derive the closure from
# `cargo metadata` rather than extending this hand-maintained list.)
_BACKEND_CRATES = ("biffix-backend", "biffix-api", "biffix-workflow", "biffix-db", "biffix-core")
_INGEST_CRATES = ("biffix-ingest", "biffix-db", "biffix-core")
_LAMBDA_SOURCE_CRATES: dict[str, tuple[str, ...]] = {
    "biffix-backend-lambda": _BACKEND_CRATES,
    "biffix-backend-sweep": _BACKEND_CRATES,
    "biffix-ingest-lambda": _INGEST_CRATES,
    "biffix-reconcile-lambda": _INGEST_CRATES,
}
# What every binary's build reads, whatever its crates. No binary embeds a file from outside
# `crates/`: the backend's `include_str!`s of docker/coordinator/entrypoint.sh and the coordinator
# construct are in a `#[cfg(test)]` module (aws/coordinator_env.rs), not built into the Lambda.
_WORKSPACE_INPUTS = ("Cargo.lock", "Cargo.toml", "rust-toolchain.toml")
# Crate subdirectories that are not compiled into a binary.
_NOT_IN_BINARY = ("tests", "benches", "examples", "target")

_BUILD_HINT = (
    "Rebuild before deploying, e.g.:\n"
    + "".join(
        f"    cargo lambda build --release --features aws --bin {name}\n"
        for name in release.LAMBDA_BINS
    )
    + "(set BIFFIX_ALLOW_UNBUILT_LAMBDA=1 for synth-only runs and tests)"
)


def _inputs(bin_name: str) -> list[Path]:
    """Every file `bin_name` is built from (see the module docstring)."""
    paths = [_REPO_ROOT / name for name in _WORKSPACE_INPUTS]
    for crate in _LAMBDA_SOURCE_CRATES[bin_name]:
        root = _REPO_ROOT / "crates" / crate
        for source in root.rglob("*"):
            rel = source.relative_to(root).parts
            if rel[0] in _NOT_IN_BINARY or any(part.startswith(".") for part in rel):
                continue
            paths.append(source)
    return [path for path in paths if path.is_file()]


def _reject_stale_artifact(bootstrap: Path, bin_name: str) -> None:
    """Fail if a built artifact predates the sources it is built from (scoped per binary)."""
    if os.environ.get("BIFFIX_ALLOW_UNBUILT_LAMBDA") == "1":
        return
    built_at = bootstrap.stat().st_mtime
    newest_source = 0.0
    newest_path: Path | None = None
    for source in _inputs(bin_name):
        mtime = source.stat().st_mtime
        if mtime > newest_source:
            newest_source, newest_path = mtime, source
    if newest_source > built_at:
        raise RuntimeError(
            f"Rust Lambda artifact for {bin_name} is STALE: {bootstrap} was built before "
            f"{newest_path} was last modified.\n{_BUILD_HINT}"
        )


def rust_lambda_code(bin_name: str) -> lambda_.Code:
    """The prebuilt Rust Lambda artifact for `bin_name` (a provided.al2023 `bootstrap`).

    A missing artifact is a hard failure (see module docstring); a stale one is rejected too, unless
    `BIFFIX_ALLOW_UNBUILT_LAMBDA=1` opts a synth-only run out of both checks with a placeholder.
    """
    if bin_name not in _LAMBDA_SOURCE_CRATES:
        # Without its crates the staleness check could not run, so it would pass silently.
        raise ValueError(
            f"unknown Rust Lambda binary {bin_name!r}: add it to _LAMBDA_SOURCE_CRATES "
            f"(known: {sorted(_LAMBDA_SOURCE_CRATES)})"
        )
    release_dir = paths.release_dir()
    if release_dir is not None:
        return _release_code(release_dir, bin_name)
    artifact = paths.lambda_target_dir(_REPO_ROOT) / bin_name
    bootstrap = artifact / "bootstrap"
    if bootstrap.exists():
        _reject_stale_artifact(bootstrap, bin_name)
        return lambda_.Code.from_asset(str(artifact))
    if os.environ.get("BIFFIX_ALLOW_UNBUILT_LAMBDA") == "1":
        placeholder = paths.CDK_OUT / f"_placeholder_{bin_name}"
        placeholder.mkdir(parents=True, exist_ok=True)
        (placeholder / "bootstrap").write_text(
            f"#!/bin/sh\necho '{bin_name} not built' >&2\nexit 1\n"
        )
        return lambda_.Code.from_asset(str(placeholder))
    raise FileNotFoundError(
        f"Rust Lambda artifact not found: {bootstrap} does not exist.\n{_BUILD_HINT}"
    )


def _release_code(release_dir: Path, bin_name: str) -> lambda_.Code:
    """`bin_name`'s zip from a fetched release (see the module docstring)."""
    release.load_verified(release_dir)  # a ReleaseError unless `fetch` finished
    zipped = release_dir / "lambda" / f"{bin_name}.zip"
    if not zipped.is_file():
        raise FileNotFoundError(f"the release in {release_dir} has no Lambda zip {zipped.name}")
    # A .zip asset is uploaded as it is.
    return lambda_.Code.from_asset(str(zipped))
