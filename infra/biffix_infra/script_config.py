"""Read deploy-config values for the shell scripts (deploy.sh, teardown.sh, build-and-push.sh).

The scripts must see the config exactly as the CDK app does, so they never parse the YAML
themselves: they ask this module, which goes through `DeployConfig.load`.

    python -m biffix_infra.script_config <config> <key> [<key> ...]

prints one line per requested key, in order. An unset optional value prints an empty line, and a
list value prints its items separated by single spaces. Unknown keys and unreadable configs exit
non-zero. The config path is taken as given; the scripts resolve it before calling.
"""

from __future__ import annotations

import sys
from collections.abc import Callable

import yaml

from biffix_infra.config import DeployConfig


def _host_aws_cli_zips(config: DeployConfig) -> list[str]:
    cli = config.host_aws_cli
    return [
        f"{arch}:{sha}:{cli.key(arch)}"
        for arch, sha in (("x86_64", cli.sha256_x86_64), ("aarch64", cli.sha256_aarch64))
    ]


_KEYS: dict[str, Callable[[DeployConfig], str | list[str] | None]] = {
    "tenant": lambda c: c.tenant,
    "region": lambda c: c.region,
    "account": lambda c: c.account,
    "google_secret_name": lambda c: c.google_secret_name,
    "app_domain": lambda c: c.app_domain,
    "app_certificate_arn": lambda c: c.app_certificate_arn,
    "coordinator_image_tag": lambda c: c.coordinator_image_tag,
    "stack_names": lambda c: c.stack_names(),
    "data_stack": lambda c: c.stack_name("data"),
    "edge_stack": lambda c: c.stack_name("edge"),
    "network_stack": lambda c: c.stack_name("network"),
    "persistence_stack": lambda c: c.stack_name("persistence"),
    # Buckets a project references but biffix does not own; teardown must never delete them.
    "imported_buckets": lambda c: c.imported_project_buckets,
    "host_aws_cli_version": lambda c: c.host_aws_cli.version,
    # `<arch>:<sha256>:<reference-bucket key>` per host architecture.
    "host_aws_cli_zips": _host_aws_cli_zips,
}


def values(config: DeployConfig, keys: list[str]) -> list[str]:
    """The printed line for each key (see the module docstring)."""
    lines = []
    for key in keys:
        if key not in _KEYS:
            raise KeyError(f"unknown key {key!r}; expected one of {sorted(_KEYS)}")
        value = _KEYS[key](config)
        line = " ".join(value) if isinstance(value, list) else (value or "")
        if "\n" in line or "\r" in line:
            raise ValueError(f"{key}: value contains a line break: {line!r}")
        lines.append(line)
    return lines


def main(argv: list[str]) -> int:
    if len(argv) < 2:
        print(
            "usage: python -m biffix_infra.script_config <config> <key> [<key> ...]",
            file=sys.stderr,
        )
        return 2
    try:
        config = DeployConfig.load(argv[0])
        lines = values(config, argv[1:])
    except (OSError, KeyError, ValueError, yaml.YAMLError) as e:
        print(f"script_config: {e}", file=sys.stderr)
        return 1
    for line in lines:
        print(line)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
