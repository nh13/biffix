"""The low-cost NAT: a single t4g.nano NAT instance on Amazon Linux 2023 (`nat_mode: instance`)."""

from __future__ import annotations

from aws_cdk import aws_ec2 as ec2
from constructs import Construct


def nat_instance_provider(scope: Construct, *, key_pair_name: str) -> ec2.NatInstanceProviderV2:
    """A NAT instance provider whose instance forwards only (no inbound from the internet) and runs
    user data that works on AL2023. Its break-glass key pair (`key_pair_name`, the private key in
    SSM) is `scope`'s child `NatInstanceKey`. Once the VPC exists, `allow_vpc_forwarding` opens the
    instance to the traffic it forwards."""
    # CDK's default NAT-instance user data resolves the uplink interface with `route`
    # (net-tools), which Amazon Linux 2023 does NOT ship — the MASQUERADE rule then binds
    # to an empty interface and all egress silently fails ("dispatch failure" in clients).
    # Provide AL2023-correct user data that uses iproute (`ip`), always present.
    user_data = ec2.UserData.for_linux()
    user_data.add_commands(
        "set -euxo pipefail",
        # A 512MB t4g.nano OOM-kills `dnf` mid-install; add 1GB swap first so the package
        # install and NAT setup complete on the cheap instance.
        "fallocate -l 1G /swapfile || dd if=/dev/zero of=/swapfile bs=1M count=1024",
        "chmod 600 /swapfile",
        "mkswap /swapfile",
        "swapon /swapfile",
        "dnf install -y iptables-services",
        "systemctl enable --now iptables",
        'echo "net.ipv4.ip_forward=1" > /etc/sysctl.d/custom-ip-forwarding.conf',
        "sysctl -p /etc/sysctl.d/custom-ip-forwarding.conf",
        "UPLINK=$(ip -o -4 route show to default | awk '{print $5; exit}')",
        'iptables -t nat -A POSTROUTING -o "$UPLINK" -j MASQUERADE',
        "iptables -F FORWARD",
        "service iptables save",
    )
    # CDK's default INBOUND_AND_OUTBOUND mode opens all inbound traffic from 0.0.0.0/0.
    # A NAT instance only needs forwarded traffic from this VPC; keep internet-facing
    # ingress closed while allowing outbound egress.
    # A managed key pair (SSM-stored private key) gives break-glass access. Updating
    # UserData alone only stop/starts the instance, and cloud-init does not re-run it, so
    # a change to the user data above takes effect only on a fresh instance.
    key = ec2.KeyPair(scope, "NatInstanceKey", key_pair_name=key_pair_name)
    return ec2.NatProvider.instance_v2(
        instance_type=ec2.InstanceType.of(ec2.InstanceClass.T4G, ec2.InstanceSize.NANO),
        default_allowed_traffic=ec2.NatTrafficDirection.OUTBOUND_ONLY,
        user_data=user_data,
        key_pair=key,
        machine_image=ec2.MachineImage.latest_amazon_linux2023(
            cpu_type=ec2.AmazonLinuxCpuType.ARM_64
        ),
    )


def allow_vpc_forwarding(provider: ec2.NatInstanceProviderV2, vpc: ec2.IVpc) -> None:
    """Let `provider`'s NAT instances take the traffic they forward: anything from `vpc`'s own
    addresses, and nothing else."""
    for gateway in provider.gateway_instances:
        gateway.connections.allow_from(
            ec2.Peer.ipv4(vpc.vpc_cidr_block),
            ec2.Port.all_traffic(),
            "forward traffic from private VPC addresses only",
        )
