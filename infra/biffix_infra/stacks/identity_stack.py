"""IdentityStack — Cognito, the permissions boundary, and the GitHub-OIDC roles.

biffix never creates IAM at runtime: the permissions boundary (which each project's run base role,
in ProjectStack, and coordinator role, in the api stack, also carry) and the GitHub-OIDC
registration and image-build roles are provisioned here. The running app makes no STS call: each
Lambda acts as its own role, and Batch hands a run's tasks theirs.
"""

from __future__ import annotations

from typing import Any

from aws_cdk import CfnOutput, Duration, SecretValue, Stack
from aws_cdk import aws_cognito as cognito
from aws_cdk import aws_ecr as ecr
from aws_cdk import aws_iam as iam
from constructs import Construct

from biffix_infra.config import GoogleOidc, ImageBuildTrust
from biffix_infra.constructs.github_oidc import GithubOidcRole
from biffix_infra.constructs.permissions_boundary import PermissionsBoundary
from biffix_infra.rbac import RBAC_GROUP_ROLES

# The registered loopback port pool for the native CLI OAuth client. A Cognito
# redirect URI must be pre-registered and matched EXACTLY, so an ephemeral port cannot work: the
# CLI binds the first free port from this fixed pool and sends the matching redirect_uri. All four
# are registered as callbacks on the CLI app client.
CLI_LOOPBACK_PORTS: tuple[int, ...] = (8976, 8977, 8978, 8979)

# `cognito_mfa` (config) → the pool's MFA setting.
_MFA = {"off": cognito.Mfa.OFF, "optional": cognito.Mfa.OPTIONAL, "required": cognito.Mfa.REQUIRED}
# The shortest password the pool accepts for its own (non-federated) users.
MIN_PASSWORD_LENGTH = 12


def cli_redirect_uris() -> list[str]:
    """The registered loopback callback URIs for the CLI client (one per pooled port)."""
    return [f"http://localhost:{port}/callback" for port in CLI_LOOPBACK_PORTS]


def _cli_logout_uris() -> list[str]:
    return [f"http://localhost:{port}/logged-out" for port in CLI_LOOPBACK_PORTS]


