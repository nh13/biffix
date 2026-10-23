"""The identity stack: the user pool, its app clients and federation, the GitHub-OIDC roles and
the permissions boundary."""

import json

import aws_cdk as cdk
import pytest
from aws_cdk import aws_iam as iam
from aws_cdk.assertions import Match, Template

from biffix_infra.app_graph import Stacks
from biffix_infra.config import (
    GoogleOidc,
    ImageBuildTrust,
)
from biffix_infra.rbac import RBAC_GROUP_ROLES
from biffix_infra.stacks.data_stack import DataStack
from biffix_infra.stacks.identity_stack import IdentityStack
from tests.synth.synth_helpers import (
    CONFIG,
    Json,
    _actions,
    _grants,
    _logical_id,
    _render,
    _resources,
    _statements_for,
)


def test_identity_has_user_pool_and_boundary() -> None:
    app = cdk.App()
    ident = IdentityStack(app, "ident")
    t = Template.from_stack(ident)
    t.resource_count_is("AWS::Cognito::UserPool", 1)
    (boundary,) = t.find_resources("AWS::IAM::ManagedPolicy").values()
    allows = [
        st
        for st in boundary["Properties"]["PolicyDocument"]["Statement"]
        if st["Effect"] == "Allow"
    ]
    # The ALLOW ceiling, exactly: `iam:*`, `kms:*` or `*:*` would each widen it.
    assert {a for st in allows for a in _actions(st)} == _EXPECTED_BOUNDARY_ALLOW
    # PassRole is its only IAM action, and only on biffix's own roles: a bounded role can never
    # pass an arbitrary (e.g. admin) role to a service.
    (pass_role,) = [st for st in allows if any(a.startswith("iam:") for a in _actions(st))]
    assert _actions(pass_role) == {"iam:PassRole"}
    assert _resources(pass_role) == ["arn:aws:iam::{Ref:AWS::AccountId}:role/biffix-*"], pass_role


# The boundary's ALLOW ceiling (identity_stack.py).
_EXPECTED_BOUNDARY_ALLOW = {
    "sts:AssumeRole",
    "sts:TagSession",
    "batch:*",
    "s3:*",
    "kms:Decrypt",
    "kms:Encrypt",
    "kms:GenerateDataKey",
    "kms:DescribeKey",
    "logs:*",
    "sqs:*",
    "secretsmanager:GetSecretValue",
    "ecr:*",
    "ecs:*",
    "ec2:Describe*",
    "ec2:CreateTags",
    "iam:PassRole",
}


def test_every_runtime_and_oidc_role_is_bounded(default_stacks: Stacks) -> None:
    """The boundary is the privilege-escalation guard only on the roles that carry it: each
    project's run role, each coordinator role, and the GitHub-OIDC registration and image-build
    roles."""
    identity = default_stacks["identity"]
    boundary_id = _logical_id(identity, identity.permissions_boundary)

    def boundary_of(stack: cdk.Stack, role: iam.IRole) -> str:
        roles = Template.from_stack(stack).find_resources("AWS::IAM::Role")
        return _render(roles[_logical_id(stack, role)]["Properties"].get("PermissionsBoundary"))

    # Across stacks the boundary is an import of the identity stack's managed policy.
    imported = f"{{Import:identity:ExportsOutputRef{boundary_id}"
    for project in default_stacks["projects"]:
        assert boundary_of(project, project.run_base_role).startswith(imported), project
    api = default_stacks["api"]
    assert api.coordinator_roles
    for pid, role in api.coordinator_roles.items():
        assert boundary_of(api, role).startswith(imported), pid
    app = cdk.App()
    ident = IdentityStack(
        app,
        "ident",
        github_subjects=["repo:acme/pipe:ref:refs/heads/main"],
        image_build_trust=ImageBuildTrust(subject="repo:example/biffix:environment:image-build"),
    )
    own = f"{{Ref:{_logical_id(ident, ident.permissions_boundary)}}}"
    for oidc_role in (ident.registration_role, ident.image_build_role):
        assert oidc_role is not None
        assert boundary_of(ident, oidc_role) == own, oidc_role


