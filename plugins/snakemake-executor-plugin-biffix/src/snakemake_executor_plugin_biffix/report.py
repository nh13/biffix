"""Each job's image, reported to whatever reads Snakemake's log records.

A logger plugin cannot ask an executor anything: it only reads log records. So, as the AWS Batch
executor reports a Batch job's state, this attaches a structured payload to an ordinary log record
under the ``extra`` key ``remote_job_state``, in that executor's payload shape
(``snakemake_executor_plugin_aws_batch/remote_state.py``, schema version 1). A job here has no
external job id, which is what tells a reader it never left the host.
"""

from __future__ import annotations

from typing import cast

from snakemake_interface_executor_plugins.jobs import (
    GroupJobExecutorInterface,
    JobExecutorInterface,
)

# The log record attribute (``extra`` key) a reader looks for.
WIRE_KEY = "remote_job_state"
SCHEMA_VERSION = 1
EXECUTOR = "biffix"


def member_jobids(job: JobExecutorInterface) -> list[int]:
    """The ids of the jobs Snakemake announces for ``job``: its own, or for a group job those of
    the jobs in it (the group's own id names no job a log record announces)."""
    if not job.is_group():
        return [job.jobid]
    # A group job is also a GroupJobExecutorInterface, and each of its jobs a job in full.
    members = cast(GroupJobExecutorInterface, job).jobs
    return [cast(JobExecutorInterface, member).jobid for member in members]


def image_payload(jobid: int, image: str, attempt: int) -> dict[str, object]:
    """The payload for one job about to start in ``image``."""
    return {
        "schema_version": SCHEMA_VERSION,
        "kind": "state",
        "jobid": jobid,
        "executor": EXECUTOR,
        "phase": "queued",
        "image": image,
        "attempt": attempt,
    }


def emit_job_image(logger: object, job: JobExecutorInterface, image: str | None) -> None:
    """Report ``image`` for ``job`` (for every job of a group job), one log record per job.
    Nothing is reported for a job without an image.

    ``logger`` is the executor's: the interface types it as its own ``LoggerExecutorInterface``,
    whose ``info`` takes the message alone, and Snakemake passes a ``logging.Logger``. Anything
    with an ``info`` that takes ``extra``, as ``logging.Logger.info`` does, gets the records; any
    other logger gets none, since the report is best-effort and must never fail the job."""
    info = getattr(logger, "info", None)
    if image is None or not callable(info):
        return
    for jobid in member_jobids(job):
        try:
            info(
                f"job {jobid} runs in {image}",
                extra={WIRE_KEY: image_payload(jobid, image, job.attempt)},
            )
        except TypeError:
            return
