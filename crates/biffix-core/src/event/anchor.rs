//! The run's provenance: the `run_submitted` anchor ([`RunSubmitted`]) and the facts it records,
//! and the `run_preparing` payload a run carries until the anchor supersedes it.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::event::SampleSpec;

// Named only by the doc link of `Retention`.
#[cfg(doc)]
use crate::event::EventData;

/// A samplesheet biffix wrote for a run (see `RunSubmitted::input_tables`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct InputTable {
    /// Where it was written (`s3://<project bucket>/biffix/runs/<run>/inputs/<param>.<ext>`).
    pub uri: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub etag: Option<String>,
    /// The rows, by column name.
    pub rows: Vec<std::collections::BTreeMap<String, String>>,
}

/// The reference bundle a run was launched with (see `RunSubmitted::reference_bundle`): its id and
/// the URIs it filled, by bundle key, as they were at submit.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ReferenceUse {
    pub id: String,
    pub uris: std::collections::BTreeMap<String, String>,
}

/// The engine a run was launched with (see `RunSubmitted::runtime`): the version's pinned release,
/// the runtime built for it, and the exact coordinator image (base image + that runtime) it ran on.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RuntimeUse {
    pub engine_version: String,
    /// sha256 of the runtime's recipe (`biffix_workflow::runtime::runtime_key`).
    pub runtime_key: String,
    /// `<repo>@sha256:…` of the coordinator variant.
    pub coordinator_image: String,
}

/// What happens to a run's intermediate files (a Nextflow run's `work/`) once it ends. Published
/// results are never deleted.
///
/// An open enum like the rest of the contract's vocabularies: it sits inside the `run_submitted`
/// anchor, so a value this build does not know (a newer backend's, read by an older one) must parse
/// as [`Retention::Unknown`] rather than degrade the whole anchor to [`EventData::Unknown`].
/// Consumers act on `Unknown` as [`Retention::Default`]: the projection stores the spelling
/// verbatim and the retention sweep treats anything but `keep`/`delete_on_success` as the default.
/// The control plane only ever *writes* a known value (submit rejects an unknown one).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Retention {
    /// Deleted the deploy's retention period after the run ends (kept forever when it has none).
    #[default]
    Default,
    /// Deleted as soon as the run succeeds; a failed run keeps them for the default period.
    DeleteOnSuccess,
    /// Never deleted.
    Keep,
    /// A value this build does not know; preserved verbatim, acted on as [`Retention::Default`].
    #[serde(untagged)]
    Unknown(String),
}

crate::impl_open_enum_schema!(
    Retention,
    "Retention",
    "What happens to a run's intermediate files once it ends (open enum).",
    ["default", "delete_on_success", "keep"]
);

impl Retention {
    /// The wire spelling (`default`, `delete_on_success`, `keep`, or an unknown value verbatim).
    pub fn as_str(&self) -> &str {
        match self {
            Retention::Default => "default",
            Retention::DeleteOnSuccess => "delete_on_success",
            Retention::Keep => "keep",
            Retention::Unknown(raw) => raw,
        }
    }
}

/// Parses a stored spelling (the inverse of [`Retention::as_str`]); never fails, since an unknown
/// spelling is [`Retention::Unknown`].
impl std::str::FromStr for Retention {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "default" => Retention::Default,
            "delete_on_success" => Retention::DeleteOnSuccess,
            "keep" => Retention::Keep,
            other => Retention::Unknown(other.to_string()),
        })
    }
}

/// How a run records a user who launched it ([`RunSubmitted::submitter`]): `user:<actor id>`.
/// Other submitters (`watcher:<id>`) are not users.
pub const USER_SUBMITTER_PREFIX: &str = "user:";

/// How a run records the user `actor_id` launched it as ([`RunSubmitted::submitter`]):
/// `user:<actor id>`, the actor id being what the backend's authorization returns (the token's
/// `sub`, or `local` with auth off). The one place the format is spelled ([`submitter_user`]
/// reads it back), so "my runs" matches what submit wrote.
pub fn user_submitter(actor_id: &str) -> String {
    format!("{USER_SUBMITTER_PREFIX}{actor_id}")
}

/// The user id of a run's submitter (`user:<id>` ⇒ `<id>`), or `None` when it was not a user.
pub(crate) fn submitter_user(submitter: &str) -> Option<&str> {
    submitter.strip_prefix(USER_SUBMITTER_PREFIX).filter(|u| !u.is_empty())
}

