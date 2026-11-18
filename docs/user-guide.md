# User guide

This guide is for two kinds of people: **workflow authors**, who package a Snakemake or Nextflow
workflow for biffix and register it, and **lab users**, who launch runs and read the results. The
first half covers authoring, the second launching and monitoring. The web UI also has a **Learn**
panel on each page with short, page-specific help.

The UI has two views of the same data, switched at the top of the page. The **Lab** view speaks
the bench's language (pipelines, analyses, results) and shows what a lab user needs: a pipeline
catalog, a step-by-step launch, per-sample progress and results. The **Expert** view adds the
engine-level detail: every Batch job and its log, the task graph, the coordinator's log, cost,
parameters, workflow versions and images. This guide uses the Expert words (workflow, run) except
where it describes a Lab page.

## Concepts

- **Workflow version.** A frozen, immutable snapshot of a workflow: its source at an exact git
  commit (or a staged source tarball), its engine and exact engine release, its container images
  pinned by digest, and the launch form derived from its parameter schema. Every run records the
  version it ran, so running the same version again runs the same analysis.
- **Project.** An isolation boundary set by whoever deploys biffix: its own S3 bucket, encryption
  key and AWS roles. A workflow version is registered into a project, and its runs read and write
  that project's data.
- **Run.** One execution of a version with a set of parameters. It is one coordinator AWS Batch
  job that runs the engine, which submits the tasks as their own Batch jobs.
- **Released and developer versions.** A newly registered version is a *developer* version. A
  Developer or Admin *releases* it, and the Lab view offers and launches each pipeline's newest
  released version.
- **Roles.** Viewer (read), Operator (launch runs, manage experiments), Developer (register and
  release workflows), Admin (reference bundles, site settings). Your deployment's administrator
  assigns them.

## Installing the CLI and signing in

The `biffix` CLI is on crates.io; install it with Rust:

```sh
cargo install --locked biffix
```

Install the release your deployment runs (`--version <x.y.z>`), or build it from that release's
checkout with `cargo install --locked --path crates/biffix-cli`.

Sign in once per deployment. The URL is your deployment's address followed by `/api`:

```sh
biffix login --url https://biffix.example.org/api
biffix whoami
```

`login` opens a browser for the deployment's sign-in page (use `--no-browser` on a machine
without one, and paste the redirect URL back). Credentials are cached per **profile** in
`~/.config/biffix/`, on macOS too (`$XDG_CONFIG_HOME/biffix/` when that is set;
`BIFFIX_CONFIG_DIR` overrides both). To use several deployments, name a profile
with `--profile <name>` or `BIFFIX_PROFILE`; the first profile you log in to becomes the default.

Every command takes `--output human|json` (by default, human on a terminal and JSON when piped).
`biffix engines` lists the engine releases the deployment supports.

## Authoring a workflow

### What biffix needs from a workflow

A workflow is a git repository with a Snakemake or Nextflow entry point. biffix needs:

- **An engine release.** Set `engine_version` in `biffix.yaml` to one exact release in the range
  `biffix engines` prints. Snakemake runs any 9.x release; Nextflow runs only the releases biffix
  pins (25.10.4, 26.04.9 and 26.10.5 today), which `biffix engines` lists. A pipeline that uses
  Nextflow 26 language features (record types, typed processes and workflows) or declares
  `manifest.nextflowVersion = '>=26.04.0'` pins a 26 release.
- **Container images for the tasks.** Either a default image for every task, or each rule and
  process naming its own: Nextflow's `container` directive, or Snakemake's
  `aws_batch_container_image` resource (the AWS Batch executor does not read Snakemake's
  `container:` directive). Images must be declared in `biffix.yaml`; see below.
- **A parameter schema** for the launch form: an nf-schema `nextflow_schema.json` for Nextflow
  (found by default), or a JSON Schema for the Snakemake config, named by `schema:` in
  `biffix.yaml`. Without one the version registers with no form.

Tasks run on AWS Batch with S3 as storage. Write workflows so their inputs and outputs are paths
the engine resolves against its default storage; biffix sets that to the run's output prefix in
the project bucket.

