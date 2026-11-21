"""biffix Snakemake executor plugin (Snakemake 9): every job of a run on the host Snakemake runs
on, each in the container image its AWS Batch job would run.

It is a remote executor that submits nowhere. Being remote is what makes Snakemake form group
jobs and merge their resources, exactly as it does for the AWS Batch executor; each job, or group
job, is then one process on this host: Snakemake's own job command (``python -m snakemake …``,
from the executor interface's ``format_job_exec``), inside the job's image through Apptainer.
Snakemake's scheduler decides what runs when, within the cores and resources it was given.

It uses only ``snakemake-interface-executor-plugins``, never Snakemake's own modules.
"""

from __future__ import annotations

import os
import signal
import subprocess
import threading
import time
from collections.abc import AsyncGenerator
from pathlib import Path

from snakemake_interface_common.exceptions import WorkflowError
from snakemake_interface_executor_plugins.executors.base import SubmittedJobInfo
from snakemake_interface_executor_plugins.executors.remote import RemoteExecutor
from snakemake_interface_executor_plugins.jobs import JobExecutorInterface
from snakemake_interface_executor_plugins.settings import CommonSettings

from snakemake_executor_plugin_biffix.command import apptainer_command, host_command, job_env
from snakemake_executor_plugin_biffix.images import ImageMap, image_file, job_image
from snakemake_executor_plugin_biffix.report import emit_job_image
from snakemake_executor_plugin_biffix.settings import ExecutorSettings

__all__ = ["Executor", "ExecutorSettings", "common_settings"]

# How often the jobs' processes are polled. A poll asks nothing of any service, so it is far
# shorter than Snakemake's default for remote executors (10 s), which would add that to every job.
POLL_SECONDS = 0.5
# How long a cancelled job has to stop on SIGTERM before it is killed.
CANCEL_GRACE_SECONDS = 5.0


common_settings = CommonSettings(
    # Remote, so that Snakemake forms group jobs and merges their resources as it does for AWS
    # Batch; and so that each job is a Snakemake of its own, which can run inside an image.
    non_local_exec=True,
    # The jobs run on this host: the working directory, the sources and the local copies of
    # storage files are the ones the main process uses.
    implies_no_shared_fs=False,
    job_deploy_sources=False,
    pass_default_storage_provider_args=True,
    pass_default_resources_args=True,
    # Passed into the container by name (command.job_env), as the AWS Batch executor passes them
    # in the job's environment, never on its command line.
    pass_envvar_declarations_to_cmd=False,
    # The image carries the storage plugin, as an AWS Batch job's does.
    auto_deploy_default_storage_provider=False,
    init_seconds_before_status_checks=0,
)


