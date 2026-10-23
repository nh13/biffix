"""Small helpers the section parsers share.

One rule for a value the config writes: null means what the key documents for it (its default, or
"none" where a key says so, as `retention` does), and any other value must already have the key's
type. Nothing is coerced, so a quoted `"false"` is not true, an unquoted `1.10` is not the name
"1.10", and a string is not split into a list.
"""

from __future__ import annotations

from typing import Any, TypeGuard

# A mapping of the deploy config as YAML loads it. Its values are whatever the file wrote (a str,
# int, float, bool, None, list or mapping), unchecked until a section parser reads and checks them.
Raw = dict[str, Any]
# A section's DeployConfig fields: keyword arguments of their own field types, for
# `DeployConfig(**fields)`.
Fields = dict[str, Any]


def present(block: Raw, *keys: str) -> Fields:
    """The `keys` `block` sets (a null value included), so a key the config leaves out keeps its
    dataclass default: each default is written once, on the dataclass field."""
    return {key: block[key] for key in keys if key in block}


def is_count(value: object, minimum: int) -> TypeGuard[int]:
    """Whether `value` is a whole number of at least `minimum` (YAML's `true` is not one)."""
    return not isinstance(value, bool) and isinstance(value, int) and value >= minimum


def count(where: str, value: object, minimum: int, maximum: int | None = None) -> int:
    """`value`, a whole number from `minimum` to `maximum` (inclusive; no upper bound when None);
    anything else (null included) is refused."""
    if not is_count(value, minimum) or (maximum is not None and value > maximum):
        bound = f"{minimum}..{maximum}" if maximum is not None else f">= {minimum}"
        raise ValueError(f"{where} {value!r}: expected a whole number ({bound})")
    return value


def text(where: str, value: object) -> str:
    """`value`, a non-empty string; anything else (null included) is refused."""
    if not isinstance(value, str) or not value:
        raise ValueError(f"{where} {value!r}: expected a non-empty string")
    return value


def optional_text(where: str, value: object) -> str | None:
    """`value`, a non-empty string, or None when it is null."""
    return None if value is None else text(where, value)


def optional_texts(block: Raw, where: str, *keys: str) -> Fields:
    """The optional string `keys` `block` sets (as `present`), each checked with `optional_text`;
    `where` prefixes their names in errors (`"image_archive."`, or "" at the top level)."""
    return {
        key: optional_text(f"{where}{key}", value) for key, value in present(block, *keys).items()
    }


def flag(where: str, value: object, default: bool = False) -> bool:
    """`value`, a YAML boolean, or `default` when it is null."""
    if value is None:
        return default
    if not isinstance(value, bool):
        raise ValueError(f"{where} {value!r}: expected true or false")
    return value


def text_list(where: str, value: object) -> list[str]:
    """`value`, a list of non-empty strings, or [] when it is null."""
    if value is None:
        return []
    if not isinstance(value, list):
        raise ValueError(f"{where} {value!r}: expected a list of strings")
    return [text(f"{where} entry", entry) for entry in value]
