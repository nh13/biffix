"""The network section: `nat_mode` and `worker_egress` (NetworkStack)."""

from __future__ import annotations

from biffix_infra.config.fields import Fields, Raw, present

NAT_MODES = ("gateway", "instance")
WORKER_EGRESS = ("nat", "ecr_s3_only")


def parse(raw: Raw) -> Fields:
    """The network keys the config sets, as DeployConfig fields (the rest keep their defaults)."""
    fields = present(raw, "nat_mode", "worker_egress")
    if "nat_mode" in fields and fields["nat_mode"] not in NAT_MODES:
        raise ValueError(f"nat_mode {fields['nat_mode']!r}, expected one of {sorted(NAT_MODES)}")
    if "worker_egress" in fields and fields["worker_egress"] not in WORKER_EGRESS:
        raise ValueError(
            f"worker_egress {fields['worker_egress']!r}, expected one of {sorted(WORKER_EGRESS)}"
        )
    return fields
