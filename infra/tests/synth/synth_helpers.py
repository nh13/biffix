"""What the synth tests share: the frozen deploy config, the stack graph built from it, and
readers for the synthesized IAM policies, Lambda environments and project contexts.

`_build` wires the graph with `app_graph.build_stacks`, the function app.py calls, so the tests
assert on the cross-stack grants a deploy really synthesizes. The deploy config's own parsing and
validation is in tests/test_config_keys.py.
"""

import dataclasses
import fnmatch
import json
from collections.abc import Iterable, Mapping
from pathlib import Path
from typing import Any

import aws_cdk as cdk
from aws_cdk import aws_iam as iam
from aws_cdk.assertions import Template
from constructs import Construct, IConstruct

from biffix_infra.app_graph import Stacks, build_stacks
from biffix_infra.config import (
    DataSource,
    DeployConfig,
    Project,
)
from biffix_infra.stacks.api_stack import ApiStack
from biffix_infra.stacks.project_stack import ProjectStack
from tests.deploy_config_file import FIXTURE

CONFIG = DeployConfig.load(FIXTURE)

# A piece of a synthesized CloudFormation template (a resource, an IAM statement, ...), as the
# JSON `Template` gives it.
Json = dict[str, Any]


def _build(tmp_path: Path, config: DeployConfig = CONFIG) -> Stacks:
    """The deploy's stack graph, wired by the same `build_stacks` app.py calls, and returned by
    short name (the stacks keep their short ids, so cross-stack imports read `Import:<short>:`). A
    throwaway UI build satisfies the edge stack's hard index.html requirement."""
    ui = tmp_path / "ui"
    ui.mkdir(exist_ok=True)
    (ui / "index.html").write_text("<!doctype html><title>biffix</title>")
    return build_stacks(cdk.App(), config, ui_asset_path=ui)


def _single_stacks(stacks: Stacks) -> dict[str, cdk.Stack]:
    """The deploy's stacks by short name, but for the project stacks (and the app)."""
    single: dict[str, cdk.Stack] = {
        "network": stacks["network"],
        "data": stacks["data"],
        "identity": stacks["identity"],
        "compute": stacks["compute"],
        "build": stacks["build"],
        "persistence": stacks["persistence"],
        "ingest": stacks["ingest"],
        "api": stacks["api"],
        "edge": stacks["edge"],
    }
    # A stack `build_stacks` gains is listed here too, or the tests iterating these miss it.
    assert set(single) == set(stacks) - {"app", "projects"}, sorted(stacks)
    return single


def _logical_id(stack: cdk.Stack, construct: IConstruct | None) -> str:
    """The logical id `stack` gives `construct`'s CloudFormation resource (its default child).
    `construct` is optional only as CDK types some (`fn.role`); it must be there."""
    assert construct is not None
    child = construct.node.default_child
    assert isinstance(child, cdk.CfnElement), construct.node.path
    return stack.get_logical_id(child)


def _actions(statement: Json) -> set[str]:
    a = statement["Action"]
    return {a} if isinstance(a, str) else set(a)


def _grants(statement: Json, action: str) -> bool:
    """Whether `statement` names `action`, wildcards included (`s3:DeleteObject*`, `s3:*` and `*`
    all grant `s3:DeleteObject`): a negative check must see through them."""
    return any(fnmatch.fnmatchcase(action, a) for a in _actions(statement))


def _policies(template: Template) -> list[Mapping[str, Any]]:
    """Every IAM policy in `template`: inline ones, and the managed ones CDK moves a role's
    statements into once its inline policy would outgrow IAM's size limit (`OverflowPolicy`)."""
    return [
        *template.find_resources("AWS::IAM::Policy").values(),
        *template.find_resources("AWS::IAM::ManagedPolicy").values(),
    ]


def _statements(template: Template) -> list[Json]:
    """Every IAM policy statement in `template`, whichever principal holds it."""
    return [
        st for pol in _policies(template) for st in pol["Properties"]["PolicyDocument"]["Statement"]
    ]


def _actions_for_role(template: Template, stack: cdk.Stack, role: iam.IRole | None) -> set[str]:
    """Actions granted to a SPECIFIC role (see `_statements_for`): a stack-wide union would only
    prove *some* principal has an action, which passes even if the wrong role holds it."""
    return {a for st in _statements_for(template, stack, role) for a in _actions(st)}


