"""The status alarms, the alerts topic and the status probe."""

import dataclasses
from pathlib import Path

import aws_cdk as cdk
from aws_cdk.assertions import Template

from biffix_infra.app_graph import Stacks, build_stacks
from tests.synth.synth_helpers import (
    CONFIG,
    _actions,
    _actions_for_role,
    _build,
    _logical_id,
    _render,
    _statements_for,
)


def _alarm_names(stacks: Stacks) -> list[str]:
    return [
        alarm["Properties"]["AlarmName"]
        for stack in (stacks["ingest"], stacks["api"])
        for alarm in Template.from_stack(stack).find_resources("AWS::CloudWatch::Alarm").values()
    ]


def test_reconcile_watches_exactly_this_deploy_s_alarms(
    default_stacks: Stacks, tmp_path: Path
) -> None:
    """The status probe lists the alarms whose names start with `BIFFIX_ALARM_PREFIX`: every alarm
    of this deploy (ingest's and api's) is named under it, whatever the stack names, and a sibling
    tenant `<tenant>-<x>` in the same account names none of its alarms under it."""
    ingest = default_stacks["ingest"]
    t = Template.from_stack(ingest)
    fn_id = _logical_id(ingest, ingest.reconcile_lambda)
    env = t.find_resources("AWS::Lambda::Function")[fn_id]["Properties"]["Environment"]
    prefix = env["Variables"]["BIFFIX_ALARM_PREFIX"]
    assert prefix == CONFIG.alarm_prefix == f"biffix-{CONFIG.tenant}/"
    names = _alarm_names(default_stacks)
    assert len(names) == 8 and all(n.startswith(prefix) for n in names), names
    (describe,) = [
        st
        for st in _statements_for(t, ingest, ingest.reconcile_lambda.role)
        if "cloudwatch:DescribeAlarms" in _actions(st)
    ]
    assert _actions(describe) == {"cloudwatch:DescribeAlarms"}
    # app.py's stack names, for this tenant and a sibling that extends its name.
    for tenant in (CONFIG.tenant, f"{CONFIG.tenant}-x"):
        ui = tmp_path / tenant / "ui"
        ui.mkdir(parents=True)
        (ui / "index.html").write_text("<!doctype html>")

        def name(part: str, tenant: str = tenant) -> str:
            return f"biffix-{tenant}-{part}"

        stacks = build_stacks(
            cdk.App(), dataclasses.replace(CONFIG, tenant=tenant), name=name, ui_asset_path=ui
        )
        watched = [n for n in _alarm_names(stacks) if n.startswith(prefix)]
        assert len(watched) == (8 if tenant == CONFIG.tenant else 0), (tenant, watched)


def _alarm_ids(template: Template) -> set[str]:
    return set(template.find_resources("AWS::CloudWatch::Alarm"))


def test_alarms_notify_the_alerts_topic_on_alarm_and_recovery(default_stacks: Stacks) -> None:
    ingest = Template.from_stack(default_stacks["ingest"])
    api = Template.from_stack(default_stacks["api"])
    ingest.resource_count_is("AWS::SNS::Topic", 1)
    (topic_id,) = ingest.find_resources("AWS::SNS::Topic")
    names = {i.rstrip("0123456789ABCDEF") for i in _alarm_ids(ingest) | _alarm_ids(api)}
    for expected in (
        "IngestDlqNotEmptyAlarm",
        "IngestBacklogAgeAlarm",
        "IngestErrorsAlarm",
        "ReconcileErrorsAlarm",
        "ReconcileStoppedAlarm",
        "BackendErrorsAlarm",
        "SweepErrorsAlarm",
        "Api5xxAlarm",
    ):
        assert any(n.startswith(expected) for n in names), f"missing {expected}: {names}"
    # Each alarm notifies exactly the alerts topic: by Ref in its own stack, by import in api.
    for template, stack in ((ingest, "ingest"), (api, "api")):
        alarms = template.find_resources("AWS::CloudWatch::Alarm").values()
        for alarm in alarms:
            props = alarm["Properties"]
            for actions in (props["AlarmActions"], props["OKActions"]):
                (action,) = actions
                if stack == "ingest":
                    assert action == {"Ref": topic_id}, props
                else:
                    assert _render(action).startswith(
                        f"{{Import:ingest:ExportsOutputRef{topic_id}"
                    ), props


