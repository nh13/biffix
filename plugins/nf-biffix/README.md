# nf-biffix

A Nextflow plugin that connects a Nextflow run to [biffix](https://github.com/nh13/biffix), the
AWS-native control plane for running and tracking workflows. It is the Nextflow counterpart of
the Snakemake logger plugin in `../snakemake-logger-plugin-biffix`, and both report the same wire
contract (`schemas/wire.engine.schema.json`).

The plugin id is `nf-biffix`; its classes live in the `io.github.nh13.biffix` package. It requires
Nextflow 25.10.4 or later and nf-amazon 3.4.4 or later. One release serves both the 25.10 and
26.x lines: it is built against 25.10.4 and nf-amazon 3.4.4, and CI runs its specs, on those same
classes, against each pinned 26.x release and its nf-amazon (3.9 and 3.12).

## What it does

The plugin has three extension points:

- **`BiffixObserver`** (a `TraceObserverV2`, registered by `BiffixObserverFactory`) reports the
  run and task lifecycle as biffix engine events. It POSTs them in batches to the biffix ingest
  route (`{BIFFIX_INGEST_URL}/events`, bearer-authenticated with the run's capability token), and
  also appends each one to a durable `events.jsonl` file that the coordinator uploads and replays
  through ingest when the run exits, so an event the live POST drops still arrives (ingest dedupes
  the repeats). For a task on AWS Batch it emits `task_submitted` with the Batch job id, and AWS
  EventBridge supplies the terminal state; for a task that never reached Batch (the `local`
  executor, or a failed submit) it emits the terminal event itself.
- **`BiffixContainerResolver`** runs each task on the tenant's copy of its container. When
  `BIFFIX_IMAGE_MAP_FILE` is set, every process `container` is looked up in that run's image map,
  and a task whose image is not in the map fails instead of pulling from a public registry.
- **`BiffixAwsBatchExecutor`** is nf-amazon's AWS Batch executor under the same `awsbatch` name
  (it wins the name whichever plugin loads first). In a biffix run it registers no job
  definitions: each task is submitted to the task job definition the biffix backend registered
  for its image at launch, read from the run's job definition map, and biffix's IAM policies let
  the coordinator submit to those definitions only. A task whose image has no definition fails, as
  does a task that asks for a setting nf-amazon would put in a definition it registered, since a
  pre-registered definition cannot carry it: a `containerOptions` directive that nf-amazon maps
  onto the definition (`--privileged`, `-e`/`--env`, `-u`/`--user`, `--read-only`, `--ulimit`,
  `--shm-size`, `--tmpfs`, `--memory-swap`, `--memory-swappiness`, `--init`), `aws.batch.volumes`,
  Fusion, Fargate, `aws.batch.executionRole`, or the `awsbatch/consumableResources` hint. Options
  nf-amazon ignores on AWS Batch (such as `--gpus`), and Docker's `docker.runOptions`, are
  unaffected.

Outside a biffix run (none of the variables below set) the plugin changes nothing: the observer
is inert, the resolver defers to Nextflow's, and the executor behaves exactly as nf-amazon's.

## How biffix loads it

Each version is published to the [Nextflow plugin registry](https://registry.nextflow.io/plugins/nf-biffix)
(`.github/workflows/publish.yml` publishes a version the registry does not have yet, so a release
is a version bump merged to `main`), but biffix does not load it from there. biffix builds it
(`./gradlew assemble`, giving `build/distributions/nf-biffix-<version>.zip`) and ships the zip in
the coordinator base image (`docker/coordinator/build-wheels.sh`). The runtime layer build
(`codebuild/runtime/build.sh`) unpacks it into the layer's Nextflow plugins directory next to
nf-amazon and nf-cloudcache, and the coordinator (`docker/coordinator/lib/nextflow.sh`) runs
Nextflow offline against that directory. It enables the plugin through `NXF_PLUGINS_DEFAULT` when
the runtime layer was built with the workflow's own plugins (the layer has
`nf/workflow-plugins`), so the pipeline's `plugins {}` stays in force, and otherwise through a
`plugins {}` block in its generated config. A pipeline does not list `nf-biffix` itself.

The version in `build.gradle` must match `nf_biffix_version` in
`crates/biffix-workflow/src/runtime/pins.rs`; a test in `runtime/tests.rs` checks it.

## Configuration

The plugin reads environment variables, which the biffix coordinator sets for each run. It has no
`nextflow.config` scope.

| Variable | Used by | Meaning |
| --- | --- | --- |
| `BIFFIX_RUN_ID` | observer | The biffix run the events belong to. |
| `BIFFIX_INGEST_URL` | observer | Base URL of the ingest route; events go to `{url}/events`. |
| `BIFFIX_INGEST_TOKEN` | observer | The run's capability token (a secret). |
| `BIFFIX_EVENT_FILE` | observer | Name of the durable events file, relative to the launch directory (default `.biffix_events.jsonl`). |
| `BIFFIX_IMAGE_MAP_FILE` | container resolver | Path of the run's image map: a JSON object from each declared image reference to the image to run. |
| `BIFFIX_JOB_DEFINITION_MAP_FILE` | executor | Path of the run's job definition map: a JSON object from each tenant image (as the image map names it) to the ARN, with revision, of the task job definition the backend registered for it. Read once, when the executor starts; a missing or malformed file stops the run. |

The observer needs all three of `BIFFIX_RUN_ID`, `BIFFIX_INGEST_URL` and `BIFFIX_INGEST_TOKEN`;
with only some of them it warns and names the missing ones.

The live poster's limits (largest batch, largest request body, buffer size, retries and timeouts)
follow the ingest route's. They are defined once, in `crates/biffix-core/src/ingest_limits.rs`, and
written to `schemas/ingest-limits.json`; `IngestPosterSpec` fails if `IngestPoster`'s constants
differ from that file.

The plugin logs under the `io.github.nh13.biffix` logger. Nextflow logs only its own packages at
DEBUG, so add `-debug io.github.nh13.biffix` to the `nextflow` command line to see the plugin's
DEBUG lines in `.nextflow.log` (the coordinator does).

## Building and testing

Java 17 is required.

```sh
./gradlew --no-daemon test       # Spock specs, including the wire schema and ingest limits checks
./gradlew --no-daemon assemble   # build/distributions/nf-biffix-<version>.zip
```

## License

MIT; see [LICENSE](../../LICENSE).
