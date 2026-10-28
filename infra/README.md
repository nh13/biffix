# infra — biffix CDK

The biffix control plane deploys as a CDK (Python) app. **One deploy = one tenant = one AWS
account** (`pyproject.toml`, `biffix_infra/config/`). For how the pieces fit, see
[docs/architecture.md](../docs/architecture.md); for a guided first deploy, see
[docs/deploying.md](../docs/deploying.md).

This README is the operational deployment reference. It covers the **automated** steps (the CI
image build) and the **manual** steps (building artifacts and running `cdk deploy`), in the order
you run them, plus the guardrails the deploy enforces and the ones you enforce yourself.

There are two ways to deploy. **From a release** (`infra/deploy.sh --config <file> --release
vX.Y.Z` from a checkout of the `vX.Y.Z` tag, see **Releases**) needs no Rust or Docker toolchain,
no UI build and no image build of your own: it deploys the published Lambdas, UI and coordinator
image. It still needs Node for the CDK CLI (`npm ci` in `infra/`). **From source** (`infra/deploy.sh` without
`--release`) builds everything from your working tree and needs the image build below; it is the
path for contributors and for running changes that are not released.

## What gets deployed

`app.py` synthesizes these stacks, deployed in dependency order (CDK infers the order from
cross-stack resource references — there are no manual `add_dependency` calls in `app.py`):

| Stack | Purpose |
|---|---|
| `biffix-<tenant>-network` | VPC, subnets, NAT (see **Network posture**) |
| `biffix-<tenant>-data` | Shared reference S3 bucket and its KMS key, the image archive bucket and its KMS key, ECR repos (coordinator, workflow, builds) |
| `biffix-<tenant>-identity` | Cognito user pool + clients, the permissions boundary, the GitHub-OIDC roles |
| `biffix-<tenant>-compute` | AWS Batch compute environments + queues (with the host launch template) |
| `biffix-<tenant>-build` | The CodeBuild projects: `runtime-build` (coordinator variants: base image + engine runtime), `image-import` (import, probe, derive and archive workflow images), and `image-build-amd64` / `image-build-arm64` (build workflow images from a Dockerfile or pixi recipe) |
| `biffix-<tenant>-project-<id>` | Per-project S3 bucket, KMS key, run role (one per `projects[]` entry) |
| `biffix-<tenant>-persistence` | Aurora (event store) + the capability-token signing secret |
| `biffix-<tenant>-ingest` | Ingest SQS queue + DLQ, the ingest + reconcile Rust Lambdas, the reconcile schedule |
| `biffix-<tenant>-api` | The backend Rust Lambda, HTTP API + Cognito authorizer, one coordinator role + job definition template per project (`biffix-<tenant>-coordinator-<project>`), each project's SSM context parameter (`/biffix/<tenant>/projects/<id>`), the scheduled backend sweep (run provenance crates, image and runtime builds and their retries, launching runs parked while their images are prepared) |
| `biffix-<tenant>-edge` | CloudFront + the private UI bucket |

`<tenant>` comes from `tenant:` in the deploy config and is prefixed with `biffix-` for you — so
`tenant: dev` yields `biffix-dev-*`. Do **not** put `biffix-` in `tenant` or names double up.

## Tooling prerequisites

- **Node + the CDK CLI** — vendored at `infra/node_modules`; invoke as `./node_modules/.bin/cdk`
  (run `npm ci` in `infra/` once). `cdk.json` runs the app via `uv run python app.py`.
- **uv** — the Python deps are managed with uv; run `uv sync` in `infra/` once.
- **Rust + `cargo-lambda`** — `cargo install cargo-lambda` (once) to build the Lambda bootstraps.
  Not needed with `--release`.
- **`crane`** ([go-containerregistry](https://github.com/google/go-containerregistry), e.g.
  `brew install crane`) — only with `--release`, which copies the coordinator image with it.
- **Docker with buildx** — only for the *local* image-build fallback; the CI path needs no local Docker.
- **AWS credentials for the target account** — see **Targeting the right account** below.

### One-time per account+region

```sh
./node_modules/.bin/cdk bootstrap aws://<account-id>/<region>
```

**GitHub OIDC provider (only if you use the CI image build).** The image-build role's trust
*references* the account's GitHub OIDC provider, but the CDK app does **not** create it — it is an
account-level prerequisite you create once. Without it, the CI build fails at assume-role with *"the
web identity token could not be validated."*

```sh
aws iam create-open-id-connect-provider \
  --url https://token.actions.githubusercontent.com \
  --client-id-list sts.amazonaws.com \
  --thumbprint-list 6938fd4d98bab03faadb97b34396831e3780aea1 1c58a3a8518e8759bf075b76b750d4f2df264fca
```

## Automated steps — building the coordinator/worker image (CI)

A release deploy skips this section: the release workflow publishes the image and
`deploy.sh --release` imports it (see **Releases**). Build the image yourself, as below, to deploy
unreleased changes to it, from a fork whose CI can push into your account.

The coordinator image also runs a Snakemake version's tasks when it has no image of its own (the
Dockerfile carries no `ENTRYPOINT`; the Batch job definition picks the role). It is built **in CI, not on a laptop**, for two reasons
baked into `.github/workflows/build-coordinator-image.yml`:

1. The fleet runs **both** x86 and Graviton instance families (the defaults include `c6i`, `m6i`
   and `c7g`; see `instance_families`). CI builds each arch
   natively (`ubuntu-24.04` + `ubuntu-24.04-arm`) and assembles a **manifest list**. A laptop
   `--platform linux/amd64` build silently makes every Graviton queue-class unusable.
2. The image is built from a known commit, and **the tag is that commit SHA**. The coordinator ECR
   repo is configured **immutable**, so a run always resolves the same bytes and a rebuild of the
   same commit fails loudly instead of silently replacing what the fleet executes.

**Credentials are keyless.** The workflow mints a short-lived GitHub OIDC token; AWS returns ~1h
credentials. Nothing is stored in repository secrets. The role it assumes comes from the repo
variable `AWS_IMAGE_BUILD_ROLE_ARN`, and its trust is provisioned from
`registration.biffix_image_build` in the deploy config (a `subject` scoped to the `image-build`
environment, pinned to one branch, `ref`, default `refs/heads/main`). Only that branch can push;
dispatching from another fails at credential minting. This is deliberately **separate** from `registration.github_subjects`, which
trusts third-party *workflow* repos, not the biffix image build.

**Prerequisites for the build to succeed:**

- The **account-level GitHub OIDC provider must exist** (see the one-time section above) — the app
  references it but does not create it.
- The deploy config must set **`registration.biffix_image_build.subject`** (and optionally `ref`,
  the one branch that may push; default `refs/heads/main`).
  After deploying the identity stack, set the repo variable `AWS_IMAGE_BUILD_ROLE_ARN` to the created
  `ImageBuildRole`'s ARN — a stale ARN (e.g. from a torn-down deploy) fails with `NoSuchEntity`.
- Set the repo variables **`AWS_REGION`** (the config's `region`) and **`BIFFIX_TENANT`** (its
  `tenant`). The workflow finds the coordinator repository with `docker/coordinator/resolve-repo.sh`:
  the one repository named `biffix-<tenant>-data-coordinatorrepo*`. If a retained repository from an
  earlier deploy also matches, the build stops; set **`COORDINATOR_REPO`** to the exact name of the
  data stack's `CoordinatorRepo` instead (it wins over `BIFFIX_TENANT`). The workflow stops at its
  first step when these are missing.
- **Immutable OIDC subjects.** If the repo/org has GitHub's immutable subject enabled
  (`gh api repos/<org>/<repo>/actions/oidc/customization/sub` shows `use_immutable_subject: true`),
  the token's `sub` carries numeric IDs — `repo:<org>@<org_id>/<repo>@<repo_id>:environment:<env>` —
  so `registration.biffix_image_build.subject` must use that exact immutable form, or assume-role
  fails with *"Not authorized"*. The `sub_claim_prefix` from that API call is the prefix to use.
