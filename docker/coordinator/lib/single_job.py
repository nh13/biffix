"""A single-job run's task images, converted to Apptainer image files before the engine starts and
put where each engine looks for them; and, for Snakemake, the wrapper Snakefile that checks every
rule's image before any job starts and sets the host's memory budget.

Usage: single_job.py convert <images.json> <images dir> <log dir> <failure file> [<image>...]
       single_job.py snakefile-wrapper <snakefile> <wrapper> <images.json> <images dir> \
           <default image> check|no-check <task memory MiB> [<variable>...]

<images.json> is the run's image map (declared image -> the tenant image to run), or "" for none;
each <image> is one more tenant image the run may name as itself (the version's default image).

`convert` pulls every tenant image (the map's distinct values and the <image>s) into <images dir>
as `apptainer pull` does, several at once (BIFFIX_SIF_PARALLEL, default 4), each named as Nextflow
names an image file in its `apptainer.libraryDir` (image_file_name). An image in the region's ECR
(BIFFIX_REGION) is pulled with an ECR login token, which no other registry is sent and nothing logs.
The first image that cannot be pulled stops the rest: the reason goes to <failure file> and stderr,
and the exit status is 1; so does any other error (a malformed map). A SIGTERM (a cancel) stops
every pull too, and the exit status is then 143, with no failure recorded. Each pull's own output
is in <log dir>.

`snakefile-wrapper` exclusively creates <wrapper> ("" chooses a unique sibling of <snakefile>)
and prints its path. It includes <snakefile>, keeping the workflow's own default target
(`workflow.include(..., overwrite_default_target=True)`). In Snakemake's main process it also,
before the workflow is parsed, caps the memory of the jobs running at once at <task memory MiB> on
top of the workflow profile's own resource limits, scopes memory and disk to the whole host beside
the profile's own scopes, and clears the remote-execution `nodes` limit that would lift
Snakemake's CPU budget (configure_local_resources); registers each <variable> with Snakemake as one
every job gets, beside a profile's `envvars:`; and, once the workflow is parsed, checks every
rule's image (check_rule_images): a rule whose image the executor would refuse for its jobs fails
the run before any job starts, as does (with `check`) an image file the conversion did not leave
in <images dir>. The image each job runs in is chosen later, per
job and group job, by the biffix executor plugin (plugins/snakemake-executor-plugin-biffix), from
the same map and files. A job's own Snakemake, which the executor starts in the job's image, loads
the wrapper too and only includes the workflow: this file is not in a task image.
"""

from __future__ import annotations

import base64
import json
import os
import re
import shutil
import signal
import subprocess
import sys
import tempfile
import threading
import time
from collections.abc import Callable, Iterable, Mapping
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
from typing import Any

DEFAULT_PARALLEL = 4
# The bytes of a failed pull's output quoted in the failure.
FAILURE_TAIL_BYTES = 4096
# How long a stopped pull's processes have to end on SIGTERM before they are killed.
STOP_GRACE_SECONDS = 2.0
# How often a worker checks on its child once the conversion has failed.
STOP_POLL_SECONDS = 0.05
# A key naming the run's default image, for people reading the map (crates/biffix-backend
# images/map.rs); never a container a workflow names.
RESERVED_KEY_PREFIX = "$"


def read_map(path: str) -> dict[str, str]:
    """The image map at `path` ({} for ""), keeping only string values: the images to convert."""
    if not path:
        return {}
    with open(path) as fh:
        raw: object = json.load(fh)
    if not isinstance(raw, dict):
        raise ValueError(f"the image map {path} is not a JSON object")
    return {str(k): v for k, v in raw.items() if isinstance(v, str) and v}


def image_file_name(image: str) -> str:
    """The file Nextflow looks for in `apptainer.libraryDir` for a tenant image: its
    `SingularityCache.simpleName` of `docker://<image>`, the name Nextflow's Apptainer resolution
    gives a reference without a scheme (ContainerHandler.normalizeApptainerImageName)."""
    name = image
    extension = ".img"
    if ".sif:" in name:
        extension = ".sif"
        name = name.replace(".sif:", "-")
    elif name.endswith(".sif"):
        extension = ".sif"
        name = name[:-4]
    return name.replace(":", "-").replace("/", "-") + extension


