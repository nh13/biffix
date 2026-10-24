# Deploying biffix

This guide takes a new operator from an empty AWS account to a working deployment. It covers the
common path and links to [infra/README.md](../infra/README.md), the full operational reference,
for the details.

A deployment is one **tenant** in one AWS account. Everything it creates is named
`biffix-<tenant>-…`, and the deploy and teardown scripts refuse to act on any account but the one
the config names. To run several isolated environments (say, staging and production), use one
account and one config file for each.

## Prerequisites

**An AWS account** you can deploy into with administrator rights, in a region with AWS Batch,
Aurora Serverless v2 and Cognito. If the account belongs to an AWS Organization, deploy into a
member account: the scripts always refuse the organization's management account.

**Tools.** What you need depends on whether you deploy a published release or build from source:

| Tool | From a release | From source |
|---|---|---|
| AWS CLI v2, with credentials for the target account | yes | yes |
| [uv](https://docs.astral.sh/uv/) (Python for the CDK app) | yes | yes |
| Node.js 24 and npm (the CDK CLI, and the UI build from source) | yes | yes |
| [crane](https://github.com/google/go-containerregistry) (copies the release's coordinator image) | yes | no |
| Rust (the toolchain in `rust-toolchain.toml`) and [cargo-lambda](https://www.cargo-lambda.info/) | no | yes |
| The `biffix` CLI on `PATH`, for the post-deploy health check | recommended | built for you |

Install the infra dependencies once:

```sh
cd infra
npm ci       # the CDK CLI, vendored in infra/node_modules
uv sync      # the CDK app's Python dependencies
```

## One-time account setup

Bootstrap CDK in the account and region:

```sh
cd infra
./node_modules/.bin/cdk bootstrap aws://<account-id>/<region>
```

That is all a release deploy needs. If you will build the coordinator image yourself from a fork
of the repository through GitHub Actions, the account also needs a GitHub OIDC identity provider,
which CDK references but does not create. See "Automated steps" in
[infra/README.md](../infra/README.md#automated-steps--building-the-coordinatorworker-image-ci) for
the command and the repository variables the image build reads.

## Write a deploy config

One YAML file describes a deployment. Start from the commented example, which documents every
key with its default:

```sh
cp infra/biffix-deploy.example.yaml infra/biffix-deploy.yaml
```

`infra/biffix-deploy.yaml` and `infra/biffix-deploy-<name>.yaml` are gitignored, since a real
config names your account, buckets and domain. There is no default config: every script and CDK
command names the file it acts on. The loader refuses keys it does not know, so a misspelled key
fails the deploy instead of being ignored.

The keys, grouped as in [the example](../infra/biffix-deploy.example.yaml):

- **Tenancy.** `tenant` (required; names every resource: lowercase letters, digits and `-`, at
  most 32 characters, not starting with `biffix-`), `region` (required), and `account` (the
  12-digit account id; quote it).
- **Network.** `nat_mode` chooses a NAT instance (`instance`, cheap, a single point of failure)
  or a managed NAT gateway (`gateway`, for production). `worker_egress: ecr_s3_only` moves task
  workers to subnets with no internet route, reaching only ECR, ECS, CloudWatch Logs and S3
  through VPC endpoints.
- **Compute.** `queue_classes` (required) is the menu of AWS Batch compute environments and
  queues workflows can use. Exactly one entry has `coordinator: true`, and it must be `ON_DEMAND`
  (synth refuses a spot coordinator). Each
  entry sets `capacity` (`SPOT` or `ON_DEMAND`), `instances` (family keys or exact instance types),
  `max_vcpus` (its spend ceiling), and optionally `ebs_gb`, `ebs_throughput`, `ebs_iops` and
  `imds_hop_limit`. `instance_families` overrides what a family key such as `c` means.
  `coordinator_image_tag` names the coordinator base image; leave it unset when deploying a
  release. `host_aws_cli` overrides the pinned AWS CLI every Batch host installs.
- **Projects.** `projects` (required) lists the isolation boundaries. Each has an `id`, and
  optionally `key_arn` (your own KMS key), `bucket_name` (an existing bucket to use instead of
  creating one), `uploads_cors_applied` and `viewer_cors_applied` (for an existing bucket, once its
  owner has applied the CORS rules the deploy prints), and `data_sources` (other S3 locations the
  project's runs may read, each with `id`, `name`, `bucket`, `prefix`, `description`, `kind`,
  `key_arn` and `viewer_cors_applied`).
- **Data.** `reference_key_arn` (your own key for the shared reference bucket); `retention`
  (`intermediates_days`, `snakemake_cache_days`, `noncurrent_versions_days`); `image_archive`
  (the storage tiers, optional S3 Object Lock and key for the archive of every pinned workflow
  image).
- **Identity.** `authorization.authenticated_default_role` (the role of a signed-in user no group
  grants one; `viewer` by default), `cognito_mfa`, `cognito_domain_prefix` (the hosted sign-in
  domain, needed for browser and CLI sign-in), and `google_oidc` (Google Workspace sign-in).
- **Registration.** `registration.github_subjects` lists GitHub Actions OIDC subjects (your
  workflow repositories) allowed to assume a role that pushes images to the deployment's workflow
  ECR repository. Each is a `repo:<owner>/<repository>:<context>` string; a wildcard may widen the
  context but never the owner or repository, and anything else fails the load.
  `registration.biffix_image_build` trusts your fork's coordinator image build.
- **Persistence.** `aurora_backup_retention_days` for the event store, and `aurora_key_arn` (your
  own KMS key for it and its secrets; set it at the first deploy, since a change replaces the
  cluster).
- **Alarms.** `alert_email` receives the operational alarms (more recipients can be added in the
  UI under Settings).
- **Edge.** `app_domain` is the app's hostname; `app_certificate_arn` is its certificate if you
  issued one yourself.

A minimal config for a first deployment sets `tenant`, `region`, `account`, `queue_classes` (the
example's three entries are a reasonable start), one project, and `cognito_domain_prefix`.

## Deploy

`infra/deploy.sh` runs the whole flow: it checks the target account, builds or fetches the
artifacts, deploys every stack with CDK, protects the deployed coordinator image from ECR
cleanup, builds the coordinator variants a new coordinator image needs (waiting up to 30 minutes
for them, so that once they are built the first runs after the deploy use it; if a build fails or
the wait times out, the deploy only warns, and runs launch on the previous image's variant
meanwhile), and ends with `biffix health` against the new API. It is idempotent, so rerunning it after a failure is safe. `infra/deploy.sh --help` lists
every flag.

### From a release (recommended)

A release is the four Lambda bundles, the UI build and a multi-architecture coordinator image,
built once by CI from a tagged commit and checksummed. Deploy it from a checkout of the same tag,
so the infra code matches the artifacts:

```sh
git checkout v0.1.0
infra/deploy.sh --config biffix-deploy.yaml --release v0.1.0
```

The script downloads the release, verifies each file against its `SHA256SUMS`, copies the
coordinator image by digest into the tenant's own ECR repository, and deploys. No Rust, Node build
or Docker is involved. Leave `coordinator_image_tag` unset: its default is the release tag.

To deploy from a mirror or a fork's releases, set `BIFFIX_RELEASE_BASE_URL`.

### From source

Without `--release`, `deploy.sh` builds the UI and the four Rust Lambdas from your working tree.
Use this to run changes that are not released. Two things differ:

- **The coordinator image is not built by `deploy.sh`.** Build it with the
  `build-coordinator-image` GitHub Actions workflow on your fork, or with
  `docker/coordinator/build-and-push.sh` from a machine with Docker buildx, then set
  `coordinator_image_tag` to the tag it prints. Tags are immutable; a new image means a new tag.
  The first deploy creates the ECR repository, so it can run before the image exists, but no run
  can launch until the tag names an image there.
- **Do not edit `crates/` or `ui/` while a deploy is running.** The deploy builds from the working
  tree, and the CDK app refuses a Lambda bundle older than its source.

```sh
infra/deploy.sh --config biffix-deploy.yaml
```

### First sign-in and the first admin

After the first deploy, open the app (its URL is the CloudFront domain, or `app_domain`) and sign
in once. Every user starts with the role `authenticated_default_role` gives them (Viewer by
default): a role otherwise comes from Cognito group membership, and a fresh pool has no members
in any group. Add yourself to `biffix-admins` with the AWS CLI; "Roles & the first-admin
bootstrap" in [infra/README.md](../infra/README.md#roles--the-first-admin-bootstrap) has the
commands. Sign out and in again so the new token carries the group.

The four groups are cumulative:

| Cognito group | Role | Adds |
|---|---|---|
| `biffix-viewers` | Viewer | view runs, results and workflows |
| `biffix-operators` | Operator | launch runs, manage experiments |
| `biffix-developers` | Developer | register and release workflow versions, edit the site title and logo, manage alert recipients |
| `biffix-admins` | Admin | manage reference bundles and the support contact |

Grant the least role that unblocks someone. Users and groups are managed in Cognito or your
identity provider, never in biffix.

## Custom domain and certificate

Without `app_domain`, the app is served on CloudFront's `*.cloudfront.net` domain, and sign-in
accepts only a local development callback. Set `app_domain` for any deployment people use.

When `app_domain` is set and `app_certificate_arn` is not, `deploy.sh` requests a DNS-validated
ACM certificate in `us-east-1` (CloudFront requires that region), prints the validation record,
waits for the certificate to be issued, and attaches the domain to CloudFront in the same run. You
add two records at your DNS provider: the ACM validation CNAME, and `app_domain` pointing at the
CloudFront distribution's domain. CloudFront refuses the alias while the name still points at a
different distribution, so repoint that record first when moving a domain.

If you issue the certificate yourself, put its ARN in `app_certificate_arn`.

## Sign-in: Cognito and Google Workspace

By default, users are Cognito users with a username and password, created in the Cognito console
or with the AWS CLI. `cognito_mfa` controls TOTP multi-factor sign-in (`optional` by default).

To use Google Workspace instead, create an OAuth client in the Google Cloud console and set:

```yaml
cognito_domain_prefix: biffix-acme
google_oidc:
  client_id: 1234567890-abc.apps.googleusercontent.com
```

Google then becomes the only sign-in method. The client secret never goes in the config:
`deploy.sh` asks for it once and stores it in Secrets Manager (or reads
`BIFFIX_GOOGLE_CLIENT_SECRET` in CI). Add the redirect URI the identity stack prints as its
`GoogleAuthorizedRedirectUri` output to the Google client's authorized redirect URIs. That URI is
derived from `cognito_domain_prefix`, so keep the prefix stable. Google users have no Cognito
groups until you add them, so they get `authenticated_default_role` until then.

## Projects and data sources

Each project has its own bucket, KMS key, run role, coordinator role and log group, and a run can
reach only its own project's data. Use separate projects where data must be kept apart; use one
project where a team shares data.

- **Buckets.** By default biffix creates the project's bucket. To use an existing one, set
  `bucket_name`; biffix grants access but never changes it, so the deploy prints the CORS and
  lifecycle rules for the bucket's owner to apply (needed for browser uploads and the alignment
  viewer). After applying them, set `uploads_cors_applied` and `viewer_cors_applied` and redeploy.
- **Data sources** are read-only S3 locations a project's runs may take inputs from, such as a
  sequencing core's delivery bucket or a reference collection. They appear by name in the file
  picker. A bucket in another account also needs a bucket policy there allowing the project's
  roles.

Adding a project to a deployment that already has workflow versions needs care, because a
version is tied to a project when registered. Read "Projects and request routing" in
[infra/README.md](../infra/README.md#projects-and-request-routing-projects) first.

## Upgrading

To move to a new release, check out its tag and deploy it:

```sh
git checkout v0.2.0
infra/deploy.sh --config biffix-deploy.yaml --release v0.2.0
```

Database migrations apply themselves: every biffix process (the API, sweep, ingest and reconcile
Lambdas, and the local server) runs any pending ones when it starts. A new coordinator base image is picked up by
the backend sweep within a few minutes, which rebuilds the engine runtimes registered versions
use; runs keep launching on the previous image until then.

Upgrading can change the engine pins (the Snakemake or Nextflow releases and plugin versions
biffix builds runtimes from). Existing versions keep the runtime they recorded. To move them to
the new pins, re-register them:

```sh
biffix workflows reregister --all --dry-run    # see what would change
biffix workflows reregister --all
```

Re-register workflow versions after deploying a backend that adds launch-form features, too: a
version registered against an older backend lacks the newer form fields.

A release that replaces the event store (for example, one that changes the database's encryption
key) cannot be applied in place. CloudFormation fails safely and rolls back the persistence stack,
leaving the tenant as it was. Tear the tenant down with `infra/teardown.sh` and deploy the release
fresh instead; `--keep-data` keeps the old Aurora cluster, but the new deploy does not reuse it.
The new tenant's event log starts empty: no runs, registered workflows, reference bundles, site
settings or alert recipients. Imported project buckets, the image archive and the settings held in
the deploy config survive.

## Tearing down

`infra/teardown.sh` is the inverse of `deploy.sh` for one tenant in one account. It clears
termination protection, deletes the retained Aurora cluster before destroying its stack, and
empties the versioned buckets, none of which a bare `cdk destroy` does.

```sh
infra/teardown.sh --config biffix-deploy.yaml --account <account-id>
infra/teardown.sh --config biffix-deploy.yaml --account <account-id> --keep-data   # keep Aurora and the buckets
```

`--keep-data` also keeps the persistence and network stacks, which the kept Aurora cluster needs.

The image archive bucket is kept unless you pass `--delete-image-archive`. An imported project
bucket is never touched. The script lists what stays by design (ECR repositories, the Cognito
pool, KMS keys). DNS records, the Google OAuth client and the CDK bootstrap stack are outside
biffix and stay until you remove them.

## Costs

biffix's fixed costs are small; most spend follows usage.

- **Compute** dominates: the EC2 instances AWS Batch starts for coordinators and tasks, plus their
  EBS volumes. Spot queue-classes cut task cost substantially. `max_vcpus` on each queue-class caps
  how much runs at once. Compute environments scale to zero when idle.
- **Network.** A NAT instance is about $4 a month, a NAT gateway about $32 a month plus data
  charges. `worker_egress: ecr_s3_only` adds roughly $90 a month in VPC interface endpoints.
- **Aurora Serverless v2** for the event store, which scales with load and pauses (0 ACU, storage
  only) after 5 minutes with no connection. The scheduled reconcile and sweep Lambdas run every 5
  minutes while a run is active or ended in the last 15 minutes, and only once an hour otherwise,
  and they close their connections after each invocation, so an idle tenant's database is paused
  most of the time. Each hourly wake costs a few minutes at 0.5 ACU: about $4 a month in
  `us-east-1`, instead of about $44 for a database that never pauses. An idle tenant's floor is
  then roughly the network and KMS lines here, Secrets Manager (cents per secret), and storage
  (Aurora, ECR, buckets): around $15 a month with the NAT instance.
- **KMS** customer-managed keys, about $1 a month each (one for the reference bucket, one per
  project, one for the image archive, one for the event store), unless you bring your own.
- **Storage**: run outputs and intermediates in project buckets (see `retention`), and the image
  archive, which moves to Glacier tiers by default.
- **Lambda, API Gateway, SQS, CloudFront and CodeBuild** are usually small.

The UI's Usage page and each run's cost tab show estimated compute cost from EC2 prices: the
deploy region's on-demand and gp3 list prices (checked in for `us-east-1`, read from the AWS
Pricing API in any other region) and EC2's spot price history. These are estimates, not a bill;
use AWS Cost Explorer and AWS Budgets for actual spend and limits.

## Troubleshooting

**Check health first.**

```sh
biffix health --url https://<app-domain>/api
```

It exits 0 only when the API's liveness and readiness checks pass, and shows component status
when you are signed in. `deploy.sh` runs it as its last step. The app's Status page shows the same
from the browser. Check real endpoints rather than the app's API docs page (`/api-docs`), whose
spec is bundled with the UI and can lag a deploy.

The component status comes from the reconcile Lambda's status probe, which samples each time the
Lambda runs in full: every 5 minutes while a run is active, but only once an hour on an idle tenant
(see Costs). A component is called stale, and the reconciler "not reporting", only once its sample
is 5 minutes past when the next was due: 10 minutes during a run, 65 when idle. So an idle tenant's
hour-old sample is healthy, and a fresh deploy with no run yet can say "awaiting first status
sample" until the top of the hour.

**Common failures:**

- **"Rust Lambda artifact for … is STALE"** (from source). A Lambda bundle is older than its
  source. Rebuild all four Lambdas (`deploy.sh` does this unless `--skip-lambdas`); build them with
  `cargo lambda build`, not `cargo build`.
- **Wrong account.** `deploy.sh` and `teardown.sh` stop when the caller's account is not the
  config's `account` or `--account`. Assume the right role (`--assume-role`) rather than editing
  the config.
- **Runs never launch.** `coordinator_image_tag` names an image that is not in the tenant's
  coordinator repository. Check the tag, or deploy a release, which imports its image.
- **A version is refused at submit with a build log.** Its engine runtime or one of its images is
  still building or failed. The workflow's page in the UI, or
  `biffix workflows show <id> --output json` (its `state`, `not_ready`, `runtime` and `images`),
  shows what is missing.
  A build is started at most three times automatically (the first and two retries); after that,
  a Developer starts it again with `POST /workflows/{id}/runtime/rebuild` or
  `POST /workflows/{id}/images/{name}/rebuild`.
- **Google sign-in fails with `redirect_uri_mismatch`.** The Google client lacks the identity
  stack's `GoogleAuthorizedRedirectUri`. This also happens after changing `cognito_domain_prefix`
  or moving to a new account.
- **Every user is a Viewer.** Expected on a fresh deployment; add users to Cognito groups.
- **Uploads or the alignment viewer do not work on an imported bucket.** The bucket lacks the CORS
  rules the deploy printed, or the config does not yet say they are applied.
- **Events stop arriving.** Operational alarms (dead-letter queue, ingest lag, reconcile stopped,
  API errors, a failing sweep) email `alert_email` and the recipients added under Settings.

The "Gotchas" section of [infra/README.md](../infra/README.md#gotchas-in-one-place) collects the
rest.