def test_registration_role_absent_without_github_subjects() -> None:
    app = cdk.App()
    ident = IdentityStack(app, "ident")
    assert ident.registration_role is None
    t = Template.from_stack(ident)
    for r in t.find_resources("AWS::IAM::Role").values():
        for st in r["Properties"]["AssumeRolePolicyDocument"]["Statement"]:
            assert "Federated" not in st.get("Principal", {}), "no federated (OIDC) role expected"


def test_registration_role_present_and_scoped_with_subjects() -> None:
    app = cdk.App()
    ident = IdentityStack(app, "ident", github_subjects=["repo:acme/pipe:ref:refs/heads/main"])
    assert ident.registration_role is not None
    # Trusted only for the configured subjects: `aud` alone is the same for every GitHub workflow.
    assert _oidc_conditions(Template.from_stack(ident), "RegistrationRole") == {
        "StringEquals": {"token.actions.githubusercontent.com:aud": "sts.amazonaws.com"},
        "StringLike": {
            "token.actions.githubusercontent.com:sub": ["repo:acme/pipe:ref:refs/heads/main"]
        },
    }


def test_identity_wires_google_federation_and_hosted_ui() -> None:
    app = cdk.App()
    ident = IdentityStack(
        app,
        "ident",
        public_base_url="https://biffix.example.org",
        cognito_domain_prefix="biffix-acme",
        google_oidc=GoogleOidc(client_id="123.apps.googleusercontent.com"),
        google_secret_name="biffix/acme/google-oidc",
    )
    t = Template.from_stack(ident)
    t.resource_count_is("AWS::Cognito::UserPoolIdentityProvider", 1)
    t.resource_count_is("AWS::Cognito::UserPoolDomain", 1)
    t.has_resource_properties(
        "AWS::Cognito::UserPoolIdentityProvider", Match.object_like({"ProviderType": "Google"})
    )
    # The client secret is resolved from Secrets Manager at the derived name (a dynamic reference),
    # not embedded as plaintext — so the template's ClientSecret cites that secret name.
    t.has_resource_properties(
        "AWS::Cognito::UserPoolIdentityProvider",
        Match.object_like(
            {
                "ProviderDetails": Match.object_like(
                    {"client_secret": Match.string_like_regexp("biffix/acme/google-oidc")}
                )
            }
        ),
    )
    t.has_resource_properties(
        "AWS::Cognito::UserPoolClient",
        Match.object_like({"SupportedIdentityProviders": ["Google"]}),
    )
    t.has_resource_properties(
        "AWS::Cognito::UserPoolClient",
        Match.object_like(
            {"CallbackURLs": Match.array_with(["https://biffix.example.org/auth/callback"])}
        ),
    )


def test_identity_has_web_and_cli_app_clients() -> None:
    app = cdk.App()
    ident = IdentityStack(app, "ident")
    t = Template.from_stack(ident)
    t.resource_count_is("AWS::Cognito::UserPoolClient", 2)
    t.has_resource_properties(
        "AWS::Cognito::UserPoolClient",
        Match.object_like(
            {
                "CallbackURLs": Match.array_with(["http://localhost:8976/callback"]),
                "GenerateSecret": False,
            }
        ),
    )


def test_identity_emits_google_redirect_uri_output() -> None:
    # The Google OAuth client's Authorized redirect URI is a manual, cross-cloud step CDK cannot
    # own; the stack surfaces the exact value so a deploy tells the operator what to register in
    # Google Cloud (guards against the redirect_uri_mismatch footgun). A concrete env makes the
    # region a literal so the emitted URI is a plain string, not a Ref(AWS::Region) join.
    app = cdk.App()
    ident = IdentityStack(
        app,
        "ident",
        cognito_domain_prefix="biffix-acme",
        google_oidc=GoogleOidc(client_id="123.apps.googleusercontent.com"),
        google_secret_name="biffix/acme/google-oidc",
        env=cdk.Environment(account="222233334444", region="us-east-1"),
    )
    t = Template.from_stack(ident)
    t.has_output(
        "GoogleAuthorizedRedirectUri",
        {"Value": "https://biffix-acme.auth.us-east-1.amazoncognito.com/oauth2/idpresponse"},
    )


def test_identity_without_google_has_no_idp() -> None:
    app = cdk.App()
    ident = IdentityStack(app, "ident")
    t = Template.from_stack(ident)
    assert not t.find_resources("AWS::Cognito::UserPoolIdentityProvider")
    # No federation => no redirect-URI output to register (the output is Google-only).
    assert not t.find_outputs("GoogleAuthorizedRedirectUri")
    assert not t.find_resources("AWS::Cognito::UserPoolDomain")


