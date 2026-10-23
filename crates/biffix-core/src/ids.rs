//! Correlation identifiers.
//!
//! There are several distinct id types in the contract ([`RunId`], [`TaskKey`], [`BatchJobId`],
//! [`InstanceId`], [`CoordinatorJobId`], [`WorkflowVersionId`]) and the [`Attempt`] counter. The
//! ids are all strings on the wire, but modelled as distinct newtypes so they cannot be
//! accidentally interchanged.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

macro_rules! string_id {
    ($(#[$m:meta])* $name:ident) => {
        $(#[$m])*
        #[derive(
            Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
        )]
        #[serde(transparent)]
        pub struct $name(pub String);

        impl $name {
            pub fn new(s: impl Into<String>) -> Self {
                Self(s.into())
            }
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl From<String> for $name {
            fn from(s: String) -> Self {
                Self(s)
            }
        }

        impl From<&str> for $name {
            fn from(s: &str) -> Self {
                Self(s.to_string())
            }
        }
    };
}

string_id!(
    /// biffix-minted run identifier. Nullable on the wire until attribution.
    RunId
);
string_id!(
    /// Stable logical-task identity: Snakemake `rule:sorted(wildcards)` / Nextflow the task name,
    /// `process (index)` untagged and `process (tag) #<index>` tagged (a tag need not be unique;
    /// the index is, and depends on no other task: nf-biffix `BiffixObserver.taskKey`).
    /// Survives retries — all attempts of one rule/process share it.
    TaskKey
);
string_id!(
    /// AWS Batch job id for a single attempt; the join key between the engine and AWS streams.
    BatchJobId
);
string_id!(
    /// EC2 instance id (spot attribution).
    InstanceId
);
string_id!(
    /// The coordinator (head job) Batch job id.
    CoordinatorJobId
);
string_id!(
    /// A frozen workflow version's immutable id, `{git_sha}:{content_hash}:{build}`.
    /// Distinct from its component `git_sha`/`content_hash`.
    WorkflowVersionId
);

/// 1-based attempt counter distinguishing retries of a single `task_key`.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(transparent)]
pub struct Attempt(pub u32);