Each task runs on an AWS Batch job definition biffix registers for its image, not one the
workflow chooses. Its container is not privileged and sees nothing of the host but what the engine
needs, so a few engine settings that would change the job definition are refused rather than
silently dropped:

- Snakemake: a rule's `aws_batch_job_definition` or `aws_batch_privileged` resource fails that
  job. `/dev/shm` is 256 GiB for every task (what a task writes there counts against its own
  memory), so a rule's `shared_memory_size_mb` resource is accepted up to that. Tools that need a
  privileged container, such as Apptainer inside the task container, do not run.
- Nextflow: a task fails when its `containerOptions` set anything nf-amazon would put in the job
  definition (`--privileged`, `-e`/`--env`, `-u`/`--user`, `--read-only`, `--ulimit`,
  `--shm-size`, `--tmpfs`, `--memory-swap`, `--memory-swappiness`, `--init`), when the config sets
  `aws.batch.volumes` or `aws.batch.executionRole`, with Fusion or Fargate, or with the
  `awsbatch/consumableResources` hint (the
  [nf-biffix README](../plugins/nf-biffix/README.md#what-it-does) keeps the list). A
  `container 'job-definition://…'` is refused too.

Because the job definition is shared, each Snakemake job's command and environment travel in its
`SubmitJob` container overrides rather than in a definition of its own, and AWS limits the size of
those overrides (8,192 characters of overrides in all). A very large group job, whose command names
many rules and files, can be refused at submit; split the group (a smaller `group-components`, or
fewer rules per group) if one is.

### The `biffix.yaml` manifest

`biffix.yaml` at the workflow's root describes what the engine's own files cannot. Every key is
optional. A small example:

```yaml
engine: nextflow             # snakemake | nextflow | auto (detect from the files; the default)
engine_version: "25.10.4"
entrypoint: main.nf          # default: main.nf, or Snakemake's own search order
about: docs/ABOUT.md         # a Markdown page shown on the workflow's About tab

images:                      # every image the tasks name; `biffix inspect --write` fills it in
  default: {ref: ghcr.io/acme/rnaseq-tools:1.4.0}
plugins: [nf-schema@2.7.2]   # Nextflow only: the pipeline's own plugins, exact versions

catalog:                     # how the pipeline appears in the Lab's catalog
  description: Bulk RNA-seq from FASTQ to gene counts and a QC report.
  tags: [rna-seq, human, mouse]
  verified: true             # the author vouches for it (not checked by biffix)
  contact: {name: Genomics core, email: core@example.org}

form:                        # curate the generated launch form
  title: Bulk RNA-seq
  sections:
    - {id: reads, title: Reads}
    - {id: tuning, title: Tuning, advanced: true}
  params:
    input: {section: reads, format: samplesheet}
    min_mapq: {section: tuning, label: Minimum mapping quality, min: 0, max: 60}
  references:                # one reference-bundle dropdown that fills several params
    kind: assembly
    fill: {fasta: fasta, gtf: gtf}

reports:                     # outputs the run page should show, relative to the output prefix
  - {name: MultiQC, path: multiqc/multiqc_report.html}
  - {name: Counts, path: counts/{sample}.tsv, kind: table, per_sample: true}

samples:                     # how tasks map to samples, when the defaults do not fit
  tag: '^(?<sample>[^_]+)_'
```

What each key does:

| Key | Purpose |
|---|---|
| `engine`, `engine_version`, `entrypoint`, `target` | Which engine, which exact release, which file to run, and an optional default target (a Snakemake target rule or file, or a Nextflow `-entry` workflow; Nextflow 26 has no `-entry`, so a version pinned to it takes no target and selects the workflow with a param instead). |
| `schema` | The parameter schema file. Nextflow defaults to `nextflow_schema.json`; Snakemake has no default. |
| `params` | Per-parameter hints the schema cannot express: `format` (`s3-file`, `s3-dir`, `reference-bundle`, `samplesheet`), `suffix` (accepted file endings), `multiple`, `output` (a path the workflow writes, never checked for existence), `secret` (never stored or shown in clear), `help`, `unit`. |
| `form` | The form's `title` and `description`, `sections` (order, titles, `advanced`), per-parameter overrides under `params` (label, help, section, hidden, default, min/max/step, `presets`, `show_if`, validation `rules`, `samplesheet` columns), named `examples` that fill the whole form, a `run_name` template, and `references`. |
| `about` | A Markdown file shown as the workflow's About page. A README is not picked up by default. |
| `catalog` | The Lab catalog card: `description`, `tags`, `verified`, and a `contact` that failed runs offer to ask. |
| `reports` | Reports a run writes: `name`, `path` (a glob relative to the output prefix), optional `description`, `kind` (`html`, `table`, `image`, `pdf`, `alignment`, `reads`, `fastqc`, `file`) and `per_sample`. |
| `samples` | A hint for per-sample progress: the Snakemake `wildcard` that names the sample, a Nextflow `tag` regex with a `sample` group, `steps` forced to `study` or `sample` scope, and display `labels` for steps. |
| `image`, `images` | The container images, by name. `image:` is shorthand for `images: {default: …}`. See below. |
| `plugins` | Nextflow only: the pipeline's own plugins as `id@version`. Runs are offline, so a plugin Nextflow would load implicitly (`nf-tower`/`nf-wave` for a `tower`, `wave` or `fusion` scope; from 26.10 `nf-agent`/`nf-agent-pi` for an `agent` scope) must be declared, in the pipeline's `plugins {}` and here. |
| `cache` | Snakemake only: reuse results of `cache: True` rules across runs (kept per project). |
| `queues` | Which worker queue-class each task runs on: a `default`, and tasks picked `by_label` (Nextflow) or `by_name`. See [Choosing queues](#choosing-queues). |

Unknown keys are warned about, not silently ignored, except inside an `images:` entry or
`queues:`, where an unknown key is refused. The full field-by-field reference is the
doc comments in
[`crates/biffix-workflow/src/manifest.rs`](../crates/biffix-workflow/src/manifest.rs); the
form it produces follows [`schemas/formschema.schema.json`](../schemas/formschema.schema.json).

### Container images

biffix runs tasks only on images it holds. At registration, every image the version declares is
pinned to a digest and copied into the deployment's own ECR repository. At run time the engine
gets a map from each declared reference to that copy, and a task naming an image outside the map
fails instead of pulling from a public registry.

An image entry under `images:` is one of:

- **A reference** to an existing image: `{ref: quay.io/biocontainers/samtools:1.21--h50ea8bc_0}`.
- **A Dockerfile** in the repository: `{dockerfile: docker/align/Dockerfile}` (with optional
  `context:` and `archs: [amd64]` when the base image has one architecture). biffix builds it in
  the deployment. Two more keys are optional:
  - `target:` builds one stage of a multi-stage Dockerfile (`docker buildx build --target`), so
    one Dockerfile can yield several images. Stage names are case-insensitive, as in Docker:
    `Builder` and `builder` are the same build;
  - `build_args:` is a map of `--build-arg` values. Each key must be a valid `ARG` name (a letter
    or `_`, then letters, digits or `_`) and must not start with `BUILDKIT_` (those configure
    BuildKit itself). A value may not contain a newline, carriage return or NUL. An image takes at most 64 build args,
    at most 16 KiB in all (counted as `KEY=VALUE` lines).

  Like the context's files, the target and build args are part of what identifies the build:
  changing either builds a new image, and the version is a new build, never a reuse of the old
  image.

  **Build args are not secret.** They are stored on the version, shown on its Provenance tab and
  in the API, passed to the build as plain CodeBuild environment variables, and kept in the built
  image's configuration and history. Never put a token, password or key in one; `register` warns
  about keys that look like one (`TOKEN`, `SECRET`, `PASSWORD`, `PASSWD`, `APIKEY`, `CREDENTIAL`, `_KEY`). To build an image that
  needs a credential, build it in your own CI with a Docker build secret (`--secret`), push it,
  and declare it with `ref:`. If tasks need the credential at run time, they fetch it then rather
  than having it built in.
- **A pixi environment**: `{pixi: env}`, a directory holding `pixi.toml` and `pixi.lock`.

A task names an image by its reference, or a built image by its name in `images:`
(`aws_batch_container_image="align"` in Snakemake, `container 'align'` in Nextflow).

For example, a multi-stage Dockerfile yielding a worker image and a separate build-tools image:

```yaml
images:
  default: {dockerfile: docker/Dockerfile}            # the last stage
  builder:
    dockerfile: docker/Dockerfile
    target: builder
    build_args: {TOOL_VERSION: "1.2.0"}
```

To find the images a workflow uses, run:

```sh
biffix inspect . --write
```

For Nextflow it runs `nextflow inspect` (Nextflow must be on your `PATH`; pass `-profile` and other
arguments after `--`, once per profile that changes which containers run) and also sets
`plugins:`. For Snakemake it reads the workflow's files for `aws_batch_container_image`, and lists
values that are not plain strings for you to declare by hand. Review the result and commit it.

For Snakemake, biffix adds its pinned Snakemake runtime to each image (tasks run
`python -m snakemake` inside the image), so images must be glibc-based. An image that already
carries exactly the pinned Snakemake can opt out with `runtime: own`.

### Choosing queues

A deployment offers a menu of worker queue-classes (spot and on-demand, small and large hosts;
see [deploying.md](deploying.md)). By default every task of a run goes to one of them. To send
heavy steps to large hosts and the rest to small ones, name queue-classes in `queues:`, never
the deployment's Batch queue names, so the workflow runs unchanged on any deployment that offers
those queue-classes:

```yaml
queues:
  default: small-spot          # every task no selector below picks
  by_label:                    # Nextflow only: process labels, e.g. nf-core's
    process_high: large-spot
    process_long: large-spot
  by_name:                     # a process (Nextflow) or rule (Snakemake) by name
    MULTIQC: small-ondemand
```

Register with `--queue-class` for every queue-class the block names (here `--queue-class
small-spot --queue-class large-spot --queue-class small-ondemand`): a version lists the
queue-classes it uses, and registration refuses a `queues:` entry outside that list, or a
queue-class the deployment does not offer. Labels and names are exact (no patterns): a label as
Nextflow's `label` directive accepts it, a process name, alias or fully qualified name
(`NFCORE_RNASEQ:RNASEQ:MULTIQC`), or a Snakemake rule name as Snakemake gives it (a `name:`
directive's string included). Without `default`, tasks no selector
picks run on the first `--queue-class`, else the deployment's first. The order of `--queue-class`
is part of a version only in that case (several queue-classes and no `default`): registering again
with another first is a new build. (Versions registered with several queue-classes and no `default`
before that rule existed register again once as a new build after the upgrade, even with
unchanged inputs.) Prefer `default` to choose it.

How each engine applies it:

- **Nextflow.** biffix adds `process.queue` for the default and a `withLabel` or `withName`
  selector setting `queue` for each entry to the config it passes with `-c`. Nextflow ranks a
  generic setting below the process definition, a `withLabel` selector above that, and a
  `withName` selector above all, so a `by_name` entry wins over a `by_label` one, and both win
  over a `queue` directive in the process itself. The file is read after the pipeline's own
  config, so for the same selector biffix's queue wins; a more specific selector in the
  pipeline's config (a `withName` setting `queue` for a process biffix picks by label) still
  wins. Leave `queue` out of the pipeline's config and let biffix set it. A process carrying two
  labels that `by_label` sends to different queue-classes gets one of them, depending on config
  order: give each process at most one such label, or pick it `by_name`.
- **Snakemake.** The default is the AWS Batch executor's queue, which a rule's own `batch_queue`
  resource still overrides; the `by_name` entries go to the executor's rule queue map
  (`--aws-batch-rule-queues`), which overrides a rule's own `batch_queue` and leaves every other
  resource, including the workflow profile's `set-resources`, as it is. Rules have no labels, so
  `by_label` is refused. A rule name that is not in the workflow is ignored. Rules that share a
  `group:` run as one AWS Batch job, on one queue, so they must share a queue-class: give every
  rule of a group the same one, or none of them.

A selector that matches nothing assigns nothing, and those tasks run on the default. For a
Snakemake workflow, `biffix workflows register` warns about a `by_name` entry naming a rule the
Snakefile does not define (when it can see every rule name: not through a dynamic `include:` or a
module, and not with an unnamed rule or a `name:` directive). A Nextflow workflow's process names and labels are not known until it runs, so nothing
checks them at registration: check where tasks ran instead. The Batch queue each task ran on is in
the API's task view (`host.queue`) and in the run's provenance crate (`jobQueue`).

### Previewing the form

```sh
biffix workflows form .               # an outline: sections, fields, defaults, warnings
biffix workflows form . --open        # the form itself in the UI; nothing is registered
biffix workflows form . --output json # the FormSchema exactly as `register` would send it
```

`--manifest <path>` previews with a different `biffix.yaml`. The path is resolved exactly as
`register --manifest` resolves it (relative to the checkout's root, and inside it), except that
an uncommitted file is read too.

### Registering

Registration needs the Developer role. From a clean checkout:

```sh
biffix workflows register . --project rnaseq
```

This freezes a version from the current commit, uploads its form, pins and imports its images,
and waits (up to `--wait-minutes`, 30 by default) until the engine runtime and images are ready
and the version can run. `--no-wait` returns at once. A version still not ready when the wait
ends stays registered, and the command exits 7 (as when the service is unavailable) naming it.
The form is derived only from committed files: a `biffix.yaml`, schema or About page that git
does not track is refused (`biffix workflows form` previews the working tree as it is).

Options worth knowing:

- `--source-mode s3` stages a tarball of the git-tracked files (including populated submodules)
  to the project bucket. It is required for private repositories, since the default
  (`--source-mode git`) records the origin for an anonymous clone at run time. No GitHub
  credential ever reaches the run. `--allow-dirty` registers uncommitted changes, with
  `--source-mode s3` only.
- `--project` chooses the project. On a deployment with several projects, a new workflow needs it.
- `--name`, `--engine-version`, `--entrypoint` override the directory name and `biffix.yaml`.
- `--manifest <path>` reads another manifest instead of the root's `biffix.yaml`, for example one
  per deployment of the same repository. The path is relative to the repository root, must stay
  inside it (symlinks included) and must name an existing, committed file, like `biffix.yaml`. Paths inside it (`schema`, `about`, image Dockerfiles)
  stay relative to the root. The version records the path (shown on its Provenance tab), and
  `reregister` keeps it.
- `--image` sets a default task image from the command line.
- `--queue-class` names the worker queue-classes the version uses (repeatable). The first is
  where its tasks run unless `queues:` says otherwise; see [Choosing queues](#choosing-queues).

Registering the same source with the same inputs again is refused (exit code 6) and names the
existing version.

To run a version from the Lab, release it:

```sh
biffix workflows list                  # version ids, each marked released or developer
biffix workflows release <version-id>
biffix workflows unrelease <version-id>
biffix workflows show <version-id>     # one version and its parameters
```

`biffix workflows reregister` registers versions again from what they recorded, on the current
engine runtime and with their images pinned again; use it after a biffix upgrade
(`reregister --all --dry-run` first). To change the form or About page, register the checkout.

A version's engine runtime and image builds are retried automatically, at most three builds each.
If they still fail (for example during an upstream outage), a Developer starts them again with
`biffix workflows rebuild <version-id>`: the runtime and every image, or only what `--runtime` and
`--image <name>` name. Anything ready or still building is left as it is, and one rebuild that
fails does not stop the others (the command then names what was and was not started). Like
`register`, it then waits until the version can run (`--no-wait`, `--wait-minutes`), so a failed
build it did not rebuild ends the wait at once.

## Launching runs

### From the Lab view

**Start an analysis** (or **Start** on a pipeline card) walks through four steps:

1. **Pick a pipeline.** Pipelines are cards with their description, tags and a *Verified* badge
   (the author's claim). Star one to pin it to My work. Pipelines with no released version are
   hidden unless **Developer** is on.
2. **Add samples.** Choose the sequencing files from a data folder; biffix pairs reads by sample
   and builds the samplesheet. An existing samplesheet can be imported instead.
3. **Reference & options.** Pick a reference bundle (such as GRCh38) or use your own files, and
   set the pipeline's options.
4. **Review & launch.** Check the summary, and an estimated cost once the pipeline has enough
   past runs. Nothing runs until **Launch analysis**.

An analysis can be launched into an **experiment** (see below).

### From the Expert view

**New run** lists each workflow once, at its newest released version; with **Developer** on it also
lists workflows that have no released version yet (at their newest).
A workflow's **Run** tab is its launch form; pick another version in the **Version** menu above
the tabs. The workflow page's other tabs show its About page, its parameters, its versions, and
its provenance (source commit and each image, with why a version cannot run yet).

### From the CLI

```sh
biffix runs submit <version-id> --params params.yaml --name "batch 12" --follow
```

Options:

- `--params <file>` (YAML or JSON) and `--param key=value` (repeatable, overrides the file).
- `--reference <bundle>` fills the form's reference parameters from a reference bundle.
- `--name` gives the run a name; without one the UI shows a name derived from the pipeline, date
  and sample count.
- `--target` overrides the version's target for this run.
- `--output-prefix` writes outputs to a stable prefix in the project bucket instead of a fresh
  per-run one, so a later run can build on them.
- `--retries <n>` sets the default number of retries for a failed task (the workflow's own retry
  settings still win); `--no-keep-going` stops at the first failed task.
- `--retention default|delete-on-success|keep` (Nextflow) decides what happens to the run's
  intermediate files.
- `--image <ref>` (repeatable) adds or overrides an image tasks may name; the run waits until it is
  imported.
- `--experiment <id>` or `--no-experiment`.
- `--follow` waits for the run to finish: exit code 0 on success, 8 on failure, 9 when cancelled
  (and 1 when it ends in a state this `biffix` is too old to know).

Then:

```sh
biffix runs list --state active
biffix runs show <run-id>          # per-task state
biffix runs follow <run-id>
biffix runs cancel <run-id>
biffix runs rename <run-id> "new name"     # or --clear
biffix runs experiment <run-id> <experiment-id>   # or --none
```

Cancelling a run stops its coordinator and its running jobs. The tasks it ended read `cancelled`:
those still queued or running, and those that failed only after the cancel was requested. A task
that had already finished keeps its outcome, so one that failed on its own still reads `failed`.

## Monitoring a run

### The Lab view

An analysis has one page: a grid of samples by steps showing how far each sample got ("9 of 12
samples done"), the key results as cards when it finishes, the QC summary, what it cost, and, if
something failed, **What went wrong**. **All files** lists everything the run wrote.

### The Expert view

A run's page has tabs:

- **Overview**: state, timing, failure summary, latest outputs and reports.
- **Graph**: the task graph, from the engine's own DAG.
- **Jobs**: every AWS Batch job and attempt, with exit code, status reason, instance and log. A
  spot reclaim shows as another attempt of the same task.
- **Outputs**: the files the run wrote, with viewers for tables, text, images, PDFs, HTML reports,
  FastQC results, and BAM/CRAM files in a genome browser.
- **Reports**: the reports the workflow declared, plus the run's MultiQC report and per-sample
  FastQC results when present. MultiQC's general statistics are also drawn as a sortable table
  with MultiQC's pass, warn and fail marks.
- **Coordinator**: the coordinator job's own log (the engine's output).
- **Cost**: estimated cost per job and in total.
- **Parameters**: the parameters the run was launched with (secrets redacted).

The **Provenance** button downloads the run as a Workflow Run RO-Crate; see
[provenance.md](provenance.md).

### When a run fails

The failure summary explains a failed run in plain words: the first failed steps, the sample each
belongs to, a cause (out of memory, a spot reclaim, a missing or damaged input, a full disk, a
step's own error, and so on), the relevant log lines, and advice. **Try again** does what the cause
calls for: resume from where the run stopped, run it again unchanged, or open the launch form to
fix the samples or options first. **Copy report for support** copies the details to send to
whoever runs biffix for your lab; when the pipeline's `catalog.contact` (or the site's support
contact) is set, **Ask** writes to that person directly.

Every run also keeps its logs in the project's bucket, under the run's own prefix:
`s3://<bucket>/<run prefix>logs/`. They are uploaded while the engine runs and once more when it
ends, and hold:

- the engine's log (Nextflow's `.nextflow.log`, Snakemake's newest `.snakemake/log/` file);
- the trace file, for a pipeline that turns on Nextflow's trace without naming a file for it;
- the engine configuration biffix generated (Nextflow's `-c` and default configs, Snakemake's
  command line and wrapper Snakefile);
- for a single-job Nextflow run, the `.command.*` files of its failed tasks. Each upload sends
  those of the 20 most recently failed tasks at the time, and nothing is removed from S3, so a run
  that keeps failing tasks can leave more than 20 tasks' files there: those of earlier uploads stay
  alongside the latest 20;
- for a failed run, `failure.txt`: the end of the engine's output or, for a single-job run that
  fails converting its task images or (Snakemake) writing the Snakefile that sets each rule's
  image, that step's error output. A run stopped by a signal (a cancel, or the job's timeout) has
  none, even when its uploads succeed: its absence then does not mean an upload failed. Nor does a
  run whose coordinator Batch retried (its host went away) and whose retry then succeeded or was
  cancelled: the retry's first upload removes the earlier attempt's.

This archive is best-effort. A run that fails before the uploader starts (a required setting
missing, or a single-job host whose scratch root is unusable, say) uploads nothing; the two
single-job setup failures above are the exception, and upload their logs and `failure.txt` once.
An upload that fails or runs out of time leaves files out without failing the run. When files are missing, read the Expert view's **Coordinator** tab.

A file is uploaded up to a size limit: 8 MiB for the uploads while the engine runs, 64 MiB for the
last one. A larger file is uploaded as its last 8 or 64 MiB, so the S3 copy of a long engine log
may not be the whole log.

A dry run writes nothing to S3, so it leaves no logs there.

### Notifications and Needs attention

When a run you started ends, the bell at the top right gets a notification. Failed runs you have
not dismissed appear under **Needs attention** on My work. Notifications are in-app only.

## Resuming and re-running

- **Re-run** opens the launch form with the run's version and parameters filled in.
- **Resume** starts a new run that reuses the earlier run's finished work, so only what is missing
  or changed runs again. Nextflow resumes from its cache (`-resume`); Snakemake reruns only what is
  missing, stale or incomplete, using the run metadata biffix keeps for it. From the CLI:
  `biffix runs submit <version-id> --resume-from <run-id>`. The new run inherits the earlier run's
  parameters (except secrets, which must be given again), images and experiment.

A Nextflow run's intermediate files (its work directory) are deleted after the deployment's
retention period, or as soon as it succeeds if launched with `delete-on-success`. Results are
never deleted. A run whose work directory is gone can no longer be resumed, and its page says so.

## Experiments

An experiment is a named group of runs in one project, such as one study. Its page shows the runs'
results side by side (as a table, per result, or per run, optionally filtered to one sample), the
samples across its runs, and the total estimated cost. A run is in at most one experiment at a
time; moving it copies or deletes nothing. Creating experiments and moving runs needs the Operator
role; everyone who can see the project can read them.

## Data and uploads

The **Data** page browses the project's bucket and the data sources the deployment connects
(read-only locations such as a sequencing core's deliveries). Files open in the same viewers as
run outputs. The launch form's file picker browses the same places.

Files not yet in S3 can be uploaded from the file picker into the project bucket's `uploads/`
folder. Uploads go from the browser straight to S3 in parts, can be paused and resumed, and the
launch form waits for any file it names that is still uploading. Uploading is meant for a few
files; bulk data should arrive in S3 by other means.

## Reference bundles

A reference bundle is a named set of reference files (a genome FASTA, its index, its annotation)
registered by an Admin under Settings, References. A workflow's `form.references` in
`biffix.yaml` maps bundle keys to its parameters, so the launch form offers one dropdown instead
of a dozen paths. The run records the bundle and the exact files it used; editing a bundle later
does not change runs already launched.

## Costs

biffix estimates cost from each job's instance time at its instance type's on-demand or spot
price, plus its EBS volume, and adds the coordinator job. A run's cost grows while it runs and
settles when every job has ended. The **Usage** page (Expert view) sums estimated cost over a
period, grouped by month, workflow, submitter, instance type or project, and exports CSV. These
are estimates; your AWS bill is the authority.