def _render(value: object) -> str:
    """A synthesized IAM resource (or any CloudFormation value) as one comparable string: string
    literals as-is, `Fn::Join`s joined, the partition as `aws` and every other reference as
    `{Ref:<id>}` / `{<id>.<attr>}` / `{Import:<name>}`. Lets a test compare each resource
    exactly instead of substring-matching a whole statement's JSON."""
    if isinstance(value, str):
        return value
    if isinstance(value, dict) and len(value) == 1:
        (fn, arg), *_ = value.items()
        if fn == "Fn::Join":
            sep, parts = arg
            return str(sep).join(_render(p) for p in parts)
        if fn == "Ref":
            return "aws" if arg == "AWS::Partition" else f"{{Ref:{arg}}}"
        if fn == "Fn::GetAtt":
            return f"{{{'.'.join(arg)}}}"
        if fn == "Fn::ImportValue":
            return f"{{Import:{_render(arg)}}}"
    return json.dumps(value, sort_keys=True)


def _resources(statement: Json) -> list[str]:
    """Each of a statement's resources, rendered (see `_render`)."""
    r = statement["Resource"]
    return [_render(x) for x in (r if isinstance(r, list) else [r])]


def _statements_for(template: Template, stack: cdk.Stack, role: iam.IRole | None) -> list[Json]:
    """Every policy statement attached to `role` (by its logical id). `role` is optional only
    because CDK types a construct's role so (`fn.role`); it must be there."""
    assert role is not None
    logical_id = _logical_id(stack, role)
    return [
        st
        for pol in _policies(template)
        if {"Ref": logical_id} in pol["Properties"].get("Roles", [])
        for st in pol["Properties"]["PolicyDocument"]["Statement"]
    ]


def _lambda_env(api: ApiStack, fn: Construct) -> dict[str, Any]:
    """The environment variables `fn` (a Lambda of the `api` stack) synthesizes with."""
    functions = Template.from_stack(api).find_resources("AWS::Lambda::Function")
    lid = _logical_id(api, fn)
    env: dict[str, Any] = functions[lid]["Properties"]["Environment"]["Variables"]
    return env


def _context_parameter(api: ApiStack, pid: str) -> Json:
    """Project `pid`'s synthesized SSM context parameter (its CloudFormation properties)."""
    lid = _logical_id(api, api.project_contexts[pid])
    props: Json = Template.from_stack(api).find_resources("AWS::SSM::Parameter")[lid]["Properties"]
    return props


def _context_parts(api: ApiStack, pid: str) -> list[object]:
    """The parts of project `pid`'s context value: literal strings and CloudFormation tokens."""
    value = _context_parameter(api, pid)["Value"]
    if isinstance(value, str):
        return [value]
    sep, parts = value["Fn::Join"]
    assert sep == "", value
    return list(parts)


def _project_context(api: ApiStack, pid: str) -> Json:
    """Project `pid`'s context as the backend parses it, each deploy-time value rendered (see
    `_render`) in place."""
    context: Json = json.loads("".join(_render(p) for p in _context_parts(api, pid)))
    return context


# Every coordinator variant's job definition (the backend registers and submits only these).
_BATCH = "arn:aws:batch:{Ref:AWS::Region}:{Ref:AWS::AccountId}"


VARIANT_JOB_DEFINITIONS = f"{_BATCH}:job-definition/biffix-acme-coordinator-rt-*"


BATCH_JOBS = f"{_BATCH}:job/*"


# A project on an existing bucket with its own key: both render as literal ARNs, not as imports
# of the project's stack.
LEGACY = Project(
    id="legacy",
    bucket_name="legacy-bucket",
    key_arn="arn:aws:kms:us-east-1:111122223333:key/legacy-1",
)


def _two_project_stacks(tmp_path: Path) -> Stacks:
    """The graph with a second created project (`other`) and an imported one (`legacy`) next to
    the default one."""
    other = dataclasses.replace(CONFIG.projects[0], id="other", data_sources=(CORE,))
    projects = [*CONFIG.projects, other, LEGACY]
    return _build(tmp_path, config=dataclasses.replace(CONFIG, projects=projects))


CORE = DataSource(
    id="core",
    name="Sequencing core",
    bucket="seq-core",
    prefix="deliveries/",
    key_arn="arn:aws:kms:us-east-1:111122223333:key/core-1",
)


def _bucket_objects(resources: Iterable[str], project: ProjectStack) -> set[str]:
    """The key patterns `resources` (rendered) grant on `project`'s bucket; fails if any resource
    is not an object pattern of that bucket (another bucket, or the bucket itself)."""
    patterns: set[str] = set()
    for r in resources:
        bucket, sep, pattern = r.partition("}/")
        assert sep and f"Import:{project.stack_name}:" in bucket and "ProjectBucket" in bucket, r
        patterns.add(pattern)
    return patterns
