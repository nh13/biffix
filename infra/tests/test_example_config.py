"""The documented example, `infra/biffix-deploy.example.yaml`, stays complete and true: it loads,
names every key the schema holds, and every value it does not set on purpose is the default.

No test asserts on what the example deploys (the synth tests use the frozen fixture), so it can be
reworded freely; these only fail when it drifts from the schema or the defaults."""

import re
import textwrap
from collections.abc import Iterator
from pathlib import Path

import pytest
import yaml

from biffix_infra.config import DeployConfig
from biffix_infra.config.fields import Raw
from biffix_infra.config.schema import SCHEMA, Block
from biffix_infra.paths import INFRA_DIR

EXAMPLE = INFRA_DIR / "biffix-deploy.example.yaml"

# The keys the example sets to something other than the default (or that have none). Every other
# key it sets must hold the default, so its "Default: ..." comments cannot go stale.
DELIBERATE = ("tenant", "region", "account", "queue_classes", "projects", "cognito_domain_prefix")


def _raw() -> Raw:
    raw: Raw = yaml.safe_load(EXAMPLE.read_text())
    return raw


def _mappings(value: object, block: Block) -> list[Raw]:
    """The mappings at one block's place in the example: the value itself, or a list's entries."""
    if block.many:
        entries = value if isinstance(value, list) else []
        return [entry for entry in entries if isinstance(entry, dict)]
    return [value] if isinstance(value, dict) else []


def _blocks(block: Block, mappings: list[Raw], path: str) -> Iterator[tuple[str, Block, list[Raw]]]:
    """Each block nested in `block`, with its path and the mappings the example gives it."""
    for key, child in block.nested.items():
        values = [m[key] for m in mappings if key in m]
        found = [mapping for value in values for mapping in _mappings(value, child)]
        yield f"{path}{key}", child, found
        yield from _blocks(child, found, f"{path}{key}.")


def test_the_example_loads() -> None:
    DeployConfig.load(EXAMPLE)


def test_the_example_sets_every_top_level_key() -> None:
    assert sorted(_raw()) == sorted(SCHEMA.keys)


_NESTED = list(_blocks(SCHEMA, [_raw()], ""))


@pytest.mark.parametrize(("path", "block", "mappings"), _NESTED, ids=[p for p, _, _ in _NESTED])
def test_the_example_documents_every_nested_key(
    path: str, block: Block, mappings: list[Raw]
) -> None:
    """A block the example sets names all its keys (across a list's entries); a block it leaves at
    its null or empty default shows them in a commented-out sample instead."""
    if mappings:
        named = {key for mapping in mappings for key in mapping}
        assert sorted(set(block.keys) - named) == [], path
    else:
        named = _sample_keys(EXAMPLE.read_text(), path.rsplit(".", 1)[-1])
        undocumented = sorted(set(block.keys) - named)
        assert undocumented == [], f"{path}: its commented sample does not show {undocumented}"


def _sample_keys(text: str, key: str) -> set[str]:
    """The keys the commented-out sample of the block `key` shows: the comment run that opens
    with `#   <key>:` alone on its line and continues more deeply indented, read as YAML. Only
    that block's own sample counts, not a key of the same name in another block's."""
    lines = text.splitlines()
    for i, line in enumerate(lines):
        head = re.fullmatch(rf"(\s*#)(\s*){re.escape(key)}:\s*", line)
        if not head:
            continue
        sample = [line[len(head[1]) :]]
        for body in lines[i + 1 :]:
            m = re.fullmatch(rf"{re.escape(head[1])}(\s*)(.*)", body)
            if not m or len(m[1]) <= len(head[2]) or not m[2]:
                break
            sample.append(body[len(head[1]) :])
        value = yaml.safe_load(textwrap.dedent("\n".join(sample)))[key]
        entries = value if isinstance(value, list) else [value]
        return {k for entry in entries if isinstance(entry, dict) for k in entry}
    return set()


def test_a_block_s_sample_counts_only_its_own_keys() -> None:
    # `name` is also in the queue_classes sample: deleting the data source's must still fail.
    text = EXAMPLE.read_text()
    assert "name" in _sample_keys(text, "data_sources")
    without = re.sub(r"(?m)^\s*#\s+name: Sequencing core.*\n", "", text)
    assert without != text
    assert "name" not in _sample_keys(without, "data_sources")


def test_what_the_example_does_not_set_on_purpose_is_the_default(tmp_path: Path) -> None:
    raw = _raw()
    minimal = tmp_path / "minimal.yaml"
    minimal.write_text(yaml.safe_dump({key: raw[key] for key in DELIBERATE}))
    assert DeployConfig.load(EXAMPLE) == DeployConfig.load(minimal)
