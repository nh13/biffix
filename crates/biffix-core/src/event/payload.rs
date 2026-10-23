//! The value types the kind payloads share: the open vocabularies a task attempt is described with
//! ([`TerminationCategory`], [`TaskVerdict`], [`OutputRole`]), a run's dependency graph ([`Dag`])
//! and the output objects tasks report ([`OutputRef`]).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

// Named only by doc links (`TaskVerdict`, `Dag`).
#[cfg(doc)]
use crate::event::EventData;

/// How a task attempt ended, as the translator read it from Batch's reason (what
/// [`crate::failure::diagnose`] weighs first).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminationCategory {
    Spot,
    Oom,
    HostTerminated,
    AppError,
    MissingOutput,
    #[serde(untagged)]
    Unknown(String),
}

crate::impl_open_enum_schema!(
    TerminationCategory,
    "TerminationCategory",
    "How a task attempt ended (open enum).",
    ["spot", "oom", "host_terminated", "app_error", "missing_output"]
);

impl TerminationCategory {
    /// The wire spelling (the serde tag; an unknown value verbatim).
    pub fn as_str(&self) -> &str {
        match self {
            TerminationCategory::Spot => "spot",
            TerminationCategory::Oom => "oom",
            TerminationCategory::HostTerminated => "host_terminated",
            TerminationCategory::AppError => "app_error",
            TerminationCategory::MissingOutput => "missing_output",
            TerminationCategory::Unknown(raw) => raw,
        }
    }

    /// The category spelled `s` (its serde form), or [`TerminationCategory::Unknown`].
    pub fn parse(s: &str) -> Self {
        [
            TerminationCategory::Spot,
            TerminationCategory::Oom,
            TerminationCategory::HostTerminated,
            TerminationCategory::AppError,
            TerminationCategory::MissingOutput,
        ]
        .into_iter()
        .find(|c| c.as_str() == s)
        .unwrap_or_else(|| TerminationCategory::Unknown(s.to_string()))
    }
}

/// The workflow engine's own verdict on a task attempt ([`EventData::EngineTaskVerdict`]). An open
/// enum: a value this build does not know parses as [`TaskVerdict::Unknown`] and folds to nothing.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskVerdict {
    /// The engine failed the attempt although its Batch job may have succeeded (Nextflow: missing
    /// declared outputs, an exit status outside `validExitStatus`).
    Failed,
    #[serde(untagged)]
    Unknown(String),
}

crate::impl_open_enum_schema!(
    TaskVerdict,
    "TaskVerdict",
    "The workflow engine's verdict on a task attempt (open enum).",
    ["failed"]
);

impl TaskVerdict {
    /// The wire spelling (the serde tag; an unknown value verbatim).
    pub fn as_str(&self) -> &str {
        match self {
            TaskVerdict::Failed => "failed",
            TaskVerdict::Unknown(raw) => raw,
        }
    }
}

/// A node in a run's group-level dependency graph (a Snakemake rule or a Nextflow process).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
pub struct DagNode {
    pub id: String,
}

/// A directed dependency edge `from → to` between two graph nodes.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
pub struct DagEdge {
    pub from: String,
    pub to: String,
}

/// A run's group-level dependency graph. Carried by [`EventData::RunDag`]; the reducer folds nodes
/// and edges as set-unions, so the projected graph is a pure function of the observed event set.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Dag {
    #[serde(default)]
    pub nodes: Vec<DagNode>,
    #[serde(default)]
    pub edges: Vec<DagEdge>,
}

/// What an output object is to the run (open enum; unknown strings are preserved). The reducer
/// keeps the highest-ranked role any event claimed for an object ([`OutputRole::rank`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputRole {
    /// A file the task declared as an output (Snakemake `output:`).
    Output,
    /// A file the engine published as a result (Nextflow `publishDir` / workflow outputs).
    Published,
    /// A task's work-dir output that feeds other tasks (Nextflow work dir), hidden by default.
    Intermediate,
    /// A task log file (Snakemake `log:`).
    Log,
    /// A task benchmark file (Snakemake `benchmark:`).
    Benchmark,
    #[serde(untagged)]
    Unknown(String),
}

crate::impl_open_enum_schema!(
    OutputRole,
    "OutputRole",
    "What an output object is to the run (open enum).",
    ["output", "published", "intermediate", "log", "benchmark"]
);

impl OutputRole {
    /// The canonical wire string (the serde tag).
    pub fn as_str(&self) -> &str {
        match self {
            OutputRole::Output => "output",
            OutputRole::Published => "published",
            OutputRole::Intermediate => "intermediate",
            OutputRole::Log => "log",
            OutputRole::Benchmark => "benchmark",
            OutputRole::Unknown(raw) => raw,
        }
    }

    /// Precedence when two events claim different roles for one object: a published result
    /// outranks a declared output, which outranks the task's log/benchmark files, which outrank a
    /// work-dir intermediate. Ties (two unknown roles) break on the string, so the merge is a total
    /// order and the fold stays order-independent.
    pub fn rank(&self) -> (u8, &str) {
        let r = match self {
            OutputRole::Published => 4,
            OutputRole::Output => 3,
            OutputRole::Log => 2,
            OutputRole::Benchmark => 1,
            OutputRole::Intermediate | OutputRole::Unknown(_) => 0,
        };
        (r, self.as_str())
    }
}

/// One output object a task wrote (or, for a cached task, was satisfied by).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct OutputRef {
    pub s3_uri: String,
    /// The engine's own name for the file (e.g. a Nextflow publish target relative to its publish
    /// dir), for display. Optional; the backend derives one from `s3_uri` and the run's output
    /// prefix when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub etag: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
}