def tenant_images(image_map: dict[str, str], extra: Iterable[str]) -> list[str]:
    """Every image the run may run: the map's distinct values and `extra`, sorted."""
    return sorted({*image_map.values(), *(e for e in extra if e)})


def ecr_host(region: str) -> re.Pattern[str]:
    """A registry host in the region's ECR, of any account."""
    return re.compile(rf"^[0-9]{{12}}\.dkr\.ecr\.{re.escape(region)}\.amazonaws\.com$")


def registry_of(image: str) -> str:
    """The registry host of a reference with one ("" for a Docker Hub short name)."""
    first, sep, _ = image.partition("/")
    return first if sep and ("." in first or ":" in first or first == "localhost") else ""


def ecr_login(region: str) -> tuple[str, str]:
    """The user name and password for the region's ECR (a token good for 12 hours)."""
    import boto3

    data = boto3.client("ecr", region_name=region).get_authorization_token()["authorizationData"]
    token = data[0].get("authorizationToken", "") if data else ""
    user, _, password = base64.b64decode(token).decode().partition(":")
    if not user or not password:
        raise ValueError("ECR returned no authorization token")
    return user, password


class Conversion:
    """Pulls images into image files, several at once, stopping every pull at the first failure."""

    def __init__(self, apptainer: str, images_dir: Path, log_dir: Path, env: dict[str, str]):
        self.apptainer, self.images_dir, self.log_dir, self.env = (
            apptainer,
            images_dir,
            log_dir,
            env,
        )
        self.failed = threading.Event()
        self.failure = ""
        self.stopped_by: int | None = None
        # Taken by the workers, and on the main thread only by the signal handler (stop): never
        # by the main thread's own code, which a SIGTERM could interrupt while it held it.
        self._lock = threading.Lock()
        self._running: set[subprocess.Popen[bytes]] = set()
        # When a child the failure stopped is killed if it has not ended (monotonic time; set
        # with `failed`).
        self._kill_at = 0.0

    def login(self, region: str) -> dict[str, str]:
        """Fetch ECR credentials in a cancellable child; call from a worker like pull().

        SDK requests can block through SIGTERM, so the parent supervises a process, not a
        thread running the SDK. Its stdout is a private pipe, never an image log or an error.
        """
        with self._lock:
            if self.failed.is_set():
                return {}
            proc = subprocess.Popen(
                [sys.executable, str(Path(__file__).resolve()), "_ecr-login", region],
                stdin=subprocess.DEVNULL,
                stdout=subprocess.PIPE,
                stderr=subprocess.DEVNULL,
                env=self.env,
                start_new_session=True,
            )
            self._running.add(proc)
        try:
            output = self._supervise(proc)
        finally:
            self._retire(proc)
        if self.failed.is_set():
            return {}
        if proc.returncode != 0:
            raise RuntimeError(f"ECR authorization request exited {proc.returncode}")
        user, password = json.loads(output)
        return {"APPTAINER_DOCKER_USERNAME": user, "APPTAINER_DOCKER_PASSWORD": password}

    def pull(self, image: str, credentials: dict[str, str]) -> None:
        """Pull one image (unless another has failed), recording why when it fails."""
        # Imported here, not at the top: the wrapper Snakefile loads this module without lib/ on
        # sys.path, and only `convert` (run as a script, from lib/) reads a log.
        from run_logs import tail_bytes

        if self.failed.is_set():
            return
        name = image_file_name(image)
        dest = self.images_dir / name
        partial = self.images_dir / f".{name}.pulling"
        log = self.log_dir / f"{name}.log"
        with open(log, "wb") as out:
            with self._lock:
                if self.failed.is_set():
                    return
                proc = subprocess.Popen(
                    [self.apptainer, "pull", str(partial), f"docker://{image}"],
                    stdout=out,
                    stderr=subprocess.STDOUT,
                    stdin=subprocess.DEVNULL,
                    env=self.env | credentials,
                    # Its own process group, which fail() stops whole: Apptainer's children
                    # (mksquashfs) would outlive a signal to the apptainer process alone.
                    start_new_session=True,
                )
                self._running.add(proc)
            produced = False
            try:
                self._supervise(proc)
                produced = proc.returncode == 0 and partial.is_file()
            finally:
                status = proc.returncode

                def why() -> str:
                    # Run by _retire under the lock: a read error must not cut short its cleanup
                    # (the group's kill, the partial image's removal) nor the failure itself.
                    try:
                        tail = tail_bytes(log, FAILURE_TAIL_BYTES).decode(errors="replace")
                    except OSError as e:
                        tail = f"(its log {log} could not be read: {e})"
                    tail = tail.rstrip()
                    return (
                        f"could not convert the task image {image} (apptainer pull exited "
                        f"{status}):\n{tail}"
                    )

                # A pull that ended without its image is the conversion's failure, recorded while
                # its group is still tracked (so the failure stops it too).
                self._retire(proc, None if produced or status is None else why)
        if produced:
            os.replace(partial, dest)
            print(f"biffix-coordinator: converted {image} -> {dest}", file=sys.stderr)
            return
        partial.unlink(missing_ok=True)

    def fail(self, why: str) -> None:
        """Record the first failure and stop every pull still running (SIGTERM; each worker kills
        its own child's group once the grace period is up: _supervise, _retire)."""
        with self._lock:
            self._fail_locked(why)

    def _fail_locked(self, why: str) -> None:
        """fail(), with the lock held."""
        if self.failed.is_set():
            return
        self.failure = why
        self._kill_at = time.monotonic() + STOP_GRACE_SECONDS
        self.failed.set()
        for proc in self._running:
            # Every child here was running until at most a moment ago (its worker takes it out
            # of the set, _retire, right after its wait returns, or keeps it while its group
            # gets the grace period), and a group's id is not reused while any process of it
            # lives: so the group is the pull's, even if apptainer itself was just reaped and
            # only its children (mksquashfs) are left.
            try:
                os.killpg(proc.pid, signal.SIGTERM)
            except (ProcessLookupError, PermissionError):
                continue  # the group is gone, or its last members are exiting

    def _retire(
        self, proc: subprocess.Popen[bytes], failure: Callable[[], str] | None = None
    ) -> None:
        """Take `proc` (just waited for) out of the running children: first recording `failure`
        (its why, when it failed the conversion) unless another failure came first, under the
        same lock as fail() and stop(). A child no failure or cancel has claimed simply leaves.
        One claimed (by this failure, another, or a cancel: whichever came first, even after its
        own wait returned) stays tracked while whatever is left of its group, a child that
        ignores SIGTERM say, gets the grace period: then it is killed. So the conversion never
        returns with a claimed child's process still running. Its group id is not reused while
        any member lives."""
        with self._lock:
            if failure is not None and not self.failed.is_set():
                self._fail_locked(failure())
            if not self.failed.is_set():
                self._running.discard(proc)
                return
        try:
            while _group_alive(proc.pid):
                if time.monotonic() >= self._kill_at:
                    _kill_group(proc.pid)
                    break
                time.sleep(STOP_POLL_SECONDS)
        finally:
            with self._lock:
                self._running.discard(proc)

    def _supervise(self, proc: subprocess.Popen[bytes]) -> bytes:
        """Wait for `proc` (a login or a pull, the leader of a process group of its own) and
        return its stdout (b"" when it has none). Once the conversion has failed, its group has
        until the grace period after the failure's SIGTERM is up: then whatever is left of it, the
        child itself or what it started (mksquashfs), is killed, whether or not the child has
        ended. Bounded on its own, so no other worker's child keeps it, or the conversion,
        waiting. Run on a worker, never on the main thread."""
        output = b""
        while True:
            try:
                output, _ = proc.communicate(timeout=STOP_POLL_SECONDS)
                break
            except subprocess.TimeoutExpired:
                if self.failed.is_set() and time.monotonic() >= self._kill_at:
                    _kill_group(proc.pid)
        # The child has ended; a process it started may not have.
        while self.failed.is_set() and _group_alive(proc.pid):
            if time.monotonic() >= self._kill_at:
                _kill_group(proc.pid)
                break
            time.sleep(STOP_POLL_SECONDS)
        return output or b""

    def stop(self, signum: int) -> None:
        """A signal handler: stop the login and every pull, without recording a run failure."""
        # The handler runs on the main thread, which otherwise never takes the lock (the workers
        # kill what the SIGTERM leaves: _supervise), so a first SIGTERM at most waits for a worker
        # to release it. A second one can interrupt the first handler while that holds the
        # (non-reentrant) lock in fail(), and must return rather than wait on it forever: the
        # first handler stops every child.
        if self.stopped_by is not None:
            return
        self.stopped_by = signum
        self.fail(f"stopped by signal {signum}")


