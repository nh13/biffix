"""The scheduled reconcile Lambda: the AWS Batch reconciliation backstop, the
status probe, the pricing tick, the reaper and the retention and cache sweeps."""

from __future__ import annotations

import json
from typing import TYPE_CHECKING

from aws_cdk import Duration, Fn, Stack
from aws_cdk import aws_ec2 as ec2
from aws_cdk import aws_events as events
from aws_cdk import aws_events_targets as targets
from aws_cdk import aws_iam as iam
from aws_cdk import aws_lambda as lambda_
from aws_cdk import aws_rds as rds
from aws_cdk import aws_secretsmanager as secretsmanager
from constructs import Construct

from biffix_infra.config.model import QueuePricing
from biffix_infra.constructs.batch_access import batch_arn
from biffix_infra.constructs.ingest_pipeline import IngestPipeline
from biffix_infra.constructs.rust_function import RustFunction
from biffix_infra.wiring import wire_db

if TYPE_CHECKING:
    from biffix_infra.stacks.project_stack import ProjectStack

# The scheduled ticks' cadence (reconcile and the backend sweep): every 5 minutes, on the minutes
# that are multiples of 5, so exactly one tick an hour (minute 0) has a minute below 5. That tick
# runs in full even on an idle tenant; the others skip their database work when no coordinator
# job is active or recently stopped (`biffix_db::idle`), so an idle Aurora cluster can pause. A
# cron rather than a rate: a rate's minutes depend on when the rule was created.
TICK_SCHEDULE = events.Schedule.cron(minute="0/5")