- The workflow **builds the biffix engine plugins** before the docker build: the `nf-biffix`
  Nextflow plugin (gradle), then `docker/coordinator/build-wheels.sh` collects it with the Snakemake
  executor fork (at the sha in `docker/coordinator/wheels.env`) and logger wheels into the image's
  `/opt/biffix/wheels/`. None of these artifacts is committed.
- The image is the **base** coordinator: it has no Snakemake or Nextflow. A workflow version pins
  its engine release, and the backend builds that release into a runtime layer appended to this
  base (the `runtime-build` CodeBuild project, `codebuild/runtime/build.sh`), one coordinator
  variant + job definition per (runtime, base). A new base's variants are built by `deploy.sh`
  right after `cdk deploy` (through the backend sweep, which rebuilds the variants registered
  versions use); runs keep the previous variant until they are ready.

**To ship a new image:**

1. Run the **build-coordinator-image** workflow (`workflow_dispatch`, on `main`).
2. It prints the tag to its job summary. Set it in your deploy config (config is data, not code —
   never a source edit):
   ```yaml
   coordinator_image_tag: <sha>
   ```
3. Deploy (`infra/deploy.sh --config <file>`): the api stack's coordinator template and the
   backend's `BIFFIX_COORDINATOR_IMAGE` pick up the new base.

Left unset (or `null`), `coordinator_image_tag` is this checkout's release tag, `v<version>` with
the version from the Cargo workspace (`Cargo.toml`, `[workspace.package]`): the tag
`deploy.sh --release` imports the published image under. A deploy from source that builds its own
image sets the pushed tag instead. Either way, the first deploy (which creates the coordinator
repository) goes ahead before the image exists, and runs cannot launch until the tag names an image
in the repository.

### Engine runtimes and coordinator variants

A run does not use the base image directly. Each workflow version pins its engine release
(`engine_version`), and the backend builds that release into a runtime layer on the deployed base:
the **coordinator variant**. What to know when operating it:

- **Where they are.** The `biffix-<tenant>-runtime-build` CodeBuild project (build stack) runs
  `codebuild/runtime/build.sh`. It installs each runtime once, as layer-only images
  `runtime-<runtime key12>-{amd64,arm64}` in the coordinator repo (keep-tagged and archived like
  workflow images), and appends that stored layer to the base: variants are
  `coordinator-<runtime key12>-<base digest12>` (a two-arch index), plus the `-amd64`/`-arm64`
  images. A new base therefore reuses the layer (the build log says "already pushed") and takes
  about a minute. Each variant has a Batch job definition `biffix-<tenant>-coordinator-rt-<key12>-<base12>`,
  cloned from the `biffix-<tenant>-coordinator` template. Resolved Snakemake package locks are kept
  at `s3://<reference bucket>/runtimes/<key>/`.
- **Nextflow plugins.** The coordinator runs Nextflow offline, so a pipeline's own plugins (its
  `nextflow.config` `plugins {}`, e.g. nf-core's `nf-schema`) must be in its runtime. They are
  listed in `biffix.yaml` as `plugins: [nf-schema@2.7.2]` (`biffix inspect --write` fills it in);
  `register` pins each by the sha256 of its registry zip, and they become part of the runtime
  recipe, so versions with the same plugins share a runtime. Versions must be exact; nf-amazon,
  nf-cloudcache and nf-biffix come with biffix's runtime and are not listed. For a runtime built
  with plugins (the layer lists them in `/opt/biffix/nf/workflow-plugins`), the coordinator
  enables nf-amazon and nf-biffix through `NXF_PLUGINS_DEFAULT`, leaving the pipeline's own
  `plugins {}` in force. A runtime with no workflow plugins loads nf-biffix from a `plugins {}`
  block of the coordinator's `-c` config, which replaces the pipeline's own list. A runtime build fails if a plugin
  can't load offline (for example, it needs a newer Nextflow). A workflow plugin id may not start
  with `nf-amazon-`, `nf-cloudcache-` or `nf-biffix-`.
- **When they build.** The first registration of an engine release starts its build (a few
  minutes); later versions pinning the same release reuse it. A new base image (deploy with a new
  `coordinator_image_tag`) needs a variant for each runtime in use. A full `deploy.sh` (not
  `--stack`) builds them right after `cdk deploy`: it invokes the backend sweep Lambda with
  `{"sweep": "variants"}` (the variants sweep alone, returning its counts), every 30 s until none
  is building, and gives up waiting after `BIFFIX_PREBUILD_TIMEOUT_SECS` (default 1800). A
  failure or a timeout only warns. Otherwise the backend sweep picks the new base up (every 5 min,
  or within the hour while no run is active), as does the first submit of a version. Runs keep
  launching on the previous variant until then.
- **Failures.** A version whose variant is building or failed is refused at submit (409 with the
  build log). A build is started at most three times without a person asking (the first build and
  two retries); after that, `POST /workflows/{id}/runtime/rebuild` (Developer) builds it again.
  `GET /workflows/{id}` shows the state under `runtime`, and `biffix workflows register` waits for
  it by default (`--wait-minutes N`, or `--no-wait` to return at once).
- **Upgrading biffix** can change the pin tables
  (`crates/biffix-workflow/src/runtime/pins.rs`). Existing versions keep their recorded recipe.
  Re-registering gives a new build with the new pins.
  `biffix workflows reregister --all --dry-run` lists what re-registering every workflow's newest
  version would do; drop `--dry-run` to do it (`reregister <name>` for one, with
  `--engine-version 9.27.0` for a version that recorded only a major release). It reuses the
  version's recorded form, About page, source and images (tags are pinned again); register a
  checkout instead to derive the form again.
- **Retired releases** (`RETIRED` in `crates/biffix-workflow/src/engines.rs`; today Nextflow
  25.04.0) are refused by name at registration, and a version already registered on one is refused at
  submit and launch ("Nextflow 25.04.0 is no longer supported; re-register on 25.10.4"), reads
  `failed`, and gets no new variant builds. Move it with
  `biffix workflows reregister <name> --engine-version 25.10.4`.
- **Retention.** Variant tags are not build tags, so `deploy.sh`'s retention step never prunes
  them. Old variants and their job definitions are not cleaned up yet.

### Workflow images: import, keep-tags, archive, restore

