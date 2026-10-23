"""The edge section: the app's own domain and its certificate (EdgeStack, the OAuth callbacks)."""

from __future__ import annotations

from biffix_infra.config.fields import Fields, Raw, optional_texts


def parse(raw: Raw) -> Fields:
    """The edge keys, as DeployConfig fields."""
    fields = optional_texts(raw, "", "app_domain", "app_certificate_arn")
    if fields.get("app_certificate_arn") and not fields.get("app_domain"):
        raise ValueError("app_certificate_arn requires app_domain")
    return fields
