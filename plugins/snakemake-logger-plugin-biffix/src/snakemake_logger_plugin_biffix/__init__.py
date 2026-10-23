"""biffix Snakemake logger plugin (Snakemake >= 9.0).

Emits biffix ``EngineEvent`` RunEvents (the producer contract,
``schemas/wire.engine.schema.json``) both to the biffix ingest route (live) and to a durable
``events.jsonl`` (the archival replay copy). LogRecord-parsing logic is adapted from snakesee's
logger plugin (MIT; see the NOTICE section of README.md); this package has **no dependency** on
snakesee.
"""

from snakemake_logger_plugin_biffix.handler import LogHandler
from snakemake_logger_plugin_biffix.settings import LogHandlerSettings

__all__ = ["LogHandler", "LogHandlerSettings"]
