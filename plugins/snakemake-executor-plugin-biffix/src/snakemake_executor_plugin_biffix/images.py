# The image map (`normalize`, `ImageMap`) is adapted from the AWS Batch executor plugin
# (https://github.com/snakemake/snakemake-executor-plugin-aws-batch, its open PR #60:
# snakemake_executor_plugin_aws_batch/image_map.py), which is licensed under the MIT License; see
# the NOTICE section of this package's README.md. `normalize` is the fork's as it is (it drops a
# tag beside a digest), so that a job's image is found in the map exactly as on AWS Batch; the
# keys biffix-workflow's ImageRef writes keep that tag, and are found all the same
# (tests/test_images.py, against schemas/image-ref-cases.json).
"""The image a job runs in, chosen as the AWS Batch executor chooses a Batch job's.

A job (or group job) names its image with the ``aws_batch_container_image`` resource, else it runs
the default image; either is then looked up in the image map, and a job whose image the map does
not name fails. The lookup is a copy of the AWS Batch executor's rather than an import of it: that
package is not a dependency this one can declare (the map exists only in a fork of it, which PyPI
does not carry), and importing any of it loads boto3 and the whole executor.

The mapped image is not pulled. It is run from the image file the biffix coordinator converted
before the run, named by :func:`image_file_name`.
"""

from __future__ import annotations

import json
from collections.abc import Mapping
from pathlib import Path

from snakemake_interface_common.exceptions import WorkflowError

# The resource the AWS Batch executor reads a job's image from.
IMAGE_RESOURCE = "aws_batch_container_image"
DOCKER_HUB = "docker.io"
DOCKER_HUB_ALIASES = ("index.docker.io", "registry-1.docker.io")


def normalize(ref: str) -> str:
    """The canonical spelling of an image reference, as Docker reads it.

    The first path component is a registry only when it contains a ``.`` or ``:``, is
    ``localhost``, or has an uppercase letter; registry names are case-insensitive, so it is
    lower-cased. Otherwise the image is on Docker Hub, and a one-part name is under ``library/``.
    A digest pins the image, so a tag beside a digest is dropped (Docker pulls ``name:tag@digest``
    by its digest). A reference with neither tag nor digest means ``:latest``.
    """
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


class ImageError(WorkflowError):
    """Why a job cannot run in its image: ``reason`` (one of the constants below), ``job_name``,
    the ``image`` as the job names it (any value a resource holds), and for an image file that is
    not there its ``path``. The message, for Snakemake's log, starts with the job's name; a caller
    that words its own reads these instead."""

    # The image is not a reference ("", or not a string).
    NOT_A_REFERENCE = "not a reference"
    # The image map does not name the image.
    NOT_IN_MAP = "not in the map"
    # The image has no image file: it was not converted.
    NOT_CONVERTED = "not converted"

    def __init__(
        self, reason: str, job_name: str, image: object, message: str, path: Path | None = None
    ):
        super().__init__(f"{job_name}: {message}")
        self.reason = reason
        self.job_name = job_name
        self.image = image
        self.path = path


def _not_a_reference(image: object, job_name: str) -> ImageError:
    return ImageError(
        ImageError.NOT_A_REFERENCE,
        job_name,
        image,
        f"container image {image!r} is not an image reference",
    )


def _is_reference(value: object) -> bool:
    return isinstance(value, str) and bool(value.strip())


def _unique_keys(pairs: list[tuple[str, object]]) -> dict[str, object]:
    """A JSON object hook that rejects a key given twice (JSON keeps the last)."""
    obj: dict[str, object] = {}
    for key, value in pairs:
        if key in obj:
            raise ValueError(f"image {key!r} is in the map twice")
        obj[key] = value
    return obj


