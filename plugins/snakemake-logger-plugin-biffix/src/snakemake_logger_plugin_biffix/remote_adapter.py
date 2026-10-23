# Portions of this module are adapted from snakesee (https://github.com/fg-labs/snakesee), its
# snakemake-logger-plugin-snakesee/src/snakemake_logger_plugin_snakesee/remote_adapter.py.
# snakesee is Copyright (c) 2024 Fulcrum Genomics LLC and licensed under the MIT License; see the
# NOTICE section of this package's README.md for its license text.
"""Extract AWS-Batch join fields from a remote-executor log record.

Adapted from snakesee's remote_adapter (no snakesee dependency). A remote executor attaches a
structured payload to a log record under a well-known key, because Snakemake's LogEvent enum is
fixed upstream and an executor cannot extend it. We read the ``batch_job_id`` (and attempt) it
carries so a ``task_submitted`` can publish the binding. The rest of the AWS join —
exit code, spot, instance — comes from EventBridge, not here.

The source of truth for the payload is the pinned AWS Batch executor's
``remote_state.build_payload`` (``EXECUTOR_SHA`` in ``docker/coordinator/wheels.env``): key
``remote_job_state``, ``attempt`` = Snakemake's 1-based attempt (a ``--retries`` resubmission is a
new Batch job), and AWS's own retryStrategy count separately as ``batch_attempts``.
``tests/test_events.py`` pins the pinned shape.
"""

from __future__ import annotations

from typing import Final

# Recognized wire keys.
WIRE_KEYS: Final[tuple[str, ...]] = ("remote_job_state",)
SUPPORTED_SCHEMA_VERSIONS: Final[frozenset[int]] = frozenset({1, 2})


def payload_from_record(record: object) -> dict[str, object] | None:
    """Return the remote-state payload dict on the record, or None."""
    for key in WIRE_KEYS:
        payload = getattr(record, key, None)
        if isinstance(payload, dict):
            return payload
    return None


def batch_binding(payload: dict[str, object]) -> tuple[str, int] | None:
    """Extract ``(batch_job_id, attempt)`` from a remote payload, or None if it carries no binding.

    Only the *external* job id (the AWS Batch job id) and the attempt matter to biffix: they let
    ``task_submitted`` bind ``batch_job_id → (run_id, task_key)``. Returns None for a payload with
    an unsupported ``schema_version`` or no ``external_jobid`` (never raises). Any payload that does
    carry an ``external_jobid`` yields a binding regardless of phase — re-emitting the same
    ``task_submitted`` is a no-op downstream (the log dedupes on the deterministic key), so no
    phase/kind filter is needed here.
    """
    if payload.get("schema_version") not in SUPPORTED_SCHEMA_VERSIONS:
        return None
    external = payload.get("external_jobid")
    if not external:
        return None
    attempt = payload.get("attempt")
    # Attempt is 1-based; coerce anything non-int / bool / < 1 to the first attempt.
    if isinstance(attempt, bool) or not isinstance(attempt, int) or attempt < 1:
        attempt = 1
    return str(external), attempt


def batch_image(payload: dict[str, object]) -> str | None:
    """The container image the Batch job runs (``describe_jobs`` ``container.image``), when the
    payload carries it as ``image``, else None.

    Optional in the payload contract: the executor pinned at ``EXECUTOR_SHA`` does not send it yet
    (``remote_state.build_payload`` would add ``container.get("image")``), so until it does
    ``task_submitted`` goes without an image rather than one the logger guessed."""
    image = payload.get("image")
    return image if isinstance(image, str) and image else None
