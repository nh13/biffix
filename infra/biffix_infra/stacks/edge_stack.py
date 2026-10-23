"""EdgeStack — the public edge.

CloudFront (UI from a private S3 bucket as default, `/api/*` → the HTTP API) + the two viewer
functions + the UI BucketDeployment (`UiDistribution`). A pure DAG sink: it imports only the HTTP
API endpoint from the api stack, so the frequently-redeployed frontend never sits in a stack with
the RETAIN'd database.

The UI bucket is DESTROY + auto-delete: the SvelteKit build is reproducible from source, so there is
nothing to retain (unlike Aurora), and it keeps teardown clean.
"""

from __future__ import annotations

from pathlib import Path
from typing import Any

from aws_cdk import CfnOutput, Stack
from aws_cdk import aws_apigatewayv2 as apigw
from constructs import Construct

from biffix_infra.constructs.ui_distribution import UiDistribution
from biffix_infra.paths import ui_build_dir


class EdgeStack(Stack):
    def __init__(
        self,
        scope: Construct,
        cid: str,
        *,
        http_api: apigw.IHttpApi,
        app_domain: str | None = None,
        app_certificate_arn: str | None = None,
        ui_asset_path: str | Path | None = None,
        **kwargs: Any,
    ) -> None:
        super().__init__(scope, cid, **kwargs)
        # The stack's only component, as its `Default` child: the distribution keeps its logical
        # id (a replaced distribution could not take over the app domain's alias in place).
        ui = UiDistribution(
            self,
            "Default",
            http_api=http_api,
            ui_assets=Path(ui_asset_path) if ui_asset_path is not None else ui_build_dir(),
            app_domain=app_domain,
            app_certificate_arn=app_certificate_arn,
        )
        self.ui_bucket = ui.ui_bucket
        self.distribution = ui.distribution
        self.ui_deployment = ui.ui_deployment

        CfnOutput(self, "UiBucketName", value=self.ui_bucket.bucket_name)
        CfnOutput(self, "UiUrl", value=ui.url)