/// The `run_submitted` anchor's payload — the run's provenance. A named struct so the
/// control plane builds it with struct-update syntax from [`RunSubmitted::new`] (the required
/// fields) instead of patching a variant's `None`s after the fact.
///
/// Every field after `input_uris` is optional + additive: absent on anchors recorded
/// before it existed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RunSubmitted {
    pub workflow_version_id: String,
    pub git_sha: String,
    pub image_digest: String,
    pub params: serde_json::Value,
    pub input_uris: Vec<String>,
    /// Who/what triggered the run: `user:<id>` or `watcher:<id>`. Optional + additive.
    /// None ⇒ unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub submitter: Option<String>,
    /// A human label for the submitter (email/display name captured from the auth context at
    /// submit). Optional + additive; `None` when unknown (e.g. a watcher, or no email claim).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub submitter_label: Option<String>,
    /// Per-URI provenance tag (S3 `version-id`, else `ETag`, else empty) captured at the
    /// submit-time existence check, keyed by `input_uris` entry. Optional + additive,
    /// like `submitter`: `None` on events from before this field existed, or for a
    /// producer that has no tag to record (e.g. a prefix input has no ETag).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_provenance: Option<std::collections::BTreeMap<String, String>>,
    /// The effective target built for this run (engine-agnostic: a Snakemake target
    /// rule/output, or a Nextflow `-entry` workflow), captured at submit. A submit-time
    /// `--target` overrides the version's registered target; this records whichever was used,
    /// so two runs of one version that build different targets are distinguishable. Optional +
    /// additive: `None` on events from before this field existed, or when neither
    /// the submit nor the version set a target.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    /// The effective output prefix the workflow's outputs land under (the engine's default
    /// storage prefix), captured at submit: a submit-time `--output-prefix`, else the isolated
    /// per-run `s3://<bucket>/biffix/runs/<run_id>/`. Recorded so the run's output location is
    /// a fact of the log, not re-derived (which is wrong for an overridden prefix). Optional +
    /// additive: `None` on events from before this field existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_prefix: Option<String>,
    /// The engine session this run executes as. Nextflow: the session uuid biffix minted (a
    /// fresh run) or re-used (a resumed run) — the key under `<output_prefix>.nextflow/cache/`.
    /// Optional + additive: `None` for Snakemake, and on anchors from before this
    /// field existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine_session_id: Option<String>,
    /// The run this one resumes (`--resume-from`): it shares that run's output prefix, work dir
    /// and engine session. Optional + additive: `None` for a fresh run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resumed_from: Option<String>,
    /// The run's human name, given at submit (the UI pre-fills it from the workflow's
    /// template). Optional + additive: `None` when the submitter gave none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_name: Option<String>,
    /// Params the submit dropped because their `show_if` conditions hid them (the form's
    /// active set), so the record shows what was given but not used. Optional + additive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dropped_inactive: Option<Vec<String>>,
    /// Samplesheets biffix wrote for this run from rows given at submit, by param: where each
    /// was written and its rows, so the run can be re-run with the same samples. Optional +
    /// additive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tables: Option<std::collections::BTreeMap<String, InputTable>>,
    /// The reference bundle chosen at submit and the URIs it filled (a bundle can be edited
    /// later, so the run keeps what it used). Optional + additive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference_bundle: Option<ReferenceUse>,
    /// How many samples the run was launched with: those of the samplesheet biffix wrote for it
    /// from rows (the first, when there are several), else of the samplesheet given as an `s3://`
    /// file when submit could read its sample ids (at most 1 MiB, in storage the run may read).
    /// Absent when neither applies. Optional + additive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sample_count: Option<u32>,
    /// The run's samples and the version's hint for matching tasks to them (the Lab view's
    /// per-sample progress; see [`crate::sample_progress`]). Optional + additive: `None` on
    /// anchors from before the field, which then match tasks by the default rules alone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub samples: Option<SampleSpec>,
    /// What happens to the run's intermediate files once it ends (see [`Retention`]). Absent ⇒
    /// the deploy's default. Optional + additive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retention: Option<Retention>,
    /// The engine runtime the run's coordinator ran with (see [`RuntimeUse`]). Optional +
    /// additive: `None` on anchors from before pinned engine runtimes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<RuntimeUse>,
    /// The digest of the default image as imported, when the run's tasks ran an image derived
    /// from it (a Snakemake image with the version's engine runtime added); `image_digest` is
    /// then the derived image's. Optional + additive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_source_digest: Option<String>,
    /// The run's image map: each declared image reference (as written and normalized) to the
    /// tenant image its tasks ran (`images.json`). Optional + additive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_map: Option<std::collections::BTreeMap<String, String>>,
    /// The images given at submit (`--image`), as pinned; their keys are in `image_map`, where
    /// they win over a version image spelled the same. Optional + additive: `None` when the
    /// submit named none, and on anchors from before submit-time images.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub submit_images: Option<Vec<SubmitImage>>,
    /// The experiment the run was launched into (`RunSubmitRequest.experiment`); a later
    /// `experiment_set` moves it. Optional + additive: `None` when launched into none, and on
    /// anchors from before experiments.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub experiment_id: Option<String>,
}

/// An image given at submit, pinned: the reference as the submitter wrote it, the digest-pinned
/// source it was resolved to, and the content key its import is built under.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SubmitImage {
    pub declared_ref: String,
    pub source_ref: String,
    pub content_key: String,
}

/// The `run_preparing` payload: a run accepted at submit that waits for its submit-time images
/// before it launches. The provisional facts the run list and page need until the
/// `run_submitted` anchor (recorded at launch) supersedes them. `params` are redacted.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RunPreparing {
    pub workflow_version_id: String,
    pub params: serde_json::Value,
    pub input_uris: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub submitter: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub submitter_label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_prefix: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resumed_from: Option<String>,
    /// The submit-time images the run waits for, as pinned.
    pub images: Vec<SubmitImage>,
    /// The experiment the run was launched into, as the anchor will record it. Optional +
    /// additive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub experiment_id: Option<String>,
}

impl RunSubmitted {
    /// An anchor with its required fields set and every optional field unset.
    pub fn new(
        workflow_version_id: impl Into<String>,
        git_sha: impl Into<String>,
        image_digest: impl Into<String>,
        params: serde_json::Value,
        input_uris: Vec<String>,
    ) -> Self {
        RunSubmitted {
            workflow_version_id: workflow_version_id.into(),
            git_sha: git_sha.into(),
            image_digest: image_digest.into(),
            params,
            input_uris,
            submitter: None,
            submitter_label: None,
            input_provenance: None,
            target: None,
            output_prefix: None,
            engine_session_id: None,
            resumed_from: None,
            run_name: None,
            dropped_inactive: None,
            input_tables: None,
            reference_bundle: None,
            sample_count: None,
            samples: None,
            retention: None,
            runtime: None,
            image_source_digest: None,
            image_map: None,
            submit_images: None,
            experiment_id: None,
        }
    }
}
