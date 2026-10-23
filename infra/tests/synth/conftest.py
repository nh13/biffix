"""Fixtures for the synth tests."""

import pytest

from biffix_infra.app_graph import Stacks
from tests.synth.synth_helpers import _build, _two_project_stacks


@pytest.fixture(scope="session")
def default_stacks(tmp_path_factory: pytest.TempPathFactory) -> Stacks:
    """The full stack graph synthesized ONCE from the default config, shared across every test (in
    every synth module) that only inspects it. Each ``Template.from_stack`` call re-reads a stack,
    and nothing here mutates the stacks, so sharing is safe and avoids re-synthesizing the 10-stack
    graph per test or per module. Tests that need a non-default config still call ``_build``
    directly."""
    return _build(tmp_path_factory.mktemp("default_graph"))


@pytest.fixture(scope="session")
def two_project_stacks(tmp_path_factory: pytest.TempPathFactory) -> Stacks:
    """The stack graph of the default config plus two more projects (`_two_project_stacks`),
    synthesized once and shared like ``default_stacks``."""
    return _two_project_stacks(tmp_path_factory.mktemp("two_project_graph"))