class Reconcile(Construct):
    """The reconcile Lambda (`function`, running `code`; this construct's default child), run
    every 5 minutes (hourly while the tenant is idle, `TICK_SCHEDULE`): it polls stale attempts
    and enqueues missing terminal events through `ingest`'s writer, samples the deploy's status,
    prices runs (`queue_pricing`), terminates ended runs' child jobs and deletes expired files
    from `projects`' buckets (`intermediates_retention_days`, `snakemake_cache_days`)."""

    def __init__(
        self,
        scope: Construct,
        cid: str,
        *,
        code: lambda_.Code,
        vpc: ec2.IVpc,
        db: rds.DatabaseCluster,
        db_secret: secretsmanager.ISecret,
        ingest: IngestPipeline,
        coordinator_queue_names: list[str],
        worker_queue_names: list[str],
        api_readyz_url: str | None,
        queue_pricing: dict[str, QueuePricing] | None,
        projects: list[ProjectStack],
        intermediates_retention_days: int | None,
        snakemake_cache_days: int | None,
        alarm_prefix: str | None,
    ) -> None:
        super().__init__(scope, cid)
        stack = Stack.of(self)
        self.function = fn = RustFunction(
            self,
            "Default",
            code=code,
            vpc=vpc,
            timeout=Duration.seconds(60),
            logs_id="Logs",
            reserved_concurrent_executions=1,
            environment={
                "BIFFIX_COORDINATOR_QUEUES": ",".join(coordinator_queue_names),
                "BIFFIX_WORKER_QUEUES": ",".join(worker_queue_names),
                "BIFFIX_INGEST_QUEUE_URL": ingest.queue.queue_url,
                "BIFFIX_INGEST_DLQ_URL": ingest.dlq.queue_url,
            },
        )
        # The status probe calls the API's public readiness URL (egress via the NAT).
        if api_readyz_url:
            fn.add_environment("BIFFIX_API_READYZ_URL", api_readyz_url)
        # The ingest backlog age is only a CloudWatch metric (SQS does not return it from
        # GetQueueAttributes); GetMetricStatistics supports no resource-level scoping.
        fn.add_to_role_policy(
            iam.PolicyStatement(actions=["cloudwatch:GetMetricStatistics"], resources=["*"])
        )
        # Status probe: the DLQ depth is one input to the event_ingest health signal.
        ingest.dlq.grant(fn, "sqs:GetQueueAttributes")
        wire_db(fn, db, db_secret)
        ingest.queue.grant_send_messages(fn)
        # DescribeJobs does not support resource-level IAM permissions. DescribeJobQueues /
        # DescribeComputeEnvironments back the status probe's batch_compute health signal; neither
        # supports resource-level scoping either. ListJobs is the idle check (is a coordinator job
        # active or recently stopped?) and has no resource-level scoping either: AWS lists no
        # resource type or condition key for it.
        fn.add_to_role_policy(
            iam.PolicyStatement(
                actions=[
                    "batch:ListJobs",
                    "batch:DescribeJobs",
                    "batch:DescribeJobQueues",
                    "batch:DescribeComputeEnvironments",
                ],
                resources=["*"],
            )
        )
        # The reaper terminates the still-live child jobs of a run whose coordinator is terminal.
        # TerminateJob authorizes on the JOB resource only, and child job ids are
        # minted by the engine at run time, so this is the account's Batch job ARN space; the
        # reaper only passes ids the event log binds to an ended run.
        fn.add_to_role_policy(
            iam.PolicyStatement(
                actions=["batch:TerminateJob"],
                resources=[batch_arn(stack, "job", "*")],
            )
        )
        # The tick resolves each ECS host's spot vs on-demand lifecycle (host_registry.lifecycle);
        # DescribeInstances supports no resource-level scoping.
        fn.add_to_role_policy(
            iam.PolicyStatement(actions=["ec2:DescribeInstances"], resources=["*"])
        )
        # The cost estimator prices spot jobs from the spot price history for their type and AZ;
        # DescribeSpotPriceHistory supports no resource-level scoping.
        fn.add_to_role_policy(
            iam.PolicyStatement(actions=["ec2:DescribeSpotPriceHistory"], resources=["*"])
        )
        # Outside us-east-1 (whose list prices are checked in) it reads the region's on-demand and
        # gp3 list prices from the AWS Pricing API, which supports no resource-level scoping.
        fn.add_to_role_policy(iam.PolicyStatement(actions=["pricing:GetProducts"], resources=["*"]))
        if queue_pricing:
            fn.add_environment("BIFFIX_QUEUE_PRICING", json.dumps(queue_pricing, sort_keys=True))
        self._sweeps(projects, intermediates_retention_days, snakemake_cache_days)
        # The status page's `alarms` component: the probe lists this deploy's alarms in ALARM (by
        # name prefix). DescribeAlarms filtered by prefix is not authorized against alarm ARNs (a
        # scoped grant is refused), so it takes `*`; it reads alarm metadata only.
        if alarm_prefix:
            fn.add_environment("BIFFIX_ALARM_PREFIX", alarm_prefix)
            fn.add_to_role_policy(
                iam.PolicyStatement(actions=["cloudwatch:DescribeAlarms"], resources=["*"])
            )
        self.schedule = events.Rule(
            self,
            "Schedule",
            schedule=TICK_SCHEDULE,
        )
        self.schedule.add_target(targets.LambdaFunction(fn))

    def _sweeps(
        self,
        projects: list[ProjectStack],
        intermediates_retention_days: int | None,
        snakemake_cache_days: int | None,
    ) -> None:
        """The retention sweep deletes Nextflow runs' own `work/` dirs; unset
        days ⇒ kept forever unless a run asked for deletion on success. The cache expiry deletes
        Snakemake cache entries older than its period from every project bucket, created or
        imported (a bucket biffix does not own can't take a lifecycle rule)."""
        fn = self.function
        if intermediates_retention_days is not None:
            fn.add_environment("BIFFIX_RETENTION_DAYS", str(intermediates_retention_days))
        for project in projects:
            project.grant_retention_delete(fn)
        if snakemake_cache_days is not None and projects:
            fn.add_environment("BIFFIX_SNAKEMAKE_CACHE_DAYS", str(snakemake_cache_days))
            fn.add_environment(
                "BIFFIX_CACHE_BUCKETS", Fn.join(",", [p.bucket.bucket_name for p in projects])
            )
            for project in projects:
                project.grant_cache_expiry(fn)
