# Single-job acceptance fixtures

Two small workflows, one per engine, that exercise a single-job run end to end: `snakemake/` and
`nextflow/`. Each runs on a deployment, through the normal register, submit and Batch path, and
checks from inside its own tasks that they ran as expected. They are validation inputs, not
examples to copy.

Every task in an image checks that:

- `uname -m` is `expected_machine` (`x86_64` or `aarch64`), so it ran natively;
- it is in the image it names (each image holds its name in `/opt/biffix-acceptance-image`);
- the run's capability token (`BIFFIX_INGEST_TOKEN`) is not in its environment;
- with `expect_apptainer` (the default), it is nested in Apptainer (`/.singularity.d`) and can
  write the `/tmp` the run's tasks share.

A failed check fails the task, and with it the run.

The first task reads `input_uri`, a synthetic S3 file holding the line `first`. The second writes
`second-<expected_machine>`, the run's result (`second.txt`; Nextflow publishes it under
`outdir`). The tasks use three images, `first`, `second` and `third`, built from one Dockerfile
(`images/Dockerfile`) on one digest-pinned, multi-architecture Ubuntu base, so they share every
base layer. That is what the measurement of on-host image conversion needs. The Snakemake fixture
also runs a group job and a Python `run:` block, which execute on the Snakemake runtime biffix adds
to each image.

## Parameters

| Parameter | Values | Default |
|---|---|---|
| `input_uri` | an `s3://bucket/key` file holding `first` | required |
| `expected_machine` | `x86_64`, `aarch64` | required |
| `validation_case` | `success`, `retry`, `fail`, `cancel` | `success` |
| `expect_apptainer` | `true`, `false` | `true` |
| `outdir` (Nextflow only) | an empty `s3://` directory of this run's own | required |

`validation_case` changes only the first task:

- `success`: the run succeeds and `second.txt` holds `second-<expected_machine>`. Nested in
  Apptainer, the first task also checks that `/tmp/biffix-acceptance-cancel-sentinel` is absent
  (see `cancel`).
- `retry`: the first task exits 37 on its first attempt and succeeds on its second; the run
  succeeds after exactly two attempts of the first task.
- `fail`: the first task always exits 37. The run fails and writes no `second.txt`.
- `cancel`: the first task leaves the sentinel `/tmp/biffix-acceptance-cancel-sentinel` (nested
  in Apptainer: on the host's scratch) and sleeps 300 s. Cancel the run while that task runs:
  within those 300 s, as a later cancel finds the run past the sleep. A cancelled run leaves its
  scratch for the next single-job run on that host, which wipes it at start, so a later `success`
  run there must not find the sentinel.

Each attempt of the first task also writes `ACCEPTANCE_TASK_STARTED ... attempt=<n>` to its
standard error. Where that line lands depends on the engine: for a Snakemake single-job run, in
the coordinator's log (`biffix runs logs`); for Nextflow, in the task's own `.command.err`, which
the coordinator uploads with a failed task's files. Count attempts from the run's task records
(`biffix runs show`), which both engines report the same way, and use the marker as corroboration.

Set `expect_apptainer: false` for a run of one Batch job per task, to check the same images on
that path; it skips only the Apptainer checks.

`expected_machine` must be the architecture of the queue-class the run lands on: `aarch64` for a
Graviton single-job queue-class, `x86_64` for an x86 one. A mismatch fails every task's first
check, which is what the check is for, so a failure there means the run landed on the wrong
architecture or the parameter does not match the queue-class you picked.

## Running a case

The workflows must be registered from git-tracked files (`--source-mode s3` stages exactly
those). The commands below use these values from your deployment:

- `$PROJECT`: the project to register in; `$BUCKET`: its bucket.
- `$WORKER_QUEUE`: a worker queue-class (`--queue-class`), where the tasks of a run that is not
  single-job go.
- `$SJ_QUEUE`: the single-job queue-class to run on (`--single-job-queue-class`), which fixes
  the architecture; `$INSTANCE`: one of its instance types, which sizes the run.

```sh
# A synthetic input, in a prefix of the project's bucket kept for tests.
printf 'first\n' | aws s3 cp - "s3://$BUCKET/acceptance/input.txt"

# Register (waits for the engine runtime and the three images to be built).
biffix --output json workflows register dev/smoke/single-job/snakemake \
  --project "$PROJECT" --source-mode s3 --queue-class "$WORKER_QUEUE"

# params.yaml:
#   input_uri: s3://<bucket>/acceptance/input.txt
#   expected_machine: aarch64
#   validation_case: success
#   (Nextflow also: outdir: s3://<bucket>/acceptance/<case>-<date>/, a new prefix for each run)

biffix --output json runs submit "$VERSION" --params params.yaml \
  --single-job --single-job-queue-class "$SJ_QUEUE" --instance "$INSTANCE"
biffix --output json runs show "$RUN"     # state, tasks, and why a failed run failed
biffix runs logs "$RUN"                   # the coordinator's log
biffix runs follow "$RUN"                 # exits 0 succeeded, 8 failed, 9 cancelled
biffix runs cancel "$RUN"                 # the cancel case, within 300 s of the first task starting
```

Take `$VERSION` and `$RUN` from the commands' JSON output. Give each run a result location of its
own, so the "no `second.txt`" check of `fail` and `cancel` cannot find an earlier run's file: for
Snakemake, leave the output prefix at its default, the run's own prefix (no `--output-prefix`);
for Nextflow, an `outdir` no earlier run has written to. For a Nextflow version other than the
fixture's 25.10.4, register a copy whose `biffix.yaml` `engine_version` and `nextflow.config`
`nextflowVersion` both name it, committed in its own git repository: an uncommitted or ignored file
is not registered.

Expected outcomes:

| Case | `runs follow` | Result |
|---|---|---|
| `success` | 0 | `second.txt` is `second-<expected_machine>` |
| `retry` | 0 | as `success`, after two attempts of the first task |
| `fail` | 8 | no `second.txt`; the first task exited 37 |
| `cancel` | 9 | no `second.txt` |

A single-job run is one coordinator Batch job and no task jobs: list the run's jobs by its
`biffix:run` tag to confirm it. A missing result is not enough on its own for `fail` or `cancel`:
check in the run's events and logs that the first task started and ended as the case intends, not
that the run failed earlier.

## Checks in the repository

`docker/coordinator/tests/test_acceptance_fixtures.py` validates both parameter schemas and checks
that the workflows apply their defaults, select all three images and keep these cases.
`biffix workflows form <fixture>` previews each fixture's launch form locally.