def test_identity_creates_rbac_groups_for_every_mapped_role() -> None:
    app = cdk.App()
    ident = IdentityStack(app, "ident")
    t = Template.from_stack(ident)
    t.resource_count_is("AWS::Cognito::UserPoolGroup", len(RBAC_GROUP_ROLES))
    for group in RBAC_GROUP_ROLES:
        t.has_resource_properties(
            "AWS::Cognito::UserPoolGroup", Match.object_like({"GroupName": group})
        )


def _oidc_conditions(template: Template, role_logical_prefix: str) -> Json:
    roles = template.find_resources("AWS::IAM::Role")
    for logical, resource in roles.items():
        if not logical.startswith(role_logical_prefix):
            continue
        for stmt in resource["Properties"]["AssumeRolePolicyDocument"]["Statement"]:
            if "Federated" in str(stmt.get("Principal", {})):
                conditions: Json = stmt.get("Condition", {})
                return conditions
    raise AssertionError(f"no OIDC role found with prefix {role_logical_prefix}: {list(roles)}")


def test_biffix_image_build_role_is_pinned_to_one_branch() -> None:
    from biffix_infra.config import ImageBuildTrust

    app = cdk.App()
    ident = IdentityStack(
        app,
        "ident",
        github_subjects=["repo:acme/pipelines:environment:image-build"],
        image_build_trust=ImageBuildTrust(
            subject="repo:example/biffix:environment:image-build",
            ref="refs/heads/main",
        ),
    )
    assert ident.image_build_role is not None
    conditions = _oidc_conditions(Template.from_stack(ident), "ImageBuildRole")
    equals = conditions["StringEquals"]
    assert equals["token.actions.githubusercontent.com:ref"] == "refs/heads/main"
    assert (
        equals["token.actions.githubusercontent.com:sub"]
        == "repo:example/biffix:environment:image-build"
    )
    assert equals["token.actions.githubusercontent.com:aud"] == "sts.amazonaws.com"


def test_the_shared_registration_role_does_not_trust_biffix() -> None:
    from biffix_infra.config import ImageBuildTrust

    app = cdk.App()
    ident = IdentityStack(
        app,
        "ident",
        github_subjects=["repo:acme/pipelines:environment:image-build"],
        image_build_trust=ImageBuildTrust(subject="repo:example/biffix:environment:image-build"),
    )
    conditions = _oidc_conditions(Template.from_stack(ident), "RegistrationRole")
    assert "biffix" not in str(conditions), conditions


def test_no_image_build_role_without_configured_trust() -> None:
    app = cdk.App()
    ident = IdentityStack(app, "ident", github_subjects=["repo:acme/p:environment:image-build"])
    assert ident.image_build_role is None


# The full IAM privilege-escalation denylist the boundary DENYs (identity_stack.py). Kept here as
# the intended contract so dropping any single action from the boundary fails this test.
_EXPECTED_BOUNDARY_DENY = {
    "iam:CreateUser",
    "iam:CreateRole",
    "iam:CreatePolicy",
    "iam:CreatePolicyVersion",
    "iam:SetDefaultPolicyVersion",
    "iam:CreateAccessKey",
    "iam:CreateLoginProfile",
    "iam:UpdateLoginProfile",
    "iam:AttachUserPolicy",
    "iam:AttachRolePolicy",
    "iam:AttachGroupPolicy",
    "iam:PutUserPolicy",
    "iam:PutRolePolicy",
    "iam:PutGroupPolicy",
    "iam:UpdateAssumeRolePolicy",
    "iam:CreateInstanceProfile",
    "iam:AddRoleToInstanceProfile",
    "iam:PutRolePermissionsBoundary",
    "iam:DeleteRolePermissionsBoundary",
    "iam:PutUserPermissionsBoundary",
    "iam:DeleteUserPermissionsBoundary",
}