Every image a workflow version declares (`image:` / `images:` in `biffix.yaml`, or `--image`) is
pinned to a digest at registration and imported into the tenant workflow repository by the
`biffix-<tenant>-image-import` CodeBuild project (`codebuild/image/import.sh`). A version runs only
once its images are imported (`GET /workflows/{id}` shows `state` and each image's import);
`biffix workflows register` waits for that by default.

- **Tags.** An import tags the image `import-<key12>` and `biffix-keep-<digest12>` (each image an
  index lists `biffix-keep-<digest12>-<n>`). The repository's lifecycle only expires untagged
  images and the retention step never touches `biffix-*` tags, so a pinned image is kept. The
  tags biffix writes in the workflow repository (`biffix-keep-*`, `import-*`, `derived-*`,
  `restore-*`) are immutable there, so a push by anyone else cannot move a keep-tag off a pinned
  image; a tag someone else wrote first is reused only when it names what the step would produce.
  Every other tag stays mutable for workflow CI.
- **Probe.** An import lists each fleet platform's files without running the image: a bash, the C
  library, and the Snakemake and plugin versions installed. A version fails (`not_ready` says why)
  when an image lacks a bash, or breaks the Snakemake rules below.
- **Snakemake images are derived.** A Snakemake task runs `python -m snakemake` inside its image,
  so each image of a Snakemake version gets the version's runtime layer: the same project, in derive
  mode, appends the stored `runtime-<key12>-<arch>` layer to the imported image and pushes
  `derived-<key12>` (+`-amd64`/`-arm64`) into the workflow repository, keep-tagged and archived. Its
  entrypoint becomes `<the image's own entrypoint…> /opt/biffix/shim-exec`, which puts
  `/opt/biffix/shim` first on `PATH` after the image's own entrypoint has set up its environment
  (BioContainers' resets `PATH`): the shim's `python`/`python3` send `-m snakemake` to the pinned
  runtime and everything else to the image's own Python. Tasks run the derived image; a run's
  `run_submitted` records it as `image_digest` and the imported one as `image_source_digest`. The
  image must be glibc-based. One gap: a rule's `script:` runs under the runtime's Python, not the
  image's (use `shell:` to call the image's). A derive waits for the runtime's build, so a new
  engine release takes one runtime build plus one derive per image before its first run.
- **`runtime: own`.** An image that carries its own Snakemake (`images: {x: {ref: …, runtime:
  own}}`) is not derived, and must carry exactly the pinned Snakemake release and
  `snakemake-storage-plugin-s3` (what a task loads). The logger and executor plugins run in the
  coordinator only and are just reported.
- **Built images.** An image can instead be built from a recipe the workflow ships:
  `images: {align: {dockerfile: docker/align/Dockerfile}}` (context: the Dockerfile's directory, or
  `context:`; `archs: [amd64]` when its base has only one architecture) or
  `images: {default: {pixi: env}}` (a directory holding `pixi.toml` + `pixi.lock`; built for the
  Linux platforms the `pixi.toml` lists). `biffix workflows register` tars the context's
  **git-tracked** files (a pixi environment: exactly those two files) and stages the tarball beside
  the source (`workflow-sources/<version-id>/contexts/<tree hash>.tar.gz`), whatever the source mode.
  The backend then starts one build per architecture in the privileged
  `biffix-<tenant>-image-build-amd64`/`-arm64` projects (`codebuild/image/build.sh`, native
  `docker buildx`, pushed as `build-<key12>-<arch>`), and the import project's index step joins
  them as `build-<key12>` and keep-tags, probes and archives it like an import; from there a built
  image is derived (Snakemake) and run like an imported one. Built images (and their derives) live
  in the data stack's **builds repository**, not the workflow repository: only the image-build and
  import projects can push to it (the registration role third-party workflow CI assumes cannot, nor
  even pull), so a build may reuse a `build-*` tag it finds there. Every tag there is immutable
  except `scratch-*`; it keeps images like the workflow repository (untagged images expire, keep
  tags hold what a version pins). The backend trusts a built image only from there
  (`BIFFIX_BUILDS_IMAGE_REPO`). A pixi image is built with biffix's
  generated Dockerfile (pinned pixi and Ubuntu images, `pixi install --locked`; no AWS CLI, no
  Snakemake), shown on the workflow page. The build key covers the files' content, the recipe
  version and the architectures, so the same recipe builds once per tenant. A task names a built
  image by its `images:` name: `aws_batch_container_image="align"`, `container 'align'`.
- **Failures.** An import, build or derive is started at most three times without a person asking
  (the first and two retries); after that, `POST /workflows/{id}/images/{name}/rebuild` (Developer) imports, builds (only the failed
  architecture or index) or derives it again.
- **Archive.** Each import also writes the image, byte for byte, as an OCI layout into the data
  stack's image archive bucket: `blobs/sha256/<hex>` (stored once however many images share it) and
  `images/<digest>/{index.json,oci-layout}`. Blobs larger than 1 MiB move to Glacier Instant
  Retrieval at once and to Deep Archive after 90 days; tune it in the deploy config:
  ```yaml
  image_archive:
    instant_retrieval_after_days: 0     # null skips the tier
    deep_archive_after_days: 90         # null skips the tier
    min_tiered_object_bytes: 1048576
    object_lock: {mode: COMPLIANCE, days: 2555}   # only at the archive's first deploy
    key_arn: arn:aws:kms:…              # bring your own key (default: a biffix-managed CMK)
  ```
- **Restore** (an image was deleted or its tags lost; `--repo` is the repository the recorded
  image names: the workflow repository for an import or its derive, the builds repository for a
  built image or its derive):
  ```sh
  cd infra && uv run python -m biffix_infra.image_restore --tenant <tenant> \
    --digest sha256:… --repo <account>.dkr.ecr.<region>.amazonaws.com/<repository>
  ```
  With ops credentials for the tenant account and `crane` on PATH (log in to the registry first:
  `aws ecr get-login-password | crane auth login <registry> -u AWS --password-stdin`). A blob in
  Deep Archive is restored first: the run requests it, exits 2, and is re-run once it is readable
  (hours; `--tier Bulk` is cheaper and slower). The pushed image must have the archived digest, and
  is keep-tagged again. It reads the live data stack's archive; after a teardown and a rebuild that
  is a new, empty bucket, so add `--bucket <the kept archive>` (teardown prints its name).

### Manual fallback (local build)

`docker/coordinator/build-and-push.sh` builds and pushes from a laptop. Like CI it pushes one
linux/amd64 + linux/arm64 image (the non-native half under emulation, so it is slow), tagged with
the commit SHA of a clean tree by default (`IMAGE_TAG` overrides it). It reads the tenant and region
from the deploy config and pushes to that tenant data stack's own coordinator repo. Export AWS
credentials for the target account first, then set `coordinator_image_tag` to the printed tag:

```sh
BIFFIX_DEPLOY_CONFIG=biffix-deploy.yaml ./docker/coordinator/build-and-push.sh
```

## Releases

A release is everything a deploy needs, built once by CI from one tagged commit:
`.github/workflows/release.yml`, run by pushing a tag `v<version>`.

**What a release publishes.** A GitHub release of the tag, with these assets:

| Asset | What it is |
|---|---|
| `biffix-backend-lambda-x86_64.zip`, `biffix-backend-sweep-x86_64.zip`, `biffix-ingest-lambda-x86_64.zip`, `biffix-reconcile-lambda-x86_64.zip` | Each Rust Lambda's `bootstrap`, zipped by `cargo lambda build` with the same features `deploy.sh` builds it with. x86_64, the architecture the Lambdas run on (they set none, so Lambda's default). |
| `biffix-ui.tar.gz` | The UI's static build, against API types generated from the same commit (the workflow fails if they differ from the committed ones). |
| `release.json` | The manifest `biffix_infra/release.py` reads: version, commit, the Lambdas' architecture, and the coordinator image's registry name and index digest. |
| `SHA256SUMS` | `sha256sum` of every other asset. |

and the base coordinator image, linux/amd64 + linux/arm64 (each built on a native runner), at
`ghcr.io/<owner>/biffix-coordinator:<tag>`. The image and every asset carry a signed build
provenance attestation: `gh attestation verify <file> --repo <owner>/biffix`, or
`gh attestation verify oci://ghcr.io/<owner>/biffix-coordinator@<digest> --repo <owner>/biffix`.

**Cutting one.** Merge the release PR. `.github/workflows/publish.yml` keeps one open (release-plz,
configured in `release-plz.toml`) whenever a crate published to crates.io has changed since the last
release: it bumps the Cargo workspace version (`[workspace.package] version` in `Cargo.toml`) and
adds the release to `CHANGELOG.md`. Once it merges, the same workflow publishes the crates to
crates.io and pushes the tag `v<version>`, which runs `release.yml`. release-plz proposes a release
only for changes to those crates; to release a change elsewhere (the UI, the Lambdas, the infra),
open a PR that bumps the workspace version and merge it, and `publish.yml` does the rest. The tag
must be `v` + the workspace version; `release.yml` refuses any other, before building anything.

Run the workflow by hand (`workflow_dispatch`) for a dry run: it builds every artifact and the
images (without pushing them) and attaches the assets to the run as the `release` artifact, with no
coordinator image in `release.json`. Nothing is published.

**Repository setup (once).** `release.yml` publishes with the job's `GITHUB_TOKEN` (no secrets, no
cloud account). `publish.yml` publishes the crates by crates.io trusted publishing (each crate
trusts `publish.yml` in this repository) and needs one secret, `RELEASE_PLZ_TOKEN`: a fine-grained
token with *Contents* and *Pull requests* write on the repository, since a PR opened or a tag pushed
with `GITHUB_TOKEN` starts no workflow. For the engine plugins it also needs a PyPI trusted
publisher for `snakemake-logger-plugin-biffix` (workflow `publish.yml`, environment `pypi`) and an
`NPR_API_KEY` secret (a Nextflow plugin registry access token) for nf-biffix. A release also needs:

