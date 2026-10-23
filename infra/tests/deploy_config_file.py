"""Deploy configs for the tests: the frozen fixture the stacks are synthesized from, and a minimal
config written to disk, for tests of what `DeployConfig.load` accepts."""

from __future__ import annotations

from pathlib import Path

import yaml

# The frozen deploy config the tests synthesize and load (tenant `acme`, one project `demo`). Tests
# assert on what it yields, so they never read the documented example,
# `infra/biffix-deploy.example.yaml`.
FIXTURE = Path(__file__).parent / "fixtures" / "deploy.yaml"


def write_config(tmp_path: Path, *, raw: str = "", **overrides: object) -> Path:
    """Write a minimal valid deploy config (tenant `x`, one project `demo`, one coordinator
    queue-class), with `overrides` replacing its top-level keys, and return its path. `raw` is
    YAML appended as written (top-level keys `overrides` does not set), for the spellings
    `yaml.safe_dump` would quote: an unquoted `off` or an octal-looking account."""
    base: dict[str, object] = {
        "tenant": "x",
        "region": "us-east-1",
        "projects": [{"id": "demo"}],
        "queue_classes": [{"name": "coordinator", "capacity": "ON_DEMAND", "coordinator": True}],
    }
    base.update(overrides)
    path = tmp_path / "cfg.yaml"
    path.write_text(yaml.safe_dump(base) + raw)
    return path
