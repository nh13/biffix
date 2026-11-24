"""The process a job runs as: its Snakemake command, in its image through Apptainer or, for a job
with no image, on the host."""

from __future__ import annotations

from collections.abc import Mapping, Sequence
from pathlib import Path

# The run's capability token for biffix's ingest route. The biffix logger plugin removes it from
# Snakemake's environment once read; a job must not get it from this plugin either.
INGEST_TOKEN = "BIFFIX_INGEST_TOKEN"
# What Apptainer sets in a container from its own environment: APPTAINERENV_<NAME>=<value>.
APPTAINER_ENV_PREFIX = "APPTAINERENV_"
# Every prefix Apptainer sets a container variable from, even with --cleanenv: its own and the
# legacy Singularity one.
CONTAINER_ENV_PREFIXES = (APPTAINER_ENV_PREFIX, "SINGULARITYENV_")


def host_command(command: str) -> list[str]:
    """``command`` (a shell command line) as a process on the host."""
    return ["/bin/bash", "-c", command]


def apptainer_command(command: str, image: Path, cwd: str, binds: Sequence[str]) -> list[str]:
    """``command`` (a shell command line) in the image file ``image``, as an AWS Batch job runs
    it in its container: through the image's entrypoint (``apptainer run``; a container image's
    runscript is its ENTRYPOINT, with these arguments in place of its CMD), in ``cwd``, with
    none of the caller's environment (``--cleanenv``; see :func:`job_env` for what is passed).
    ``--no-eval`` hands the entrypoint these arguments, and the container its variables, as given:
    without it Apptainer evaluates both through a shell once more, and a quote or ``$`` in the
    command would change it.

    ``cwd`` is bound as the container's home, as Snakemake's own Apptainer support binds it: an
    image file is read-only, and this gives the job a home it can write to. ``binds`` are bound
    too. ``apptainer`` is found on PATH, where the biffix coordinator puts its wrapper.
    """
    argv = ["apptainer", "run", "--cleanenv", "--no-eval", "--home", cwd, "--pwd", cwd]
    for bind in binds:
        argv += ["--bind", bind]
    return [*argv, str(image), "/bin/bash", "-c", command]


def job_env(
    environ: Mapping[str, str], envvars: Mapping[str, str], contained: bool
) -> dict[str, str]:
    """The environment of a job's process: ``environ`` without the ingest token, and ``envvars``
    (the variables Snakemake passes to jobs: a workflow's ``envvars:``, ``--envvars`` and the
    storage plugins' own).

    A job in a container (``contained``) starts with a clean environment, so each of ``envvars``
    is named to Apptainer to set there; the rest of ``environ`` reaches Apptainer only, without
    the variables it would set in the container (CONTAINER_ENV_PREFIXES): ``envvars`` are the only
    ones set there. A job on the host has ``environ`` itself. No job gets the ingest token, under
    its own name or a container prefix.
    """
    tokens = {INGEST_TOKEN, *(prefix + INGEST_TOKEN for prefix in CONTAINER_ENV_PREFIXES)}
    env = {name: value for name, value in environ.items() if name not in tokens}
    passed = {name: value for name, value in envvars.items() if name != INGEST_TOKEN}
    if contained:
        env = {
            name: value
            for name, value in env.items()
            if not name.startswith(CONTAINER_ENV_PREFIXES)
        }
        env.update({APPTAINER_ENV_PREFIX + name: value for name, value in passed.items()})
    else:
        env.update(passed)
    return env


def group_memory_mb(is_group: bool, resources: Mapping[str, object]) -> int | None:
    """The memory the main process reserved for a group job (its merged ``mem_mb``), which its own
    Snakemake schedules the group's jobs within; None for a job of its own, or one with no memory
    reserved. ``resources`` maps a name to its value, or (Snakemake 9.x later releases) to an
    object holding it as ``value``."""
    if not is_group:
        return None
    mem = resources.get("mem_mb")
    value = getattr(mem, "value", mem)
    if isinstance(value, bool) or not isinstance(value, int | float) or value <= 0:
        return None
    return int(value)


def group_resource_args(is_group: bool, resources: Mapping[str, object]) -> str:
    """The ``--resources`` a group job's own Snakemake schedules the group's jobs within: its
    reserved memory (group_memory_mb), which nothing else bounds there when the executor interface
    passes a spawned job no resources of its own (9.4 and later). ``""`` for a job of its own, or
    one with no memory reserved."""
    mem = group_memory_mb(is_group, resources)
    return f"--resources mem_mb={mem}" if mem is not None else ""
