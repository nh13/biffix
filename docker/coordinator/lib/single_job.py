"""A single-job run's task images: converted to Apptainer image files before the engine starts, and
put where each engine looks for them.

Usage: single_job.py convert <images.json> <images dir> <log dir> <failure file> [<image>...]
       single_job.py snakefile-wrapper <snakefile> <wrapper> <images.json> <images dir> \
           <default image> <task images file> check|no-check

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
and prints its path. It includes <snakefile> and then sets every rule's container to the image
file of the image its job on AWS Batch would run (set_rule_containers): its
`aws_batch_container_image` through the map, else <default image> ("" for none). A rule naming an
image the map does not, or naming it with anything but a plain string, fails the run before any job
starts, as does (with `check`) an image file the conversion did not leave in <images dir>. So a
single-job run pulls no image, as tasks do not on AWS Batch.
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
from collections.abc import Iterable, Mapping
from concurrent.futures import ThreadPoolExecutor
from dataclasses import dataclass, replace
from pathlib import Path
from typing import Any

DEFAULT_PARALLEL = 4
# The bytes of a failed pull's output quoted in the failure.
FAILURE_TAIL_BYTES = 4096
# A key naming the run's default image, for people reading the map (crates/biffix-backend
# images/map.rs); never a container a workflow names.
RESERVED_KEY_PREFIX = "$"


def read_map(path: str) -> dict[str, str]:
    """The image map at `path` ({} for ""), keeping only string values."""
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
        self._lock = threading.Lock()
        self._running: set[subprocess.Popen[bytes]] = set()

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
            output, _ = proc.communicate()
        finally:
            with self._lock:
                self._running.discard(proc)
        if self.failed.is_set():
            return {}
        if proc.returncode != 0:
            raise RuntimeError(f"ECR authorization request exited {proc.returncode}")
        user, password = json.loads(output)
        return {"APPTAINER_DOCKER_USERNAME": user, "APPTAINER_DOCKER_PASSWORD": password}

    def pull(self, image: str, credentials: dict[str, str]) -> None:
        """Pull one image (unless another has failed), recording why when it fails."""
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
            status = proc.wait()
            with self._lock:
                if status != 0:
                    # Its own group too, before it leaves the running pulls that fail() stops: a
                    # child the failed apptainer left (mksquashfs) would keep converting. Just
                    # reaped, so its group id is not yet another's while any member is left.
                    try:
                        os.killpg(proc.pid, signal.SIGTERM)
                    except (ProcessLookupError, PermissionError):
                        pass  # the group is gone, or its last members are exiting
                self._running.discard(proc)
        if status == 0 and partial.is_file():
            os.replace(partial, dest)
            print(f"biffix-coordinator: converted {image} -> {dest}", file=sys.stderr)
            return
        partial.unlink(missing_ok=True)
        if self.failed.is_set():
            return  # stopped because another image failed
        tail = log.read_bytes()[-FAILURE_TAIL_BYTES:].decode(errors="replace").rstrip()
        self.fail(
            f"could not convert the task image {image} (apptainer pull exited {status}):\n{tail}"
        )

    def fail(self, why: str) -> None:
        """Record the first failure and stop every pull still running."""
        with self._lock:
            if self.failed.is_set():
                return
            self.failure = why
            self.failed.set()
            for proc in self._running:
                # A child not yet reaped (its worker's wait() has not returned) still holds its
                # pid, so its group cannot be another's; one already reaped is skipped.
                if proc.returncode is not None:
                    continue
                try:
                    os.killpg(proc.pid, signal.SIGTERM)
                except (ProcessLookupError, PermissionError):
                    pass  # the group is gone, or its last members are exiting

    def stop(self, signum: int) -> None:
        """A signal handler: stop the login and every pull, without recording a run failure."""
        # The handler runs on the main thread, which otherwise never takes the lock, so a first
        # SIGTERM at most waits for a worker to release it. A second one can interrupt the first
        # handler while that holds the (non-reentrant) lock in fail(), and must return rather
        # than wait on it forever: the first handler stops every child.
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
    # The main thread never holds conversion's lock, including while starting the login child,
    # so the signal handler can safely stop every child under that lock.
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


# The rule resource the AWS Batch executor reads a rule's image from.
IMAGE_RESOURCE = "aws_batch_container_image"
DOCKER_HUB = "docker.io"
DOCKER_HUB_ALIASES = ("index.docker.io", "registry-1.docker.io")


def normalize(ref: str) -> str:
    """The canonical spelling of an image reference, as Docker reads it (`ubuntu` is
    `docker.io/library/ubuntu:latest`): the AWS Batch executor's `image_map.normalize`, which looks
    an image up in the map as written and then in this form."""
    ref = ref.strip()
    name, _, digest = ref.partition("@")
    tag = None
    if ":" in name.rsplit("/", 1)[-1]:
        name, tag = name.rsplit(":", 1)
    first, _, rest = name.partition("/")
    if rest and ("." in first or ":" in first or first == "localhost" or first != first.lower()):
        registry, repository = first.lower(), rest
    else:
        registry, repository = DOCKER_HUB, name
    if registry in DOCKER_HUB_ALIASES:
        registry = DOCKER_HUB
    if registry == DOCKER_HUB and "/" not in repository:
        repository = f"library/{repository}"
    if digest:
        return f"{registry}/{repository}@{digest.lower()}"
    return f"{registry}/{repository}:{tag or 'latest'}"


@dataclass(frozen=True)
class RuleImage:
    """What a rule names as its image: its `aws_batch_container_image` resource (`image`, None
    when unset), or why that cannot be read before the run (`problem`), and whether the rule runs
    in the engine itself on AWS Batch too (`local`: a local rule)."""

    name: str
    image: str | None = None
    problem: str | None = None
    local: bool = False


def group_images(
    rules: list[RuleImage], groups: dict[str, object]
) -> tuple[list[RuleImage], list[str]]:
    """`rules` with each grouped rule that names no image given its group's (`groups`: each rule's
    group, None for none). On AWS Batch a group's jobs run as one job, in the image its rules name
    (Snakemake merges the resource across a group job, and the executor reads it once), so a rule
    that names none runs in that image, not the default one. Returns one problem per group whose
    rules name different images, which Snakemake refuses for a group job they share.

    This works per group name, from the rules alone: it runs as the Snakefile is parsed, before
    there is a DAG to form group jobs from, and a rule has one container. So it differs from AWS
    Batch where the group jobs would: rules of disconnected parts of one group (separate group
    jobs on Batch) must name one image between them here, and every rule of a group counts, run
    or not; a rule naming none takes the group's image even beside one naming it in the same
    layer of a group job, which Snakemake refuses on Batch. Running each group job as Batch does
    needs real group jobs, which only an executor plugin is given."""
    named: dict[object, set[str]] = {}
    for rule in rules:
        group = groups.get(rule.name)
        if group is not None and rule.image is not None and rule.problem is None:
            named.setdefault(group, set()).add(rule.image)
    problems = [
        f"group {group}: its rules name different images ({', '.join(sorted(images))}) in "
        f"{IMAGE_RESOURCE}, which Snakemake refuses for a group job; name one image for the "
        "group's rules"
        for group, images in named.items()
        if len(images) > 1
    ]

    def inherited(rule: RuleImage) -> str | None:
        group = groups.get(rule.name)
        images = named.get(group, set()) if group is not None else set()
        if rule.image is None and rule.problem is None and not rule.local and len(images) == 1:
            return next(iter(images))
        return None

    planned = [replace(rule, image=image) if (image := inherited(rule)) else rule for rule in rules]
    return planned, problems


def plan_rule_images(
    rules: Iterable[RuleImage], image_map: dict[str, str], default_image: str
) -> tuple[dict[str, str | None], list[str]]:
    """Each rule's tenant image, as its job on AWS Batch would run it (the executor's
    `_container_image` with the strict image map): its resource's image through the map, else the
    run's default image (`default_image`; "" for none: the rule runs in the job itself, as its
    Batch job would run in the coordinator's own image). A local rule runs in the engine, as it
    does beside a Batch run. A rule's `container:` directive plays no part, as on AWS Batch.
    Returns the images (None: no container) and one problem per rule that cannot run: a resource
    that is not a plain string, or an image the map does not name."""
    by_normal: dict[str, str] = {}
    for key, value in image_map.items():
        if not key.startswith(RESERVED_KEY_PREFIX):
            by_normal.setdefault(normalize(key), value)
    images: dict[str, str | None] = {}
    problems: list[str] = []
    for rule in rules:
        if rule.problem is not None:
            problems.append(f"rule {rule.name}: {rule.problem}")
            continue
        if rule.local:
            images[rule.name] = None
            continue
        if rule.image is None:
            images[rule.name] = default_image or None
            continue
        tenant = image_map.get(rule.image) or by_normal.get(normalize(rule.image))
        if tenant is None or rule.image.startswith(RESERVED_KEY_PREFIX):
            problems.append(
                f"rule {rule.name}: its image {rule.image!r} ({IMAGE_RESOURCE}) is not one the "
                "run's image map names; declare it in biffix.yaml's images: and re-register"
            )
            continue
        images[rule.name] = tenant
    return images, problems


def rule_image(name: str, resource: Any, local: bool) -> RuleImage:
    """A rule's RuleImage from its `aws_batch_container_image` resource (Snakemake's `Resource`,
    or None when the rule has none)."""
    if local or resource is None:
        return RuleImage(name, local=local)
    if resource.is_evaluable():
        return RuleImage(
            name,
            problem=f"its {IMAGE_RESOURCE} is set by a function, which a single-job run cannot "
            "resolve before the run; set it to the image's reference",
        )
    value = resource.value
    if value is None or value == "":
        return RuleImage(name, local=local)
    if not isinstance(value, str):
        return RuleImage(
            name,
            problem=f"its {IMAGE_RESOURCE} is not a plain string ({value!r}); set it to the "
            "image's reference",
        )
    return RuleImage(name, image=value, local=local)


def set_rule_containers(
    workflow: Any,
    map_path: str,
    images_dir: str,
    default_image: str,
    task_images_file: str,
    check_files: bool,
) -> None:
    """Run from the wrapper Snakefile once the workflow is included: set every rule's container,
    module rules (`use rule`) included, to the image file of its tenant image (plan_rule_images;
    a grouped rule naming none, its group's: group_images), or to none. Every image is then one
    the run's map names and the coordinator converted before the engine started, so no task pulls
    an image. Writes `{rule: tenant image}` to
    `task_images_file` (from Snakemake's main process only), omitting native Python run blocks
    whose optional shell container is not their execution environment, for the logger to report
    task images. Raises Snakemake's
    WorkflowError, naming every rule that cannot run, before any job starts; so does an image file
    missing when `check_files` (not in a dry run, which converts nothing)."""
    from snakemake.exceptions import WorkflowError

    # Snakemake's Workflow.is_local, less its local_exec (which holds for every rule here): a rule
    # in a group runs in its group's Batch job, in its image, even if it is a local rule. Its
    # `group` reads None outside a remote executor, so the group is read as a remote run sees it.
    local_rules = set(getattr(workflow, "_localrules", ()))
    overwrite_groups = getattr(getattr(workflow, "group_settings", None), "overwrite_groups", {})

    def group_of(rule: Any) -> object:
        return overwrite_groups.get(rule.name, getattr(rule, "_group", None))

    rules = list(workflow.rules)
    planned = [
        rule_image(
            rule.name,
            rule.resources.get(IMAGE_RESOURCE) if rule.resources is not None else None,
            group_of(rule) is None
            and (rule.name in local_rules or rule.norun or rule.is_template_engine),
        )
        for rule in rules
    ]
    planned, group_problems = group_images(planned, {rule.name: group_of(rule) for rule in rules})
    images, problems = plan_rule_images(planned, read_map(map_path), default_image)
    problems = group_problems + problems
    files = {image: Path(images_dir) / image_file_name(image) for image in images.values() if image}
    if check_files:
        problems += [
            f"the image file of {image} ({path}) is missing: it was not converted"
            for image, path in sorted(files.items())
            if not path.is_file()
        ]
    if problems:
        raise WorkflowError(
            "biffix: a single-job run runs each rule in an image its image map names, converted "
            "before the run (it pulls none):\n  " + "\n  ".join(problems)
        )
    for rule in rules:
        image = images.get(rule.name)
        rule.container_img = str(files[image]) if image else None
        rule.is_containerized = False
    # A job Snakemake spawns (a `run:` job) loads this Snakefile again and needs its rules'
    # containers, but not the report: the main process wrote it before any job started, and two
    # spawned jobs publishing it through one temporary file would race (the loser's rename fails
    # its job).
    if not getattr(workflow, "is_main_process", True):
        return
    tmp = f"{task_images_file}.tmp"
    # Native run: Python stays in the engine; only optional shell() calls use its container.
    # There is no single task image to report for such a mixed execution context.
    native_rules = {rule.name for rule in rules if rule.is_run}
    Path(tmp).write_text(
        json.dumps({k: v for k, v in sorted(images.items()) if v and k not in native_rules})
    )
    os.replace(tmp, task_images_file)


def snakefile_wrapper(
    snakefile: str,
    wrapper: str,
    map_path: str,
    images_dir: str,
    default_image: str,
    task_images_file: str,
    check_files: bool,
) -> str:
    """Create the wrapper exclusively and return its path (a unique sibling for wrapper="").

    It includes `snakefile`, preserving its default target, then sets every rule's container
    (set_rule_containers, from this file). Snakemake has no option for that outside a remote
    executor. Keeping the generated file beside the entrypoint preserves relative includes
    and scripts.
    """
    args = ", ".join(ascii(a) for a in (map_path, images_dir, default_image, task_images_file))
    text = (
        "# Written by biffix for a single-job run (lib/single_job.py): the workflow, with every\n"
        "# rule's container set to the image its job on AWS Batch would run.\n"
        f"workflow.include({ascii(str(Path(snakefile).resolve()))}, "
        "overwrite_default_target=True)\n"
        "\n"
        "import importlib.util as _biffix_importlib\n"
        "import sys as _biffix_sys\n"
        "_biffix_spec = _biffix_importlib.spec_from_file_location(\n"
        f'    "biffix_single_job", {ascii(str(Path(__file__).resolve()))}\n'
        ")\n"
        "_biffix = _biffix_importlib.module_from_spec(_biffix_spec)\n"
        "_biffix_sys.modules[_biffix_spec.name] = _biffix\n"
        "_biffix_spec.loader.exec_module(_biffix)\n"
        f"_biffix.set_rule_containers(workflow, {args}, {check_files!r})\n"
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
            task_images_file,
            check,
        ]:
            print(
                snakefile_wrapper(
                    snakefile,
                    wrapper,
                    map_path,
                    images_dir,
                    default_image,
                    task_images_file,
                    check == "check",
                )
            )
            return 0
        case _:
            print(__doc__, file=sys.stderr)
            return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
