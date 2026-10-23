"""Print the worker queue ARN child jobs are submitted to.

Usage: worker_queue.py [<queue class>]

Reads BIFFIX_QUEUE_CLASS_MAP ({queue_class_name: job_queue_arn}, the worker queue-classes in
deploy-config order). A named queue-class prints its ARN; none prints the map's FIRST entry (the
deploy config lists the intended default first). Prints an empty line when there is no such entry,
or the map is missing or is not a JSON object, and says which on stderr.
"""

import json
import os
import sys


def queues_of(raw: str) -> dict[str, object] | None:
    """The map, or None when it is not a JSON object."""
    try:
        queues: object = json.loads(raw or "{}")
    except ValueError:
        return None
    return queues if isinstance(queues, dict) else None


def _entry(queues: dict[str, object], want: str) -> object:
    """The named queue-class's entry, or the map's first one when none is named."""
    if want:
        return queues.get(want, "")
    return next(iter(queues.values()), "")


def resolve(raw: str, want: str) -> str:
    """The queue ARN, or "" when there is no such entry or it is not a string."""
    queue = _entry(queues_of(raw) or {}, want)
    return queue if isinstance(queue, str) else ""


def why_none(raw: str, want: str) -> str:
    """Why `resolve` found no queue."""
    queues = queues_of(raw)
    if queues is None:
        return "BIFFIX_QUEUE_CLASS_MAP is not a JSON object"
    if not queues:
        return "BIFFIX_QUEUE_CLASS_MAP is missing or empty"
    if not want or want in queues:
        name = f"queue-class {want!r}" if want else "the first queue-class"
        return f"{name} in BIFFIX_QUEUE_CLASS_MAP has no queue ARN string"
    return f"no queue-class {want!r} in BIFFIX_QUEUE_CLASS_MAP (it has: {', '.join(queues)})"


if __name__ == "__main__":
    want = sys.argv[1] if len(sys.argv) > 1 else ""
    raw = os.environ.get("BIFFIX_QUEUE_CLASS_MAP", "")
    queue = resolve(raw, want)
    if not queue:
        print(f"biffix-coordinator: {why_none(raw, want)}", file=sys.stderr)
    print(queue)
