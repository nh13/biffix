"""The backend API: the Rust Lambda behind an HTTP API with a Cognito JWT authorizer.

No load balancer, no always-on task: pay-per-request, ~$0 idle. The Lambda runs in the VPC so it can
reach Aurora; API Gateway → Lambda is a native integration (no VPC Link).
"""

from __future__ import annotations

from aws_cdk import Duration
from aws_cdk import aws_apigatewayv2 as apigw
from aws_cdk import aws_apigatewayv2_authorizers as apigw_auth
from aws_cdk import aws_apigatewayv2_integrations as apigw_int
from aws_cdk import aws_cognito as cognito
from aws_cdk import aws_ec2 as ec2
from aws_cdk import aws_lambda as lambda_
from constructs import Construct

from biffix_infra.constructs.rust_function import RustFunction


class BackendApi(Construct):
    """The backend Lambda (`function`, running `code`; this construct's default child) and the
    HTTP API in front of it (`http_api`), with what the backend needs to authenticate callers: the
    Cognito issuer and app clients, and the OAuth endpoints the SPA and the CLI sign in through.
    What it serves (projects, launches, builds) is wired by the stack."""

    def __init__(
        self,
        scope: Construct,
        cid: str,
        *,
        code: lambda_.Code,
        vpc: ec2.IVpc,
        region: str,
        user_pool: cognito.IUserPool,
        user_pool_client: cognito.IUserPoolClient,
        cli_user_pool_client: cognito.IUserPoolClient | None,
        cli_redirect_uris: list[str] | None,
        public_base_url: str,
        cognito_domain_prefix: str | None,
    ) -> None:
        super().__init__(scope, cid)
        issuer = f"https://cognito-idp.{region}.amazonaws.com/{user_pool.user_pool_id}"
        self.function = RustFunction(
            self,
            "Default",
            code=code,
            vpc=vpc,
            timeout=Duration.seconds(29),  # API Gateway HTTP API caps integration at 30s
            memory_size=512,
            logs_id="Logs",
            environment={
                "BIFFIX_OIDC_ISSUER": issuer,
                "BIFFIX_OIDC_AUDIENCE": user_pool_client.user_pool_client_id,
                "RUST_LOG": "info",
            },
        )

        # Native CLI app client: the backend additionally accepts its tokens
        # (multi-audience) and publishes its id + registered loopback redirect URIs at
        # GET /auth/config so the CLI discovers them (no client id baked into the binary).
        if cli_user_pool_client is not None:
            self.function.add_environment(
                "BIFFIX_CLI_CLIENT_ID", cli_user_pool_client.user_pool_client_id
            )
        if cli_redirect_uris:
            self.function.add_environment("BIFFIX_CLI_REDIRECT_URIS", ",".join(cli_redirect_uris))

        # A Cognito JWT authorizer rejects unauthenticated requests at the gateway, before the
        # Lambda runs; the backend still re-validates the JWT + RBAC per route (defense in depth).
        authorizer_clients = [user_pool_client]
        if cli_user_pool_client is not None:
            authorizer_clients.append(cli_user_pool_client)
        self.authorizer = apigw_auth.HttpUserPoolAuthorizer(
            "BackendJwtAuthorizer",
            user_pool,
            user_pool_clients=authorizer_clients,
        )
        integration = apigw_int.HttpLambdaIntegration("BackendIntegration", handler=self.function)
        self.http_api = apigw.HttpApi(
            self,
            "HttpApi",
            default_integration=integration,
            default_authorizer=self.authorizer,
        )
        # Public bootstrap/health routes bypass the Cognito authorizer (the SPA fetches its OAuth
        # config before login; orchestration probes health). The backend still validates any token.
        for public_path in ("/auth/config", "/healthz", "/readyz"):
            self.http_api.add_routes(
                path=public_path,
                methods=[apigw.HttpMethod.GET],
                integration=integration,
                authorizer=apigw.HttpNoneAuthorizer(),
            )
        # The engine ingest route bypasses the COGNITO authorizer but is not unauthenticated: the
        # coordinator presents a per-run HMAC capability token the backend verifies itself. It must
        # skip the JWT authorizer because a Cognito token is exactly what a machine cannot have.
        self.http_api.add_routes(
            path="/events",
            methods=[apigw.HttpMethod.POST],
            integration=integration,
            authorizer=apigw.HttpNoneAuthorizer(),
        )

        # OAuth URLs use the shared public_base_url (config), the SAME value IdentityStack
        # registers as Cognito callbacks, so the two can never disagree.
        self.function.add_environment(
            "BIFFIX_OAUTH_REDIRECT_URI", f"{public_base_url}/auth/callback"
        )
        self.function.add_environment("BIFFIX_OAUTH_LOGOUT_URI", f"{public_base_url}/signed-out")
        if cognito_domain_prefix:
            hosted_ui = f"https://{cognito_domain_prefix}.auth.{region}.amazoncognito.com"
            for name, path in (
                ("BIFFIX_OAUTH_AUTHORIZATION_ENDPOINT", "/oauth2/authorize"),
                ("BIFFIX_OAUTH_TOKEN_ENDPOINT", "/oauth2/token"),
                ("BIFFIX_OAUTH_LOGOUT_ENDPOINT", "/logout"),
            ):
                self.function.add_environment(name, f"{hosted_ui}{path}")