def test_reconciler_stopped_alarm_treats_silence_as_breaching(default_stacks: Stacks) -> None:
    ingest = Template.from_stack(default_stacks["ingest"])
    ingest.has_resource_properties(
        "AWS::CloudWatch::Alarm",
        {
            "MetricName": "Invocations",
            "ComparisonOperator": "LessThanThreshold",
            "TreatMissingData": "breaching",
        },
    )


def test_a_failing_sweep_alarms(default_stacks: Stacks) -> None:
    # The sweep Lambda fails an invocation when any of its sweeps did (`bin/sweep.rs`): three
    # failed ticks in a row alarm, as the reconciler's do.
    api = default_stacks["api"]
    t = Template.from_stack(api)
    sweep_id = _logical_id(api, api.sweep_lambda)
    (alarm,) = [
        a["Properties"]
        for i, a in t.find_resources("AWS::CloudWatch::Alarm").items()
        if i.startswith("SweepErrorsAlarm")
    ]
    assert alarm["MetricName"] == "Errors"
    assert alarm["Dimensions"] == [{"Name": "FunctionName", "Value": {"Ref": sweep_id}}]
    assert (alarm["Statistic"], alarm["Period"]) == ("Sum", 300)
    assert (alarm["Threshold"], alarm["EvaluationPeriods"]) == (1, 3)
    # A failed invocation is not retried by Lambda: the next tick is the retry.
    t.has_resource_properties(
        "AWS::Lambda::EventInvokeConfig",
        {"FunctionName": {"Ref": sweep_id}, "MaximumRetryAttempts": 0},
    )


def test_alert_email_is_subscribed_only_when_configured(tmp_path: Path) -> None:
    (tmp_path / "none").mkdir()
    (tmp_path / "some").mkdir()
    none = Template.from_stack(
        _build(tmp_path / "none", config=dataclasses.replace(CONFIG, alert_email=None))["ingest"]
    )
    none.resource_count_is("AWS::SNS::Subscription", 0)
    with_email = dataclasses.replace(CONFIG, alert_email="ops@example.org")
    some = Template.from_stack(_build(tmp_path / "some", config=with_email)["ingest"])
    some.has_resource_properties(
        "AWS::SNS::Subscription", {"Protocol": "email", "Endpoint": "ops@example.org"}
    )


def test_backend_manages_alert_subscribers(default_stacks: Stacks) -> None:
    api = default_stacks["api"]
    t = Template.from_stack(api)
    granted = _actions_for_role(t, api, api.backend_lambda.role)
    assert {"sns:ListSubscriptionsByTopic", "sns:Subscribe", "sns:Unsubscribe"} <= granted
    backend_id = _logical_id(api, api.backend_lambda)
    env = t.find_resources("AWS::Lambda::Function")[backend_id]["Properties"]["Environment"]
    (topic_id,) = Template.from_stack(default_stacks["ingest"]).find_resources("AWS::SNS::Topic")
    topic = _render(env["Variables"]["BIFFIX_ALERT_TOPIC_ARN"])
    assert topic.startswith(f"{{Import:ingest:ExportsOutputRef{topic_id}"), topic


def test_status_probe_gets_the_public_readyz_url_and_metric_read(tmp_path: Path) -> None:
    cfg = dataclasses.replace(CONFIG, app_domain="biffix.example.org")
    stacks = _build(tmp_path, config=cfg)
    ingest = stacks["ingest"]
    t = Template.from_stack(ingest)
    reconcile = next(
        f
        for f in t.find_resources("AWS::Lambda::Function").values()
        if "BIFFIX_INGEST_DLQ_URL" in f["Properties"]["Environment"]["Variables"]
    )
    env = reconcile["Properties"]["Environment"]["Variables"]
    assert env["BIFFIX_API_READYZ_URL"] == "https://biffix.example.org/api/readyz"
    assert "cloudwatch:GetMetricStatistics" in _actions_for_role(
        t, ingest, ingest.reconcile_lambda.role
    )
