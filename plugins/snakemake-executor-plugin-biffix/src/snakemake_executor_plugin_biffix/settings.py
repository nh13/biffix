"""The executor's options, ``--biffix-<name>`` on Snakemake's command line.

No ``from __future__ import annotations`` here, and ``Optional`` rather than ``X | None``:
Snakemake's plugin registry builds each option from its field's evaluated type, and reads only a
``typing.Optional`` for the type behind it.
"""

from dataclasses import dataclass, field
from pathlib import Path
from typing import Optional

from snakemake_interface_executor_plugins.settings import ExecutorSettingsBase


@dataclass
class ExecutorSettings(ExecutorSettingsBase):
    """The executor's options, each ``--biffix-<name>`` (none is read from the environment)."""

    images_dir: Optional[Path] = field(
        default=None,
        metadata={
            "help": "Directory of the converted Apptainer image files, one per image a job may "
            "run, each named as the biffix coordinator names it. Required.",
            "env_var": False,
            "required": True,
        },
    )
    image_map: Optional[Path] = field(
        default=None,
        metadata={
            "help": "Path to a JSON object mapping container image references to the images to "
            "run instead. When set, every job's image (--biffix-default-image, or a rule's "
            "aws_batch_container_image resource) is looked up in it, as written and then "
            "normalized, and a job whose image is not in the map fails.",
            "env_var": False,
            "required": False,
        },
    )
    default_image: Optional[str] = field(
        default=None,
        metadata={
            "help": "The image of a job that names none in its aws_batch_container_image "
            "resource. Without it, such a job runs without a container.",
            "env_var": False,
            "required": False,
        },
    )
    bind: list[str] = field(
        default_factory=list,
        metadata={
            "help": "Paths to bind into every job's container (Apptainer --bind specs), besides "
            "the working directory: the local storage prefix and any other directory a job's "
            "Snakemake reads or writes.",
            "env_var": False,
            "required": False,
            "nargs": "+",
        },
    )
