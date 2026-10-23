"""Canonical biffix RBAC group → role mapping.

One source of truth, consumed by two stacks: IdentityStack creates the Cognito groups, and ApiStack
injects the same mapping into the backend as ``BIFFIX_GROUP_ROLE_MAP``. Keeping it here means a
group can never exist without a role binding (or vice versa).

Membership is managed in the IdP/AWS console, never by biffix (identity is
managed through the IdP). Cognito emits its own group names in the ``cognito:groups``
claim even for federated (Google) identities, which is what the backend's RBAC reads. Federating
group membership from Google Workspace later replaces only *where* membership comes from, not this
mapping.
"""

from __future__ import annotations

# Ordered least → most privileged; the backend grants the highest role among a user's groups.
RBAC_GROUP_ROLES: dict[str, str] = {
    "biffix-viewers": "viewer",
    "biffix-operators": "operator",
    "biffix-developers": "developer",
    "biffix-admins": "admin",
}
