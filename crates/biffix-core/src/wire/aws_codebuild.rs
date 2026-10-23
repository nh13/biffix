//! The CodeBuild wire event (`source: aws-codebuild`): a state change of one build biffix started.
//!
//! biffix builds images and engine runtimes with CodeBuild. Each build is started with
//! `BIFFIX_BUILD_SUBJECT` naming what it builds (`runtime:<key>`, `image:<version>/<name>`, …); every
//! state change EventBridge delivers carries the build's environment variables, so the subject is
//! the join key. A finished build also carries its buildspec's exported variables — its outputs
//! (an image digest, the architectures built).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::time::Timestamp;

/// The environment variable naming what a biffix build builds.
pub const SUBJECT_VARIABLE: &str = "BIFFIX_BUILD_SUBJECT";

/// One CodeBuild build state change.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CodeBuildEvent {
    pub ts: Timestamp,
    /// `detail.build-id`: the build's ARN.
    pub build_arn: String,
    /// `detail.project-name` (empty when the event did not name it).
    #[serde(default)]
    pub project: String,
    /// `detail.build-status`.
    pub status: CodeBuildStatus,
    /// What the build builds (its `BIFFIX_BUILD_SUBJECT`).
    pub subject: String,
    /// The buildspec's exported variables (present once the build has finished).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub outputs: BTreeMap<String, String>,
    /// Why a build did not succeed: the failed phase and its message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// A link to the build's log.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logs_url: Option<String>,
}

/// `detail.build-status` (open enum).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum CodeBuildStatus {
    InProgress,
    Succeeded,
    Failed,
    Fault,
    Stopped,
    TimedOut,
    #[serde(untagged)]
    Unknown(String),
}

impl CodeBuildStatus {
    /// The CodeBuild wire string.
    pub fn as_str(&self) -> &str {
        match self {
            Self::InProgress => "IN_PROGRESS",
            Self::Succeeded => "SUCCEEDED",
            Self::Failed => "FAILED",
            Self::Fault => "FAULT",
            Self::Stopped => "STOPPED",
            Self::TimedOut => "TIMED_OUT",
            Self::Unknown(raw) => raw,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_as_str_matches_serde() {
        for status in [
            CodeBuildStatus::InProgress,
            CodeBuildStatus::Succeeded,
            CodeBuildStatus::Failed,
            CodeBuildStatus::Fault,
            CodeBuildStatus::Stopped,
            CodeBuildStatus::TimedOut,
        ] {
            assert_eq!(serde_json::to_value(&status).unwrap(), serde_json::json!(status.as_str()));
        }
        let unknown: CodeBuildStatus = serde_json::from_value(serde_json::json!("QUEUED")).unwrap();
        assert_eq!(unknown, CodeBuildStatus::Unknown("QUEUED".into()));
    }
}
