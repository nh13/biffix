"""The ingest stack: the event queue and writer, and the scheduled reconcile tick."""

import dataclasses
import json
from pathlib import Path

import pytest
from aws_cdk.assertions import Match, Template

from biffix_infra.app_graph import Stacks
from biffix_infra.constructs.ingest_pipeline import INGEST_MAX_RECEIVE_COUNT
from tests.synth.synth_helpers import (
    BATCH_JOBS,
    CONFIG,
    Json,
    _actions,
    _actions_for_role,
    _build,
    _grants,
    _logical_id,
    _render,
    _resources,
    _single_stacks,
    _statements,
    _statements_for,
)


def test_ingest_has_queue_dlq_and_writer_behind_sqs(default_stacks: Stacks) -> None:
    stacks = default_stacks
    t = Template.from_stack(stacks["ingest"])
    t.resource_count_is("AWS::SQS::Queue", 2)  # ingest + DLQ
    # ingest writer + reconcile writer.
    t.resource_count_is("AWS::Lambda::Function", 2)
    t.resource_count_is("AWS::Lambda::EventSourceMapping", 1)  # only the ingest writer drains SQS
    # The writer returns partial batch failures; without this flag SQS would ignore them and
    # delete the failed records along with the healthy ones.
    t.has_resource_properties(
        "AWS::Lambda::EventSourceMapping",
        {"FunctionResponseTypes": ["ReportBatchItemFailures"]},
    )


def test_ingest_schedules_reconciliation_through_ingest_queue(default_stacks: Stacks) -> None:
    stacks = default_stacks
    t = Template.from_stack(stacks["ingest"])
    t.has_resource_properties(
        "AWS::Events::Rule",
        # On the minutes that are multiples of 5: the idle gate runs the minute-0 tick in full.
        Match.object_like({"ScheduleExpression": "cron(0/5 * * * ? *)", "State": "ENABLED"}),
    )
    t.has_resource_properties(
        "AWS::Lambda::Function",
        Match.object_like(
            {
                "Environment": Match.object_like(
                    {
                        "Variables": Match.object_like(
                            {
                                "BIFFIX_INGEST_QUEUE_URL": Match.any_value(),
                                "BIFFIX_INGEST_DLQ_URL": Match.any_value(),
                            }
                        )
                    }
                )
            }
        ),
    )
    # The reconcile Lambda's own role (not merely some principal in the stack).
    ingest = stacks["ingest"]
    granted = _actions_for_role(t, ingest, ingest.reconcile_lambda.role)
    assert "batch:DescribeJobs" in granted
    assert "batch:ListJobs" in granted  # the idle check
    assert "sqs:SendMessage" in granted
    assert "ec2:DescribeInstances" in granted  # host spot/on-demand lifecycle


def test_reconcile_deletes_only_runs_work_dirs_after_the_retention_period(
    default_stacks: Stacks,
) -> None:
    t = Template.from_stack(default_stacks["ingest"])
    t.has_resource_properties(
        "AWS::Lambda::Function",
        Match.object_like(
            {
                "Environment": Match.object_like(
                    {"Variables": Match.object_like({"BIFFIX_RETENTION_DAYS": "30"})}
                )
            }
        ),
    )
    # The work-dir deletes (the Snakemake cache's are checked separately).
    deletes = [
        st
        for st in _statements(t)
        if _grants(st, "s3:DeleteObject") and "biffix/runs/" in json.dumps(st["Resource"])
    ]
    assert len(deletes) == 1
    assert _actions(deletes[0]) == {"s3:DeleteObject"}
    # Every resource is a project bucket's run work dir, checked one by one (a leaked whole-bucket
    # resource in the same statement must fail).
    resources = _resources(deletes[0])
    assert resources and all(r.endswith("}/biffix/runs/*/work/*") for r in resources), resources
    lists = [st for st in _statements(t) if "s3:ListBucket" in _actions(st)]
    assert lists and all(
        st["Condition"]["StringLike"]["s3:prefix"]
        in (["biffix/runs/*"], ["biffix/cache/snakemake/*"])
        for st in lists
    )