class IdentityStack(Stack):
    def __init__(
        self,
        scope: Construct,
        cid: str,
        *,
        github_subjects: list[str] | None = None,
        public_base_url: str = "http://localhost:5173",
        cognito_domain_prefix: str | None = None,
        google_oidc: GoogleOidc | None = None,
        google_secret_name: str | None = None,
        workflow_repository: ecr.IRepository | None = None,
        coordinator_repository: ecr.IRepository | None = None,
        image_build_trust: ImageBuildTrust | None = None,
        mfa: str = "optional",
        **kwargs: Any,
    ) -> None:
        super().__init__(scope, cid, **kwargs)

        # Cognito user pool — the app's OIDC token issuer, optionally federated upstream to Google
        # Workspace (identity is managed through the IdP). For the pool's own users (the
        # sign-in path when no IdP is federated): an authenticator-app second factor (config
        # `cognito_mfa`; never SMS) and a password policy fit for a HIPAA-ready deploy.
        if mfa not in _MFA:
            raise ValueError(f"mfa {mfa!r}, expected one of {sorted(_MFA)}")
        self.user_pool = cognito.UserPool(
            self,
            "UserPool",
            self_sign_up_enabled=False,
            sign_in_aliases=cognito.SignInAliases(email=True),
            mfa=_MFA[mfa],
            mfa_second_factor=(
                None if mfa == "off" else cognito.MfaSecondFactor(sms=False, otp=True)
            ),
            password_policy=cognito.PasswordPolicy(
                min_length=MIN_PASSWORD_LENGTH,
                require_lowercase=True,
                require_uppercase=True,
                require_digits=True,
                require_symbols=True,
                temp_password_validity=Duration.days(3),
            ),
        )

        # Optional hosted-UI domain — hosts the OAuth endpoints (incl. the /oauth2/idpresponse URL
        # Google redirects back to). Required whenever federation or the hosted login flow is used.
        self.user_pool_domain = None
        if cognito_domain_prefix:
            self.user_pool_domain = self.user_pool.add_domain(
                "HostedUiDomain",
                cognito_domain=cognito.CognitoDomainOptions(domain_prefix=cognito_domain_prefix),
            )

        # Optional Google Workspace federation. The client secret is read from Secrets Manager by
        # name at deploy (never in config, never plaintext in the template); deploy.sh provisions it
        # at that name. `google_secret_name` is the derived `biffix/<tenant>/google-oidc`.
        self.google_idp = None
        if google_oidc is not None:
            if not google_secret_name:
                raise ValueError(
                    "google_oidc requires google_secret_name (the Secrets Manager name)"
                )
            self.google_idp = cognito.UserPoolIdentityProviderGoogle(
                self,
                "GoogleIdp",
                user_pool=self.user_pool,
                client_id=google_oidc.client_id,
                client_secret_value=SecretValue.secrets_manager(google_secret_name),
                scopes=["openid", "email", "profile"],
                attribute_mapping=cognito.AttributeMapping(
                    email=cognito.ProviderAttribute.GOOGLE_EMAIL
                ),
            )
            # When an upstream IdP is configured it is the ONLY sign-in method: native Cognito
            # username/password is disabled so the Workspace is biffix's single identity source
            # (manage-identity-through-the-IdP). Mapped IdP groups can still grant stronger roles.
            supported_idps = [cognito.UserPoolClientIdentityProvider.GOOGLE]
        else:
            supported_idps = [cognito.UserPoolClientIdentityProvider.COGNITO]

        # App callback/logout URLs. `public_base_url` is the ONE source shared with the api stack's
        # OAuth redirect env (config.public_base_url): a custom-domain https URL, else localhost.
        # Never the CloudFront domain — that made api depend on edge while edge depends on api.
        self.user_pool_client = self.user_pool.add_client(
            "WebClient",
            o_auth=cognito.OAuthSettings(
                flows=cognito.OAuthFlows(authorization_code_grant=True),
                scopes=[cognito.OAuthScope.OPENID, cognito.OAuthScope.EMAIL],
                callback_urls=[f"{public_base_url}/auth/callback"],
                # Cognito matches logout_uri EXACTLY against this list, so the signed-out landing
                # page needs its own entry. It exists because a sign-out that only clears local
                # tokens leaves the Cognito session alive: the app would auto-redirect and be
                # silently signed straight back in, which is not a sign-out. Returning to a distinct
                # page lets the app know to stay signed out and offer Sign in.
                logout_urls=[public_base_url, f"{public_base_url}/signed-out"],
            ),
            supported_identity_providers=supported_idps,
        )
        # The client must not be created before its IdP exists, or Cognito rejects the
        # supported_identity_providers reference.
        if self.google_idp is not None:
            self.user_pool_client.node.add_dependency(self.google_idp)

        # Native CLI app client: a SECOND public client, no secret (RFC 8252),
        # authorization-code + PKCE, with loopback callbacks from the registered port pool.
        # It is separate from the web client so either can be revoked/re-scoped independently and
        # the browser client never carries localhost callbacks. The backend accepts BOTH clients'
        # tokens (multi-audience, D3); the HTTP API JWT authorizer lists both (ApiStack).
        self.cli_redirect_uris = cli_redirect_uris()
        self.cli_user_pool_client = self.user_pool.add_client(
            "CliClient",
            generate_secret=False,  # public client — no secret ships in the binary
            o_auth=cognito.OAuthSettings(
                flows=cognito.OAuthFlows(authorization_code_grant=True),
                scopes=[cognito.OAuthScope.OPENID, cognito.OAuthScope.EMAIL],
                callback_urls=self.cli_redirect_uris,
                logout_urls=_cli_logout_uris(),
            ),
            supported_identity_providers=supported_idps,
        )
        if self.google_idp is not None:
            self.cli_user_pool_client.node.add_dependency(self.google_idp)

        # Federation has one step CDK cannot own: the Google OAuth client (in Google Cloud, not
        # AWS) must whitelist Cognito's hosted-UI callback, or every sign-in fails
        # `redirect_uri_mismatch`. That URI is deterministic from the hosted-UI domain + region, so
        # emit it as a stack output: a `cdk deploy` with Google enabled prints exactly what to paste
        # into the Google client. The URI changes only if `cognito_domain_prefix` (or the region)
        # changes — which is why the config model warns against churning the prefix. Google-only.
        if google_oidc is not None and cognito_domain_prefix:
            CfnOutput(
                self,
                "GoogleAuthorizedRedirectUri",
                value=(
                    f"https://{cognito_domain_prefix}.auth.{self.region}"
                    ".amazoncognito.com/oauth2/idpresponse"
                ),
                description=(
                    "Add this EXACT URI to the Google OAuth client's Authorized redirect URIs "
                    "(Google Cloud console) or federated sign-in fails with redirect_uri_mismatch."
                ),
            )

        # Google Workspace does not emit `cognito:groups`. ApiStack injects the deployment's
        # explicit authenticated-user default role (Viewer in v1); mapped Cognito groups can still
        # grant stronger roles without mirroring users into biffix.

        # RBAC groups. biffix holds only the group→role *mapping*; group
        # MEMBERSHIP is administered in Cognito/the IdP, never through a biffix screen. Cognito puts
        # these names in the `cognito:groups` claim for federated identities too, which is exactly
        # what the backend's RBAC reads, so the group path works today with Google sign-in.
        self.rbac_groups = {
            group: cognito.CfnUserPoolGroup(
                self,
                f"RbacGroup-{role}",
                user_pool_id=self.user_pool.user_pool_id,
                group_name=group,
                description=f"biffix RBAC role: {role}",
            )
            for group, role in RBAC_GROUP_ROLES.items()
        }

        # A permissions boundary caps the maximum any biffix-managed role can ever acquire.
        self.permissions_boundary = PermissionsBoundary(self, "BiffixBoundary")

        # GitHub-OIDC-trusted registration role. Created ONLY when the deployer
        # configures `registration.github_subjects` in the deploy config — no config ⇒ no
        # GitHub-OIDC trust relationship at all (fail-safe by absence). The `sub` condition scopes
        # the role to the deployer's OWN repo(s); the `aud` claim alone is identical for every
        # GitHub workflow and must never be the only condition.
        self.registration_role: iam.Role | None = None
        subjects = github_subjects or []
        if subjects:
            self.registration_role = GithubOidcRole(
                self,
                "RegistrationRole",
                permissions_boundary=self.permissions_boundary,
                string_like={"sub": subjects},
            )
            # Third-party workflow repos push WORKFLOW images only: the role is granted the
            # workflow repository and nothing else, never the coordinator repository (the
            # coordinator image is biffix's own trusted execution environment for every run, and a
            # workflow author must not be able to replace it). Grants live on the ROLE, whose stack
            # already depends on DataStack, so the repository references flow identity -> data,
            # the direction the graph allows.
            if workflow_repository is not None:
                workflow_repository.grant_pull_push(self.registration_role)
                # Obtaining an ECR auth token is an account-level action with no resource scope.
                self.registration_role.add_to_principal_policy(
                    iam.PolicyStatement(
                        actions=["ecr:GetAuthorizationToken"],
                        resources=["*"],
                    )
                )

        # biffix's OWN image-build role. Separate from the registration role above because it
        # can be — and therefore should be — pinned to a single branch.
        #
        # The shared registration role trusts third-party workflow repositories, which dispatch
        # builds from PR branches deliberately; pinning `ref` there would break them. biffix's own
        # build has no such need, so its trust policy additionally requires the OIDC token's `ref`
        # claim to be exactly the configured branch. This is enforced by AWS at AssumeRole, so it
        # holds even though GitHub's required-reviewer rule is unavailable on this plan and the
        # `image-build` environment gate is only advisory.
        self.image_build_role: iam.Role | None = None
        if image_build_trust is not None:
            self.image_build_role = GithubOidcRole(
                self,
                "ImageBuildRole",
                permissions_boundary=self.permissions_boundary,
                # Both pinned exactly: the environment-scoped subject AND the branch.
                string_equals={"sub": image_build_trust.subject, "ref": image_build_trust.ref},
            )
            for repository in (coordinator_repository, workflow_repository):
                if repository is not None:
                    repository.grant_pull_push(self.image_build_role)
            self.image_build_role.add_to_principal_policy(
                iam.PolicyStatement(
                    # GetAuthorizationToken is an account-level action with no resource scope.
                    # DescribeRepositories is what the build-coordinator-image workflow uses to
                    # resolve the CloudFormation-generated repository names by their tenant
                    # prefix — it lists all repositories and filters client-side, so it needs
                    # resource "*" (a scoped ARN list would deny the unnamed list call). Without it
                    # the build fails at repo resolution with AccessDeniedException on
                    # ecr:DescribeRepositories.
                    actions=["ecr:GetAuthorizationToken", "ecr:DescribeRepositories"],
                    resources=["*"],
                )
            )
