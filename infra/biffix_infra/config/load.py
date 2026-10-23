"""Read a deploy config file into a DeployConfig: check its keys against the schema, then let each
section parse (and validate) its own keys."""

from __future__ import annotations

from pathlib import Path

import yaml

from biffix_infra.config import (
    alarms,
    compute,
    data,
    edge,
    identity,
    network,
    persistence,
    projects,
    registration,
    schema,
    tenancy,
)
from biffix_infra.config.fields import Fields
from biffix_infra.config.model import DeployConfig

# Each turns the raw file into the DeployConfig fields of its section.
SECTIONS = (
    tenancy,
    network,
    compute,
    projects,
    data,
    identity,
    registration,
    persistence,
    alarms,
    edge,
)


def load(path: str | Path) -> DeployConfig:
    """The deploy config at `path`. Every error names the file; a missing required key or a value
    of the wrong shape reads as a malformed config."""
    path = Path(path)
    raw = yaml.safe_load(path.read_text())
    if not isinstance(raw, dict):
        raise ValueError(f"{path}: expected a YAML mapping, got {type(raw).__name__}")
    try:
        schema.check(raw)
        fields: Fields = {}
        for section in SECTIONS:
            fields.update(section.parse(raw))
        return DeployConfig(**fields)
    except (KeyError, TypeError) as e:
        raise ValueError(f"{path}: malformed deploy config: missing/invalid {e}") from e
    except ValueError as e:
        raise ValueError(f"{path}: {e}") from e