def test_boundary_denies_the_full_privilege_escalation_set() -> None:
    # The DENY denylist is the core privilege-escalation guard (the ALLOW ceiling is separately
    # checked by test_identity_has_user_pool_and_boundary). Assert the exact set: a dropped action
    # silently re-opens an escalation vector that no green ALLOW-only assertion would catch.
    app = cdk.App()
    ident = IdentityStack(app, "ident")
    t = Template.from_stack(ident)
    denied: set[str] = set()
    for pol in t.find_resources("AWS::IAM::ManagedPolicy").values():
        for st in pol["Properties"]["PolicyDocument"]["Statement"]:
            if st.get("Effect") == "Deny":
                denied |= _actions(st)
    assert denied == _EXPECTED_BOUNDARY_DENY


def test_registration_role_pushes_workflow_repo_not_coordinator_repo() -> None:
    # Security contract (identity_stack.py): a third-party workflow repo may push WORKFLOW images
    # but must NEVER be able to replace the coordinator image (biffix's own trusted execution
    # environment for every run). Assert the ECR push grant references the workflow repo, not the
    # coordinator repo — comparing the resource ARNs, not merely that push actions exist somewhere.
    app = cdk.App()
    data = DataStack(app, "data")
    ident = IdentityStack(
        app,
        "ident",
        github_subjects=["repo:acme/pipelines:ref:refs/heads/main"],
        workflow_repository=data.workflow_repo,
        coordinator_repository=data.coordinator_repo,
        image_build_trust=ImageBuildTrust(subject="repo:example/biffix:environment:image-build"),
    )
    assert ident.registration_role is not None
    t = Template.from_stack(ident)
    # Biffix's own image build pushes both.
    build_push = json.dumps(
        [
            st
            for st in _statements_for(t, ident, ident.image_build_role)
            if _grants(st, "ecr:PutImage")
        ]
    )
    assert "WorkflowRepo" in build_push and "CoordinatorRepo" in build_push
    # The registration role is granted only what it is given as the workflow repository: with
    # just the coordinator repository wired it gets no push at all (fails closed).
    app2 = cdk.App()
    data2 = DataStack(app2, "data")
    closed = IdentityStack(
        app2,
        "ident",
        github_subjects=["repo:acme/pipelines:ref:refs/heads/main"],
        coordinator_repository=data2.coordinator_repo,
    )
    closed_statements = _statements_for(
        Template.from_stack(closed), closed, closed.registration_role
    )
    # Wildcards included (`ecr:*`, `*`): a negative check must see through them.
    pushes = ("ecr:PutImage", "ecr:InitiateLayerUpload", "ecr:CompleteLayerUpload")
    assert not any(_grants(st, a) for st in closed_statements for a in pushes)
    t = Template.from_stack(ident)
    push_statements = [
        st
        for st in _statements_for(t, ident, ident.registration_role)
        if any(_grants(st, a) for a in pushes)
    ]
    assert push_statements, "registration role should have an ECR push grant on the workflow repo"
    blob = json.dumps(push_statements)
    # Cross-stack refs carry the source construct's logical id (…WorkflowRepo… / …CoordinatorRepo…).
    assert "WorkflowRepo" in blob and "CoordinatorRepo" not in blob, blob


def test_the_user_pool_offers_totp_mfa_and_a_strong_password_policy() -> None:
    def pool(mfa: str | None = None) -> Json:
        """The user pool's properties, with `mfa` (the stack's default when None)."""
        app = cdk.App()
        ident = IdentityStack(app, "ident") if mfa is None else IdentityStack(app, "ident", mfa=mfa)
        (p,) = Template.from_stack(ident).find_resources("AWS::Cognito::UserPool").values()
        props: Json = p["Properties"]
        return props

    default = pool()
    assert default["MfaConfiguration"] == "OPTIONAL"
    assert default["EnabledMfas"] == ["SOFTWARE_TOKEN_MFA"]  # an authenticator app, never SMS
    policy = default["Policies"]["PasswordPolicy"]
    assert policy["MinimumLength"] >= 12
    assert all(
        policy[k] is True
        for k in ("RequireLowercase", "RequireUppercase", "RequireNumbers", "RequireSymbols")
    )
    assert pool(mfa="required")["MfaConfiguration"] == "ON"
    assert pool(mfa="off")["MfaConfiguration"] == "OFF"
    with pytest.raises(ValueError, match="mfa"):
        pool(mfa="sometimes")
    # (test_config_keys covers how the loader reads `cognito_mfa`.)
    assert CONFIG.cognito_mfa == "optional"
