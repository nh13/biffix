# Security policy

## Reporting a vulnerability

Report vulnerabilities privately through GitHub's private vulnerability reporting: on
[github.com/nh13/biffix](https://github.com/nh13/biffix), open the **Security** tab and choose
**Report a vulnerability**
([direct link](https://github.com/nh13/biffix/security/advisories/new)). Do not open a public
issue, pull request or discussion for a suspected vulnerability.

Please include:

- the biffix version (release tag or commit) and the component affected (backend API, ingest,
  CLI, UI, coordinator image, an engine plugin, or the CDK app);
- what an attacker can do, and what access they need to start with (none, a signed-in Viewer, an
  Operator, a workflow author, code running inside a task);
- steps to reproduce, or a proof of concept against your own deployment.

The maintainer aims to acknowledge a report within a week. biffix has one maintainer, so a fix
can take longer; you will be kept informed, and credited in the advisory unless you prefer
otherwise. Please allow a reasonable window for a fix and a release before disclosing publicly.

## Supported versions

Only the latest release receives security fixes. Upgrade to it before reporting, or say in the
report that you could not.

## Scope

biffix is self-hosted. Each deployment runs in the operator's own AWS account, and the operator is
responsible for that account's security: who holds administrator access, CloudTrail and other
audit logging, the Cognito user pool's membership and MFA policy, the identity provider, network
egress (`worker_egress`), and which workflows and container images they register and run.

What biffix itself is responsible for, and where a report is in scope:

- **Project isolation through IAM.** Each project has its own S3 bucket, KMS key, run role and
  coordinator role. A run's tasks run as the project's run role, and the coordinator may submit,
  tag and terminate only Batch jobs tagged with its own project and pass only its own project's
  roles. A way for one project's run to read or write another project's data, or act as another
  project's role, is in scope.
- **Capability tokens for engine events.** The engine plugins post events with a per-run token
  that is signed, expires, and authorizes event posts for that one run only. A token that is
  accepted for another run, after expiry, or for anything but event posts is in scope, as is a way
  to make another run's events or AWS facts attach to yours.
- **Authentication and roles.** The HTTP API is behind a Cognito JWT authorizer. A user's role
  (Viewer, Operator, Developer, Admin) comes from their Cognito group membership; a signed-in user
  in no mapped group gets the deployment's `authorization.authenticated_default_role` (default
  `viewer`), so the operator chooses what an ungrouped identity can do. The only routes open
  without a sign-in are the health endpoints (`/healthz`, `/readyz`), the OAuth bootstrap
  (`/auth/config`) and event posts (which need a capability token instead). An action allowed to
  a role that should not have it, or access to any other route without a valid sign-in or
  capability token, is in scope.
- **Secrets.** No AWS credentials are passed into a run, and parameters marked secret are redacted
  before they reach the event log. A secret that ends up in the log, a projection, a provenance
  export or a container's environment is in scope.
- **Images and sources.** Tasks run only images pinned at registration and copied into the
  tenant's ECR; a task naming an unmapped image fails. A way to make a run pull an image or fetch
  a workflow source other than the one its version pinned is in scope.

Out of scope: a registered workflow doing what its own code says (a workflow author's code runs in
the coordinator and the tasks with the project's permissions, by design); misconfiguration of the
operator's AWS account or identity provider; denial of service through the AWS account's own
quotas or cost; and issues in third-party dependencies that biffix does not expose, which belong
with their upstream projects.

biffix is designed not to rule out a deployment that handles regulated data, but it does not make
one compliant; see [docs/architecture.md](docs/architecture.md#ready-for-regulated-data).