class ImageMap:
    """Image references to the images to run in their place, each looked up as written and then
    in its normalized spelling (``ubuntu`` and ``docker.io/library/ubuntu:latest`` are one image).
    """

    def __init__(self, mapping: Mapping[str, object], path: str | None = None):
        # Where the map was read from, for error messages.
        self.path = path
        self._entries: dict[str, str] = {}
        for ref, value in mapping.items():
            if not _is_reference(ref) or not isinstance(value, str) or not _is_reference(value):
                raise ValueError(f"image map entry {ref!r}: {value!r} is not a pair of references")
            self._entries[ref] = value
        # Each entry also answers to its normalized spelling, unless the map names that spelling
        # itself. Entries that share a normalized spelling must agree on the image, or which one
        # a job gets would depend on the order of the map.
        self._lookup: dict[str, str] = dict(self._entries)
        named_by: dict[str, str] = {}
        for ref, value in self._entries.items():
            key = normalize(ref)
            if key in self._entries:
                continue
            other = named_by.setdefault(key, ref)
            if self._lookup.setdefault(key, value) != value:
                raise ValueError(
                    f"image map entries {other!r} and {ref!r} are both {key} but map to "
                    f"different images; add an entry for {key!r} to choose one"
                )

    @classmethod
    def from_file(cls, path: str) -> ImageMap:
        """Load a JSON object of reference -> reference from ``path``."""
        data = json.loads(Path(path).read_text(), object_pairs_hook=_unique_keys)
        if not isinstance(data, dict):
            raise ValueError("an image map must be a JSON object")
        return cls(data, path=path)

    def resolve(self, ref: str) -> str | None:
        """The image for ``ref``, or ``None`` if the map does not have it."""
        return self._lookup.get(ref) or self._lookup.get(normalize(ref))

    def require(self, image: object, job_name: str) -> str:
        """The image for ``job_name``'s ``image``. Raises :class:`ImageError` when ``image`` is
        not a reference or not in the map."""
        if not isinstance(image, str) or not _is_reference(image):
            raise _not_a_reference(image, job_name)
        value = self.resolve(image)
        if value is None:
            where = f" {self.path}" if self.path else ""
            raise ImageError(
                ImageError.NOT_IN_MAP,
                job_name,
                image,
                f"container image {image!r} is not in the container image map{where}; "
                "add it to the map, or map it to itself to run it unchanged",
            )
        return value


def job_image(
    resources: Mapping[str, object],
    job_name: str,
    default_image: str | None,
    image_map: ImageMap | None,
) -> str | None:
    """The image a job with ``resources`` runs in, as the AWS Batch executor's
    ``_container_image`` picks it: the job's ``aws_batch_container_image`` resource when it has
    one, else ``default_image``, then its entry in ``image_map`` when there is one.

    Returns ``None`` for a job that names no image when there is no default one: it runs without a
    container, in the environment Snakemake itself runs in. Raises :class:`ImageError` when the
    image is not a reference, or the map does not name it. A resource set to ``""`` is such an
    image, as the AWS Batch executor's map refuses it: it neither falls back to the default image
    nor runs the job without a container.
    """
    # Snakemake drops a resource set to None: only one the job does not set is None here.
    image = resources.get(IMAGE_RESOURCE)
    if image is None:
        image = default_image
    if image is None:
        return None
    if image_map is not None:
        return image_map.require(image, job_name)
    if not isinstance(image, str) or not _is_reference(image):
        raise _not_a_reference(image, job_name)
    return image


def image_file_name(image: str) -> str:
    """The name of the image file the biffix coordinator converts ``image`` to (its
    ``single_job.py image_file_name``, which a coordinator test keeps equal to this): the name
    Nextflow gives an image in its ``apptainer.libraryDir``, so both engines share the files."""
    name = image
    extension = ".img"
    if ".sif:" in name:
        extension = ".sif"
        name = name.replace(".sif:", "-")
    elif name.endswith(".sif"):
        extension = ".sif"
        name = name[:-4]
    return name.replace(":", "-").replace("/", "-") + extension


def image_file(images_dir: Path, image: str, job_name: str) -> Path:
    """The converted image file of ``image`` in ``images_dir``. Raises :class:`ImageError` when
    it is not there: no job pulls an image."""
    path = images_dir / image_file_name(image)
    if not path.is_file():
        raise ImageError(
            ImageError.NOT_CONVERTED,
            job_name,
            image,
            f"the image file of {image} ({path}) is missing: it was not converted",
            path=path,
        )
    return path
