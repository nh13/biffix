"""Print the engine settings that send some tasks to another worker queue than the default.

Usage: queue_selectors.py <engine>

Reads BIFFIX_QUEUE_ASSIGNMENT ({"by_label": {label: queue_class}, "by_name": {name: queue_class}},
the version's `queues:` selectors as registration validated them; the default queue-class travels
as BIFFIX_QUEUE_CLASS) and BIFFIX_QUEUE_CLASS_MAP ({queue_class_name: job_queue_arn}), and prints,
one per line:

- nextflow: a `process { withLabel: '<label>' { queue = '<arn>' } }` per label, then a
  `process { withName: '<name>' { queue = '<arn>' } }` per name, for the `-c` config;
- snakemake: one JSON object of rule name to queue ARN, the executor's rule queue map
  (`--aws-batch-rule-queues`), when the assignment names any rule.

Nothing for no assignment. Exits 1, saying why on stderr, when a queue-class has no queue in the
map, or the assignment is not one registration would accept (labels are Nextflow's only; every
Nextflow label and name is an exact one, since Nextflow reads selectors as regular expressions;
a Snakemake rule name is any non-empty name with no control character, since the executor's rule
queue map is matched exactly and Snakemake names a rule with any string).

Registration already checks all of this: the source of truth is
`crates/biffix-workflow/src/queues.rs` (`QueueAssignment::validate`). The checks here repeat it on
purpose, as defence in depth: the assignment reaches this script through the job's environment,
and what it prints is written into the engine's config, so a selector that is not an exact name
would match other tasks, and a quote or line break would change the config (the Snakemake map is
written as JSON, which quotes any name). Keep the grammars in
step with queues.rs. (worker_queue.py's map parsing is not imported: lib/ is not on the import path
when the tests load this file, and its errors would not say which variable is wrong.)
"""

import json
import os
import re
import sys

# A Nextflow label, as its `label` directive accepts one (ProcessBuilder.isValidLabel).
_LABEL_WORD = r"[A-Za-z](?:[A-Za-z0-9_]*[A-Za-z0-9])?"
LABEL = re.compile(rf"{_LABEL_WORD}(?:={_LABEL_WORD})?")
IDENTIFIER = r"[A-Za-z_][A-Za-z0-9_]*"
# A Nextflow process name: its own, an alias, or a fully qualified `WF:SUB:PROC`.
PROCESS = re.compile(rf"{IDENTIFIER}(?::{IDENTIFIER})*")


def is_rule_name(name: str) -> bool:
    """A Snakemake rule name as the rule queue map can carry it (queues.rs `is_rule_name`): any
    non-empty name with no control character."""
    return bool(name) and not any(_is_control(c) for c in name)


def _is_control(c: str) -> bool:
    # Rust's char::is_control: the C0 and C1 control characters.
    return ord(c) < 0x20 or 0x7F <= ord(c) <= 0x9F


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


def groovy_str(value: str, what: str) -> str:
    """`value` as a Groovy single-quoted string literal (lib/nextflow.sh's groovy_str)."""
    if any(ord(c) < 0x20 or ord(c) == 0x7F for c in value):
        raise AssignmentError(f"{what} has a control character; it cannot go into nextflow.config")
    escaped = value.replace("\\", "\\\\").replace("'", "\\'")
    return f"'{escaped}'"


def settings(engine: str, raw_assignment: str, raw_queues: str) -> list[str]:
    """The engine's settings for the assignment, one per line (see the module docstring)."""
    assignment = _object(raw_assignment, "BIFFIX_QUEUE_ASSIGNMENT")
    # Strict where the backend's stored type is tolerant: a key this coordinator does not know is
    # an assignment it cannot apply, and running every task on the default queue instead would
    # route work where the version did not ask for it. Failing the run says so.
    unknown = sorted(set(assignment) - {"by_label", "by_name"})
    if unknown:
        raise AssignmentError(f"BIFFIX_QUEUE_ASSIGNMENT has unknown keys: {', '.join(unknown)}")
    by_label = _selectors(assignment, "by_label")
    by_name = _selectors(assignment, "by_name")
    queues = _object(raw_queues, "BIFFIX_QUEUE_CLASS_MAP")

    def queue_of(queue_class: str, at: str) -> str:
        queue = queues.get(queue_class)
        if not isinstance(queue, str) or not queue:
            raise AssignmentError(
                f"queue-class {queue_class!r} ({at}) has no queue in BIFFIX_QUEUE_CLASS_MAP "
                f"(it has: {', '.join(queues) or 'none'})"
            )
        return queue

    lines: list[str] = []
    if engine == "nextflow":
        for label, queue_class in by_label.items():
            if not LABEL.fullmatch(label):
                raise AssignmentError(f"by_label {label!r} is not a Nextflow label")
            queue = groovy_str(queue_of(queue_class, f"by_label.{label}"), "a worker queue")
            lines.append(f"process {{ withLabel: '{label}' {{ queue = {queue} }} }}")
        for name, queue_class in by_name.items():
            if not PROCESS.fullmatch(name):
                raise AssignmentError(f"by_name {name!r} is not a Nextflow process name")
            queue = groovy_str(queue_of(queue_class, f"by_name.{name}"), "a worker queue")
            lines.append(f"process {{ withName: '{name}' {{ queue = {queue} }} }}")
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
    raw = os.environ.get("BIFFIX_QUEUE_ASSIGNMENT", "")
    try:
        out = settings(sys.argv[1], raw, os.environ.get("BIFFIX_QUEUE_CLASS_MAP", ""))
    except AssignmentError as e:
        print(f"biffix-coordinator: {e}", file=sys.stderr)
        sys.exit(1)
    for line in out:
        print(line)
    # Counted from the assignment, not the lines printed: Snakemake's map is one line.
    assignment = _object(raw, "BIFFIX_QUEUE_ASSIGNMENT")
    count = sum(len(_selectors(assignment, key)) for key in ("by_label", "by_name"))
    print(f"biffix-coordinator: {count} task selector(s) on other worker queues", file=sys.stderr)
