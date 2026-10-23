"""The UI's CloudFront distribution: the SPA from a private S3 bucket as default, `/api/*` → the
HTTP API, the two viewer functions, and the UI BucketDeployment."""

from __future__ import annotations

import json
from pathlib import Path

from aws_cdk import Fn, RemovalPolicy
from aws_cdk import aws_apigatewayv2 as apigw
from aws_cdk import aws_certificatemanager as acm
from aws_cdk import aws_cloudfront as cloudfront
from aws_cdk import aws_cloudfront_origins as origins
from aws_cdk import aws_s3 as s3
from aws_cdk import aws_s3_deployment as s3_deployment
from constructs import Construct

from biffix_infra.wiring import log_group


def spa_rewrite_code(ui_assets: Path) -> str:
    """The viewer-request function that serves the SPA shell for client routes.

    Only what the build actually publishes passes through: its top-level files (`/index.html`,
    `/favicon.svg`, ...) and everything under its top-level directories (`/_app/...`). Every other
    path is a client route and is rewritten to `/index.html`, including one with a dot in it (a
    data source id like `refs.grch38`): testing for a "." instead would send it to S3, which
    answers 403 for a missing key on the private bucket. A missing asset under a published
    directory still gets S3's 403.
    """
    files = sorted(f"/{p.name}" for p in ui_assets.iterdir() if p.is_file())
    dirs = sorted(f"/{p.name}/" for p in ui_assets.iterdir() if p.is_dir())
    return (
        "var FILES = " + json.dumps({f: True for f in files}) + ";\n"
        "var DIRS = " + json.dumps(dirs) + ";\n"
        "function handler(event) {\n"
        "  var req = event.request;\n"
        "  var uri = req.uri;\n"
        "  if (uri === '/' || FILES[uri] === true) {\n"
        "    return req;\n"
        "  }\n"
        "  for (var i = 0; i < DIRS.length; i++) {\n"
        "    if (uri.indexOf(DIRS[i]) === 0) {\n"
        "      return req;\n"
        "    }\n"
        "  }\n"
        "  req.uri = '/index.html';\n"
        "  return req;\n"
        "}\n"
    )