- **The package made public.** The first release creates the `biffix-coordinator` package on GHCR
  as private (GitHub's default for a new package); set it to public once (the package's
  settings, *Change visibility*), or deployers cannot pull it.
- **The `release` environment** (created on first use). Add required reviewers to it to gate every
  publish (the image tag and the GitHub release) on an approval, where the plan allows.
- **A tag ruleset** restricting who may create `v*` tags, since pushing one publishes a release.

**Deploying one.** From a checkout of the release's tag (`git checkout v0.2.0`), so the infra code
deploying the artifacts is the release's own:

```sh
infra/deploy.sh --config biffix-deploy.yaml --account <acct> --release v0.2.0
```

Step by step, `--release` replaces the source builds with:

1. **Fetch** (`python -m biffix_infra.release fetch`): download `SHA256SUMS`, then every asset, from
   `$BIFFIX_RELEASE_BASE_URL/<tag>/` (default: the GitHub releases of the repository in
   `Cargo.toml`'s `[workspace.package] repository`; set it for a fork's releases, a mirror, or a
   `file://` directory), and check each against `SHA256SUMS`. It refuses a release that is not this
   checkout's version, and warns when the checkout is not at the release's commit. The verified
   release is laid out in `infra/.release/<tag>/` (Lambda zips, the unpacked UI, `release.json`);
   a failed fetch leaves no partial directory.
2. **Synthesize from it.** deploy.sh exports `BIFFIX_RELEASE_DIR`, and the CDK app takes the Lambda
   zips and the UI from there (`paths.release_dir`), instead of `target/lambda/` and `ui/build/`.
   The from-source staleness check does not apply to them; that the release is this checkout's
   version stands in for it.
3. **Import the coordinator image.** `crane copy` of the image *by the digest in `release.json`*
   into the data stack's own coordinator ECR repository, under the release tag
   (`<account>.dkr.ecr.<region>.amazonaws.com/<repo>:v0.2.0`), then a check that the tag names
   exactly that digest. The image must be in the tenant's ECR, not only on GHCR: the backend
   resolves the deployed base's digest there, the runtime builds append their layers to it there,
   and image retention protects it there. The copy runs before `cdk deploy` when the repository
   exists, and right after it on a first deploy (which creates the repository). An image already
   imported under the tag is reused; a tag naming another digest stops the deploy (the repository's
   tags are immutable). deploy.sh prints the imported tag and its digest.
