"""Print the names of a Snakemake workflow's rules marked for between-workflow caching.

Usage: snakemake_cache_rules.py <Snakefile> <configfile>

The coordinator passes these names to `snakemake --cache`: a bare `--cache` is not forwarded to
remote jobs, and naming a rule that is not marked `cache:` is an error, so only the marked rules
may be listed. Loads the workflow the same way `snakemake --list-rules` does.

Loading the workflow runs the Snakefile's own top-level code, which may print anything. So each
name is printed as `BIFFIX_CACHE_RULE=<name>` (the coordinator keeps only those lines), and
Python-level output while the workflow loads goes to stderr. Errors go to stderr, with a non-zero
exit, for the coordinator to log.
"""

import contextlib
import sys
from pathlib import Path
from typing import Any

PREFIX = "BIFFIX_CACHE_RULE="


def cache_rule_names(workflow: Any) -> list[str]:
    """The rules of `workflow` (Snakemake's internal Workflow, read by duck type across Snakemake
    versions) marked `cache:`.

    Snakemake 9.17 moved the marking onto the rule (`rule.cache`, a RuleCache whose `output` is the
    cached output mode); before that it lives in `workflow.cache_rules` (rule name -> mode).
    """
    names: list[str] = []
    for rule in workflow.rules:
        if hasattr(rule, "cache"):
            marked = rule.cache is not None and bool(rule.cache.output)
        else:
            marked = bool(getattr(workflow, "cache_rules", {}).get(rule.name))
        if marked:
            names.append(rule.name)
    return names


def main() -> None:
    from snakemake.api import SnakemakeApi
    from snakemake.settings.types import ConfigSettings, OutputSettings, ResourceSettings

    snakefile, configfile = Path(sys.argv[1]), Path(sys.argv[2])
    with contextlib.redirect_stdout(sys.stderr), SnakemakeApi(OutputSettings(quiet={"all"})) as api:
        workflow = api.workflow(
            resource_settings=ResourceSettings(),
            config_settings=ConfigSettings(configfiles=[configfile]),
            snakefile=snakefile,
        )
        names = cache_rule_names(workflow._workflow)
    for name in names:
        print(f"{PREFIX}{name}")


if __name__ == "__main__":
    main()
