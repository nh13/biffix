"""Deregister the engine job definitions coordinators registered before the backend took that over,
run by deploy.sh after a deploy.

Coordinators used to register their child jobs' job definitions themselves: the Snakemake AWS
Batch executor one per job (`snakejob-def-<rule>-<uuid>`, deregistered on cleanup unless the run
died first), nf-amazon one per image (`nf-<image>`, never deregistered). The backend now registers
every task job definition (`biffix-<tenant>_task-…`) and coordinators may submit nothing else, so
those definitions are dead weight: this step deregisters the ACTIVE ones.

Only definitions this tenant's coordinators made: the name starts with an engine prefix, it
carries a biffix tag (`biffix:project` or `biffix:run`, as each was registered with), and its job
role is one of this tenant's project run roles (read from the projects' SSM contexts), so another
tenant in the same account, or anything not biffix's, is left alone; the step reports how many it
left and why. Deregistering is idempotent (a deregistered definition is INACTIVE and not listed
again), so re-running it is safe, and `--dry-run` only prints what it would do.

The plan is pure; `main` lists the definitions and deregisters with the aws CLI.

A one-time migration, kept in every full deploy because an operator may skip releases: remove it
(this module, its tests and deploy.sh's step) after the next minor release.
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
from dataclasses import dataclass, field
from typing import Any

# The name prefixes the engines gave the definitions they registered: nf-amazon's and the Snakemake
# AWS Batch executor's.
LEGACY_PREFIXES = ("nf-", "snakejob-def-")
# The tags a coordinator registered them with (batch_access.PROJECT_TAG and RUN_TAG; this module
# stays free of CDK so deploy.sh runs it quickly).
BIFFIX_TAGS = ("biffix:project", "biffix:run")


@dataclass(frozen=True)
class JobDefinition:
    arn: str
    name: str
    tags: dict[str, str] = field(default_factory=dict)
    job_role: str | None = None


@dataclass
class Plan:
    """The definitions to deregister, and how many engine-named ones are left (biffix-tagged but
    another role's, i.e. another tenant's or a removed project's; not biffix-tagged)."""

    deregister: list[JobDefinition] = field(default_factory=list)
    other_roles: int = 0
    untagged: int = 0


def plan_cleanup(definitions: list[JobDefinition], run_roles: set[str]) -> Plan:
    """Which of the ACTIVE `definitions` this tenant's coordinators registered (an engine name, a
    biffix tag, and a job role in `run_roles`)."""
    plan = Plan()
    for d in definitions:
        if not d.name.startswith(LEGACY_PREFIXES):
            continue
        if not any(tag in d.tags for tag in BIFFIX_TAGS):
            plan.untagged += 1
        elif d.job_role not in run_roles:
            plan.other_roles += 1
        else:
            plan.deregister.append(d)
    return plan


# ── AWS side (aws CLI; deploy.sh already requires it) ─────────────────────────────────────────


def _aws(*args: str) -> dict[str, Any]:
    """An aws command's JSON answer (its shape is the command's, read unchecked)."""
    out = subprocess.run(
        ["aws", *args, "--output", "json"], check=True, capture_output=True, text=True
    )
    answer: dict[str, Any] = json.loads(out.stdout or "{}")
    return answer


def list_active() -> list[JobDefinition]:
    """Every ACTIVE job definition in the account and region (the aws CLI follows every page)."""
    answer = _aws("batch", "describe-job-definitions", "--status", "ACTIVE")
    return [
        JobDefinition(
            arn=d["jobDefinitionArn"],
            name=d["jobDefinitionName"],
            tags=d.get("tags") or {},
            job_role=(d.get("containerProperties") or {}).get("jobRoleArn"),
        )
        for d in answer.get("jobDefinitions", [])
    ]


def run_roles(projects_path: str) -> set[str]:
    """Each of this tenant's projects' run role ARN, from its SSM context (`run_role_arn`). The
    path is read without its trailing slash, as the backend reads it."""
    answer = _aws(
        "ssm", "get-parameters-by-path", "--path", projects_path.rstrip("/"), "--recursive"
    )
    roles = set()
    for parameter in answer.get("Parameters", []):
        role = json.loads(parameter["Value"]).get("run_role_arn")
        if role:
            roles.add(role)
    return roles


def apply(plan: Plan, dry_run: bool) -> list[str]:
    """Deregister `plan`'s definitions (or print them, with `dry_run`); return the ones Batch
    refused, with why."""
    refused: list[str] = []
    for d in plan.deregister:
        print(f"  deregister {d.arn}")
        if dry_run:
            continue
        try:
            _aws("batch", "deregister-job-definition", "--job-definition", d.arn)
        except subprocess.CalledProcessError as e:
            refused.append(f"{d.arn} ({(e.stderr or '').strip()})")
    return refused


def main(argv: list[str] | None = None) -> int:
    p = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    p.add_argument("--projects-path", required=True, help="e.g. /biffix/<tenant>/projects/")
    p.add_argument("--dry-run", action="store_true")
    a = p.parse_args(argv)
    try:
        return _clean(a)
    except subprocess.CalledProcessError as e:
        # The call and AWS's own error, not a bare "returned non-zero exit status".
        call = " ".join(e.cmd[:3]) if isinstance(e.cmd, list) else e.cmd
        print(f"{call}: {(e.stderr or '').strip()}", file=sys.stderr)
        return 1


def _clean(a: argparse.Namespace) -> int:
    roles = run_roles(a.projects_path)
    if not roles:
        # Nothing to tell this tenant's definitions by: deregister nothing rather than guess.
        print(f"no project run roles under {a.projects_path}: nothing to deregister")
        return 0
    plan = plan_cleanup(list_active(), roles)
    print(
        f"engine job definitions: {len(plan.deregister)} to deregister; left alone: "
        f"{plan.other_roles} biffix-tagged with another role (another tenant's or a removed "
        f"project's), {plan.untagged} not biffix-tagged"
    )
    refused = apply(plan, a.dry_run)
    for line in refused:
        print(f"Batch did not deregister {line}", file=sys.stderr)
    return 1 if refused else 0


if __name__ == "__main__":
    sys.exit(main())