# Registry logins a caller's environment can carry: Apptainer's own, the Singularity names it still
# honours, and Docker's, dropped too so no tool a pull runs can pick one up.
REGISTRY_LOGIN_ENV = tuple(
    f"{prefix}DOCKER_{what}"
    for prefix in ("APPTAINER_", "SINGULARITY_", "")
    for what in ("USERNAME", "PASSWORD")
)


def pull_env(environ: Mapping[str, str]) -> dict[str, str]:
    """The environment of a pull: the caller's, less any registry login it had, which must never
    reach a registry it was not meant for (an image's own login is added per pull)."""
    return {k: v for k, v in environ.items() if k not in REGISTRY_LOGIN_ENV}


def convert(
    map_path: str, images_dir: str, log_dir: str, failure_file: str, extra: list[str]
) -> int:
    """Convert every tenant image the run may name; 0 when all are converted, 128 + the signal's
    number when one stopped it, else 1 (any error, its reason in `failure_file`)."""
    try:
        return _convert(map_path, images_dir, log_dir, failure_file, extra)
    except Exception as e:
        return _failed(failure_file, f"could not convert the task images: {e}")


def _convert(
    map_path: str, images_dir: str, log_dir: str, failure_file: str, extra: list[str]
) -> int:
    images = tenant_images(read_map(map_path), extra)
    names: dict[str, str] = {}
    for image in images:
        other = names.setdefault(image_file_name(image), image)
        if other != image:
            return _failed(
                failure_file,
                f"the task images {other} and {image} would share the image file "
                f"{image_file_name(image)}",
            )
    parallel = _parallel(os.environ.get("BIFFIX_SIF_PARALLEL", ""))
    apptainer = shutil.which("apptainer")
    if apptainer is None:
        return _failed(failure_file, "this image has no apptainer to convert the task images with")
    for d in (images_dir, log_dir):
        os.makedirs(d, exist_ok=True)
    conversion = Conversion(apptainer, Path(images_dir), Path(log_dir), pull_env(os.environ))
    # The main thread never holds conversion's lock, including while starting the login child or
    # once every pull has returned (each worker kills its own stopped child: _supervise), so the
    # signal handler can safely stop every child under that lock.
    previous_handler = signal.signal(signal.SIGTERM, lambda signum, _frame: conversion.stop(signum))
    try:
        region = os.environ.get("BIFFIX_REGION", "")
        ecr = ecr_host(region)
        credentials: dict[str, str] = {}
        with ThreadPoolExecutor(max_workers=min(parallel, max(1, len(images)))) as pool:
            if any(ecr.match(registry_of(image)) for image in images):
                credentials = pool.submit(_login_guarded, conversion, region).result()
            if not conversion.failed.is_set():
                print(
                    f"biffix-coordinator: converting {len(images)} task image(s), "
                    f"{parallel} at a time",
                    file=sys.stderr,
                )
                for image in images:
                    creds = credentials if ecr.match(registry_of(image)) else {}
                    pool.submit(_guarded, conversion, image, creds)
        if conversion.stopped_by is not None:
            print("biffix-coordinator: the task image conversion was stopped", file=sys.stderr)
            return 128 + conversion.stopped_by
        if conversion.failed.is_set():
            return _failed(failure_file, conversion.failure)
        return 0
    finally:
        signal.signal(signal.SIGTERM, previous_handler)


