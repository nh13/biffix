//! What a run records about its samples (`RunSubmitted::samples`): the samplesheet's ids, the
//! version's [`SampleHint`] for matching tasks to them, and a resumed run's reused samples. Contract
//! types: the anchor carries them, and `sample_progress` attributes and counts with them.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

// Named only by the doc link of `SampleSpec::reused`.
#[cfg(doc)]
use crate::sample_progress::SampleGrid;

/// The author's hint for attributing tasks to samples (`samples:` in `biffix.yaml`), frozen with
/// the version's form. Every key is optional; without one the default rules apply.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SampleHint {
    /// Snakemake: the wildcard that names the sample. Trusted as given: its value is the sample
    /// even when the run's sheet does not list it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wildcard: Option<String>,
    /// Nextflow: a regex over the task's tag whose `sample` group is the sample id. Trusted as
    /// given (its match is the sample even when the sheet does not list it), and it overrides the
    /// default rules, file-name tags included.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
    /// Steps forced to one scope, e.g. `joint_call: study`. A step is named by its full group name
    /// or, for a Nextflow process path, its last `:`-separated part (`STAR_ALIGN` names
    /// `NFCORE_RNASEQ:RNASEQ:STAR_ALIGN`); the full name wins when both are given.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub steps: BTreeMap<String, StepScope>,
    /// Display names for steps, e.g. `STAR_ALIGN: "Align (STAR)"`, named as in `steps`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: BTreeMap<String, String>,
}

impl SampleHint {
    /// Whether the author gave no hint at all.
    pub fn is_empty(&self) -> bool {
        self == &SampleHint::default()
    }
}

/// Whether a step runs once per sample or once for the whole study (open enum).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepScope {
    Sample,
    Study,
    /// A scope this build does not know; preserved verbatim, acted on as no forcing at all.
    #[serde(untagged)]
    Unknown(String),
}

crate::impl_open_enum_schema!(
    StepScope,
    "StepScope",
    "Whether a step runs once per sample or once for the whole study (open enum).",
    ["sample", "study"]
);

/// What a run knows about its samples, captured at submit (`RunSubmitted::samples`): the sample
/// ids of the samplesheet biffix wrote from rows, else of the samplesheet given as an `s3://` file
/// when submit could read them (at most 1 MiB, in storage the run may read); and the version's
/// hint. `ids` is empty when neither applies: the set is then open, and a sample is whatever a
/// task key names.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SampleSpec {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ids: Vec<String>,
    #[serde(default, skip_serializing_if = "SampleHint::is_empty")]
    pub hint: SampleHint,
    /// A resumed run's samples as the run it resumes left them: each sample's finished per-sample
    /// steps there ([`SampleGrid::reused_by_resume`] of that run's grid, captured at submit). The
    /// grid shows them as reused where this run has no task of its own (a Snakemake resume creates
    /// no task for a finished step, so without these a sample it skipped entirely would vanish).
    /// Carried on the anchor rather than read from the other run at fold time, so the grid stays a
    /// function of this run's own log. Optional + additive: empty for a run that resumes nothing.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reused: Vec<ReusedSample>,
}

/// One sample as the run a resumed run resumes left it ([`SampleSpec::reused`]).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ReusedSample {
    pub id: String,
    /// The sample was done there.
    #[serde(default)]
    pub done: bool,
    /// Its finished per-sample steps there, by group, with how many tasks each had.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub steps: BTreeMap<String, i64>,
}
