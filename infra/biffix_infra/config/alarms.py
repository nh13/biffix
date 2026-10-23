"""The alarms section: who the operational alarms email (the ingest stack's alerts topic)."""

from __future__ import annotations

from biffix_infra.config.fields import Fields, Raw, optional_texts


def parse(raw: Raw) -> Fields:
    """The alarm keys, as DeployConfig fields."""
    return optional_texts(raw, "", "alert_email")
