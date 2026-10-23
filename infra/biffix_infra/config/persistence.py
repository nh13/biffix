"""The persistence section: the Aurora event store's backup window and key (PersistenceStack)."""

from __future__ import annotations

from biffix_infra.config.fields import Fields, Raw, count, optional_texts, present

# The backup retention window RDS allows, in days.
_BACKUP_RETENTION_DAYS = (1, 35)


def parse(raw: Raw) -> Fields:
    """The persistence keys, as DeployConfig fields."""
    fields = present(raw, "aurora_backup_retention_days")
    if "aurora_backup_retention_days" in fields:
        count(
            "aurora_backup_retention_days",
            fields["aurora_backup_retention_days"],
            *_BACKUP_RETENTION_DAYS,
        )
    return {**fields, **optional_texts(raw, "", "aurora_key_arn")}
