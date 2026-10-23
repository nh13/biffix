"""Where the deploy goes: `tenant`, `region` and `account`."""

from __future__ import annotations

import re

from biffix_infra.config.fields import Fields, Raw, text

_TENANT = re.compile(r"[a-z0-9][a-z0-9-]*")
_TENANT_MAX = 32


def parse_account(value: object) -> str | None:
    """The deploy's AWS account id as its 12-digit string. An unquoted YAML id parses as an int:
    one of 12 digits is the id, but a shorter one has lost its leading zeros and may have been
    read as octal (`0123…` with only digits 0-7), so it is refused rather than guessed."""
    if value is None:
        return None
    if isinstance(value, bool) or not isinstance(value, (int, str)):
        raise ValueError(f"account {value!r}: expected a 12-digit AWS account id")
    if isinstance(value, int) and value < 10**11:
        raise ValueError(
            f"account {value!r}: YAML read the id as a number and dropped its leading zeros; "
            'quote it (account: "0…")'
        )
    account = str(value) if isinstance(value, int) else value.strip()
    if len(account) != 12 or not account.isdigit():
        raise ValueError(f"account {value!r}: expected a 12-digit AWS account id")
    return account


def parse_tenant(value: object) -> str:
    """The tenant: lowercase letters, digits and `-`, at most 32 characters. It names every stack
    (`biffix-<tenant>-<part>`, which allows only letters, digits and `-`), and CloudFormation
    lowercases it in the bucket and repository names it generates, which teardown and the image
    build find by name; the length leaves room in every name it is part of."""
    if (
        not isinstance(value, str)
        or not _TENANT.fullmatch(value)
        or len(value) > _TENANT_MAX
        or value.startswith("biffix-")
    ):
        raise ValueError(
            f"tenant {value!r}: expected lowercase letters, digits and `-` (at most {_TENANT_MAX}, "
            "starting with a letter or digit, not with `biffix-`)"
        )
    return value


def parse(raw: Raw) -> Fields:
    """`tenant` and `region` (required) and `account`, as DeployConfig fields."""
    return {
        "tenant": parse_tenant(raw["tenant"]),
        "region": text("region", raw["region"]),
        "account": parse_account(raw.get("account")),
    }