def test_ingest_routes_batch_ecs_host_events_to_ingest_queue(default_stacks: Stacks) -> None:
    t = Template.from_stack(default_stacks["ingest"])
    rules = t.find_resources(
        "AWS::Events::Rule",
        {"Properties": {"EventPattern": {"source": ["aws.ecs"]}}},
    )
    assert len(rules) == 1
    (rule,) = rules.values()
    pattern = rule["Properties"]["EventPattern"]
    assert pattern["detail-type"] == ["ECS Container Instance State Change"]
    # Only Batch-managed clusters: the prefix ends in `cluster/AWSBatch-`.
    (cluster_match,) = pattern["detail"]["clusterArn"]
    prefix = json.dumps(cluster_match["prefix"])
    assert "cluster/AWSBatch-" in prefix
    (target,) = rule["Properties"]["Targets"]
    queue_logical_id = target["Arn"]["Fn::GetAtt"][0]
    assert queue_logical_id.startswith("IngestQueue")


def test_ingest_routes_only_this_deploy_s_batch_jobs_to_ingest_queue(
    default_stacks: Stacks,
) -> None:
    # The immutable event log takes only jobs on this deploy's own queues, by their exact ARNs:
    # not every Batch job in the account (a sibling tenant's, or another workload's).
    rules = Template.from_stack(default_stacks["ingest"]).find_resources(
        "AWS::Events::Rule",
        {"Properties": {"EventPattern": {"source": ["aws.batch"]}}},
    )
    (rule,) = rules.values()
    queues = rule["Properties"]["EventPattern"]["detail"]["jobQueue"]
    arn = "arn:aws:batch:{Ref:AWS::Region}:{Ref:AWS::AccountId}:job-queue/"
    assert sorted(_render(q) for q in queues) == sorted(
        arn + CONFIG.queue_name(qc) for qc in CONFIG.queue_classes
    )


def test_ingest_routes_this_deploy_s_codebuild_builds_to_ingest_queue(
    default_stacks: Stacks,
) -> None:
    t = Template.from_stack(default_stacks["ingest"])
    t.has_resource_properties(
        "AWS::Events::Rule",
        Match.object_like(
            {
                "EventPattern": {
                    "source": ["aws.codebuild"],
                    "detail-type": ["CodeBuild Build State Change"],
                    "detail": {"project-name": CONFIG.build_project_names},
                },
                "Targets": Match.array_with([Match.object_like({"Arn": Match.any_value()})]),
            }
        ),
    )
    # Exactly this deploy's projects: a prefix would also match a sibling tenant `<tenant>-<x>`.
    projects = Template.from_stack(default_stacks["build"]).find_resources(
        "AWS::CodeBuild::Project"
    )
    assert sorted(CONFIG.build_project_names) == sorted(
        p["Properties"]["Name"] for p in projects.values()
    )
    assert all(n.startswith(f"biffix-{CONFIG.tenant}-") for n in CONFIG.build_project_names)


def test_ingest_visibility_timeout_exceeds_lambda_timeout(default_stacks: Stacks) -> None:
    # SQS visibility ≈ 6× the Lambda timeout, so a message can't reappear mid-process.
    stacks = default_stacks
    t = Template.from_stack(stacks["ingest"])
    t.has_resource_properties("AWS::SQS::Queue", Match.object_like({"VisibilityTimeout": 360}))
    # Single-writer discipline: the ingest writer (the function draining the queue) and the
    # reconcile writer are each pinned to one concurrent execution.
    functions = t.find_resources("AWS::Lambda::Function")
    (mapping,) = t.find_resources("AWS::Lambda::EventSourceMapping").values()
    writer_id = mapping["Properties"]["FunctionName"]["Ref"]
    reconcile_id = _logical_id(stacks["ingest"], stacks["ingest"].reconcile_lambda)
    assert writer_id != reconcile_id
    for fn_id in (writer_id, reconcile_id):
        assert functions[fn_id]["Properties"]["ReservedConcurrentExecutions"] == 1, fn_id


def test_ingest_tolerates_throttled_receives_before_dead_lettering(default_stacks: Stacks) -> None:
    t = Template.from_stack(default_stacks["ingest"])
    queues = t.find_resources("AWS::SQS::Queue")
    (redrive,) = [
        q["Properties"]["RedrivePolicy"]
        for q in queues.values()
        if "RedrivePolicy" in q["Properties"]
    ]
    assert redrive["maxReceiveCount"] == INGEST_MAX_RECEIVE_COUNT >= 20


@pytest.mark.parametrize("stack", ["ingest", "api", "edge"])
def test_every_lambda_logs_to_a_group_that_expires(default_stacks: Stacks, stack: str) -> None:
    t = Template.from_stack(_single_stacks(default_stacks)[stack])
    groups = t.find_resources("AWS::Logs::LogGroup")
    assert groups
    for group in groups.values():
        assert group["Properties"]["RetentionInDays"] == 365
    functions = {
        lid: f["Properties"]
        for lid, f in t.find_resources("AWS::Lambda::Function").items()
        # CDK's own singleton providers (bucket auto-delete) take no log group.
        if not lid.startswith("CustomS3AutoDeleteObjects")
    }
    assert functions
    # Each logs to one of those expiring groups (not merely to some group).
    for lid, fn in functions.items():
        assert fn["LoggingConfig"]["LogGroup"] in [{"Ref": gid} for gid in groups], lid


