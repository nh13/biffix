"""IngestStack — the write side.

EventBridge (Batch job-state on this deploy's queues, spot interruption, Batch hosts' ECS
container-instance and this deploy's CodeBuild build-state changes) → SQS (+ DLQ) → the Rust
batched-writer Lambda
(`IngestPipeline`), plus the scheduled reconcile Lambda backstop (`Reconcile`) and the alarms on
both. Grouped because they share a redeploy cadence and all write to Aurora; kept out of the
api/edge stacks, so frequently-redeployed frontend never touches the write path.
"""

from __future__ import annotations

from typing import Any

from aws_cdk import Duration, Stack
from aws_cdk import aws_cloudwatch as cloudwatch
from aws_cdk import aws_ec2 as ec2
from aws_cdk import aws_rds as rds
from aws_cdk import aws_secretsmanager as secretsmanager
from aws_cdk import aws_sns as sns
from aws_cdk import aws_sns_subscriptions as subscriptions
from constructs import Construct

from biffix_infra.config.model import QueuePricing
from biffix_infra.constructs.alarms import Alarms
from biffix_infra.constructs.ingest_pipeline import IngestPipeline
from biffix_infra.constructs.reconcile import Reconcile
from biffix_infra.lambda_artifacts import rust_lambda_code
from biffix_infra.stacks.project_stack import ProjectStack


class IngestStack(Stack):
    def __init__(
        self,
        scope: Construct,
        cid: str,
        *,
        vpc: ec2.IVpc,
        db: rds.DatabaseCluster,
        db_secret: secretsmanager.ISecret,
        coordinator_queue_names: list[str],
        build_project_names: list[str],
        worker_queue_names: list[str],
        api_readyz_url: str | None,
        alert_email: str | None,
        queue_pricing: dict[str, QueuePricing],
        retention_projects: list[ProjectStack],
        intermediates_retention_days: int | None,
        snakemake_cache_days: int | None,
        alarm_prefix: str,
        **kwargs: Any,
    ) -> None:
        super().__init__(scope, cid, **kwargs)

        # Operational alarms (this stack's and the api stack's) publish here. The backend manages
        # extra email subscribers from the Settings page; `alert_email` is the config-managed one.
        self.alerts_topic = sns.Topic(self, "AlertsTopic", display_name="biffix alerts")
        if alert_email:
            self.alerts_topic.add_subscription(subscriptions.EmailSubscription(alert_email))

        self.ingest = IngestPipeline(
            self,
            "Ingest",
            code=rust_lambda_code("biffix-ingest-lambda"),
            vpc=vpc,
            db=db,
            db_secret=db_secret,
            coordinator_queue_names=coordinator_queue_names,
            queue_names=[*coordinator_queue_names, *worker_queue_names],
            build_project_names=build_project_names,
        )
        self.ingest_queue = self.ingest.queue
        self.ingest_dlq = self.ingest.dlq
        self.ingest_lambda = self.ingest.function

        self.reconcile_lambda = Reconcile(
            self,
            "ReconcileWriter",
            code=rust_lambda_code("biffix-reconcile-lambda"),
            vpc=vpc,
            db=db,
            db_secret=db_secret,
            ingest=self.ingest,
            coordinator_queue_names=coordinator_queue_names,
            worker_queue_names=worker_queue_names,
            api_readyz_url=api_readyz_url,
            queue_pricing=queue_pricing,
            projects=retention_projects,
            intermediates_retention_days=intermediates_retention_days,
            snakemake_cache_days=snakemake_cache_days,
            alarm_prefix=alarm_prefix,
        ).function
        self._add_alarms(alarm_prefix)

    def _add_alarms(self, prefix: str) -> None:
        """Alarms on the write path, each notifying the alerts topic on ALARM and on recovery, and
        each named under `prefix` (the one the probe lists them by)."""
        five_min = Duration.minutes(5)
        alarms = Alarms(self, topic=self.alerts_topic, prefix=prefix)
        alarms.add(
            "IngestDlqNotEmptyAlarm",
            description="Events are in the ingest dead-letter queue (redrive needed).",
            metric=self.ingest_dlq.metric_approximate_number_of_messages_visible(
                period=five_min, statistic="Maximum"
            ),
            threshold=0,
            comparison=cloudwatch.ComparisonOperator.GREATER_THAN_THRESHOLD,
        )
        alarms.add(
            "IngestBacklogAgeAlarm",
            description="The ingest queue's oldest event has waited over 15 minutes: "
            "run events are not being ingested.",
            metric=self.ingest_queue.metric_approximate_age_of_oldest_message(
                period=five_min, statistic="Maximum"
            ),
            threshold=900,
            comparison=cloudwatch.ComparisonOperator.GREATER_THAN_THRESHOLD,
        )
        alarms.add(
            "IngestErrorsAlarm",
            description="The ingest Lambda is failing repeatedly.",
            metric=self.ingest_lambda.metric_errors(period=Duration.minutes(15), statistic="Sum"),
            threshold=5,
        )
        alarms.add(
            "ReconcileErrorsAlarm",
            description="The reconcile Lambda has failed on three consecutive ticks.",
            metric=self.reconcile_lambda.metric_errors(period=five_min, statistic="Sum"),
            threshold=1,
            evaluation_periods=3,
        )
        alarms.add(
            "ReconcileStoppedAlarm",
            description="The reconcile Lambda has not run for 15 minutes: no status "
            "samples, no missed-terminal backstop, no reaping.",
            metric=self.reconcile_lambda.metric_invocations(
                period=Duration.minutes(15), statistic="Sum"
            ),
            threshold=1,
            comparison=cloudwatch.ComparisonOperator.LESS_THAN_THRESHOLD,
            # No datapoints means no invocations — that IS the failure.
            treat_missing_data=cloudwatch.TreatMissingData.BREACHING,
        )
