"""The identity section: who may sign in and what they may do by default (IdentityStack, the
backend's OAuth client)."""

from __future__ import annotations

from dataclasses import dataclass

from biffix_infra.config.fields import Fields, Raw, optional_texts, present, text
from biffix_infra.rbac import RBAC_GROUP_ROLES

COGNITO_MFA_MODES = ("off", "optional", "required")


@dataclass(frozen=True)
class GoogleOidc:
    """Google Workspace federation into the Cognito pool (manage-identity-through-the-IdP).

    Only the (non-secret) OAuth client_id lives in config. The client SECRET is never in config or
    git: `deploy.sh` provisions it interactively into Secrets Manager at the derived name
    `biffix/<tenant>/google-oidc` in the target account (prompting once if absent), and Cognito
    reads it from there at deploy — see `DeployConfig.google_secret_name`.
    """

    client_id: str


def parse(raw: Raw) -> Fields:
    """The identity keys, as DeployConfig fields."""
    fields = {
        **present(raw, "cognito_mfa"),
        **optional_texts(raw, "", "cognito_domain_prefix"),
    }
    if "cognito_mfa" in fields:
        # YAML reads the documented, unquoted `off` as false; nothing else is coerced.
        if fields["cognito_mfa"] is False:
            fields["cognito_mfa"] = "off"
        if fields["cognito_mfa"] not in COGNITO_MFA_MODES:
            raise ValueError(
                f"cognito_mfa {fields['cognito_mfa']!r}, expected one of "
                f"{sorted(COGNITO_MFA_MODES)}"
            )
    # `or {}`: a present-but-null `authorization:` block means its defaults.
    authorization = raw.get("authorization") or {}
    if "authenticated_default_role" in authorization:
        role = authorization["authenticated_default_role"]
        # rbac.py is the one source of the role set.
        roles = set(RBAC_GROUP_ROLES.values())
        if role not in roles:
            raise ValueError(
                f"authorization.authenticated_default_role {role!r}, expected one of "
                f"{sorted(roles)}"
            )
        fields["authenticated_default_role"] = role
    if raw.get("google_oidc"):
        client_id = text("google_oidc.client_id", raw["google_oidc"].get("client_id"))
        fields["google_oidc"] = GoogleOidc(client_id=client_id)
        # Google federation rides the Cognito hosted UI, which needs a domain prefix.
        if not fields.get("cognito_domain_prefix"):
            raise ValueError(
                "google_oidc requires cognito_domain_prefix (the hosted-UI domain that hosts the "
                "OAuth redirect Google returns to)"
            )
    return fields
