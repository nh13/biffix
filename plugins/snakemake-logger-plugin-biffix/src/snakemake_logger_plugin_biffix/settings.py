"""Plugin settings — CLI options exposed as ``--logger-biffix-<field>`` (Snakemake >= 9.0).

The biffix coordinator supplies the run id, ingest endpoint, capability token and (optionally) the
events.jsonl name through the ``BIFFIX_RUN_ID`` / ``BIFFIX_INGEST_URL`` / ``BIFFIX_INGEST_TOKEN`` /
``BIFFIX_EVENT_FILE`` env vars, which the handler reads directly (see ``handler.py``); this is the
coordinator's contract (``docker/coordinator/entrypoint.sh``). Each can also be passed on the CLI,
and a non-empty CLI value wins over the env var. Every field has ``env_var: False``, so
Snakemake's settings machinery never sources one from its own derived variable name
(``SNAKEMAKE_LOGGER_BIFFIX_*``): the ``BIFFIX_*`` names are the only env channel, and
``BIFFIX_INGEST_TOKEN`` is how to keep the secret token off the command line.
"""

from dataclasses import dataclass, field

from snakemake_interface_logger_plugins.settings import LogHandlerSettingsBase

# The events.jsonl name when neither --logger-biffix-event-file nor BIFFIX_EVENT_FILE is set. The
# coordinator's `events_file` (docker/coordinator/lib/events.sh) reads the same default.
DEFAULT_EVENT_FILE = ".biffix_events.jsonl"


@dataclass
class LogHandlerSettings(LogHandlerSettingsBase):
    run_id: str = field(
        default="",
        metadata={"help": "biffix run id this workflow belongs to.", "env_var": False},
    )
    ingest_url: str = field(
        default="",
        metadata={"help": "biffix ingest route base URL.", "env_var": False},
    )
    ingest_token: str = field(
        default="",
        metadata={
            "help": "Per-run capability token (a secret; prefer the BIFFIX_INGEST_TOKEN env var).",
            "env_var": False,
        },
    )
    event_file: str = field(
        default="",
        metadata={
            "help": (
                "Name of the durable events.jsonl archival replay copy, relative to the directory "
                f"Snakemake starts in (default: BIFFIX_EVENT_FILE, else {DEFAULT_EVENT_FILE})."
            ),
            "env_var": False,
        },
    )