class UiDistribution(Construct):
    """The distribution (`distribution`) serving the SvelteKit build at `ui_assets` from a private
    bucket (`ui_bucket`) and `http_api` under `/api/*`, on `app_domain` when a certificate for it
    is given (`app_certificate_arn`, in us-east-1), else on its CloudFront domain (`url`)."""

    def __init__(
        self,
        scope: Construct,
        cid: str,
        *,
        http_api: apigw.IHttpApi,
        ui_assets: Path,
        app_domain: str | None,
        app_certificate_arn: str | None,
    ) -> None:
        super().__init__(scope, cid)
        if not (ui_assets / "index.html").is_file():
            raise FileNotFoundError(
                f"SvelteKit build not found at {ui_assets}; run `npm ci && npm run build` in ui/"
            )
        self.ui_bucket = s3.Bucket(
            self,
            "UiBucket",
            block_public_access=s3.BlockPublicAccess.BLOCK_ALL,
            enforce_ssl=True,
            minimum_tls_version=1.2,
            # Reproducible from `npm run build`, so nothing to retain — and DESTROY + auto-delete
            # keeps teardown clean (unlike the RETAIN'd Aurora, which lives in persistence).
            removal_policy=RemovalPolicy.DESTROY,
            auto_delete_objects=True,
        )
        # HTTP API endpoint is https://<id>.execute-api.<region>.amazonaws.com; CloudFront needs
        # just the host.
        api_origin = origins.HttpOrigin(
            Fn.parse_domain_name(http_api.api_endpoint),
            protocol_policy=cloudfront.OriginProtocolPolicy.HTTPS_ONLY,
        )
        # SPA deep-link fallback: a hard refresh on a client route like /runs/abc has no
        # matching S3 object, so the private OAC bucket returns 403. A distribution-wide
        # 404/403→index.html error page would corrupt the API's real 404s, so instead a
        # viewer-request function rewrites every path the UI build does not publish to /index.html
        # (`spa_rewrite_code`); it is attached to the default (UI) behavior only, so /api/* is
        # untouched.
        spa_rewrite = cloudfront.Function(
            self,
            "SpaRewrite",
            code=cloudfront.FunctionCode.from_inline(spa_rewrite_code(ui_assets)),
        )
        # The API behavior selects on /api/*, then strips that public routing prefix before the
        # request reaches axum (whose code-first contract uses /runs, /status, etc.).
        api_rewrite = cloudfront.Function(
            self,
            "ApiRewrite",
            code=cloudfront.FunctionCode.from_inline(
                "function handler(event) {\n"
                "  var req = event.request;\n"
                "  req.uri = req.uri.substring(4) || '/';\n"
                "  return req;\n"
                "}\n"
            ),
        )

        # Optional custom app domain. Certificate issuance is deliberately OUTSIDE this stack:
        # external DNS must publish ACM's validation CNAME, and an in-stack Certificate would
        # block/rollback the deploy while waiting. First deploy with no ARN (CloudFront default
        # domain), request+validate the cert out-of-band, then set app_certificate_arn and
        # re-deploy to attach the alias. CloudFront requires us-east-1.
        domain_names = None
        certificate = None
        if app_domain and app_certificate_arn:
            certificate = acm.Certificate.from_certificate_arn(self, "AppCert", app_certificate_arn)
            domain_names = [app_domain]

        self.distribution = cloudfront.Distribution(
            self,
            "Edge",
            domain_names=domain_names,
            certificate=certificate,
            default_behavior=cloudfront.BehaviorOptions(
                origin=origins.S3BucketOrigin.with_origin_access_control(self.ui_bucket),
                viewer_protocol_policy=cloudfront.ViewerProtocolPolicy.REDIRECT_TO_HTTPS,
                compress=True,
                cache_policy=cloudfront.CachePolicy.CACHING_OPTIMIZED,
                response_headers_policy=cloudfront.ResponseHeadersPolicy.SECURITY_HEADERS,
                function_associations=[
                    cloudfront.FunctionAssociation(
                        function=spa_rewrite,
                        event_type=cloudfront.FunctionEventType.VIEWER_REQUEST,
                    )
                ],
            ),
            additional_behaviors={
                "/api/*": cloudfront.BehaviorOptions(
                    origin=api_origin,
                    viewer_protocol_policy=cloudfront.ViewerProtocolPolicy.REDIRECT_TO_HTTPS,
                    allowed_methods=cloudfront.AllowedMethods.ALLOW_ALL,
                    cache_policy=cloudfront.CachePolicy.CACHING_DISABLED,
                    origin_request_policy=cloudfront.OriginRequestPolicy.ALL_VIEWER_EXCEPT_HOST_HEADER,
                    response_headers_policy=cloudfront.ResponseHeadersPolicy.SECURITY_HEADERS,
                    function_associations=[
                        cloudfront.FunctionAssociation(
                            function=api_rewrite,
                            event_type=cloudfront.FunctionEventType.VIEWER_REQUEST,
                        )
                    ],
                ),
            },
            default_root_object="index.html",
            minimum_protocol_version=cloudfront.SecurityPolicyProtocol.TLS_V1_2_2021,
        )
        self.url = (
            f"https://{app_domain}"
            if domain_names
            else f"https://{self.distribution.distribution_domain_name}"
        )

        # Publish the SvelteKit adapter-static output (checked above: missing index.html is a hard
        # failure, since a placeholder UI would make a successful deploy unusable and hide a broken
        # release pipeline).
        self.ui_deployment = s3_deployment.BucketDeployment(
            self,
            "DeployUi",
            sources=[s3_deployment.Source.asset(str(ui_assets))],
            destination_bucket=self.ui_bucket,
            distribution=self.distribution,
            distribution_paths=["/*"],
            prune=True,
            # The bucket is DESTROY + auto-delete, so don't retain the deployed objects either —
            # teardown stays clean, and the UI is reproducible from `npm run build`.
            retain_on_delete=False,
            log_group=log_group(self, "DeployUiLogs"),
            cache_control=[
                s3_deployment.CacheControl.no_cache(),
                s3_deployment.CacheControl.no_store(),
                s3_deployment.CacheControl.must_revalidate(),
            ],
        )
