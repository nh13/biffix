"""The backend sweep: the backend's scheduled work (provenance crates, builds, parked launches).

The same code and state as the API Lambda, but only what the sweep uses: no OIDC (no auth context).
It launches the coordinator of a run parked in `preparing_images` once its submit-time images are
ready, builds the variants registered versions need (a base-image bump re-derives them) and writes
each ended run's RO-Crate; the stack grants it what those need.
"""

from __future__ import annotations

from aws_cdk import Duration
from aws_cdk import aws_ec2 as ec2
from aws_cdk import aws_events as events
from aws_cdk import aws_events_targets as targets
from aws_cdk import aws_lambda as lambda_
from constructs import Construct

from biffix_infra.constructs.ingest_pipeline import build_state_change_pattern
from biffix_infra.constructs.reconcile import TICK_SCHEDULE
from biffix_infra.constructs.rust_function import RustFunction


class BackendSweep(Construct):
    """The sweep Lambda (`function`, running `code`; this construct's default child), invoked
    every 5 minutes (hourly while the tenant is idle, `TICK_SCHEDULE`) and whenever one of
    `build_project_names`' builds changes state (`build_rule`)."""

    def __init__(
        self,
        scope: Construct,
        cid: str,
        *,
        code: lambda_.Code,
        vpc: ec2.IVpc,
        build_project_names: list[str],
    ) -> None:
        super().__init__(scope, cid)
        self.function = RustFunction(
            self,
            "Default",
            code=code,
            vpc=vpc,
            timeout=Duration.minutes(5),
            memory_size=512,
            logs_id="Logs",
            environment={"RUST_LOG": "info"},
            # An invocation fails when any sweep did (so its errors can alarm); the next tick is
            # the retry, not Lambda's async retries of the same event.
            retry_attempts=0,
        )
        # Every 5 minutes, like reconcile: a run's crate lands within ~15 min of its last event
        # (10 min settle + one tick). Also the fallback for the build-event trigger below. While
        # no coordinator job is active or recently stopped, only the hourly tick runs its sweeps;
        # the idle check's batch:ListJobs is the one `launch` grants it (ApiStack).
        events.Rule(
            self,
            "Schedule",
            schedule=TICK_SCHEDULE,
        ).add_target(targets.LambdaFunction(self.function))
        # A finished image import/derive or runtime build advances what waits on it (a parked run's
        # launch) promptly instead of on the next tick: CodeBuild state changes of this deploy's
        # build projects (the same filter as IngestStack's rule) invoke the sweep with the raw
        # EventBridge event, which it folds before sweeping (the SQS copy then dedups to a no-op).
        self.build_rule = events.Rule(
            self,
            "CodeBuildRule",
            event_pattern=events.EventPattern(**build_state_change_pattern(build_project_names)),
        )
        self.build_rule.add_target(targets.LambdaFunction(self.function))
