"""The ingest write path: EventBridge → SQS (+ DLQ) → the Rust batched-writer Lambda (translate →
append → advance the projection)."""

from __future__ import annotations

from collections.abc import Sequence
from typing import Any

from aws_cdk import Duration, Stack
from aws_cdk import aws_ec2 as ec2
from aws_cdk import aws_events as events
from aws_cdk import aws_events_targets as targets
from aws_cdk import aws_lambda as lambda_
from aws_cdk import aws_lambda_event_sources as event_sources
from aws_cdk import aws_rds as rds
from aws_cdk import aws_secretsmanager as secretsmanager
from aws_cdk import aws_sqs as sqs
from constructs import Construct

from biffix_infra.constructs.batch_access import batch_arn
from biffix_infra.constructs.rust_function import RustFunction
from biffix_infra.wiring import wire_db

# How often SQS may hand a message to the writer before it is dead-lettered. The writer runs with
# reserved concurrency 1 behind SQS's several concurrent pollers, so during a burst most receives
# are THROTTLES (the message returns after the visibility timeout with its receive count raised),
# not failures; a small count would dead-letter healthy events. 20 receives x the 6-minute
# visibility is ~2 h before a genuinely failing event reaches the DLQ.
INGEST_MAX_RECEIVE_COUNT = 20


def build_state_change_pattern(build_project_names: Sequence[str]) -> dict[str, Any]:
    """The event pattern of CodeBuild state changes of this deploy's build projects, by their exact
    names: a name prefix would also match a sibling tenant's projects in the same account."""
    return {
        "source": ["aws.codebuild"],
        "detail_type": ["CodeBuild Build State Change"],
        "detail": {"project-name": list(build_project_names)},
    }


class IngestPipeline(Construct):
    """The ingest queue (`queue`) and its dead-letter queue (`dlq`), the writer Lambda (running
    `code`) draining it into Aurora (`function`), and the EventBridge rules feeding it: the
    job-state changes of the Batch jobs on `queue_names` (every queue of this deploy), spot
    interruption warnings, Batch hosts' ECS container-instance changes, and the state changes of
    `build_project_names`' builds."""

    def __init__(
        self,
        scope: Construct,
        cid: str,
        *,
        code: lambda_.Code,
        vpc: ec2.IVpc,
        db: rds.DatabaseCluster,
        db_secret: secretsmanager.ISecret,
        coordinator_queue_names: list[str],
        queue_names: list[str],
        build_project_names: list[str],
    ) -> None:
        super().__init__(scope, cid)
        stack = Stack.of(self)

        # SQS + DLQ absorbs fan-out bursts and decouples ingest from Aurora.
        self.dlq = sqs.Queue(self, "Dlq", retention_period=Duration.days(14))
        timeout = Duration.seconds(60)
        self.queue = sqs.Queue(
            self,
            "Queue",
            # Visibility must comfortably exceed the Lambda timeout (AWS recommends ~6x) or a msg
            # can reappear while it is still being processed, causing double-delivery.
            visibility_timeout=Duration.seconds(timeout.to_seconds() * 6),
            dead_letter_queue=sqs.DeadLetterQueue(
                max_receive_count=INGEST_MAX_RECEIVE_COUNT, queue=self.dlq
            ),
        )

        self.function = RustFunction(
            self,
            "Writer",
            code=code,
            vpc=vpc,
            timeout=timeout,
            # Single-writer discipline: the reducer reads a cursor and upserts
            # rows without cross-writer locking, so concurrent invocations would race. Pin
            # the writer to one concurrent execution.
            reserved_concurrent_executions=1,
            environment={"BIFFIX_COORDINATOR_QUEUES": ",".join(coordinator_queue_names)},
        )
        wire_db(self.function, db, db_secret)
        # Partial batch responses: the writer reports only the records whose own append failed, so
        # SQS redelivers (and eventually dead-letters) just those, not the healthy rest of the
        # batch. The writer's response is only honoured with this flag set.
        self.function.add_event_source(
            event_sources.SqsEventSource(self.queue, batch_size=10, report_batch_item_failures=True)
        )

        # EventBridge rules feed the ingest SQS queue; the Lambda consumes from SQS. Batch jobs only
        # on this deploy's queues, matched by their exact ARNs (an event's `jobQueue`): the log is
        # immutable, and a name prefix would also take a sibling tenant `<tenant>-<x>`'s jobs.
        self._rule(
            "BatchStateChangeRule",
            source=["aws.batch"],
            detail_type=["Batch Job State Change"],
            detail={"jobQueue": [batch_arn(stack, "job-queue", name) for name in queue_names]},
        )
        self._rule(
            "SpotInterruptionRule",
            source=["aws.ec2"],
            detail_type=["EC2 Spot Instance Interruption Warning"],
        )
        # ECS container-instance state changes carry the host facts (EC2 id, type, AZ, size) that
        # Batch job events lack; the Lambda records them in host_registry. Scoped to Batch-managed
        # clusters in this account (Batch names them `AWSBatch-…`; the CE's cluster ARN is
        # service-created, so a prefix is the tightest synth-time match).
        self._rule(
            "EcsContainerInstanceRule",
            source=["aws.ecs"],
            detail_type=["ECS Container Instance State Change"],
            detail={
                "clusterArn": [
                    {
                        "prefix": stack.format_arn(
                            service="ecs", resource="cluster", resource_name="AWSBatch-"
                        )
                    }
                ]
            },
        )
        # CodeBuild state changes of this deploy's build projects (images, engine runtimes): the
        # Lambda folds them into build_projection by the subject each build carries. Matched by the
        # projects' exact names: a name prefix would also match a sibling tenant's projects in the
        # same account, whose builds would then be adopted as this tenant's.
        self._rule("CodeBuildStateChangeRule", **build_state_change_pattern(build_project_names))

    def _rule(self, cid: str, **pattern: Any) -> events.Rule:
        """A rule sending the events matching `pattern` to the ingest queue."""
        rule = events.Rule(self, cid, event_pattern=events.EventPattern(**pattern))
        rule.add_target(targets.SqsQueue(self.queue))
        return rule