def test_reconcile_gets_queue_pricing_and_spot_price_history(default_stacks: Stacks) -> None:
    ingest = default_stacks["ingest"]
    t = Template.from_stack(ingest)
    reconcile = next(
        f
        for f in t.find_resources("AWS::Lambda::Function").values()
        if "BIFFIX_INGEST_DLQ_URL" in f["Properties"]["Environment"]["Variables"]
    )
    pricing = json.loads(
        reconcile["Properties"]["Environment"]["Variables"]["BIFFIX_QUEUE_PRICING"]
    )
    assert set(pricing) == {CONFIG.queue_name(qc) for qc in CONFIG.queue_classes}
    for queue in pricing.values():
        assert queue["capacity"] in {"spot", "on_demand"}
        assert queue["volume"]["gb"] > 0
    actions = _actions_for_role(t, ingest, ingest.reconcile_lambda.role)
    assert "ec2:DescribeSpotPriceHistory" in actions
    # Another region's list prices come from the Pricing API.
    assert "pricing:GetProducts" in actions


def test_reconcile_may_terminate_batch_jobs_to_reap_orphans(default_stacks: Stacks) -> None:
    """The reaper terminates a dead coordinator's live children: without TerminateJob every reap
    fails and orphaned jobs bill to completion. The grant is on Batch job ARNs only."""
    ingest = default_stacks["ingest"]
    t = Template.from_stack(ingest)
    statements = [
        st
        for st in _statements_for(t, ingest, ingest.reconcile_lambda.role)
        if "batch:TerminateJob" in _actions(st)
    ]
    assert len(statements) == 1
    assert _resources(statements[0]) == [BATCH_JOBS], statements
    assert _actions(statements[0]) == {"batch:TerminateJob"}


def test_reconcile_expires_only_the_snakemake_cache(default_stacks: Stacks, tmp_path: Path) -> None:
    """The reconcile tick expires Snakemake cache entries after `snakemake_cache_days` (default 90)
    in every project bucket, and may list and delete only under `biffix/cache/snakemake/`; `null`
    keeps entries forever (no sweep, no grant)."""

    def reconcile(stacks: Stacks) -> tuple[Json, list[Json]]:
        ingest = stacks["ingest"]
        t = Template.from_stack(ingest)
        fn_id = _logical_id(ingest, ingest.reconcile_lambda)
        env = t.find_resources("AWS::Lambda::Function")[fn_id]["Properties"]["Environment"][
            "Variables"
        ]
        return env, _statements_for(t, ingest, ingest.reconcile_lambda.role)

    env, statements = reconcile(default_stacks)
    assert env["BIFFIX_SNAKEMAKE_CACHE_DAYS"] == "90"
    assert "BIFFIX_CACHE_BUCKETS" in env
    # Every resource the reconcile role may delete, one by one, is either a run's work dir or the
    # cache (a whole-bucket resource next to them in one statement must fail), and it has both.
    deleted = [r for st in statements if _grants(st, "s3:DeleteObject") for r in _resources(st)]
    assert all(
        r.endswith("}/biffix/runs/*/work/*") or r.endswith("}/biffix/cache/snakemake/*")
        for r in deleted
    ), deleted
    assert any(r.endswith("}/biffix/cache/snakemake/*") for r in deleted), deleted
    # No project bucket carries a cache lifecycle rule: the sweep is the one mechanism.
    buckets = [
        b
        for project in default_stacks["projects"]
        for b in Template.from_stack(project).find_resources("AWS::S3::Bucket").values()
    ]
    assert len(buckets) == len(default_stacks["projects"]), buckets
    for b in buckets:
        rules = b["Properties"]["LifecycleConfiguration"]["Rules"]
        assert all(r.get("Prefix") != "biffix/cache/snakemake/" for r in rules), rules

    forever = _build(tmp_path, config=dataclasses.replace(CONFIG, snakemake_cache_days=None))
    env, statements = reconcile(forever)
    assert "BIFFIX_SNAKEMAKE_CACHE_DAYS" not in env
    assert not any("biffix/cache/snakemake/" in json.dumps(st) for st in statements)