def _group_alive(pgid: int) -> bool:
    """Whether process group `pgid` still has a process. A copy of the biffix executor plugin's
    (plugins/snakemake-executor-plugin-biffix `_group_alive`), which `convert` cannot import: a
    Nextflow run's python3 has no Snakemake runtime, nor that plugin."""
    try:
        os.killpg(pgid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        pass  # a process of the group that is not ours to signal: the group lives
    return True


def _kill_group(pgid: int) -> None:
    """SIGKILL process group `pgid`, if any process of it is left."""
    try:
        os.killpg(pgid, signal.SIGKILL)
    except (ProcessLookupError, PermissionError):
        pass


def _parallel(raw: str) -> int:
    """BIFFIX_SIF_PARALLEL: how many images are pulled at once (DEFAULT_PARALLEL when unset)."""
    if not raw:
        return DEFAULT_PARALLEL
    if not raw.isdigit() or int(raw) < 1:
        raise ValueError(f"BIFFIX_SIF_PARALLEL must be a whole number of at least 1: {raw!r}")
    return int(raw)


def _login_guarded(conversion: Conversion, region: str) -> dict[str, str]:
    """Record login errors in the worker, where taking the signal handler's lock is safe."""
    try:
        return conversion.login(region)
    except Exception:
        conversion.fail(f"could not log in to ECR in {region}: authorization request failed")
        return {}


def _guarded(conversion: Conversion, image: str, credentials: dict[str, str]) -> None:
    """One pull, any error in it recorded as the run's failure (an executor would swallow it)."""
    try:
        conversion.pull(image, credentials)
    except Exception as e:
        conversion.fail(f"could not convert the task image {image}: {e}")


def _failed(failure_file: str, why: str) -> int:
    """Report `why` on stderr and in `failure_file`; 1. A file that cannot be written (its
    filesystem full, say) is noted on stderr, never raised in place of the failure."""
    print(f"biffix-coordinator: {why}", file=sys.stderr)
    try:
        Path(failure_file).write_text(why + "\n")
    except OSError as e:
        print(f"biffix-coordinator: could not write {failure_file}: {e}", file=sys.stderr)
    return 1


def is_function(resource: Any) -> bool:
    """Whether a rule's resource (Snakemake's `Resource`, or the raw value older Snakemake 9
    releases keep) is set by a function."""
    return callable(resource) or (hasattr(resource, "is_evaluable") and resource.is_evaluable())


def check_rule_images(
    workflow: Any, map_path: str, images_dir: str, default_image: str, check_files: bool
) -> None:
    """Run from the wrapper Snakefile once the workflow is included: check that every rule, module
    rules (`use rule`) included, names an image the run can run, before any job starts. The biffix
    executor plugin chooses each job's image when the job starts, and would only fail that one
    job, perhaps hours in. So each rule's image is chosen here by the plugin's own functions
    (`job_image`, then `image_file`), from what the executor is given (the map's file as it is,
    `default_image`, the image files in `images_dir`). Raises Snakemake's WorkflowError naming,
    each with its remedy, every rule whose resource is set by a function (which cannot be read
    before the run), names the map's reserved key, or names an image the executor would refuse
    (one the map lacks, or a resource that is no reference, "" included); and, when
    `check_files` (not in a dry run, which converts nothing), every image file missing from
    `images_dir`.

    An ungrouped local rule is not checked: it runs in the engine, as it does beside a Batch run.
    Which image a group job runs in, and whether its jobs may share one, is Snakemake's to decide
    when it forms the group job, as for AWS Batch. A rule is taken to be in a group by its
    Snakefile's `group:` alone: Snakemake installs the groups of `--groups` (or a profile's
    `groups:`) only after the workflow is parsed, so a local rule they put in a group is not
    checked here, and a job of it that cannot have its image fails when it starts.

    This reads Snakemake internals no API exposes (`workflow._localrules`, and each rule's
    `_group`, `norun` and `is_template_engine`): a release without one is refused, naming it,
    rather than checked wrongly."""
    import snakemake
    from snakemake.exceptions import WorkflowError
    from snakemake_executor_plugin_biffix.images import (
        IMAGE_RESOURCE,
        ImageError,
        ImageMap,
        image_file,
        job_image,
    )

    # Snakemake's Workflow.is_local, less its local_exec, which a dry run sets (it runs on
    # Snakemake's dryrun executor), and which would make every rule local: a rule in a group runs
    # in its group's job, in its image, even if it is a local rule.
    rules = list(workflow.rules)
    missing = [f"workflows have no {a}" for a in ("_localrules",) if not hasattr(workflow, a)] + [
        f"rules have no {a}"
        for a in ("_group", "norun", "is_template_engine")
        if any(not hasattr(rule, a) for rule in rules)
    ]
    if missing:
        raise WorkflowError(
            f"biffix cannot check the rules' images on Snakemake {snakemake.__version__}: its "
            + ", ".join(missing)
        )
    local_rules = workflow._localrules

    def is_local(rule: Any) -> bool:
        return rule._group is None and (
            rule.name in local_rules or rule.norun or rule.is_template_engine
        )

    try:
        image_map = ImageMap.from_file(map_path) if map_path else None
    except (OSError, ValueError) as e:
        raise WorkflowError(f"biffix: the run's image map {map_path} cannot be used: {e}") from e
    declare = "declare it in biffix.yaml's images: and re-register"
    problems: list[str] = []
    images: set[str] = set()
    for rule in rules:
        if is_local(rule):
            continue
        resource: Any = rule.resources.get(IMAGE_RESOURCE) if rule.resources is not None else None
        if is_function(resource):
            problems.append(
                f"rule {rule.name}: its {IMAGE_RESOURCE} is set by a function, which a single-job "
                "run cannot resolve before the run; set it to the image's reference"
            )
            continue
        # Older Snakemake 9 releases keep raw values; newer ones wrap them in Resource.
        value = resource.value if hasattr(resource, "is_evaluable") else resource
        if isinstance(value, str) and value.startswith(RESERVED_KEY_PREFIX):
            problems.append(
                f"rule {rule.name}: its image {value!r} ({IMAGE_RESOURCE}) is the map's reserved "
                "key, not an image; set it to the image's reference"
            )
            continue
        named = "its image" if value is not None else "the default image"
        try:
            image = job_image(
                {} if value is None else {IMAGE_RESOURCE: value},
                f"rule {rule.name}",
                default_image or None,
                image_map,
            )
        except ImageError as e:
            if e.reason == ImageError.NOT_IN_MAP:
                problems.append(
                    f"{e.job_name}: {named} {e.image!r} is not in the run's image map; {declare}"
                )
            elif e.reason == ImageError.NOT_A_REFERENCE:
                problems.append(
                    f"{e.job_name}: {named} {e.image!r} ({IMAGE_RESOURCE}) is not an image "
                    "reference; set it to the image's reference"
                )
            else:
                problems.append(str(e))
            continue
        if image is not None:
            images.add(image)
    if check_files:
        for image in sorted(images):
            try:
                image_file(Path(images_dir), image, f"image {image}")
            except ImageError as e:
                problems.append(
                    f"image {image}: its image file {e.path} is missing (it was not converted); "
                    + declare
                )
    if problems:
        raise WorkflowError(
            "biffix: a single-job run runs each rule in an image its image map names, converted "
            "before the run (it pulls none), so it cannot run:\n  " + "\n  ".join(problems)
        )


# The resources whose limits bound the jobs running at once on the host: for a remote executor
# Snakemake reckons them per job (each on a node of its own), but every job here shares the host.
HOST_SCOPED_RESOURCES = {"mem_mb": "global", "disk_mb": "global"}
# The same in MiB, for a Snakemake 9 release before `snakemake.resources.Resource`: it takes each
# unit for a resource of its own, so a profile could keep `mem_mib` local, and the scheduler would
# then admit jobs on their `mem_mb` alone. A later release normalizes the aliases to the MB names.
LEGACY_HOST_SCOPED_RESOURCES = {"mem_mib": "global", "disk_mib": "global"}


def configure_local_resources(workflow: Any, memory_mib: int) -> None:
    """Keep profile limits, defaults and resource scopes, adding the host budget before rules are
    parsed.

    Clearing the nodes limit is essential: Snakemake's scheduler lifts its CPU limit whenever one
    is set, as --jobs sets it, and a remote executor (the biffix executor) cannot run without
    --jobs. Only the scheduler's limit (`global_resources`) is cleared: the setting itself stays,
    since Snakemake checks, after the workflow is parsed, that a remote executor was given one.
    Mutate the effective resources rather than passing --resources, which would discard the
    workflow profile's other limits. Likewise memory and disk are scoped to the whole host
    (HOST_SCOPED_RESOURCES, and LEGACY_HOST_SCOPED_RESOURCES on a release that keeps the MiB
    units apart) on top of the profile's `set-resource-scopes`, which
    --set-resource-scopes would replace: in the settings, which outrank a workflow's own
    `resource_scopes:` and reach each job's Snakemake, and in the scopes the workflow already
    took from them.

    This reaches into Snakemake internals no option exposes: `workflow.resource_settings` (its
    `default_resources` and `overwrite_resource_scopes`), `workflow.global_resources`,
    `workflow.resource_scopes`, and the `cached_property` cache
    `ResourceSettings._parsed_default_resources`. A release without the first two fails here,
    loudly, rather than running unbounded.
    """
    from snakemake import resources as sm_resources

    missing = [
        name for name in ("resource_settings", "global_resources") if not hasattr(workflow, name)
    ]
    if missing:
        raise RuntimeError(
            "biffix cannot set this Snakemake's single-job resource limits: its workflow has no "
            + ", ".join(missing)
        )
    settings = workflow.resource_settings
    limits = workflow.global_resources
    limits["_nodes"] = None
    # Snakemake's own ResourceScopes, which spells each size as its release does (`mem` or
    # `mem_mb`), as it spells the profile's.
    scopes = type(workflow.resource_scopes)
    legacy = not hasattr(sm_resources, "Resource")
    host_scopes = scopes(HOST_SCOPED_RESOURCES | (LEGACY_HOST_SCOPED_RESOURCES if legacy else {}))
    settings.overwrite_resource_scopes = scopes(
        {**settings.overwrite_resource_scopes, **host_scopes}
    )
    workflow.resource_scopes.update(host_scopes)
    memory_mb = memory_mib * 1048576 // 1000000
    if not legacy:
        caps = sm_resources.Resources.from_mapping({"mem_mb": memory_mb})
        for name, cap in caps.items():
            existing = limits.get(name)
            limits[name] = cap.constrain(existing) if existing is not None else cap
        if settings.default_resources is None:
            settings.default_resources = sm_resources.Resources.default("full")
            # ResourceSettings caches its parsed defaults; no rule may retain the bare set.
            settings.__dict__.pop("_parsed_default_resources", None)
    else:
        for name, cap in (("mem_mb", memory_mb), ("mem_mib", memory_mib)):
            existing = limits.get(name)
            limits[name] = min(existing, cap) if existing is not None else cap
        defaults = settings.default_resources
        if defaults is None or set(defaults.parsed) <= {"_cores", "_nodes", "tmpdir"}:
            settings.default_resources = sm_resources.DefaultResources(
                args=defaults.args if defaults is not None else None
            )


def snakefile_wrapper(
    snakefile: str,
    wrapper: str,
    map_path: str,
    images_dir: str,
    default_image: str,
    check_files: bool,
    memory_mib: int,
    envvars: list[str] | None = None,
) -> str:
    """Create the wrapper exclusively and return its path (a unique sibling for wrapper="").

    In Snakemake's main process it sets the host's resource budget (configure_local_resources,
    `memory_mib` for the jobs running at once), registers `envvars` as variables every job gets,
    includes `snakefile` keeping its default target, then checks every rule's image
    (check_rule_images, from this file). Snakemake has no option for the budget or the check; the
    variables are registered as `envvars:` registers them, so Snakemake takes the union with a
    workflow profile's (or the command line's) `envvars`, which `--envvars` would replace instead.
    (The executor passes registered variables to its jobs on every Snakemake 9: before 9.6
    Snakemake itself passes a remote executor's jobs only `--envvars`.) A job's own Snakemake
    (which the executor starts, in the job's image) loads the wrapper too, and only includes the
    workflow: this file is in the coordinator's image, not a task's, and the budget and the checks
    are the main process's (a group job's Snakemake gets the memory reserved for the group job
    from the biffix executor instead, as --resources mem_mb). Keeping the generated file beside
    the entrypoint preserves relative includes and scripts.
    """
    args = ", ".join(ascii(a) for a in (map_path, images_dir, default_image))
    text = (
        "# Written by biffix for a single-job run (lib/single_job.py): the workflow, with the\n"
        "# host's resource budget and a check of every rule's image, in Snakemake's main process.\n"
        "\n"
        "_biffix = None\n"
        "if workflow.is_main_process:\n"
        "    import importlib.util as _biffix_importlib\n"
        "    import sys as _biffix_sys\n"
        "    _biffix_spec = _biffix_importlib.spec_from_file_location(\n"
        f'        "biffix_single_job", {ascii(str(Path(__file__).resolve()))}\n'
        "    )\n"
        "    _biffix = _biffix_importlib.module_from_spec(_biffix_spec)\n"
        "    _biffix_sys.modules[_biffix_spec.name] = _biffix\n"
        "    _biffix_spec.loader.exec_module(_biffix)\n"
        f"    _biffix.configure_local_resources(workflow, {memory_mib})\n"
        + (f"    workflow.register_envvars(*{ascii(list(envvars))})\n" if envvars else "")
        + f"workflow.include({ascii(str(Path(snakefile).resolve()))}, "
        "overwrite_default_target=True)\n"
        "if _biffix is not None:\n"
        f"    _biffix.check_rule_images(workflow, {args}, {check_files!r})\n"
    )
    if wrapper:
        with open(wrapper, "x") as out:
            out.write(text)
    else:
        fd, wrapper = tempfile.mkstemp(
            prefix=".biffix-container-", suffix=".smk", dir=Path(snakefile).resolve().parent
        )
        with os.fdopen(fd, "w") as out:
            out.write(text)
    return wrapper


def main(argv: list[str]) -> int:
    match argv:
        case ["_ecr-login", region]:
            # Only the supervising parent reads stdout; never print SDK exceptions or tokens.
            try:
                print(json.dumps(ecr_login(region)))
                return 0
            except Exception:
                return 1
        case ["convert", map_path, images_dir, log_dir, failure_file, *extra]:
            return convert(map_path, images_dir, log_dir, failure_file, extra)
        case [
            "snakefile-wrapper",
            snakefile,
            wrapper,
            map_path,
            images_dir,
            default_image,
            check,
            memory,
            *envvars,
        ]:
            print(
                snakefile_wrapper(
                    snakefile,
                    wrapper,
                    map_path,
                    images_dir,
                    default_image,
                    check == "check",
                    int(memory),
                    envvars,
                )
            )
            return 0
        case _:
            print(__doc__, file=sys.stderr)
            return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
