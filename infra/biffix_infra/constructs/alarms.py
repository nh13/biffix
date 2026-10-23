"""Operational alarms: each named under the deploy's alarm prefix and notifying the alerts topic."""

from __future__ import annotations

from aws_cdk import aws_cloudwatch as cloudwatch
from aws_cdk import aws_cloudwatch_actions as cw_actions
from aws_cdk import aws_sns as sns
from constructs import Construct

_AT_LEAST = cloudwatch.ComparisonOperator.GREATER_THAN_OR_EQUAL_TO_THRESHOLD


def alarm_name(prefix: str | None, alarm_id: str) -> str | None:
    """An alarm's name under the deploy's alarm prefix (`DeployConfig.alarm_prefix`), which the
    status probe lists them by; `None` (a CloudFormation-generated name) without a prefix."""
    return f"{prefix}{alarm_id}" if prefix else None


class Alarms:
    """A stack's alarms. Each is created in `scope` under its own id (so an alarm's logical id is
    its id, whatever groups it), named `<prefix><id>`, and notifies `topic` when it fires and when
    it recovers. Not a construct: a named alarm whose logical id changed would be created anew
    under the same name and then deleted with the old one."""

    def __init__(self, scope: Construct, *, topic: sns.ITopic, prefix: str | None) -> None:
        self._scope = scope
        self._action = cw_actions.SnsAction(topic)
        self._prefix = prefix

    def add(
        self,
        alarm_id: str,
        *,
        description: str,
        metric: cloudwatch.IMetric,
        threshold: float,
        comparison: cloudwatch.ComparisonOperator = _AT_LEAST,
        evaluation_periods: int = 1,
        treat_missing_data: cloudwatch.TreatMissingData = cloudwatch.TreatMissingData.NOT_BREACHING,
    ) -> cloudwatch.Alarm:
        alarm = cloudwatch.Alarm(
            self._scope,
            alarm_id,
            alarm_name=alarm_name(self._prefix, alarm_id),
            alarm_description=description,
            metric=metric,
            threshold=threshold,
            comparison_operator=comparison,
            evaluation_periods=evaluation_periods,
            treat_missing_data=treat_missing_data,
        )
        alarm.add_alarm_action(self._action)
        alarm.add_ok_action(self._action)
        return alarm
