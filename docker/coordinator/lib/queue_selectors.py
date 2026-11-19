"""Print the engine settings that send some tasks to another worker queue than the default.

Usage: queue_selectors.py <engine>

Reads BIFFIX_QUEUE_ASSIGNMENT ({"by_label": {label: queue_class}, "by_name": {name: queue_class}},
the version's `queues:` selectors as registration validated them; the default queue-class travels
as BIFFIX_QUEUE_CLASS) and BIFFIX_QUEUE_CLASS_MAP ({queue_class_name: job_queue_arn}), and prints:

- nextflow: one tab-separated `withLabel <label> <queue ARN>` line per label, then one
  `withName <name> <queue ARN>` line per name, which lib/nextflow.sh writes into the `-c` config as
  `process { withLabel: '<label>' { queue = '<arn>' } }` (quoting the ARN with its groovy_str);
- snakemake: one JSON object of rule name to queue ARN, the executor's rule queue map
  (`--aws-batch-rule-queues`), when the assignment names any rule.

Nothing for no assignment. Exits 1, saying why on stderr, when a queue-class has no queue in the
map, a queue has a control character, or the assignment is not one registration would accept
(labels are Nextflow's only; every Nextflow label and name is an exact one, since Nextflow reads
selectors as regular expressions; a Snakemake rule name is any non-empty name with no control
character, since the executor's rule queue map is matched exactly and Snakemake names a rule with
any string).

Registration already checks all of this: the source of truth is
`crates/biffix-workflow/src/queues.rs` (`QueueAssignment::validate`). The checks here repeat it on
purpose, as defence in depth: the assignment reaches this script through the job's environment,
and what it prints is written into the engine's config, so a selector that is not an exact name
would match other tasks, and a quote or line break would change the config. The grammars are kept
in step with queues.rs by `schemas/queue-selector-cases.json`, which the tests replay here.
(worker_queue.py's map parsing is not imported: lib/ is not on the import path when the tests load
this file, and its errors would not say which variable is wrong.)
"""

import json
import os
import re
import sys
import unicodedata

# A Nextflow label, as its `label` directive accepts one (ProcessBuilder.isValidLabel).
_LABEL_WORD = r"[A-Za-z](?:[A-Za-z0-9_]*[A-Za-z0-9])?"
LABEL = re.compile(rf"{_LABEL_WORD}(?:={_LABEL_WORD})?")


def is_process_name(name: str) -> bool:
    """A Nextflow process name: its own, an alias, or a fully qualified `WF:SUB:PROC`, each part
    an identifier (queues.rs PROCESS_NAME: XID_Start or `_`, then XID_Continue, which is what
    str.isidentifier checks; a regex `\\w` would refuse combining marks registration accepts)."""
    return all(part.isidentifier() for part in name.split(":"))


def is_rule_name(name: str) -> bool:
    """A Snakemake rule name as the rule queue map can carry it (queues.rs `is_rule_name`): any
    non-empty name with no control character."""
    return bool(name) and not _has_control(name)


def _has_control(value: str) -> bool:
    # Rust's char::is_control: the C0 and C1 control characters (Unicode category Cc).
    return any(unicodedata.category(c) == "Cc" for c in value)


class AssignmentError(Exception):
    """The assignment cannot be turned into engine settings."""


def _object(raw: str, what: str) -> dict[str, object]:
    try:
        value: object = json.loads(raw or "{}")
    except ValueError as e:
        raise AssignmentError(f"{what} is not JSON") from e
    if not isinstance(value, dict):
        raise AssignmentError(f"{what} is not a JSON object")
    return value


def _selectors(assignment: dict[str, object], key: str) -> dict[str, str]:
    value = assignment.get(key, {})
    if not isinstance(value, dict) or not all(
        isinstance(k, str) and isinstance(v, str) for k, v in value.items()
    ):
        raise AssignmentError(f"BIFFIX_QUEUE_ASSIGNMENT.{key} is not an object of strings")
    return value


def parse_assignment(raw: str) -> tuple[dict[str, str], dict[str, str]]:
    """BIFFIX_QUEUE_ASSIGNMENT's `by_label` and `by_name` selectors."""
    assignment = _object(raw, "BIFFIX_QUEUE_ASSIGNMENT")
    # Strict where the backend's stored type is tolerant: a key this coordinator does not know is
    # an assignment it cannot apply, and running every task on the default queue instead would
    # route work where the version did not ask for it. Failing the run says so.
    unknown = sorted(set(assignment) - {"by_label", "by_name"})
    if unknown:
        raise AssignmentError(f"BIFFIX_QUEUE_ASSIGNMENT has unknown keys: {', '.join(unknown)}")
    return _selectors(assignment, "by_label"), _selectors(assignment, "by_name")


def settings(
    engine: str, by_label: dict[str, str], by_name: dict[str, str], raw_queues: str
) -> list[str]:
    """The engine's settings for the selectors, one per line (see the module docstring)."""
    queues = _object(raw_queues, "BIFFIX_QUEUE_CLASS_MAP")

    def queue_of(queue_class: str, at: str) -> str:
        queue = queues.get(queue_class)
        if not isinstance(queue, str) or not queue:
            raise AssignmentError(
                f"queue-class {queue_class!r} ({at}) has no queue in BIFFIX_QUEUE_CLASS_MAP "
                f"(it has: {', '.join(queues) or 'none'})"
            )
        # A line break or tab would split the line nextflow.sh reads; nothing legitimate has one.
        if _has_control(queue):
            raise AssignmentError(f"the worker queue of {queue_class!r} has a control character")
        return queue

    lines: list[str] = []
    if engine == "nextflow":
        for label, queue_class in by_label.items():
            if not LABEL.fullmatch(label):
                raise AssignmentError(f"by_label {label!r} is not a Nextflow label")
            lines.append(f"withLabel\t{label}\t{queue_of(queue_class, f'by_label.{label}')}")
        for name, queue_class in by_name.items():
            if not is_process_name(name):
                raise AssignmentError(f"by_name {name!r} is not a Nextflow process name")
            lines.append(f"withName\t{name}\t{queue_of(queue_class, f'by_name.{name}')}")
    elif engine == "snakemake":
        if by_label:
            raise AssignmentError("by_label applies only to Nextflow; Snakemake rules have none")
        rule_queues: dict[str, str] = {}
        for name, queue_class in by_name.items():
            if not is_rule_name(name):
                raise AssignmentError(f"by_name {name!r} is not a Snakemake rule name")
            rule_queues[name] = queue_of(queue_class, f"by_name.{name}")
        if rule_queues:
            lines.append(json.dumps(rule_queues, sort_keys=True))
    else:
        raise AssignmentError(f"unknown engine {engine!r}")
    return lines


if __name__ == "__main__":
    if len(sys.argv) != 2:
        sys.exit("usage: queue_selectors.py <engine>")
    try:
        labels, names = parse_assignment(os.environ.get("BIFFIX_QUEUE_ASSIGNMENT", ""))
        out = settings(sys.argv[1], labels, names, os.environ.get("BIFFIX_QUEUE_CLASS_MAP", ""))
    except AssignmentError as e:
        print(f"biffix-coordinator: {e}", file=sys.stderr)
        sys.exit(1)
    for line in out:
        print(line)
    # Counted from the selectors, not the lines printed: Snakemake's map is one line.
    count = len(labels) + len(names)
    print(f"biffix-coordinator: {count} task selector(s) on other worker queues", file=sys.stderr)
