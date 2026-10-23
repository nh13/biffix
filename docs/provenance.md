# Run provenance (Workflow Run RO-Crate)

biffix describes every run as a [Workflow Run RO-Crate](https://www.researchobject.org/workflow-run-crate/)
conforming to the **Provenance Run Crate 0.6** profile (and so also Process Run Crate 0.6, Workflow
Run Crate 0.6 and Workflow RO-Crate 1.1), on RO-Crate 1.3.

The crate is derived from the run's event log, the same log the run page folds, so the two cannot
disagree. One implementation (`crates/biffix-backend/src/runs/provenance.rs`) serves both engines; engine
plugins such as nf-prov are not needed.

## Getting it

- **Download** (built from the log at that moment, so it includes every event received so far):
  the run page's **Provenance** button once the run has ended, or `GET /runs/{run_id}/provenance`
  at any time, including mid-run.
- **In S3, next to the run:** once a run has ended and its log has been quiet for 10 minutes, the
  backend sweep writes `s3://<project bucket>/biffix/runs/<run_id>/provenance/ro-crate-metadata.json`.
  A later event (a late reconcile, say) rewrites it. The copy travels with the run's data, so it is
  readable without biffix.

The crate is *detached*: it holds only `ro-crate-metadata.json`, and every data entity is the
run's S3 object, identified by its `s3://` URI. Nothing is copied.

## What it records

| Crate entity | From |
|---|---|
| `ComputationalWorkflow` (`mainEntity`) | the workflow version: its entrypoint at the pinned git sha (a GitHub URL for a GitHub source), its engine as `programmingLanguage`, its parameters as `FormalParameter`s |
| `ContainerImage` | the image the run's tasks ran in by default (`#image`), by `sha256` digest, with its ECR registry and repository where known: the digest the run launched with (for a Snakemake image the engine runtime was added to, the derived image, which `isBasedOn` the imported one), else the version's. Every other image in the run's image map is listed too, on the run's `CreateAction` |
| `CreateAction` for the run | the run: start/end, status, submitter as `agent`; `object` = inputs, parameters, reference-bundle files and samplesheets; `result` = published outputs, a resumed run's including those it reused from the runs it resumed (each described as reused, and naming the run that made it) |
| `File` inputs | each input URI, with `version` = its S3 version id (else ETag) when the run was submitted |
| `FormalParameter` + value | each parameter as submitted. One filled by input files (a URI, or a list of them, `multipleValues`) is realized by those `File`s, each `exampleOfWork` of it; any other by a `PropertyValue` whose `additionalType` is its data type (an S3 location that is not an input, such as an output directory, is `Text`). Secrets are the redacted placeholder, never the value |
| `SoftwareApplication` + `HowToStep` | each rule (Snakemake) or process (Nextflow); steps are numbered so producers come before consumers |
| `CreateAction` per task | each task (`task_key`): its winning attempt's times and status, the error for a failure, its outputs (intermediates included) as `result`, its resource usage, and `containerImage` when the run has one image (biffix does not record which image each task of a multi-image run used) |
| `ControlAction` per task | links the task's step to its execution |
| `OrganizeAction` | the engine run (Snakemake/Nextflow and its version), whose `result` is the run |
| `File` events | the run's archived engine event stream (`events.jsonl`) |

A crate lists at most 100,000 outputs; a run with more says so in the root dataset's
`description` (how many it has, how many are listed), and the rest are listed by
`POST /runs/{run_id}/outputs/query`.

S3 objects are identified by `version` (an input's S3 version id when it has one, else its ETag;
any other object's ETag) and size (`contentSize`); biffix does not read objects to compute `sha256`.

## biffix terms

Resource usage on a task's `CreateAction` uses these `propertyID`s.

### attempts

How many AWS Batch attempts the task took (retries included).

### instancetype

The EC2 instance type the task's winning attempt ran on.

### estimatedcost

The task's estimated cost in US dollars (`unitCode` QUDT `USD`): instance time at the on-demand or
spot rate, plus its EBS share. An estimate, not a bill.

### cached

`true` when the task was not run because its result came from the engine's cache.
