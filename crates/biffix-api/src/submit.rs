//! `submit` — launch a run (request) and its acknowledgement (response).

use serde::{Deserialize, Serialize};

use biffix_core::ids::{CoordinatorJobId, RunId, WorkflowVersionId};

/// `POST /runs` request: launch a frozen workflow version with resolved parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct RunSubmitRequest {
    /// The frozen workflow version id to launch (`{git_sha}:{content_hash}:{build}`).
    #[cfg_attr(feature = "openapi", schema(value_type = String))]
    pub workflow_version_id: WorkflowVersionId,
    /// Parameter values keyed by FormSchema field name: a JSON object (`null` means none given).
    pub params: serde_json::Value,
    /// Submit-time target to build (engine-agnostic): a Snakemake target rule/output, or a
    /// Nextflow `-entry` workflow. When set, overrides the version's registered target for this
    /// run; when absent, the version's baked target is used. Lets one frozen version drive
    /// different targets (e.g. a quick `smoke` vs the full `all`) without re-registering. One word
    /// of the engine's command line, so a 400 unless it reads as one target: Snakemake, a rule name
    /// or relative file path (ASCII letters, digits and `_ . - + /`, not starting with `-` or `/`);
    /// Nextflow, a workflow name (an identifier).
    #[serde(default)]
    pub target: Option<String>,
    /// Submit-time output prefix: the workflow's default storage location (Snakemake
    /// `--default-storage-prefix`; Nextflow work/publish dir). Must be within the project bucket and
    /// outside the prefixes biffix keeps for itself (`biffix/runs/`, `biffix/cache/`,
    /// `workflow-sources/`); a trailing `/` is added when missing.
    /// When set, outputs land at this caller-chosen stable prefix — so a series of runs can share
    /// one location and re-run incrementally (Snakemake skips outputs that already exist there),
    /// and downstream tooling finds them where it expects. When absent, falls back to the isolated
    /// per-run `biffix/runs/<run_id>/`. biffix's own run artifacts (events, task records) stay
    /// under `biffix/runs/<run_id>/` either way — only the workflow's data outputs follow this. One
    /// run at a time: a prefix another run is still using (or another submit is launching into) is
    /// a 409.
    #[serde(default)]
    pub output_prefix: Option<String>,
    /// Submit-time default retry count for failed tasks (e.g. spot reclaims). A default only: a
    /// Snakemake rule's own `retries:` and a Nextflow pipeline's own `errorStrategy`/`maxRetries`
    /// still win. Absent ⇒ the deploy's default (3). At most 10.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retries: Option<u32>,
    /// Submit-time keep-going: whether the rest of the DAG keeps running after a task fails
    /// (Snakemake `--keep-going`; Nextflow has no equivalent and ignores it). Absent ⇒ the
    /// deploy's default (true).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keep_going: Option<bool>,
    /// Resume a previous run: this run re-uses that run's output prefix and work dir, so tasks
    /// whose inputs and code are unchanged are satisfied from what it left (Snakemake from the
    /// metadata the prior run synced there; Nextflow also from its engine session's cache). The
    /// prior run must be a finished run of the same workflow (any version) and engine, and a
    /// Nextflow one must have a recorded session; `output_prefix`, if also given, must equal the
    /// prior run's. A resume is
    /// refused (409) while another run is still using that prefix. The server does not inherit
    /// params: `params` is the complete set (the CLI pre-fills it from the prior run).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(value_type = Option<String>))]
    pub resume_from: Option<RunId>,
    /// A human name for the run, shown in run lists and on the run page. At most 200 characters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_name: Option<String>,
    /// Samplesheet rows by param (`{column: value}` per row), for params whose form declares a
    /// samplesheet. biffix validates them, writes the sheet under the run's own prefix and sets the
    /// param to it; a value also given in `params` for that param is replaced.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tables:
        Option<std::collections::BTreeMap<String, Vec<std::collections::BTreeMap<String, String>>>>,
    /// A reference bundle id, for a version whose form has a reference dropdown. biffix fills the
    /// form's reference params from the bundle (replacing any value given for them in `params`) and
    /// refuses a bundle lacking a key the form requires.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
    /// What happens to the run's intermediate files once it ends (Nextflow runs only): `default`
    /// (the deploy's retention period), `delete_on_success` or `keep`. Results are never deleted;
    /// deleting a run's intermediate files means it can no longer be resumed. Only a run's own
    /// per-run prefix is ever swept, so `delete_on_success` is refused for a custom `output_prefix`
    /// (or a resume of a run in one).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(value_type = Option<String>))]
    pub retention: Option<biffix_core::event::Retention>,
    /// Container images a task may name that the version does not declare (repeatable
    /// references). Each is pinned at submit and imported (and, for a Snakemake version, given
    /// the version's runtime) like a declared image; until every one is runnable the run waits in
    /// `preparing_images`. One wins every spelling it has over a version image spelled the same;
    /// one given with a digest (`ubuntu:24.04@sha256:…`, how a re-run or resume gives its prior
    /// run's images) also wins its spelling without the digest (`ubuntu:24.04`).
    /// An image already imported into the deploy (registered, or named at an earlier submit, as a
    /// re-run or resume of that run does) needs only the role that may launch; one new to the
    /// deploy needs the Developer role, since importing it is what registration does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub images: Option<Vec<String>>,
    /// The run this one tries again (the Lab view's *Try again* re-run; a resume names it in
    /// `resume_from`). Its notification is marked read and dismissed for the caller, so it
    /// leaves *Needs attention*. Must be a run the caller can see.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_of: Option<String>,
    /// Launch the run into this experiment (of the version's project); recorded on the run from
    /// birth (`RunSubmitted.experiment_id`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub experiment: Option<String>,
}

/// `POST /runs` response: the new run's id and state, plus its coordinator job / assumed role once
/// it has launched.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct SubmittedRun {
    #[cfg_attr(feature = "openapi", schema(value_type = String))]
    pub run_id: RunId,
    /// `submitted` (launched now) or `preparing_images` (waiting for its submit-time images).
    pub state: String,
    /// Absent while the run is preparing images.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(value_type = Option<String>))]
    pub coordinator_job_id: Option<CoordinatorJobId>,
    /// Absent while the run is preparing images.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assumed_role_arn: Option<String>,
}
