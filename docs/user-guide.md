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
  pins (25.10.4 today), which `biffix engines` lists.
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
| `engine`, `engine_version`, `entrypoint`, `target` | Which engine, which exact release, which file to run, and an optional default target (a Snakemake target rule or file, or a Nextflow `-entry` workflow). |
| `schema` | The parameter schema file. Nextflow defaults to `nextflow_schema.json`; Snakemake has no default. |
| `params` | Per-parameter hints the schema cannot express: `format` (`s3-file`, `s3-dir`, `reference-bundle`, `samplesheet`), `suffix` (accepted file endings), `multiple`, `output` (a path the workflow writes, never checked for existence), `secret` (never stored or shown in clear), `help`, `unit`. |
| `form` | The form's `title` and `description`, `sections` (order, titles, `advanced`), per-parameter overrides under `params` (label, help, section, hidden, default, min/max/step, `presets`, `show_if`, validation `rules`, `samplesheet` columns), named `examples` that fill the whole form, a `run_name` template, and `references`. |
| `about` | A Markdown file shown as the workflow's About page. A README is not picked up by default. |
| `catalog` | The Lab catalog card: `description`, `tags`, `verified`, and a `contact` that failed runs offer to ask. |
| `reports` | Reports a run writes: `name`, `path` (a glob relative to the output prefix), optional `description`, `kind` (`html`, `table`, `image`, `pdf`, `alignment`, `reads`, `fastqc`, `file`) and `per_sample`. |
| `samples` | A hint for per-sample progress: the Snakemake `wildcard` that names the sample, a Nextflow `tag` regex with a `sample` group, `steps` forced to `study` or `sample` scope, and display `labels` for steps. |
| `image`, `images` | The container images, by name. `image:` is shorthand for `images: {default: …}`. See below. |
| `plugins` | Nextflow only: the pipeline's own plugins as `id@version`. |
| `cache` | Snakemake only: reuse results of `cache: True` rules across runs (kept per project). |

Unknown keys are warned about, not silently ignored, except inside an `images:` entry, where an
unknown key is refused. The full field-by-field reference is the
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
  the deployment.
- **A pixi environment**: `{pixi: env}`, a directory holding `pixi.toml` and `pixi.lock`.

A task names an image by its reference, or a built image by its name in `images:`
(`aws_batch_container_image="align"` in Snakemake, `container 'align'` in Nextflow).

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

### Previewing the form

```sh
biffix workflows form .               # an outline: sections, fields, defaults, warnings
biffix workflows form . --open        # the form itself in the UI; nothing is registered
biffix workflows form . --output json # the FormSchema exactly as `register` would send it
```

`--manifest <file>` previews with a different `biffix.yaml`.

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
- `--image` sets a default task image from the command line.
- `--queue-class` names the worker queue-classes the version uses (repeatable).

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