4. **Deploy** as usual. `coordinator_image_tag` must be the release tag: leave it unset (its default
   is this checkout's release tag), or deploy.sh refuses the release.

`--release` needs `crane` (but not cargo or cargo-lambda; Node is still needed for the CDK CLI),
and does not combine with `--skip-ui`/`--skip-lambdas`. With `--dry-run` it fetches and verifies
the release and synthesizes from it, but imports nothing. The health check needs the `biffix` CLI on `PATH` (or cargo to build
it); without either a release deploy skips it with a warning.

## Deploying the control plane

### The fast path: `infra/deploy.sh`

`infra/deploy.sh` wires the whole manual flow together — account guard → UI build → all four
Lambda builds → host AWS CLI staging → `cdk deploy` → image retention → coordinator variant builds → `biffix health` verify — and
is idempotent. The deploy config is required (`--config`, see **Config file**), and so is the
target account: pass `--account`, or set `account: "<12-digit id>"` in the deploy config (both
must agree when both are given). It always refuses the AWS Organizations management account.
`--assume-role <arn>` (here and in `teardown.sh`) acts as that role through a profile in a private,
temporary AWS config file, so the role session is renewed as it nears its one-hour expiry and a
long deploy or teardown never runs out of credentials (as long as the caller's own last). A
non-zero `cdk deploy` exit fails the deploy unless cdk reported every targeted stack deployed and
each is `CREATE_COMPLETE`/`UPDATE_COMPLETE`.

```sh
# Full deploy of every stack, guarded on the target account, verified at the end.
infra/deploy.sh --config biffix-deploy.yaml --account <tenant-account-id>

# Assume a member-account role first, then deploy just the api stack (the control-plane hub; the
# control plane is the persistence/ingest/api/edge stacks — use --all for a full deploy).
infra/deploy.sh --config biffix-deploy.yaml \
  --assume-role arn:aws:iam::<acct>:role/OrganizationAccountAccessRole \
  --account <acct> --stack biffix-<tenant>-api

# cdk synth only (still builds artifacts; changes nothing in AWS)
infra/deploy.sh --config biffix-deploy.yaml --dry-run --account <acct>

# Deploy the published release v0.1.0 instead of building (from a checkout of that tag)
infra/deploy.sh --config biffix-deploy.yaml --account <acct> --release v0.1.0
infra/deploy.sh --help           # all flags
```

Built from source, it intentionally does **not** build/push the coordinator image or bump the
image tags — that stays the reviewed CI path (above); `--release` imports the release's image
instead (see **Releases**). Run it rather than its steps by hand: besides building, deploying and
verifying, it stages the host AWS CLI into the reference bucket (see **Host AWS CLI**; without it
every Batch host's install fails and Nextflow tasks fail), provisions the Google client secret and
the ACM certificate when the config needs them (see **Google Workspace federation** and **Custom
app domain**), runs the image-retention step (`python -m biffix_infra.image_retention`), and
builds the coordinator variants of the deployed base image (see **Engine runtimes and coordinator
variants**).
`deploy.sh --help` lists every flag; `--exclusively` (`--stack`) deploys only the named stack
(skips its dependencies), for a targeted api/compute redeploy once the rest of the graph is up.

### Lambda artifacts and their guards

CDK packages prebuilt artifacts (no Docker, fast synth): `deploy.sh` builds the UI and the four Rust
Lambdas (`cargo lambda build --release --features aws --bin <bin>`) first. Each produces
`target/lambda/<bin>/bootstrap` (under `$CARGO_TARGET_DIR` instead when that is set; `deploy.sh`
and the CDK app both honor it), packaged via `rust_lambda_code` (`biffix_infra/lambda_artifacts.py`):
the backend and sweep bootstraps by `ApiStack`, the ingest + reconcile bootstraps by `IngestStack`.
The UI build is uploaded to the private UI bucket by the `edge` stack's `BucketDeployment`, which
also invalidates CloudFront (`distribution_paths=["/*"]`, `prune=True`) and marks the assets
`no-cache`.

**Two guards protect a deploy — both are hard failures, not warnings:**

- **Missing artifact.** If a `bootstrap` is absent, synth raises `FileNotFoundError`, never a
  placeholder: one exits non-zero on every invocation, so a deploy looks healthy while every event
  silently lands in the DLQ and no run is ever visible. Never deploy a placeholder. (`BIFFIX_ALLOW_UNBUILT_LAMBDA=1` opts synth/tests out — never a deploy.)
- **Stale artifact.** Synth also rejects a `bootstrap` older than any file in the crates it is
  built from (migrations and embedded data included, not only `.rs`/`.toml`), `Cargo.lock`, the
  root `Cargo.toml` or `rust-toolchain.toml` (`_reject_stale_artifact`, keyed by
  `_LAMBDA_SOURCE_CRATES`). The error reads
  `Rust Lambda artifact for <bin> is STALE`. **Every binary depends on `biffix-core` and
  `biffix-db`, so any change under either invalidates all of them** — and the backend binaries
  additionally track `biffix-api` and `biffix-workflow` (their other first-party deps), so an
  API- or workflow-only change invalidates the backend bootstraps too. (Build with
  `cargo lambda build`, not `cargo build` — the latter writes elsewhere and leaves the bootstrap
  stale.)

### Verify

`deploy.sh` ends with `biffix health --url <api>`, which exits 0 only when both `/healthz` and
`/readyz` pass (`--output json` for a machine-readable answer), retrying for about two minutes
so a cold start's slow first request does not fail the deploy; `biffix runs list --url <api>`
additionally exercises auth and reads. Check the **real** endpoints, not the in-app API docs
(`/api-docs`) — their spec is bundled with the UI and can lag a fresh deploy, so it is not a
reliable confirmation that new fields shipped.

### Replaying the projection after a fix

Everything the UI shows is a fold of the event log, so a fix to the fold (or a new projection
column) reaches existing runs by replay. The reconcile Lambda does it one page of runs at a time on
a manual invoke; pass each answer's `last_run_id` back as `after` until it is `null`:

```sh
FN=$(aws lambda list-functions --query \
  "Functions[?contains(FunctionName, 'Reconcile')].FunctionName | [0]" --output text)
aws lambda invoke --function-name "$FN" --cli-binary-format raw-in-base64-out \
  --payload '{"action": "refold", "limit": 100}' /dev/stdout
aws lambda invoke --function-name "$FN" --cli-binary-format raw-in-base64-out \
  --payload '{"action": "refold", "after": "<last_run_id>", "limit": 100}' /dev/stdout
```

`limit` is 1 to 200 (default 25); each page holds the projection lock, pausing ingest, while it
runs. The first page also rebuilds the spot-interruption and image-build projections. Any other
`action`, or a field of the wrong type, is refused rather than run as a tick.

## Config file

One YAML file describes a deployment: tenant, region and account, the network posture, the
`queue_classes` menu registration validates against, the projects and their buckets, retention,
the coordinator image tag, sign-in (Cognito, Google), the app domain, KMS keys and the
`registration` trust blocks. It is data, not code: a new deployment is a new file, not a change to
biffix. `infra/biffix-deploy.example.yaml` documents every key, with its default and when to set
it. Start from a copy:

```sh
cp infra/biffix-deploy.example.yaml infra/biffix-deploy.yaml
```

**There is no default config.** `deploy.sh` and `teardown.sh` require `--config <file>`, and the CDK
app requires `-c configFile=<file>` or `BIFFIX_DEPLOY_CONFIG` (the context wins when both are
set), so every command names the tenant it acts on: a teardown can never pick up a file that
happens to be lying around. A relative path is looked up in `infra/` first, then (for the scripts)
in the current directory:

```sh
infra/deploy.sh --config biffix-deploy.yaml --account <acct>
cdk deploy --all -c configFile=biffix-deploy.yaml
BIFFIX_DEPLOY_CONFIG=biffix-deploy.yaml cdk deploy --all
```

A real config names your account, buckets and domain, so keep it out of a public repository:
`.gitignore` ignores `infra/biffix-deploy.yaml` and `infra/biffix-deploy-<name>.yaml` (one per
tenant, e.g. `biffix-deploy-staging.yaml`). A deployment repo of your own can keep its configs
anywhere and pass absolute paths.

The loader (`biffix_infra/config/`) refuses a key it does not know, so a misspelled key fails the
deploy instead of being ignored. Its schema (`config/schema.py`) is the one list of keys, and a
test fails if the example stops naming one of them or its defaults drift from the loader's.

### Projects and request routing (`projects[]`)

Each `projects[]` entry is its own isolation boundary: its own bucket (created, or imported with
`bucket_name`), KMS key, run base role, task log group, coordinator role and coordinator job
definition template. The deploy refuses ids outside `[A-Za-z0-9-]` (an id names the `project-<id>`
stack and the `/biffix/<tenant>/projects/<id>` parameter), duplicate ids, and two projects on one
`bucket_name` (the backend tells a run's project by the bucket its outputs are in).

The backend routes each request to a project: a submit to its version's project (`--project` at
register), a run page to the bucket the run ran in. It reads the projects from SSM, one Standard String
parameter per project at `/biffix/<tenant>/projects/<id>` (ApiStack writes them), holding a JSON
object: `id`, `bucket`, `run_role_arn`, `coordinator_job_definition` (the project's template ARN),
`task_log_group`, `uploads`, `viewer_cors` and `data_sources` (synth refuses a context that could
outgrow the parameter's 4 KB). The backend and sweep Lambdas get only `BIFFIX_PROJECTS_PATH` (that
path) and `BIFFIX_DEFAULT_PROJECT` (the first entry: the project a request that names none goes
to), so their environment stays the same size however many projects there are (Lambda caps it at
4 KB). They read the parameters with `ssm:GetParametersByPath` only. Inspect one with
`aws ssm get-parameter --name /biffix/<tenant>/projects/<id>`; the backend caches them for a few
minutes (a request naming a project it has not seen reads them again at once), so a redeploy's
change lands within that.

A submit refuses what the run's project cannot reach, before anything launches: an input or
reference-bundle file in another project's bucket or in a data source only other projects have
(the 400 names the project whose runs can read it), and a version whose staged source or build
context is in another project's bucket. Files outside every configured bucket and data source
(public data, buckets granted out of band) are only checked to exist.

Registering without `--project`: on a deploy of one project the CLI registers into that project
(such a deploy runs every version in its one project, whatever its label); on a deploy of several
it reuses the project of the workflow's earlier versions, and a new workflow needs `--project`.

Adding a second project to a deploy: versions registered while it had one project carry the label
they were registered with (by default the one project's id, or whatever `--project` named). Once
the deploy has several projects, a version whose label is not a configured project id cannot run
(submit answers 409), and one whose label matches a newly added project runs in that project (its
staged source, in the first project's bucket, is then refused at submit). Before adding a project,
list the labels in use (`SELECT project, count(*) FROM workflow_versions GROUP BY 1`). Move a
version into a project with `biffix workflows reregister <version id> --project <id>` (a
git-sourced version with no image built from a staged context), or register it again from its
checkout with `--project <id>` (an S3-staged source stays in the bucket it was staged in).

Only the control plane spans projects: the backend and sweep Lambdas hold every project's
control-plane grants (read its bucket and data sources, write runs' `inputs/` and staged
`workflow-sources/`, uploads where the bucket takes them, the sweep's `provenance/`) and may pass
every project's coordinator and execution roles (never a run role), and the image builds read
`workflow-sources/` in every project bucket. Each coordinator and run base role still reaches only
its own project's bucket, key and data sources. Roles in the app are tenant-wide, though: any
viewer may browse every project's bucket and data sources in the file picker and data browser
(`?project=`).

### Imported buckets and CORS (`uploads_cors_applied`, `viewer_cors_applied`)

The browser talks to S3 directly in two places: uploads (presigned multipart PUTs) and the alignment
viewer (presigned Range GETs of a BAM/CRAM, its index and the reference FASTA). Both need a CORS rule
for the site's origin (`app_domain`, else the local dev UI) on the bucket. A project bucket biffix
creates gets both rules (`config.project_bucket_cors`). biffix never changes a bucket it does not own,
so for an imported project bucket (`bucket_name`) and for a data source's bucket the deploy prints
(stderr, captured in deploy.sh's log) what the owner applies:

- an imported project bucket missing either flag: ONE `aws s3api put-bucket-cors` carrying both the
  upload (PUT) rule and the viewer (GET/HEAD, `Range`) rule, plus the abandoned-uploads lifecycle
  rule when uploads are off. `put-bucket-cors` **replaces** the bucket's rules, which is why the
  command always carries the upload rule too (merge any other rules the bucket has first);
- each other bucket a data source reads from that no source flags `viewer_cors_applied`: a
  GET/HEAD-only `put-bucket-cors` (once per bucket; merge any other rules the bucket has first). The
  rule is the bucket's, so one source's flag covers every source in that bucket, as the backend reads
  it. A source inside the project bucket is covered by the project's command.

After applying, set `uploads_cors_applied: true` and `viewer_cors_applied: true` on the project (or
`viewer_cors_applied: true` on the data source) and redeploy: the flags turn the UI's upload button on
(`uploads` in the project's SSM context) and tell the viewer the bucket is set up (`viewer_cors`,
and `viewer_cors` per entry of its `data_sources`; the viewer still probes, so a missing rule shows as
"ask your admin" rather than a broken viewer). Check with `aws s3api get-bucket-cors --bucket <b>`.

A reference bundle's `igv_genome` (e.g. `hg38`) makes the viewer load that genome from igv.org's
public hosts instead of the bundle's FASTA — the browser then contacts those hosts, so use it only
for public assemblies.

### Host AWS CLI (`host_aws_cli`)

Every Batch host installs AWS CLI v2 at `/opt/biffix/aws-cli/bin/aws` at boot (the compute stack's
launch template runs `biffix_infra/host/install_aws_cli.sh`). Nextflow bind-mounts it into each task
(`aws.batch.cliPath`) to stage the task's S3 I/O, so task images need no AWS CLI of their own. The
bundle is pinned by version and per-arch sha256 in `biffix_infra/config/compute.py`
(`HostAwsCli`); `deploy.sh` downloads it once, checks the checksum, and uploads it to
`s3://<reference bucket>/host/aws-cli/<version>/`.
Hosts fetch it from there (no internet needed) and verify the checksum again. To move to a newer CLI,
set all three fields:

```yaml
host_aws_cli:
  version: "2.37.4"
  sha256_x86_64: <sha256 of awscli-exe-linux-x86_64-<version>.zip>
  sha256_aarch64: <sha256 of awscli-exe-linux-aarch64-<version>.zip>
```

A host whose install fails still joins its compute environment (a failed boot would make Batch
replace it in a loop); the reason is in `/var/log/biffix-host-aws-cli.log` and the instance's console
output, and Nextflow tasks placed on it fail.

### Custom app domain with external DNS

`app_domain` is the stable browser/OAuth callback hostname. The CloudFront alias needs an ACM
certificate in `us-east-1` (CloudFront reads certificates only from there, whatever the deploy
region). The CDK app does not create it: an in-stack certificate would leave the `edge` stack
blocked on DNS validation, and roll it back if validation times out. `deploy.sh` provisions it
instead, before `cdk deploy`, when the config sets `app_domain` and no `app_certificate_arn`:

1. It reuses an `ISSUED` certificate for `app_domain` if one exists, else resumes one in
   `PENDING_VALIDATION`, else requests a new DNS-validated one.
2. For a certificate not yet issued, it prints the validation CNAME (name, type, value) and polls
   for up to about 30 minutes while you add that record at your DNS provider. Any provider works;
   Route 53 is not required. If the certificate is still not issued, the deploy stops; add the
   record and re-run, and it picks up the pending certificate.
3. It passes the issued certificate's ARN to the CDK app (`-c appCertificateArn=…`, which wins over
   the config), and the `edge` stack attaches `app_domain` to the distribution in the same run.

Then point `app_domain` at the distribution's domain (a CNAME, or your provider's alias record).
CloudFront refuses the alias while the name still points at a different distribution, so when
moving a domain, repoint that record first. A `--dry-run` reuses an issued certificate but never
requests one or waits; without one it synthesizes the template without the alias.

To manage the certificate yourself, request it in `us-east-1` and set `app_certificate_arn`;
deploy.sh then skips all of the above. (`app_certificate_arn` without `app_domain` is a config
error.)

### Google Workspace federation (optional)

Federation is **off by default** — with no `google_oidc` block the pool is Cognito-only
(username/password), and everything below can be skipped. When you do want Google sign-in, set:

```yaml
cognito_domain_prefix: biffix-acme            # required whenever google_oidc is set
google_oidc:
  client_id: <id>.apps.googleusercontent.com   # the client SECRET is NOT here — deploy.sh provisions it
```

Enabling federation makes Google the **only** sign-in method — native Cognito username/password is
disabled, so the Workspace becomes biffix's single identity source. RBAC still works: mapped
`cognito:groups` grant roles, and the `authorization.authenticated_default_role` covers everyone else.

Two pieces live **outside** the config:

1. **The client secret** is never in config or the template. `deploy.sh` provisions it: when the
   config has a `google_oidc` block and the secret is not yet in Secrets Manager at the derived name
   `biffix/<tenant>/google-oidc` (in the target account), it prompts once (input hidden, never in
   argv/history/git), stores it there, and Cognito reads it by name at deploy. If the secret already
   exists it is reused. For non-interactive/CI use, set `BIFFIX_GOOGLE_CLIENT_SECRET`. So the only
   manual step is creating the Google OAuth client in the Google Cloud console to get its `client_id`
   (config) and secret (typed into the deploy prompt).
2. **The Google client's Authorized redirect URI** must include Cognito's hosted-UI callback, or
   every sign-in fails `Error 400: redirect_uri_mismatch`. That URI is
   `https://<cognito_domain_prefix>.auth.<region>.amazoncognito.com/oauth2/idpresponse`. The identity
   stack prints the exact string as the **`GoogleAuthorizedRedirectUri`** CloudFormation output on
   every deploy — copy it into the Google client's *Authorized redirect URIs* (not JavaScript
   origins; exact host, `https`, no trailing slash).

Because that redirect URI is derived from `cognito_domain_prefix` (and the region/account), treat
the prefix as a **stable identifier**: changing it — or standing the tenant up in a new account —
silently breaks federated login until the Google client's redirect URI is re-registered for the new
value. The reused client will still carry the old tenant's URI, which is a common footgun.

## Roles & the first-admin bootstrap

A user's role comes from Cognito group membership, mapped in `biffix_infra/rbac.py`
(`RBAC_GROUP_ROLES`) — one source of truth consumed by both the identity stack (which creates the
groups) and the API stack (which injects the mapping into the backend as `BIFFIX_GROUP_ROLE_MAP`);
a user in no mapped group gets `authorization.authenticated_default_role` (below):

| Cognito group | Role | Grants (cumulative — each role includes the ones above) |
|---|---|---|
| `biffix-viewers` | viewer | view runs, results and workflows |
| `biffix-operators` | operator | + launch runs, manage experiments |
| `biffix-developers` | developer | + register and release workflow versions, edit the site title and logo, manage alert recipients |
| `biffix-admins` | admin | + manage reference bundles and the support contact |

The backend grants the **highest** role among a user's groups (`Rbac::role_for`). A federated
Google user emits **no `cognito:groups`**, so anyone signed in with no group falls back to
`authorization.authenticated_default_role` — deliberately **`viewer`** by default. That is
by design, not a bug: pool admission alone grants read-only, and elevated roles are an explicit,
auditable act.

**Group MEMBERSHIP is administered in Cognito/the IdP, never through biffix** (identity lives in
the identity provider; biffix stores only the group-to-role mapping, see
[docs/architecture.md](../docs/architecture.md#manage-aws-through-aws)). So a **fresh deployment has no operator/developer/admin** — every
user is a Viewer until someone adds them to a group. This is a required one-time bootstrap after the
first sign-in (the federated user record only exists once they have signed in at least once):

```sh
# In the tenant account (assume the role first if it is a member account), find the user by email
# and add them to biffix-admins (admin ⊇ every capability). Re-login afterward so the new token
# carries the group claim.
POOL=<user-pool-id>        # e.g. <region>_xxxxxxxxx (identity-stack output / Cognito console)

USERNAME=$(aws cognito-idp list-users --region <region> --user-pool-id "$POOL" \
  --filter 'email = "person@example.com"' --query 'Users[0].Username' --output text)

aws cognito-idp admin-add-user-to-group --region <region> \
  --user-pool-id "$POOL" --username "$USERNAME" --group-name biffix-admins

# Verify:
aws cognito-idp admin-list-groups-for-user --region <region> \
  --user-pool-id "$POOL" --username "$USERNAME" --query 'Groups[].GroupName' --output text
```

For a federated Google user the `Username` is an opaque `Google_<sub>` string — always resolve it by
email as above rather than assuming it is the email address. Grant the least role that unblocks the
person (`biffix-operators` to launch runs, `biffix-developers` to also register workflows);
reserve `biffix-admins` for those who manage reference bundles and the support contact.

## Network posture (NAT; optional: locked-down workers)

**Default.** Private subnets are `PRIVATE_WITH_EGRESS` behind one NAT device
(`network_stack.py`, `nat_gateways=1`). Workers, the coordinator, and the backend/ingest Lambdas
reach AWS services and the public internet outbound. Only the free S3 *gateway* endpoint is
provisioned; ECR, Logs, and STS ride NAT because paid interface endpoints cost more than their NAT
traffic at this scale. Inbound is unaffected: only CloudFront is internet-facing, and the HTTP API
is gated by a Cognito JWT authorizer (`HttpUserPoolAuthorizer`) — nothing public without auth.

`nat_mode` selects a t4g.nano NAT instance (`instance`, ~$4/mo, low-cost/SPOF) or a managed NAT
gateway (`gateway`, ~$32/mo, recommended HA). Pipelines can pull public conda/PyPI/Docker/genome
data without a staging step.

**Locked-down workers (`worker_egress: ecr_s3_only`, off by default).** The Batch workers (every
compute environment but the coordinator's) move to isolated subnets with no route to the internet,
reaching only interface endpoints for ECR (`ecr.api`, `ecr.dkr`), the ECS agent (`ecs`,
`ecs-agent`, `ecs-telemetry`) and CloudWatch Logs, plus an S3 gateway endpoint of their own. A task
can then pull no image the tenant does not hold and fetch nothing from the internet (a task that
downloads data or packages at run time fails). The workers' S3 endpoint policy reaches only
same-region buckets in the tenant account, the configured data sources' buckets (read) and imported
project buckets, and ECR's own layer bucket, so a task cannot copy data to a bucket elsewhere; a
public dataset bucket must be declared as a data source to be readable. The coordinator and the
Lambdas keep the NAT and an unrestricted S3 endpoint. Cost: the six endpoints across two AZs, ~$90/mo. Turning it on or off
replaces the worker compute environments, so do it while they are idle. The strict per-run image
map already stops unmapped image pulls; this closes the network behind it.

**The residual risk NAT accepts:** outbound internet egress from workloads means a *compromised
container or a poisoned dependency* could exfiltrate data or beacon out. NAT is outbound-only (no
new inbound hole), so this is purely an egress/exfil concern. Acceptable when you trust every
image and dependency; not ideal for PHI/HIPAA or untrusted third-party workflows.

### NAT cost profile (`nat_mode`)

Private-subnet outbound egress runs through one shared NAT device, selectable in the deploy config:

- **`instance`** (the default, ~$4/mo) — a single t4g.nano NAT instance. Cheapest, but a
  single point of failure you own and patch. Fine for a small internal tool where a brief outbound
  blip is tolerable.
- **`gateway`** (recommended for production, ~$32/mo) — a managed, highly-available NAT gateway
  with no maintenance.

Interface VPC endpoints are **not** provisioned by default (`worker_egress: ecr_s3_only` adds six,
for the workers only): at ~$14.6/mo each across two AZs (~$58/mo for just ECR, ECR-Docker, Logs
and STS) they cost more than the NAT data charges that traffic incurs at this scale, so it rides
NAT instead. Only the free S3 *gateway* endpoint is kept (it saves real NAT data charges on bulk
S3).

Approx. network/fixed floor: `instance` + S3 gateway + 4 KMS CMKs (reference, image archive,
event store, one project) ≈ **~$10/mo**; switching to `gateway` ≈ **~$38/mo**. (Aurora scale-to-zero and Batch spot compute are usage-based, on top. While no run is active, the reconcile and sweep ticks run hourly rather than every 5 minutes and close their connections after each invocation, so an idle cluster pauses between them: ~$4/mo of hourly wakes plus Aurora storage.)

### A fully-closed VPC

`worker_egress: ecr_s3_only` is the supported way to cut the workers off from the internet. A VPC
with no egress at all (no NAT for the coordinator and the Lambdas either) is not a configuration
the stacks offer: besides an interface endpoint for every AWS service those call, the coordinator
posts engine events to the API's public URL, and an API Gateway HTTP API has no private endpoint.

## Encryption keys (BYOK optional)

S3 data and the event store are always encrypted; the choice is *which* key. By default biffix
creates customer-managed KMS keys (one shared reference-bucket key, one for the image archive, one
for the event store and its secrets, and one per project) — this gives per-project crypto
isolation and customer-managed keys for regulated data (see
[docs/architecture.md](../docs/architecture.md#design-principles)) for ~$1/key/mo.

Deployers who run their own key infrastructure can **bring their own key** (managed in KMS,
not by biffix) instead of having biffix mint one. Set the key ARN in the deploy config:

```yaml
# shared reference bucket
reference_key_arn: arn:aws:kms:us-east-1:111122223333:key/abcd-1234
# the event store (Aurora and its secrets); set it at the first deploy: a change replaces the
# cluster
aurora_key_arn: arn:aws:kms:us-east-1:111122223333:key/db-5678
# the image archive
image_archive:
  key_arn: arn:aws:kms:us-east-1:111122223333:key/arch-4321

projects:
  - id: demo
    key_arn: arn:aws:kms:us-east-1:111122223333:key/proj-9999   # optional, per project
```

When an ARN is set, biffix imports the key (no CMK created, no ~$1/mo, and biffix never sets a
removal policy on a key it doesn't own). Omit the field to keep the biffix-managed default. The
imported key's policy must allow the run/coordinator roles to encrypt/decrypt; an event store key
must also let RDS and Secrets Manager use it and trust the account's IAM policies (the Lambdas
are granted decrypt by their own roles).

## Tearing down a deployment

`infra/teardown.sh` is the guarded inverse of `deploy.sh` — it destroys **one tenant in one
account**. A clean teardown is not just `cdk destroy`; three things bite, and the script encodes the
fix for each:

1. **Termination protection.** Stateful stacks carry it; CloudFormation refuses to delete them until
   it is cleared. The script clears it on every stack of this config first.
2. **The RETAIN'd Aurora event store.** `PersistenceStack` sets `RemovalPolicy.RETAIN` +
   deletion-protection, so `cdk destroy` deliberately leaves the cluster — and the *retained* cluster
   still holds the persistence stack's security group, so deleting that stack then fails with a
   `DependencyViolation` on the SG. The script deletes the cluster (instances, then cluster, skipping
   the final snapshot) **before** `cdk destroy`.
3. **Retained, versioned S3 buckets.** The data/reference/UI buckets are retained too, and the
   reference bucket is versioned, so a plain `aws s3 rm` leaves noncurrent versions + delete-markers
   and the bucket lingers. The script purges all versions, then deletes the bucket.

```sh
# Full teardown of a member-account tenant (destroys stacks AND deletes the Aurora cluster + buckets).
# --account is REQUIRED and guards the (post-assume) caller account so you can never destroy the
# wrong (e.g. management) account. Prompts for the tenant name to confirm unless --yes.
infra/teardown.sh \
  --assume-role arn:aws:iam::<acct>:role/OrganizationAccountAccessRole \
  --account <acct> --config <config>.yaml

infra/teardown.sh --account <acct> --config <config>.yaml --keep-data  # stacks only; KEEP Aurora + buckets
infra/teardown.sh --account <acct> --config <config>.yaml --yes        # non-interactive (CI)
infra/teardown.sh --account <acct> --config <config>.yaml --delete-image-archive  # also delete the image archive
infra/teardown.sh --help
```

`--keep-data` destroys the stacks but preserves the retained Aurora cluster and S3 buckets (use it
when the data must survive); the default is a full teardown, since those resources are otherwise
orphans. A kept cluster holds the persistence stack's security group and sits in the network stack's
subnets, so `--keep-data` keeps those two stacks as well; a later full teardown removes them.

The image archive bucket is kept unless you pass `--delete-image-archive`: it is meant to
outlive an account rebuild, and an Object-Locked archive cannot be emptied anyway (the purge stops,
and reports, on any version it cannot delete).

Everything is found by exact identity, never by a `biffix-<tenant>-` name prefix (which tenant
`acme-dev`'s names also start with): the stack names the CDK app gives this config, the buckets and
Aurora clusters those stacks own, and buckets/clusters tagged `biffix:tenant=<tenant>` (which finds
ones an earlier, partial teardown left without a stack). The image archive among them is
recognized by its `biffix:role=image-archive` tag (or, from before that tag, the data stack and
logical id CloudFormation tagged it with), never by its generated name. A tagged bucket or cluster
whose stack is still live but not in the config (a project removed from it) is left to that stack,
with a warning. A project's imported `bucket_name` is never touched. The script verifies at the end that those stacks, clusters and buckets are gone, and fails
loudly if any remain. It then lists what stays by design: the ECR repositories (with their images),
the Cognito user pool, the KMS keys and the capability secret.

**Bits that stay manual** (external to AWS, so teardown cannot touch them): the two DNS records
at your DNS provider for a custom domain (the ACM validation CNAME and the `app_domain` alias), the Google OAuth
client's redirect URI, and — if you are done with the account entirely — the CDK bootstrap stack
(`CDKToolkit`) and its assets bucket. The cross-account bucket policy on shared S3 buckets (e.g. one
that allows `<tenant-account>:root`) is managed **outside** biffix and is not removed by teardown.

## Gotchas (in one place)

- **Rebuild all four Lambdas after any `biffix-core` or `biffix-db` change, or a `Cargo.lock`,
  root `Cargo.toml` or `rust-toolchain.toml` change** (the backend two after a `biffix-api` or
  `biffix-workflow` one) — the stale guard fails the deploy otherwise.
  Use `cargo lambda build`, never `cargo build`.
- **One deploy = one account** — assume the tenant role and guard on the account id, or you create
  orphan stacks in the wrong (usually management) account.
- **Image tags are immutable** — ship a new image by pushing a new SHA tag (CI) and bumping
  `coordinator_image_tag` in the deploy config; a same-commit rebuild fails
  on purpose. A release's image is imported under its release tag, which is the default.
- **`--release` deploys the release's artifacts with this checkout's infra code** — check out the
  release tag first; deploy.sh refuses another version and warns on another commit.
- **`--exclusively`** deploys just the named stack; drop it (or use `--all`) when a change crosses
  stack boundaries.
- **Don't trust the in-app API docs (`/api-docs`)** to confirm a deploy — check the live endpoints.
- **A fresh deploy has no admin** — every user is a Viewer until added to a Cognito group; bootstrap
  the first admin with `admin-add-user-to-group` (see **Roles & the first-admin bootstrap**).
- **Removing an env var the running code requires fails cold starts during the deploy** —
  CloudFormation applies a Lambda's new environment a few seconds before its new code, so the old
  binary starts without the variable and exits; cold starts fail for a few seconds, which can trip
  `BackendErrorsAlarm`. For a breaking env change, deploy twice: first code
  that accepts both shapes, then remove the old variables.
- **Teardown is not `cdk destroy` alone** — use `infra/teardown.sh` (it deletes the RETAIN'd Aurora
  first and purges the versioned buckets); a bare `cdk destroy` leaves the cluster, its SG, and the
  buckets behind (see **Tearing down a deployment**).

## Private workflow sources (S3-staged)

By default `biffix workflows register` records the git origin and the coordinator anonymously
clones it at run time — which only works for **public** repos. For a **private** repo (or for
stronger reproducibility on any repo), stage the source to the version's project bucket instead:

```sh
biffix workflows register . --image <img> --source-mode s3
```

With `--source-mode s3` the CLI tars the tracked-file set (`git ls-files`, **recursing into any
populated submodule** so its source is staged too — which handles *private* submodules for free,
since their bytes are already in your local checkout), asks the backend to presign a PUT, uploads
the tarball over plain HTTPS, and registers `source = {kind:s3, uri:…}`. The coordinator pulls that
immutable snapshot from S3 at run time and extracts it (containment-filtered). **No GitHub
credential ever reaches the coordinator** — which matters because the coordinator executes the
workflow author's own code (Snakefile `run:` blocks, Nextflow Groovy), so any secret in its env
would be readable by that code. (A submodule that isn't checked out locally is warned about and
skipped — run `git submodule update --init --recursive` first if the workflow needs it.)

Staging lands at `s3://<project-bucket>/workflow-sources/<version-id>/source.tar.gz`, in the bucket
of the project the version registers into (content-addressed → re-registering identical source is
idempotent). The backend Lambda role holds `s3:PutObject` scoped to `workflow-sources/*` in every
project bucket (to sign the PUT); register rejects any `kind:s3` source uri outside the version's
project bucket + that prefix, since the coordinator fetches and executes whatever it names.

## Worker images

`--image` sets the **default** worker image (the container child jobs run in). Two related knobs:

- **Any registry.** `--image` takes a reference in any registry: registration pins its digest and
  imports it into the tenant workflow repository (see "Workflow images" above), which is what tasks
  pull. A bare `sha256:` digest names an image already in the workflow repository.
- **No default image.** `--image` is optional: omit it when **every** rule/process declares its own
  container — Nextflow's `container` directive, or Snakemake's `aws_batch_container_image` resource
  (the native `container:` directive, and a `container_image` resource, are *not* honored by the
  AWS Batch executor). This is the nf-core model (one biocontainer per process). Register refuses a
  Nextflow version whose processes lack a `container` and no default image (a warning instead when
  `withName:`/`withLabel:` selectors may cover them).
- **Built from the workflow.** `images:` entries with `dockerfile:` or `pixi:` are built in the
  tenant (see "Workflow images" above) instead of imported.
- **Declared in `images:`, found with `biffix inspect`.** A version imports exactly the images
  `biffix.yaml` declares (`image:` / `images:`); `register` discovers nothing itself. To find the
  images a workflow's tasks name, run `biffix inspect [path] --write`, review, and commit
  `biffix.yaml`. It edits only the `images:` block (appends; comments and other keys untouched)
  and, for Nextflow, `plugins:`:
  - **Nextflow:** runs `nextflow inspect -format json` (the `nextflow` on your PATH is required).
    Arguments after `--` go to it; run it once per `-profile` / `-params-file` / `--param` that
    changes which containers are used (e.g. a launch-time image param). It also runs `nextflow
    config -output json` (with the `-profile`, if any; it takes no `-params-file`) and, with
    `--write`, sets `plugins:` to the pipeline's declared plugins (see "Engine runtimes" above).
    An unversioned plugin is an error (but for biffix's own, which are dropped): pin it in
    `nextflow.config`. If `nextflow config` fails, `inspect` warns, still handles the images, and
    leaves `plugins:` as it is. `register` reads the pipeline's plugins the same way (no profile)
    and refuses a `plugins:` that lacks one, or lists it at another version (extra entries are
    fine, since a profile can add plugins), or a pipeline plugin `inspect` would call an error;
    if `nextflow config` fails it only warns, and without `nextflow` on the PATH it reads
    `nextflow.config`'s text and only warns.
  - **Snakemake:** reads the tracked files for `aws_batch_container_image` (rule `resources:`,
    helper modules, profile/config YAML); nothing is run. A value that is not a plain string (a
    function, a config lookup) is listed for you to declare by hand, and `container_image` (read by
    no executor) is flagged.
- **The run's image map.** At submit, the backend writes `biffix/runs/<run>/inputs/images.json`
  (each declared reference, as written and normalized, → the tenant image tasks run: derived for
  Snakemake, imported for Nextflow; every tenant image → itself) and records it on
  `run_submitted.image_map`. The coordinator passes it to the engine: the Snakemake AWS Batch
  executor's `container_image_map` and nf-biffix's container resolver. Both are **strict**: a task
  naming an image that is not in the map fails before it is submitted, so tasks pull only from the
  tenant's ECR, never a public registry.