class Executor(RemoteExecutor):
    """Runs each job as a process on this host, in its image."""

    def __post_init__(self) -> None:
        settings: ExecutorSettings = self.executor_settings
        if settings.images_dir is None:
            raise WorkflowError("the biffix executor needs --biffix-images-dir")
        self.images_dir = Path(settings.images_dir)
        self.default_image = settings.default_image or None
        self.binds = list(settings.bind)
        self.image_map: ImageMap | None = None
        if settings.image_map is not None:
            try:
                self.image_map = ImageMap.from_file(str(settings.image_map))
            except (OSError, ValueError) as e:
                raise WorkflowError(
                    f"Failed to read the container image map {settings.image_map}: {e}"
                ) from e
        # Every job's process still running, by pid. Kept here, not read from the interface's own
        # list of active jobs, which is empty while a status check is in progress.
        self._processes: dict[int, subprocess.Popen[bytes]] = {}
        self._processes_lock = threading.Lock()
        self._cancelled = False
        # Each job's image, resolved when the job is announced (run_job_pre) and used when it is
        # started (run_job): a WorkflowError when the job cannot run.
        self._images: dict[int | str, Path | None | WorkflowError] = {}
        self.next_seconds_between_status_checks = POLL_SECONDS

    # Every job's own Snakemake, a group job's included, gets the main process's --cores (the
    # interface's own cores and get_job_args): it is what a workflow reads as workflow.cores, so a
    # DAG, threads or params built from it must read the same in the job as in the main process. A
    # group job's Snakemake therefore schedules the group's jobs within the host's cores, not the
    # width the main process reserved for the group job (its threads), and may run more threads
    # at once than that width: on AWS Batch too it gets the main process's --cores, but there its
    # container's vCPUs bound what it uses, and here nothing does. No option bounds a Snakemake's
    # threads but --cores, which is workflow.cores (--jobs bounds how many jobs, whatever their
    # threads; --max-threads one job's threads; --resources refuses _cores).

    def envvars(self) -> dict[str, str]:
        """The variables Snakemake passes to jobs, with every one the workflow registered (its
        ``envvars:``, or ``workflow.register_envvars``): Snakemake 9.6 and later pass those to a
        remote executor's jobs themselves, earlier Snakemake 9 releases only ``--envvars`` and the
        storage plugins'. The registered ones are read from the workflow (which the interface
        does not declare), so a Snakemake without them passes what it passes."""
        passed = dict(super().envvars())
        for name in getattr(self.workflow, "envvars", ()):
            if name in os.environ:
                passed.setdefault(name, os.environ[name])
        return passed

    def get_python_executable(self) -> str:
        """The job's Snakemake is the image's own: ``python`` on the PATH its entrypoint leaves,
        as for an AWS Batch job, not this process's interpreter."""
        return "python"

    def run_job_pre(self, job: JobExecutorInterface) -> None:
        """Resolve the job's image and report it before Snakemake announces the job, so that a
        logger plugin knows the image of every job the announcement names."""
        self._images[job.jobid] = self._resolve_image(job)
        super().run_job_pre(job)

    def _resolve_image(self, job: JobExecutorInterface) -> Path | None | WorkflowError:
        """The image file ``job`` runs from (None: it runs without a container), reporting the
        image; or the WorkflowError that says why the job cannot run."""
        try:
            image = job_image(job.resources, job.name, self.default_image, self.image_map)
            file = None if image is None else image_file(self.images_dir, image, job.name)
        except WorkflowError as e:
            return e
        emit_job_image(self.logger, job, image)
        return file

    def run_job(self, job: JobExecutorInterface) -> None:
        """Start the job's Snakemake as a process of its own (and a process group of its own, so
        that a cancel stops what the job started too)."""
        image = self._images.pop(job.jobid)
        if isinstance(image, WorkflowError):
            # Fail this job like any other failed job: the jobs already running finish, and
            # --keep-going and --retries apply.
            self.report_job_error(SubmittedJobInfo(job=job), msg=str(image))
            return
        command = self.format_job_exec(job)
        # Where Snakemake started, as Snakemake's own local executor starts a job's Snakemake
        # there: a `workdir:` has moved the main process since, and the job's Snakemake reads the
        # workflow, and applies its `workdir:`, again.
        cwd = str(self.workflow.workdir_init)
        argv = (
            host_command(command)
            if image is None
            else apptainer_command(command, image, cwd, self.binds)
        )
        env = job_env(os.environ, self.envvars(), contained=image is not None)
        with self._processes_lock:
            # Not started after a cancel, and not reported either, as a job the cancel stopped is
            # not (check_active_jobs): the scheduler takes no more results, and the job did not
            # fail. (Snakemake cancels from the thread that runs jobs, so this is only a guard.)
            if self._cancelled:
                return
            try:
                process = subprocess.Popen(
                    argv, cwd=cwd, env=env, stdin=subprocess.DEVNULL, start_new_session=True
                )
            except OSError as e:
                self.report_job_error(
                    SubmittedJobInfo(job=job), msg=f"could not start {argv[0]}: {e}"
                )
                return
            self._processes[process.pid] = process
        self.report_job_submission(
            SubmittedJobInfo(job=job, external_jobid=str(process.pid), aux={"process": process})
        )

    # The interface declares a coroutine that returns a generator, but iterates this with
    # `async for`: an async generator is what it takes.
    async def check_active_jobs(  # type: ignore[override]
        self, active_jobs: list[SubmittedJobInfo]
    ) -> AsyncGenerator[SubmittedJobInfo, None]:
        """Report every job whose process has exited, and yield the rest."""
        self.next_seconds_between_status_checks = POLL_SECONDS
        for job_info in active_jobs:
            process: subprocess.Popen[bytes] = (job_info.aux or {})["process"]
            status = process.poll()
            if status is None:
                yield job_info
                continue
            with self._processes_lock:
                self._processes.pop(process.pid, None)
                cancelled = self._cancelled
            if cancelled:
                # Stopped by the cancel (cancel_jobs) rather than failed: the scheduler has
                # stopped and takes no more results, and a report would log every stopped job as
                # a failed one.
                continue
            if status == 0:
                self.report_job_success(job_info)
            else:
                self.report_job_error(job_info, msg=f"The job's process {_ended(status)}.")

    def print_job_error(
        self, job_info: SubmittedJobInfo, msg: str | None = None, **kwargs: object
    ) -> None:
        """Log the job's failure without the remote executors' pointer to a cluster or cloud log:
        there is none, and the job's output is in Snakemake's own."""
        job_info.job.log_error(msg, **kwargs)

    def cancel_jobs(self, active_jobs: list[SubmittedJobInfo]) -> None:  # noqa: ARG002
        """Stop every job still running: SIGTERM to each job's process group, then SIGKILL to
        the groups that still have a process after CANCEL_GRACE_SECONDS.

        A group is signalled by its id, the job's process's pid, even once that process has
        exited and been reaped (a job's Snakemake stops on SIGTERM; a tool it started may not): a
        process group's id is not reused while any process of the group lives, and every job here
        was still running, or had only just ended, when the cancel began. So only a group found
        alive at the last check is killed: an empty one's id may since have been reused."""
        with self._processes_lock:
            self._cancelled = True
            processes = list(self._processes.values())
            self._processes.clear()
        for process in processes:
            _signal_group(process.pid, signal.SIGTERM)
        deadline = time.monotonic() + CANCEL_GRACE_SECONDS
        alive = processes
        while alive:
            alive = [p for p in alive if _group_alive(p)]
            if not alive or time.monotonic() >= deadline:
                break
            time.sleep(0.05)
        for process in alive:
            _signal_group(process.pid, signal.SIGKILL)
        for process in processes:
            process.wait()


def _signal_group(pgid: int, signum: int) -> None:
    """Send ``signum`` to process group ``pgid``, if any process of it is left."""
    try:
        os.killpg(pgid, signum)
    except (ProcessLookupError, PermissionError):
        pass


def _group_alive(process: subprocess.Popen[bytes]) -> bool:
    """Whether any process of the group ``process`` leads is left (``process`` itself is reaped
    first: until then it would count, exited or not). The biffix coordinator's conversion keeps
    a copy (docker/coordinator/lib/single_job.py ``_group_alive``): its Nextflow runs have no
    Snakemake, nor this package, to import it from."""
    process.poll()
    try:
        os.killpg(process.pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        pass  # a process of the group that is not ours to signal: the group lives
    return True


def _ended(status: int) -> str:
    """How a process that returned ``status`` ended, for a message."""
    if status < 0:
        return f"was stopped by signal {-status}"
    return f"exited {status}"
