"""The names and URLs a deploy derives from its config: stacks, queues, build projects, alarms.

Each is the ONE source of its formula: the stacks that create a resource and the code that finds
it again (the scripts, the ingest routing, the status probe) all read it here, from DeployConfig.
"""

from __future__ import annotations

from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from biffix_infra.config.compute import QueueClass
    from biffix_infra.config.identity import GoogleOidc
    from biffix_infra.config.projects import Project

# The architectures workflow images are built for (one CodeBuild project each).
IMAGE_BUILD_ARCHS = ("amd64", "arm64")

# The parts of every stack name, in the app's creation order. Project stacks (`project-<id>`) come
# from the config and go after `build`.
_STACK_PARTS_BEFORE_PROJECTS = ("network", "data", "identity", "compute", "build")
_STACK_PARTS_AFTER_PROJECTS = ("persistence", "ingest", "api", "edge")


class DeployNames:
    """The derived names, mixed into DeployConfig (whose fields these read)."""

    tenant: str
    projects: list[Project]
    app_domain: str | None
    google_oidc: GoogleOidc | None

    def stack_name(self, part: str) -> str:
        """The physical CloudFormation stack name of a stack part (`network`, `project-<id>`, …)."""
        return f"biffix-{self.tenant}-{part}"

    def stack_names(self) -> list[str]:
        """Every stack the CDK app synthesizes for this config (what `cdk deploy --all` targets)."""
        parts = [
            *_STACK_PARTS_BEFORE_PROJECTS,
            *(f"project-{p.id}" for p in self.projects),
            *_STACK_PARTS_AFTER_PROJECTS,
        ]
        return [self.stack_name(part) for part in parts]

    @property
    def api_readyz_url(self) -> str | None:
        """The API's public readiness URL (through CloudFront, as users reach it), for the status
        probe; None without an app domain."""
        return f"https://{self.app_domain}/api/readyz" if self.app_domain else None

    @property
    def public_base_url(self) -> str:
        """The stable browser origin for OAuth callback/redirect URLs — `https://<app_domain>` when
        a custom domain is set, else a localhost dev URL. The ONE source both IdentityStack (Cognito
        callback registration) and the api stack (the backend's redirect env) read, so the two can
        never disagree. Deliberately NOT the CloudFront domain: deriving it there made the api stack
        depend on the edge stack while edge depends on api (a cycle), and with no app_domain Cognito
        only registers the localhost callback anyway, so the CloudFront fallback never logged in.
        """
        return f"https://{self.app_domain}" if self.app_domain else "http://localhost:5173"

    @property
    def google_secret_name(self) -> str | None:
        """Secrets Manager name holding the Google OAuth client secret, or None when federation is
        off. `deploy.sh` creates/reuses the secret at EXACTLY this name in the target account, and
        IdentityStack reads it here — the one convention both sides share, so the secret never needs
        a hand-managed ARN in config."""
        return f"biffix/{self.tenant}/google-oidc" if self.google_oidc is not None else None

    @property
    def alarm_prefix(self) -> str:
        """The name prefix of this deploy's CloudWatch alarms (each is named `<prefix><id>`); the
        status probe lists the alarms by it. The `/` ends the tenant: a tenant is part of every
        stack name, which CloudFormation limits to letters, digits and `-`, so no sibling tenant
        `<tenant>-<x>` in the account can share it (a bare `biffix-<tenant>-` would match its
        alarms)."""
        return f"biffix-{self.tenant}/"

    @property
    def build_project_prefix(self) -> str:
        """The name prefix of this deploy's CodeBuild projects (image and engine-runtime builds):
        every biffix build project is named with it. Build events are routed by the projects'
        exact names (`build_project_names`), not by this prefix."""
        return f"biffix-{self.tenant}-"

    @property
    def build_project_names(self) -> list[str]:
        """Every CodeBuild project this deploy creates. Build events are routed by these EXACT
        names, not by the prefix, which a sibling tenant `<tenant>-<x>` in the account shares."""
        return [
            self.runtime_build_project_name,
            self.image_import_project_name,
            *(self.image_build_project_name(arch) for arch in IMAGE_BUILD_ARCHS),
        ]

    @property
    def image_import_project_name(self) -> str:
        """The CodeBuild project that imports workflow images into the tenant repository."""
        return f"{self.build_project_prefix}image-import"

    def image_build_project_name(self, arch: str) -> str:
        """The CodeBuild project that builds workflow images from their recipes for `arch`
        (`amd64` or `arm64`)."""
        return f"{self.build_project_prefix}image-build-{arch}"

    @property
    def runtime_build_project_name(self) -> str:
        """The CodeBuild project that builds coordinator variants (base image + engine runtime)."""
        return f"{self.build_project_prefix}runtime-build"

    def queue_name(self, qc: QueueClass) -> str:
        """The physical Batch queue name for a queue-class — the ONE source of this formula.

        Both ComputeStack (which creates the queues) and app_graph (which passes coordinator queue
        names to the ingest Lambda) must agree on this, or the Lambda's worker-vs-coordinator
        classification silently breaks.
        """
        return f"{self.queue_name_prefix}{qc.name}"

    @property
    def queue_name_prefix(self) -> str:
        """What every physical Batch queue name starts with ([`queue_name`] = prefix + class)."""
        return f"biffix-{self.tenant}-"
